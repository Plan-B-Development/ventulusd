//! Hardware diagnostics endpoint.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;

use super::{error_response, json_ok, AppState};
use crate::api::diagnostics;
use crate::api::responses::*;

/// The primary AMD GPU's diagnostics (`select_primary_gpu`), with the kernel
/// advisories for every AMD GPU on the machine (DEC-449, `BRD-q`).
///
/// Takes the whole list and selects the primary card itself, so a caller cannot
/// hand the advisories the primary card alone. `kernel_release` is injected so a
/// test can pick an affected one.
fn amd_gpu_diagnostics(
    gpus: &[crate::hwmon::gpu_detect::AmdGpuInfo],
    amd_pci_raw: &[crate::hwmon::gpu_detect::AmdPciDevice],
    kernel_release: Option<&str>,
) -> Option<GpuDiagnostics> {
    crate::hwmon::gpu_detect::select_primary_gpu(gpus).map(|gpu| {
        let ppfeaturemask = diagnostics::read_ppfeaturemask();
        let bit14_set = ppfeaturemask
            .as_ref()
            .map(|s| {
                let trimmed = s.trim().strip_prefix("0x").unwrap_or(s.trim());
                u32::from_str_radix(trimmed, 16)
                    .map(|v| (v & 0x4000) != 0)
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        // DEC-119: firmware-enforced OD_RANGE fan-speed bounds (the ~15% min
        // on RDNA3+ that the user perceives as a "minimum"). Read on demand —
        // diagnostics already runs on the blocking pool.
        let (fan_speed_min_pct, fan_speed_max_pct) = gpu
            .fan_curve_path
            .as_ref()
            .and_then(|p| crate::hwmon::gpu_fan::read_fan_curve(p).ok())
            .and_then(|c| c.speed_range)
            .map_or((None, None), |(lo, hi)| (Some(lo), Some(hi)));

        // Best-effort PMFW fan_minimum_pwm (optional attribute).
        let fan_minimum_pwm = gpu
            .fan_minimum_pwm_path()
            .as_deref()
            .and_then(crate::hwmon::gpu_fan::read_fan_minimum_pwm);

        // Kernel-regression advisories (same catalog as
        // /capabilities.amd_gpu.kernel_warnings, duplicated for the bundle) —
        // for every AMD GPU, each message naming its card (DEC-449, `BRD-q`).
        let kernel_warnings = kernel_release
            .map(|r| crate::hwmon::kernel_warnings::detect_kernel_warnings(r, gpus))
            .unwrap_or_default();

        // Driver-bound status cross-referenced from the PCI scan; an hwmon
        // node implies a bound driver, so default to true if the BDF is
        // somehow absent from the PCI listing.
        let amdgpu_driver_bound = amd_pci_raw
            .iter()
            .find(|d| d.pci_bdf == gpu.pci_bdf)
            .is_none_or(|d| d.amdgpu_bound());

        GpuDiagnostics {
            pci_bdf: gpu.pci_bdf.clone(),
            // M11: emit the same BDF under both names so callers aligned to
            // `/capabilities.amd_gpu.pci_id` can use the identical field here.
            pci_id: gpu.pci_bdf.clone(),
            pci_device_id: gpu.pci_device_id,
            pci_revision: gpu.pci_revision,
            model_name: gpu.marketing_name.clone(),
            fan_control_method: gpu.fan_control_method().to_string(),
            overdrive_enabled: gpu.overdrive_enabled,
            ppfeaturemask,
            ppfeaturemask_bit14_set: bit14_set,
            zero_rpm_available: gpu.fan_zero_rpm_path.is_some(),
            fan_speed_min_pct,
            fan_speed_max_pct,
            fan_minimum_pwm,
            amdgpu_driver_bound,
            kernel_warnings,
        }
    })
}

/// GET /diagnostics/hardware — comprehensive hardware readiness report.
///
/// The report performs ~6 blocking sysfs/procfs reads (modules, ioports, DMI,
/// cpuinfo, kmsg, ppfeaturemask), so it runs on the blocking pool rather than
/// stalling a Tokio worker — mirroring the OpenFan write handlers (DEC-099).
pub async fn hardware_diagnostics_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    match tokio::task::spawn_blocking(move || build_hardware_diagnostics(&state)).await {
        Ok(resp) => resp,
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &ErrorEnvelope::internal(format!("hardware diagnostics task failed: {e}")),
        ),
    }
}

/// Build the hardware-readiness report. Synchronous and blocking — invoked via
/// `spawn_blocking` from the handler above.
/// One `chips_detected[]` entry per `(chip, device)` that has headers.
///
/// `observed` is the bound-driver scan (`BRD-g`, DEC-469), taken by the caller
/// before it locks the controller so no sysfs read happens under that lock.
/// `bound_driver` is published only where the scan saw one, and
/// `in_mainline_kernel` follows it where it contradicts the name's guess.
fn chips_detected(
    headers: &[&crate::hwmon::pwm_discovery::PwmHeaderDescriptor],
    observed: &[crate::hwmon::bound_driver::ObservedDriver],
) -> Vec<HwmonChipInfo> {
    // Keyed by the canonical chip name (DEC-442); the sysfs spelling rides
    // beside it and is the same for every header of one chip.
    let mut chip_map: HashMap<(String, String), usize> = HashMap::new();
    let mut sysfs_names: HashMap<(String, String), String> = HashMap::new();
    for h in headers {
        let key = (h.chip_name.clone(), h.device_id.clone());
        sysfs_names
            .entry(key.clone())
            .or_insert_with(|| h.sysfs_chip_name().to_string());
        *chip_map.entry(key).or_insert(0) += 1;
    }

    chip_map
        .into_iter()
        .map(|((chip_name, device_id), count)| {
            let bound_driver =
                crate::hwmon::bound_driver::driver_for_device(observed, &chip_name, &device_id)
                    .map(str::to_string);
            let in_mainline =
                diagnostics::chip_driver_in_mainline_bound(&chip_name, bound_driver.as_deref());
            let sysfs_chip_name = sysfs_names
                .remove(&(chip_name.clone(), device_id.clone()))
                .unwrap_or_else(|| chip_name.clone());
            HwmonChipInfo {
                expected_driver: diagnostics::expected_driver(&chip_name).to_string(),
                chip_name,
                sysfs_chip_name,
                device_id,
                bound_driver,
                in_mainline_kernel: in_mainline,
                header_count: count,
            }
        })
        .collect()
}

fn build_hardware_diagnostics(state: &AppState) -> (StatusCode, Json<serde_json::Value>) {
    let observed = crate::hwmon::bound_driver::scan_bound_drivers(std::path::Path::new(
        crate::hwmon::HWMON_SYSFS_ROOT,
    ));
    let chips_detected: Vec<HwmonChipInfo> = match state.hwmon_controller {
        Some(ref controller) => {
            chips_detected(&controller.headers().iter().collect::<Vec<_>>(), &observed)
        }
        None => Vec::new(),
    };

    let total_headers = chips_detected.iter().map(|c| c.header_count).sum::<usize>();
    let writable_headers = state
        .hwmon_controller
        .as_ref()
        .map(|c| c.headers().iter().filter(|h| h.is_writable).count())
        .unwrap_or(0);

    // DEC-119: PCI-space scan for AMD VGA devices + driver binding. Done
    // independently of the hwmon scan so a GPU whose amdgpu driver did not
    // bind (blacklist, KMS failure, vfio-pci passthrough) is still reported —
    // such a device has no hwmon node and is absent from `gpu` below.
    let amd_pci_raw = crate::hwmon::gpu_detect::detect_amd_pci_devices();
    let amdgpu_module_loaded = crate::hwmon::gpu_detect::amdgpu_module_loaded();
    let amd_pci_devices: Vec<AmdPciDeviceInfo> = amd_pci_raw
        .iter()
        .map(|d| AmdPciDeviceInfo {
            pci_bdf: d.pci_bdf.clone(),
            pci_device_id: d.pci_device_id,
            driver: d.driver.clone(),
            amdgpu_bound: d.amdgpu_bound(),
            hwmon_present: state.amd_gpus.iter().any(|g| g.pci_bdf == d.pci_bdf),
        })
        .collect();

    // Kernel release read once and reused for the GPU advisories and the report.
    let kernel_release = crate::hwmon::kernel_warnings::read_kernel_release();

    // GPU diagnostics from detected GPUs
    let gpu_diag = amd_gpu_diagnostics(&state.amd_gpus, &amd_pci_raw, kernel_release.as_deref());

    // Intel discrete GPU diagnostics (DEC-121). Read-only — the note explains
    // why fan control is unavailable, grounded in the kernel ABI / firmware.
    let intel_gpu_diag =
        crate::hwmon::intel_gpu_detect::select_primary_intel_gpu(&state.intel_gpus).map(|gpu| {
            IntelGpuDiagnostics {
                pci_bdf: gpu.pci_bdf.clone(),
                pci_id: gpu.pci_bdf.clone(),
                pci_device_id: gpu.pci_device_id,
                pci_revision: gpu.pci_revision,
                model_name: gpu.marketing_name.clone(),
                driver: gpu.driver.clone(),
                fan_control_method: gpu.fan_control_method().to_string(),
                fan_rpm_available: gpu.has_fan_rpm,
                fan_control_note:
                    "Intel GPU fan control is managed autonomously by on-card firmware: the \
                     mainline xe/i915 drivers register no PWM interface, so nothing can set \
                     the fan on a stock kernel. Temperature and fan RPM are read-only."
                        .to_string(),
            }
        });

    // NVIDIA discrete GPU diagnostics (DEC-204). Read-only — the note explains
    // why fan control is unavailable for both driver legs.
    let nvidia_gpu_diag =
        crate::hwmon::nvidia::select_primary_nvidia_gpu(&state.nvidia_gpus).map(|gpu| {
            NvidiaGpuDiagnostics {
                pci_bdf: gpu.pci_bdf.clone(),
                pci_id: gpu.pci_bdf.clone(),
                model_name: gpu.model_name.clone(),
                driver: gpu.driver.to_string(),
                driver_version: gpu.driver_version.clone(),
                fan_control_method: gpu.fan_control_method().to_string(),
                fan_rpm_available: gpu.fan_rpm_available,
                fan_control_note:
                    "NVIDIA GPU fan control is not exposed to this daemon: the open nouveau \
                     driver's writable pwm1 (some cards older than Turing) is deliberately \
                     excluded for safety, and the proprietary NVML backend is read-only \
                     telemetry, whose fan duty and RPM are the driver's intended speed, not \
                     a measurement. Temperature and fan telemetry are read-only."
                        .to_string(),
            }
        });

    // Thermal safety — report thresholds and whether a CPU sensor is USABLE.
    //
    // DEC-269: "present" is the wrong question now that the safety rule filters
    // by age (DEC-267). Answering it from the raw snapshot made
    // `{"state": "no_sensor_fallback", "cpu_sensor_found": true}` reachable —
    // a self-contradicting response, rendered by the GUI as one line reading
    // "State: no_sensor_fallback · CPU sensor: true". Apply the same freshness
    // budget the rule applies, so this field answers the question the state
    // beside it was decided on.
    let snap = state.cache.snapshot();
    let cpu_sensor_found = !matches!(
        crate::profile_engine::hottest_cpu_reading(
            &snap.sensors,
            std::time::Instant::now(),
            state.cache.cpu_temp_stale_after(),
        ),
        crate::profile_engine::CpuReading::Absent | crate::profile_engine::CpuReading::Stale(_)
    );

    let thermal_state = snap.thermal_override_state.as_deref().unwrap_or("normal");

    let thermal_safety = ThermalSafetyInfo {
        state: thermal_state.to_string(),
        cpu_sensor_found,
        // DEC-292: read the single source, never a literal. These were bare
        // the threshold values here, so moving the trip point in `safety.rs` would
        // have left the daemon REPORTING the old value while ACTING on the new
        // one — and the GUI renders this field verbatim as "Limit: N °C".
        //
        // DEC-308 makes that invariant load-bearing rather than merely tidy: the
        // trip point is now derived per machine from the CPU's own reported design
        // ceiling, so the constant is only the floor. Report what the engine
        // actually acted on, published in the same write as the state beside it,
        // and fall back to the constant only before the first tick.
        emergency_threshold_c: snap
            .thermal_emergency_trigger_c
            .unwrap_or(crate::constants::THERMAL_EMERGENCY_TRIGGER_C),
        release_threshold_c: crate::constants::THERMAL_EMERGENCY_RELEASE_C,
        // DEC-443: the coolant thresholds the engine acted on, published in the
        // same write as `state`; the limit in force before the first tick.
        coolant_limit_c: snap
            .coolant_limit_c
            .unwrap_or_else(|| f64::from(state.cache.coolant_limit_c())),
        coolant_release_c: snap.coolant_release_c.unwrap_or_else(|| {
            f64::from(state.cache.coolant_limit_c()) - crate::constants::COOLANT_RELEASE_MARGIN_C
        }),
    };

    // Kernel module detection
    let kernel_modules = diagnostics::detect_loaded_modules();

    // ACPI conflict detection
    let acpi_conflicts = diagnostics::detect_acpi_conflicts();

    // Revert counts from pwm_enable watchdog
    let (enable_revert_counts, enable_revert_last_seen_ms) = state
        .hwmon_controller
        .as_ref()
        .map(|c| {
            // One lock, both maps: taken separately they could straddle a
            // reclaim and publish a count without the age that dates it. On
            // the blocking pool (the handler's `spawn_blocking`), not a worker.
            let ctrl = c.controller().lock();
            let now = std::time::Instant::now();
            (
                ctrl.enable_revert_counts().clone(),
                ctrl.enable_revert_ages_ms(now),
            )
        })
        .unwrap_or_default();

    // DMI board identification
    let board = diagnostics::read_board_info();

    // DEC-110: CPU vendor — lets the GUI scope Intel-vs-AMD platform
    // quirks on boards from vendors that ship both (MSI, ASUS, ASRock,
    // Gigabyte). Empty string when /proc/cpuinfo is unreadable or the
    // vendor_id is unknown (hypervisors etc.).
    let cpu_vendor = diagnostics::read_cpu_vendor();

    // DEC-101: dual-chip detection support. `expected_chips` is the
    // deterministic DMI-board lookup; `kernel_detected_chips` is the
    // best-effort kmsg parse. Both fields default to empty Vec on
    // failure paths and are skipped from the wire when empty so older
    // clients ignore them.
    // `DC-cp`: resolved against the chips bound here, the same list a client
    // compares `expected_chips` with, so an accepted alternative primary that
    // is the one bound is not reported as a missing chip.
    let bound_names: Vec<&str> = chips_detected
        .iter()
        .map(|c| c.chip_name.as_str())
        .collect();
    let expected_chips =
        diagnostics::expected_chips_for_board(&board.vendor, &board.name, &bound_names);
    // `BRD-j`: the subset of `expected_chips` that carries no fan header, from
    // the same table row — so a client can word a missing one as lost
    // temperatures and voltages rather than lost fan headers.
    let expected_fanless_chips = diagnostics::fanless_chips_for_board(&board.vendor, &board.name);
    let kernel_detected_chips = diagnostics::read_kernel_detected_chips();

    // `X87-d`: the board's own firmware-declared counts, where `it87` exports
    // them. Read unconditionally rather than gated on the DMI vendor — the file
    // exists only on boards whose driver published it, so its presence IS the
    // detection, and gating on a vendor string would reintroduce the DMI
    // dependency this field exists to stop relying on. Read-only sysfs: one
    // small file, no port I/O, unaffected by the port-probe gate.
    // Read as the raw word once: the decoded counts and the voltage catalogue
    // key (`VOLT-b`) are then two views of one read, and cannot disagree.
    let siv_word = crate::hwmon::gigabyte_siv::read_siv_word(std::path::Path::new(
        crate::hwmon::gigabyte_siv::GIGABYTE_SIV_PATH,
    ));
    let board_firmware_counts = siv_word.and_then(crate::hwmon::gigabyte_siv::parse_siv_word);

    // DEC-105 / DEC-106: known-bad simultaneous-load detection. The
    // flagship case is (nct6687, nct6775) — both must never be loaded at
    // the same time on a SINGLE-chip board with NCT6797D because they
    // overlap on chip ID 0xd450 and either can corrupt the chip's
    // non-volatile fan registers. DEC-106 refinement: when chips_detected
    // contains two distinct nct6 chips (e.g. ASRock X870E Taichi Lite
    // has NCT6686 + NCT6799 at separate addresses), each driver legitimately
    // owns its chip and the collision is suppressed.
    let chip_bindings: Vec<diagnostics::ChipBinding<'_>> = chips_detected
        .iter()
        .map(|c| diagnostics::ChipBinding {
            chip_name: c.chip_name.as_str(),
            device_id: c.device_id.as_str(),
        })
        .collect();
    let module_collisions = diagnostics::detect_module_collisions(&chip_bindings);

    // `WIRE-ag`: board voltage rails, named from the board catalogue where it
    // covers this board (`VOLT-b`) — keyed on the same `cpu_vendor` and SIV
    // word this response publishes.
    let voltages = voltage_entries(
        std::path::Path::new(crate::hwmon::HWMON_SYSFS_ROOT),
        &cpu_vendor,
        siv_word,
    );

    // DEC-500: the catalogue's names for this board's channels, under the key
    // the controller was built with (`AppState::board_context`), so a header's
    // entry always repeats what `/hwmon/headers` publishes — even after an SIV
    // appears or goes mid-session, which `board_firmware_counts` above, a fresh
    // read, does show. Headers carry what discovery decided (it is what their
    // role was inferred from); monitor-only fans (`/inventory/hwmon`) and
    // temperatures (`/sensors`) are named here. Skipped — no scan — on a board
    // with no SIV.
    let board_context = &state.board_context;
    let board_layout = if board_context.siv.is_some() {
        let headers: Vec<&crate::hwmon::pwm_discovery::PwmHeaderDescriptor> =
            match state.hwmon_controller {
                Some(ref controller) => controller.headers().iter().collect(),
                None => Vec::new(),
            };
        let fans = crate::hwmon::inventory::discover_monitor_only_fans(std::path::Path::new(
            crate::hwmon::HWMON_SYSFS_ROOT,
        ))
        .unwrap_or_else(|e| {
            log::warn!("Monitor-only fan discovery failed: {e}");
            Vec::new()
        });
        let temps: Vec<(&str, &str, &str)> = snap
            .sensors
            .values()
            .map(|r| (r.id.as_str(), r.chip_name.as_str(), r.label.as_str()))
            .collect();
        board_layout(board_context, &board.name, &headers, &fans, &temps)
    } else {
        None
    };

    json_ok(
        StatusCode::OK,
        HardwareDiagnosticsResponse {
            api_version: API_VERSION,
            hwmon: HwmonDiagnostics {
                chips_detected,
                total_headers,
                writable_headers,
                enable_revert_counts,
                enable_revert_last_seen_ms,
            },
            gpu: gpu_diag,
            intel_gpu: intel_gpu_diag,
            nvidia_gpu: nvidia_gpu_diag,
            thermal_safety,
            kernel_modules,
            acpi_conflicts,
            board,
            // DEC-405: the same read the GPU advisories above use, now also
            // published — capped like every other environment fact.
            kernel_release: kernel_release
                .as_deref()
                .and_then(crate::hwmon::chip_db::cap_env_fact),
            expected_chips,
            expected_fanless_chips,
            board_firmware_counts,
            kernel_detected_chips,
            module_collisions,
            cpu_vendor,
            amd_pci_devices,
            amdgpu_module_loaded,
            voltages,
            board_layout,
        },
    )
}

/// The `board_layout` object (DEC-500): this board's SIV, whether upstream
/// lists the board for it, and the catalogue's verdict on each discovered
/// channel it covers.
///
/// `None` without an SIV or a known CPU vendor — the catalogue is keyed on
/// both, and a layout object with no key would claim a match nobody made.
/// `temps` is `(id, canonical chip, label)` per published sensor. A channel is
/// named only where its driver published no label: a header's `board` was
/// decided at discovery under that rule, and a monitor-only fan or temperature
/// qualifies here only when its label is the synthesised `fanN` / `tempN`.
fn board_layout(
    board: &crate::hwmon::board_catalogue::BoardContext,
    dmi_board_name: &str,
    headers: &[&crate::hwmon::pwm_discovery::PwmHeaderDescriptor],
    fans: &[crate::hwmon::inventory::FanInputDescriptor],
    temps: &[(&str, &str, &str)],
) -> Option<BoardLayout> {
    use crate::hwmon::board_catalogue::{self, ChannelKind};
    let siv = board.siv?;
    let platform = match board.cpu_vendor.as_str() {
        "AMD" | "Intel" => board.cpu_vendor.clone(),
        _ => return None,
    };
    let listed = board_catalogue::listed_boards(&platform, siv);
    let dmi = dmi_board_name.trim();
    let board_listed = !dmi.is_empty() && listed.iter().any(|n| n.eq_ignore_ascii_case(dmi));

    let mut channels: Vec<BoardLayoutChannel> = headers
        .iter()
        .filter_map(|h| Some(BoardLayoutChannel::new(&h.id, "pwm", h.board?)))
        .collect();
    for f in fans {
        if f.label == format!("fan{}", f.fan_index) {
            if let Some(c) = board.channel(&f.chip_name, ChannelKind::Fan, f.fan_index) {
                channels.push(BoardLayoutChannel::new(&f.id, "fan", c));
            }
        }
    }
    for &(id, chip, label) in temps {
        let index = label
            .strip_prefix("temp")
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| label == format!("temp{n}"));
        if let Some(c) = index.and_then(|n| board.channel(chip, ChannelKind::Temp, n)) {
            channels.push(BoardLayoutChannel::new(id, "temp", c));
        }
    }
    channels.sort_by(|a, b| a.id.cmp(&b.id));

    Some(BoardLayout {
        siv: format!("{siv:08X}"),
        in_catalogue: board_catalogue::knows_siv(&platform, siv),
        platform,
        catalogue_commit: board_catalogue::UPSTREAM_COMMIT.to_string(),
        listed_boards: listed.iter().map(|n| n.to_string()).collect(),
        board_listed,
        channels,
    })
}

/// The `voltages` array: discovery, then the board catalogue (`VOLT-b`,
/// DEC-464), then the wire shape.
///
/// Read-only sysfs on the same hwmon tree this handler already walks — no port
/// I/O, unaffected by the port-probe gate, and on the `spawn_blocking` side like
/// every other read here. A failed scan degrades to an empty list: a rail
/// display is the least important thing on this response and must never fail
/// the whole report.
fn voltage_entries(
    hwmon_root: &std::path::Path,
    cpu_vendor: &str,
    siv_word: Option<u32>,
) -> Vec<VoltageEntry> {
    let mut rails = crate::hwmon::voltages::discover_voltages(hwmon_root).unwrap_or_else(|e| {
        log::warn!("Voltage rail discovery failed: {e}");
        Vec::new()
    });
    crate::hwmon::voltages::apply_board_catalogue(&mut rails, cpu_vendor, siv_word);
    rails.into_iter().map(VoltageEntry::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BRD-g`: the observed driver reaches `chips_detected[]`, and the
    /// mainline answer follows it where the name guessed wrong. Driven from a
    /// fake sysfs tree through the real scan, so the join on `device_id` is
    /// the one production performs.
    #[test]
    fn chips_detected_publishes_the_bound_driver_and_corrects_mainline() {
        use crate::hwmon::pwm_discovery::PwmHeaderDescriptor;
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let class = root.path().join("class");
        for (dir, name, device, driver) in [
            ("hwmon3", "nct6687", "nct6683.2592", Some("nct6683")),
            ("hwmon4", "it8696", "it87.2624", None),
        ] {
            let hw = class.join(dir);
            let dev = root.path().join("devices").join(device);
            std::fs::create_dir_all(&hw).unwrap();
            std::fs::create_dir_all(&dev).unwrap();
            std::fs::write(hw.join("name"), format!("{name}\n")).unwrap();
            symlink(&dev, hw.join("device")).unwrap();
            if let Some(d) = driver {
                let drv = root.path().join("drivers").join(d);
                std::fs::create_dir_all(&drv).unwrap();
                symlink(&drv, dev.join("driver")).unwrap();
            }
        }
        let observed = crate::hwmon::bound_driver::scan_bound_drivers(&class);
        let header = |chip: &str, dir: &str, idx: u8| PwmHeaderDescriptor {
            chip_name: chip.into(),
            device_id: crate::hwmon::discovery::device_id_for_hwmon_dir(&class.join(dir)),
            pwm_index: idx,
            ..Default::default()
        };
        let headers = [
            header("nct6687", "hwmon3", 1),
            header("nct6687", "hwmon3", 2),
            header("it8696", "hwmon4", 1),
        ];
        let refs: Vec<&PwmHeaderDescriptor> = headers.iter().collect();
        let chips = chips_detected(&refs, &observed);

        let nct = chips.iter().find(|c| c.chip_name == "nct6687").unwrap();
        assert_eq!(nct.expected_driver, "nct6687", "the name still guesses");
        assert_eq!(nct.bound_driver.as_deref(), Some("nct6683"));
        assert!(
            !diagnostics::chip_driver_in_mainline_bound("nct6687", None),
            "precondition: by name alone this chip is out-of-tree"
        );
        assert!(nct.in_mainline_kernel, "the in-kernel nct6683 bound it");
        assert_eq!(nct.header_count, 2);

        let ite = chips.iter().find(|c| c.chip_name == "it8696").unwrap();
        assert_eq!(ite.bound_driver, None, "no driver link → not observed");
        assert!(!ite.in_mainline_kernel, "it8696 stays DKMS-only by name");
        let wire = serde_json::to_value(ite).unwrap();
        assert!(
            wire.get("bound_driver").is_none(),
            "absent, not null: {wire}"
        );
    }

    /// `VOLT-b`: the handler's builder carries the catalogue through to the
    /// wire — the arm only a wired call site can produce is a named, scaled
    /// rail and an unmapped one; with no SIV the same tree reports neither.
    #[test]
    fn voltage_entries_carry_the_board_catalogue_onto_the_wire() {
        let root = tempfile::tempdir().unwrap();
        for (dir, chip, rails) in [
            (
                "hwmon4",
                "it8696",
                &[(2u8, "1992", None), (7, "3288", Some("3VSB"))][..],
            ),
            ("hwmon5", "it87952", &[(0u8, "1804", None)][..]),
        ] {
            let d = root.path().join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("name"), format!("{chip}\n")).unwrap();
            for (ch, mv, label) in rails {
                std::fs::write(d.join(format!("in{ch}_input")), mv).unwrap();
                if let Some(l) = label {
                    std::fs::write(d.join(format!("in{ch}_label")), l).unwrap();
                }
            }
        }
        let find = |entries: &[VoltageEntry], chip: &str, ch: u8| {
            serde_json::to_value(
                entries
                    .iter()
                    .find(|e| e.chip_name == chip && e.channel == ch)
                    .unwrap(),
            )
            .unwrap()
        };

        let named = voltage_entries(root.path(), "AMD", Some(0xA008_090A));
        let twelve = find(&named, "it8696", 2);
        assert_eq!(twelve["board_label"], "+12V");
        assert_eq!(twelve["board_multiplier"], 6.0);
        assert_eq!(
            twelve["label"], "in2",
            "the driver's own fields are unchanged"
        );
        assert_eq!(find(&named, "it87952", 0)["board_unmapped"], true);
        let vsb = find(&named, "it8696", 7);
        assert!(vsb.get("board_label").is_none() && vsb.get("board_unmapped").is_none());

        let plain = voltage_entries(root.path(), "AMD", None);
        for entry in &plain {
            let v = serde_json::to_value(entry).unwrap();
            assert!(v.get("board_label").is_none(), "{v}");
            assert!(v.get("board_unmapped").is_none(), "{v}");
        }
    }

    /// The `BRD-q` machine: an RDNA2 discrete card, which `select_primary_gpu`
    /// puts first, and an RDNA3 iGPU behind it.
    fn rdna2_card_and_rdna3_igpu() -> Vec<crate::hwmon::gpu_detect::AmdGpuInfo> {
        let card = |bdf: &str, device_id: u16, name: &str, discrete: bool| {
            crate::hwmon::gpu_detect::AmdGpuInfo {
                pci_bdf: bdf.into(),
                pci_device_id: device_id,
                pci_revision: 0xC0,
                pci_class: 0x030000,
                marketing_name: Some(name.into()),
                hwmon_path: std::path::PathBuf::from("/nonexistent"),
                fan_curve_path: None,
                fan_zero_rpm_path: None,
                is_discrete: discrete,
                has_fan_rpm: true,
                has_pwm: true,
                has_pwm_enable: true,
                overdrive_enabled: false,
            }
        };
        vec![
            card("0000:03:00.0", 0x73BF, "AMD Radeon RX 6900 XT", true),
            card("0000:c5:00.0", 0x15BF, "AMD Radeon 780M", false),
        ]
    }

    /// DEC-449 (`BRD-q`): the report's GPU section describes the primary card
    /// and carries the iGPU's advisory, as `/capabilities` does.
    #[test]
    fn the_report_warns_about_an_affected_card_that_is_not_the_primary() {
        let gpus = rdna2_card_and_rdna3_igpu();
        let diag = amd_gpu_diagnostics(&gpus, &[], Some("6.18.2")).expect("a GPU");
        assert_eq!(diag.pci_bdf, "0000:03:00.0", "the primary card");
        assert_eq!(diag.kernel_warnings.len(), 1);
        assert!(diag.kernel_warnings[0]
            .message
            .contains("the AMD Radeon 780M (0000:c5:00.0)"));
        assert!(amd_gpu_diagnostics(&gpus, &[], Some("6.18.7"))
            .expect("a GPU")
            .kernel_warnings
            .is_empty());
    }

    // ── DEC-500: board_layout ─────────────────────────────────────────────

    mod board_layout_tests {
        use super::super::board_layout;
        use crate::hwmon::board_catalogue::{BoardChannel, BoardContext};
        use crate::hwmon::inventory::FanInputDescriptor;
        use crate::hwmon::pwm_discovery::PwmHeaderDescriptor;

        const HOST_SIV: u32 = 0xA008_090A;

        fn ctx(vendor: &str, siv: Option<u32>) -> BoardContext {
            BoardContext {
                cpu_vendor: vendor.into(),
                siv,
            }
        }

        fn header(chip: &str, idx: u8, board: Option<BoardChannel>) -> PwmHeaderDescriptor {
            PwmHeaderDescriptor {
                id: format!("hwmon:{chip}:nodev:pwm{idx}:pwm{idx}"),
                label: format!("pwm{idx}"),
                chip_name: chip.into(),
                pwm_index: idx,
                board,
                ..Default::default()
            }
        }

        fn fan(chip: &str, idx: u8, label: &str) -> FanInputDescriptor {
            FanInputDescriptor {
                id: format!("hwmon:{chip}:nodev:fan{idx}:{label}"),
                chip_name: chip.into(),
                fan_index: idx,
                label: label.into(),
                input_path: std::path::PathBuf::new(),
            }
        }

        fn names(
            layout: &crate::api::responses::BoardLayout,
        ) -> Vec<(&str, &str, Option<&str>, bool)> {
            layout
                .channels
                .iter()
                .map(|c| {
                    (
                        c.id.as_str(),
                        c.kind,
                        c.board_label.as_deref(),
                        c.board_unmapped,
                    )
                })
                .collect()
        }

        /// The reference board: the catalogue's verdict on each unlabelled
        /// channel, joined on the ids the other endpoints publish; driver
        /// labels win, and a header carries what discovery decided.
        #[test]
        fn the_reference_board_layout_names_its_unlabelled_channels() {
            let cpu = header("it8696", 1, Some(BoardChannel::Named("CPU_FAN")));
            let spare = header("it8696", 6, Some(BoardChannel::Unmapped));
            let labelled = header("it87952", 3, None);
            let fans = [
                fan("it87952", 3, "fan3"),
                // A driver label: the catalogue's SYS_FAN5_PUMP is not offered.
                fan("it87952", 1, "CHA_FAN9"),
            ];
            let temps = [
                ("hwmon:it8696:nodev:temp5", "it8696", "temp5"),
                ("hwmon:it8696:nodev:temp3", "it8696", "CPU"),
                ("hwmon:it87952:nodev:temp2", "it87952", "temp2"),
                ("hwmon:k10temp:pci:Tctl", "k10temp", "Tctl"),
            ];
            let layout = board_layout(
                &ctx("AMD", Some(HOST_SIV)),
                "X870E AORUS MASTER",
                &[&cpu, &spare, &labelled],
                &fans,
                &temps,
            )
            .expect("a layout");

            assert_eq!(layout.siv, "A008090A");
            assert_eq!(layout.platform, "AMD");
            assert!(layout.in_catalogue);
            assert!(layout.board_listed);
            assert_eq!(
                layout.catalogue_commit,
                crate::hwmon::board_catalogue::UPSTREAM_COMMIT
            );
            assert!(layout.listed_boards.iter().any(|b| b == "X870E AORUS PRO"));
            assert_eq!(
                names(&layout),
                vec![
                    (
                        "hwmon:it8696:nodev:pwm1:pwm1",
                        "pwm",
                        Some("CPU_FAN"),
                        false
                    ),
                    ("hwmon:it8696:nodev:pwm6:pwm6", "pwm", None, true),
                    ("hwmon:it8696:nodev:temp5", "temp", Some("VRM MOS"), false),
                    (
                        "hwmon:it87952:nodev:fan3:fan3",
                        "fan",
                        Some("SYS_FAN4"),
                        false
                    ),
                    ("hwmon:it87952:nodev:temp2", "temp", Some("EC_TEMP2"), false),
                ]
            );
        }

        /// Upstream lists boards per SIV; the match is the DMI name, trimmed and
        /// case-folded. An unlisted board on a known layout still gets the
        /// names — the key is the SIV, never the board name.
        #[test]
        fn board_listing_follows_the_dmi_name_but_never_gates_the_names() {
            let cpu = header("it8696", 1, Some(BoardChannel::Named("CPU_FAN")));
            for (dmi, listed) in [
                ("X870E AORUS PRO", true),
                ("  x870e aorus master ", true),
                ("B650 AORUS ELITE AX", false),
                ("", false),
            ] {
                let layout = board_layout(&ctx("AMD", Some(HOST_SIV)), dmi, &[&cpu], &[], &[])
                    .expect("a layout");
                assert_eq!(layout.board_listed, listed, "{dmi:?}");
                assert_eq!(layout.channels.len(), 1, "{dmi:?}");
            }
        }

        #[test]
        fn no_siv_or_an_unknown_platform_publishes_no_layout() {
            for c in [
                ctx("AMD", None),
                ctx("", Some(HOST_SIV)),
                ctx("Hygon", Some(HOST_SIV)),
            ] {
                assert!(
                    board_layout(&c, "X870E AORUS MASTER", &[], &[], &[]).is_none(),
                    "{c:?}"
                );
            }
        }

        /// An SIV nobody has mapped — or a CPU platform the layout was not
        /// written for — is reported as such, with nothing named.
        #[test]
        fn an_unmapped_layout_is_reported_with_no_channels() {
            let temps = [("hwmon:it8696:nodev:temp5", "it8696", "temp5")];
            for c in [ctx("AMD", Some(0xDEAD_BEEF)), ctx("Intel", Some(HOST_SIV))] {
                let layout =
                    board_layout(&c, "X870E AORUS MASTER", &[], &[], &temps).expect("a layout");
                assert!(!layout.in_catalogue, "{c:?}");
                assert!(
                    layout.listed_boards.is_empty() && !layout.board_listed,
                    "{c:?}"
                );
                assert!(layout.channels.is_empty(), "{c:?}");
            }
        }

        /// A0030507 has a block for the primary chip only: its names stand, and
        /// a second chip on the board gets none rather than cancelling them.
        #[test]
        fn a_partial_layout_names_only_the_chip_it_covers() {
            use crate::hwmon::board_catalogue::ChannelKind;
            let c = ctx("AMD", Some(0xA003_0507));
            let primary = c.name("it8696", ChannelKind::Temp, 1);
            assert!(primary.is_some(), "precondition: the primary chip is named");
            assert_eq!(c.channel("it87952", ChannelKind::Temp, 1), None);
            let temps = [
                ("hwmon:it8696:nodev:temp1", "it8696", "temp1"),
                ("hwmon:it87952:nodev:temp1", "it87952", "temp1"),
            ];
            let layout = board_layout(&c, "", &[], &[], &temps).expect("a layout");
            assert!(layout.in_catalogue);
            assert_eq!(
                names(&layout),
                vec![("hwmon:it8696:nodev:temp1", "temp", primary, false)]
            );
        }

        /// A channel the two derivations disagree on is withheld: no name, and
        /// no unmapped verdict either — it is simply not listed.
        #[test]
        fn a_withheld_channel_is_not_listed() {
            let temps = [
                ("hwmon:it87952:nodev:temp1", "it87952", "temp1"),
                ("hwmon:it87952:nodev:temp3", "it87952", "temp3"),
            ];
            let layout = board_layout(&ctx("AMD", Some(0xA108_090A)), "", &[], &[], &temps)
                .expect("a layout");
            assert_eq!(
                names(&layout),
                vec![("hwmon:it87952:nodev:temp3", "temp", Some("System 2"), false)]
            );
        }
    }
}
