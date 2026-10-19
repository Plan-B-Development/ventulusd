//! Discover controllable PWM outputs on motherboard fan headers.
//!
//! Scans hwmon sysfs for `pwmN` files and builds descriptors with
//! stable IDs that do not depend on the hwmon index.

use std::path::{Path, PathBuf};

use crate::error::HwmonError;

/// Descriptor for a controllable PWM fan header.
#[derive(Debug, Clone, Default)]
pub struct PwmHeaderDescriptor {
    /// Stable identifier (e.g. `hwmon:it8696:it87.2624:pwm3:PUMP`).
    pub id: String,
    /// Human-readable label (from `fanN_label` / `pwmN_label`, or fallback).
    pub label: String,
    /// Chip name (e.g. `it8696`) — **canonical**: the it87 v2.0 board suffix
    /// is stripped where the name is read, so this and every id built from it
    /// stay the same across that driver's rename (DEC-442).
    pub chip_name: String,
    /// The hwmon `name` attribute exactly as sysfs published it, when that
    /// differs from `chip_name` — `Some("it8696_a008090a")` on an it87 v2.0
    /// Gigabyte chip, `None` everywhere else. Read it through
    /// [`PwmHeaderDescriptor::sysfs_chip_name`]. Only `/etc/sensors.d` matching
    /// wants this spelling; never build an id or compare a table from it.
    pub sysfs_chip_name: Option<String>,
    /// Device identifier (PCI BDF or platform device name).
    pub device_id: String,
    /// PWM index (the N in `pwmN`).
    pub pwm_index: u8,
    /// Whether `pwmN_enable` exists (needed for mode control).
    pub supports_enable: bool,
    /// Path to the `pwmN` file for writing.
    pub pwm_path: String,
    /// Path to the `pwmN_enable` file (if it exists).
    pub enable_path: Option<String>,
    /// Whether `fanN_input` exists (RPM reading available).
    pub rpm_available: bool,
    /// Path to `fanN_input` for RPM reading (if it exists).
    pub rpm_path: Option<String>,
    /// Minimum PWM percent (policy floor).
    pub min_pwm_percent: u8,
    /// Maximum PWM percent (policy ceiling).
    pub max_pwm_percent: u8,
    /// Whether the `pwmN` file is writable (checked at discovery time).
    ///
    /// This is the kernel sysfs permission bit and is authoritative per-channel:
    /// liquid-cooler drivers expose a writable `pwmN` only for genuinely
    /// controllable channels (e.g. NZXT Kraken `pwm1`=pump, Aquacomputer D5
    /// Next `pwm1`=pump). Monitor-only devices (NZXT Kraken2, Aquacomputer
    /// flow/leak sensors) simply expose no `pwmN`, so no writable header is
    /// produced. See `aio.rs`.
    pub is_writable: bool,
    /// PWM/DC mode from `pwmN_mode` (0=DC, 1=PWM, None if file absent).
    pub pwm_mode: Option<u8>,
    /// Whether this header belongs to a liquid cooler (NZXT Kraken /
    /// Aquacomputer) — a daemon-authoritative AIO hint so the GUI can cluster
    /// and floor pumps without re-deriving hardware knowledge. Writability
    /// still rides `is_writable`. See `aio.rs`.
    pub is_aio: bool,
    /// What this channel drives, **inferred** from its label and chip
    /// (AIO-MB Phase 1). Per-channel, and orthogonal to the chip-level
    /// `is_aio` — a pump on a motherboard `AIO_PUMP` header is
    /// `role: Pump, is_aio: false`, which is the whole point of the field.
    ///
    /// Discovery has no access to the user's `runtime.toml` assignments, so
    /// this is the inference **only**. Consumers must overlay the assignment
    /// with `roles::resolve_role` — see `AppState::resolved_header_role`.
    pub role: crate::hwmon::roles::HeaderRole,
    /// How `role` was established. `RoleSource::None` whenever the role is
    /// `Unknown`.
    pub role_source: crate::hwmon::roles::RoleSource,
    /// What the board catalogue says about this header (DEC-500), where it says
    /// anything — **only** on a header whose label discovery synthesised: a
    /// driver's own label is never second-guessed from a table. A `Named`
    /// entry is the board's name for the header and is part of the evidence
    /// `role` was inferred from; read it through
    /// [`PwmHeaderDescriptor::board_label`].
    pub board: Option<crate::hwmon::board_catalogue::BoardChannel>,
    /// Read-only driver capability audit (AIO-MB Phase 4, DEC-316). Static
    /// facts sampled once here; every field is `Option` and stays `None` when
    /// the driver does not expose the attribute. See
    /// [`crate::hwmon::header_caps`].
    pub caps: crate::hwmon::header_caps::HeaderCaps,
    /// Path to `fanN_alarm` when the driver exposes one.
    ///
    /// The alarm is *state*, not a capability, so only its path is captured
    /// here — the value is sampled on the 1 Hz poll and published on `/poll`.
    /// Freezing it into this snapshot would let it read "clear" while a fan is
    /// failing, because clients refetch headers only occasionally.
    pub alarm_path: Option<String>,
}

impl PwmHeaderDescriptor {
    /// The chip name as sysfs published it (DEC-442): the suffixed spelling on
    /// an it87 v2.0 Gigabyte chip, otherwise `chip_name`.
    pub fn sysfs_chip_name(&self) -> &str {
        self.sysfs_chip_name.as_deref().unwrap_or(&self.chip_name)
    }

    /// The board catalogue's name for this header, where it has one (DEC-500).
    pub fn board_label(&self) -> Option<&'static str> {
        match self.board? {
            crate::hwmon::board_catalogue::BoardChannel::Named(name) => Some(name),
            crate::hwmon::board_catalogue::BoardChannel::Unmapped => None,
        }
    }
}

/// Discover all controllable PWM outputs under a given hwmon root.
///
/// A header is considered controllable if it has a `pwmN` file. `board` keys
/// the board catalogue (DEC-500): production passes
/// [`BoardContext::read`](crate::hwmon::board_catalogue::BoardContext::read),
/// and a header whose driver publishes no label takes the catalogue's name as
/// role evidence. `BoardContext::default()` matches nothing, which is exactly
/// the pre-catalogue behaviour.
pub fn discover_pwm_headers(
    hwmon_root: &Path,
    board: &crate::hwmon::board_catalogue::BoardContext,
) -> Result<Vec<PwmHeaderDescriptor>, HwmonError> {
    let mut descriptors = Vec::new();

    let entries = std::fs::read_dir(hwmon_root).map_err(|e| HwmonError::ReadError {
        path: hwmon_root.display().to_string(),
        message: e.to_string(),
    })?;

    let mut hwmon_dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("hwmon"))
        })
        .collect();

    hwmon_dirs.sort();

    for hwmon_dir in hwmon_dirs {
        match discover_device_pwm(&hwmon_dir, board) {
            Ok(headers) => descriptors.extend(headers),
            Err(e) => {
                log::warn!("Skipping PWM discovery for {}: {e}", hwmon_dir.display());
            }
        }
    }

    Ok(descriptors)
}

/// Discover PWM outputs for a single hwmon device directory.
fn discover_device_pwm(
    hwmon_dir: &Path,
    board: &crate::hwmon::board_catalogue::BoardContext,
) -> Result<Vec<PwmHeaderDescriptor>, HwmonError> {
    let names = crate::hwmon::chip_name::read_chip_name(hwmon_dir)?;
    let chip_name = names.canonical;
    let sysfs_chip_name = (names.sysfs != chip_name).then_some(names.sysfs);

    // GPU-owned hwmon `pwm1` is never surfaced as an hwmon header.
    // - amdgpu (DEC-102): RDNA4 exposes `pwm1` read-only with no `pwm1_enable`,
    //   so binding it produces a 1 Hz 503/EACCES storm; RDNA3 exposes both, but
    //   a write can silently no-op (DEC-430).
    // - nouveau (DEC-204): exposes a *writable* `pwm1`; leaking it here would let
    //   the profile engine drive a GPU fan, breaking the read-only contract.
    // GPU fans are addressed exclusively via the `amd_gpu:` / `nvidia_gpu:`
    // prefixes and the GPU endpoints. Single source of truth: `is_gpu_owned_hwmon_chip`.
    if crate::hwmon::is_gpu_owned_hwmon_chip(&chip_name) {
        log::debug!(
            "Skipping GPU-owned hwmon chip '{chip_name}' at {} — GPU fans are owned by the GPU subsystem (DEC-102/DEC-204)",
            hwmon_dir.display()
        );
        return Ok(Vec::new());
    }

    let device_id = resolve_device_id(hwmon_dir);

    // Daemon-authoritative AIO hint (per chip, not per channel). NZXT Kraken /
    // Aquacomputer pumps + radiator fans flow through the normal hwmon path;
    // this flag lets the GUI cluster and floor them without re-deriving the
    // chip-name list. Writability is still the kernel permission bit below.
    let is_aio = crate::hwmon::aio::is_liquid_cooler_chip(&chip_name);

    // Find all pwmN files (pwm1, pwm2, ...)
    let entries = std::fs::read_dir(hwmon_dir).map_err(|e| HwmonError::ReadError {
        path: hwmon_dir.display().to_string(),
        message: e.to_string(),
    })?;

    let mut pwm_files: Vec<(u8, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Match "pwm1", "pwm2", etc. but NOT "pwm1_enable" etc.
            if let Some(rest) = name.strip_prefix("pwm") {
                if let Ok(index) = rest.parse::<u8>() {
                    return Some((index, e.path()));
                }
            }
            None
        })
        .collect();

    pwm_files.sort_by_key(|(idx, _)| *idx);

    let mut headers = Vec::new();
    for (pwm_index, pwm_path) in pwm_files {
        let enable_path = hwmon_dir.join(format!("pwm{pwm_index}_enable"));
        let supports_enable = enable_path.exists();

        let fan_input = hwmon_dir.join(format!("fan{pwm_index}_input"));
        let rpm_available = fan_input.exists();

        // Try label sources in priority order
        let label = read_label(hwmon_dir, pwm_index);

        let id = build_stable_id(&chip_name, &device_id, pwm_index, &label);

        // The board catalogue speaks only where the driver did not (DEC-500):
        // `read_label` synthesised `pwmN` because no label file exists.
        let board_channel = if label == format!("pwm{pwm_index}") {
            board.channel(
                &chip_name,
                crate::hwmon::board_catalogue::ChannelKind::Pwm,
                pwm_index,
            )
        } else {
            None
        };
        let board_label = match board_channel {
            Some(crate::hwmon::board_catalogue::BoardChannel::Named(name)) => Some(name),
            _ => None,
        };

        // Per-channel role (AIO-MB Phase 1). Inference only — the user's
        // assignment lives in `runtime.toml` and is overlaid by the API layer.
        let (role, role_source) =
            crate::hwmon::roles::classify_header_role(&chip_name, pwm_index, &label, board_label);

        // Check if pwmN is writable (probe file permissions)
        let is_writable = std::fs::metadata(&pwm_path)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false);

        // Read-only capability audit + the alarm path (AIO-MB Phase 4). Pure
        // reads; this adds no write path to discovery.
        let caps = crate::hwmon::header_caps::read_header_caps(hwmon_dir, pwm_index);
        let alarm_path = crate::hwmon::header_caps::alarm_path(hwmon_dir, pwm_index);

        // Read pwmN_mode if present (0=DC, 1=PWM)
        let mode_path = hwmon_dir.join(format!("pwm{pwm_index}_mode"));
        let pwm_mode = if mode_path.exists() {
            std::fs::read_to_string(&mode_path)
                .ok()
                .and_then(|s| s.trim().parse::<u8>().ok())
        } else {
            None
        };

        headers.push(PwmHeaderDescriptor {
            id,
            label,
            chip_name: chip_name.clone(),
            sysfs_chip_name: sysfs_chip_name.clone(),
            device_id: device_id.clone(),
            pwm_index,
            supports_enable,
            pwm_path: pwm_path.display().to_string(),
            enable_path: if supports_enable {
                Some(enable_path.display().to_string())
            } else {
                None
            },
            rpm_available,
            rpm_path: if rpm_available {
                Some(fan_input.display().to_string())
            } else {
                None
            },
            min_pwm_percent: 0,
            max_pwm_percent: 100,
            is_writable,
            pwm_mode,
            is_aio,
            role,
            role_source,
            board: board_channel,
            caps,
            alarm_path,
        });
    }

    Ok(headers)
}

/// Read the best available label for a PWM header.
fn read_label(hwmon_dir: &Path, pwm_index: u8) -> String {
    // Try pwmN_label first, then fanN_label, then fallback
    let pwm_label_path = hwmon_dir.join(format!("pwm{pwm_index}_label"));
    if let Ok(label) = read_sysfs_string(&pwm_label_path) {
        let trimmed = label.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    let fan_label_path = hwmon_dir.join(format!("fan{pwm_index}_label"));
    if let Ok(label) = read_sysfs_string(&fan_label_path) {
        let trimmed = label.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    format!("pwm{pwm_index}")
}

// classify_header_floor() removed — thermal safety is centralized in ThermalSafetyRule.

/// Build a stable ID for a PWM header.
fn build_stable_id(chip_name: &str, device_id: &str, pwm_index: u8, label: &str) -> String {
    format!("hwmon:{chip_name}:{device_id}:pwm{pwm_index}:{label}")
}

/// Resolve the stable device ID from the sysfs device symlink.
fn resolve_device_id(hwmon_dir: &Path) -> String {
    let device_link = hwmon_dir.join("device");
    if device_link.exists() {
        let resolved = std::fs::read_link(&device_link)
            .or_else(|_| std::fs::canonicalize(&device_link))
            .unwrap_or_else(|e| {
                log::warn!(
                    "Could not resolve device symlink for {}: {}",
                    hwmon_dir.display(),
                    e
                );
                std::path::PathBuf::from("unknown")
            });
        device_id_from_path(&resolved)
    } else {
        "nodev".to_string()
    }
}

use super::util::{device_id_from_path, read_sysfs_string};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hwmon::board_catalogue::BoardContext;
    use std::fs;

    /// Create a hwmon fixture with PWM outputs.
    fn create_pwm_fixture(
        base: &Path,
        dir_name: &str,
        chip_name: &str,
        pwm_outputs: &[(u8, Option<&str>, bool, bool)], // (index, label, has_enable, has_fan_input)
    ) -> PathBuf {
        let hwmon_dir = base.join(dir_name);
        fs::create_dir_all(&hwmon_dir).unwrap();
        fs::write(hwmon_dir.join("name"), chip_name).unwrap();

        for &(index, label, has_enable, has_fan_input) in pwm_outputs {
            fs::write(hwmon_dir.join(format!("pwm{index}")), "128\n").unwrap();
            if has_enable {
                fs::write(hwmon_dir.join(format!("pwm{index}_enable")), "2\n").unwrap();
            }
            if has_fan_input {
                fs::write(hwmon_dir.join(format!("fan{index}_input")), "1200\n").unwrap();
            }
            if let Some(lbl) = label {
                fs::write(
                    hwmon_dir.join(format!("fan{index}_label")),
                    format!("{lbl}\n"),
                )
                .unwrap();
            }
        }

        hwmon_dir
    }

    #[test]
    fn discover_single_pwm_output() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(tmp.path(), "hwmon0", "it8696", &[(1, None, true, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].chip_name, "it8696");
        assert_eq!(headers[0].pwm_index, 1);
        assert!(headers[0].supports_enable);
        assert!(headers[0].rpm_available);
        assert_eq!(headers[0].label, "pwm1");
    }

    #[test]
    fn discover_multiple_pwm_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[
                (1, Some("CPU_FAN"), true, true),
                (2, Some("CHA_FAN1"), true, true),
                (3, Some("PUMP"), true, false),
            ],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].label, "CPU_FAN");
        assert_eq!(headers[1].label, "CHA_FAN1");
        assert_eq!(headers[2].label, "PUMP");
    }

    #[test]
    fn stable_id_format() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(1, Some("CPU_FAN"), true, true)],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(headers[0].id.starts_with("hwmon:it8696:"));
        assert!(headers[0].id.contains("pwm1"));
        assert!(headers[0].id.contains("CPU_FAN"));
        // Must not contain hwmon index
        assert!(!headers[0].id.contains("hwmon0"));
    }

    #[test]
    fn discover_without_enable_file() {
        // Non-amdgpu chip without `pwmN_enable`. Some legacy hwmon drivers
        // (e.g. older nct67xx revisions) expose `pwm1` without
        // `pwm1_enable`; those headers are still discoverable so the GUI can
        // attempt direct PWM writes (the kernel may accept them depending on
        // chip behaviour). The chip name is what gates inclusion, not the
        // presence of `pwm1_enable` — see `discover_amdgpu_excluded`.
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(tmp.path(), "hwmon0", "nct6798", &[(1, None, false, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 1);
        assert!(!headers[0].supports_enable);
        assert!(headers[0].enable_path.is_none());
    }

    /// DEC-102: `amdgpu` chips must never appear in hwmon PWM discovery,
    /// regardless of how their `pwm1` file looks. RDNA4 exposes a
    /// read-only `pwm1` (no `pwm1_enable`); RDNA3 and older expose a
    /// writable-looking `pwm1` + `pwm1_enable` (on RDNA3 a write can
    /// silently no-op, DEC-430). Both shapes are GPU-subsystem
    /// concerns and must be addressed via `amd_gpu:` prefix endpoints,
    /// not via `/hwmon/{header_id}/pwm`. Pre-DEC-102, the read-only
    /// shape produced a 1 Hz 503/EACCES storm whenever a GUI profile bound
    /// it; this test pins the bug fix.
    #[test]
    fn discover_amdgpu_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        // RDNA4 shape: pwm1 + fan1_input, no pwm1_enable.
        create_pwm_fixture(tmp.path(), "hwmon0", "amdgpu", &[(1, None, false, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(
            headers.is_empty(),
            "amdgpu hwmon (no pwm1_enable shape) must not be advertised: {headers:#?}"
        );
    }

    #[test]
    fn discover_amdgpu_excluded_even_with_enable_file() {
        let tmp = tempfile::tempdir().unwrap();
        // RDNA3-and-older shape: pwm1 + pwm1_enable + fan1_input.
        // Still excluded — GPU fans are owned by the GPU subsystem.
        create_pwm_fixture(tmp.path(), "hwmon0", "amdgpu", &[(1, None, true, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(
            headers.is_empty(),
            "amdgpu hwmon (RDNA2-style writable shape) must not be advertised: {headers:#?}"
        );
    }

    #[test]
    fn discover_amdgpu_excluded_alongside_motherboard_chip() {
        // Mixed system: motherboard PWM chip and amdgpu both present.
        // Discovery returns only the motherboard chip's headers.
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(1, Some("CPU_FAN"), true, true)],
        );
        create_pwm_fixture(tmp.path(), "hwmon1", "amdgpu", &[(1, None, false, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 1, "headers: {headers:#?}");
        assert_eq!(headers[0].chip_name, "it8696");
    }

    /// DEC-204 (safety): the open `nouveau` driver exposes a **writable**
    /// `pwm1` + `pwm1_enable`. It must never appear as an hwmon header, or the
    /// profile engine could drive a GPU fan — violating the read-only-telemetry
    /// contract for Phase-1 NVIDIA support. Pins the write-block.
    #[test]
    fn discover_nouveau_excluded_even_though_writable() {
        let tmp = tempfile::tempdir().unwrap();
        // Writable shape: pwm1 + pwm1_enable + fan1_input — still excluded.
        create_pwm_fixture(tmp.path(), "hwmon0", "nouveau", &[(1, None, true, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(
            headers.is_empty(),
            "nouveau hwmon (writable pwm1) must not be advertised as a header: {headers:#?}"
        );
    }

    #[test]
    fn discover_nouveau_excluded_alongside_motherboard_chip() {
        // Mixed system: motherboard PWM chip and nouveau both present. Discovery
        // returns only the motherboard chip's headers — the nouveau exclusion
        // must not drop unrelated chips (mirrors the amdgpu mixed-system case).
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(1, Some("CPU_FAN"), true, true)],
        );
        create_pwm_fixture(tmp.path(), "hwmon1", "nouveau", &[(1, None, true, true)]);

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 1, "headers: {headers:#?}");
        assert_eq!(headers[0].chip_name, "it8696");
    }

    #[test]
    fn discover_without_fan_input() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(3, Some("PUMP"), true, false)],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 1);
        assert!(!headers[0].rpm_available);
        assert!(headers[0].rpm_path.is_none());
    }

    #[test]
    fn all_headers_have_zero_floor() {
        // No per-header floors — thermal safety handled centrally
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[
                (1, Some("CPU_FAN"), true, true),
                (2, Some("CHA_FAN1"), true, true),
            ],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers[0].min_pwm_percent, 0);
        assert_eq!(headers[1].min_pwm_percent, 0);
    }

    #[test]
    fn pump_header_has_zero_floor() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(3, Some("PUMP"), true, false)],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers[0].min_pwm_percent, 0);
    }

    #[test]
    fn discover_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn discover_skips_chip_without_pwm() {
        let tmp = tempfile::tempdir().unwrap();
        // Create hwmon device with sensors but no PWM
        let hwmon_dir = tmp.path().join("hwmon0");
        fs::create_dir_all(&hwmon_dir).unwrap();
        fs::write(hwmon_dir.join("name"), "k10temp").unwrap();
        fs::write(hwmon_dir.join("temp1_input"), "55000\n").unwrap();

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn discover_multiple_devices() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(1, Some("CPU_FAN"), true, true)],
        );
        create_pwm_fixture(
            tmp.path(),
            "hwmon1",
            "nct6798",
            &[(1, Some("SYS_FAN1"), true, true)],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn pwm_label_preferred_over_fan_label() {
        let tmp = tempfile::tempdir().unwrap();
        let hwmon_dir = tmp.path().join("hwmon0");
        fs::create_dir_all(&hwmon_dir).unwrap();
        fs::write(hwmon_dir.join("name"), "it8696").unwrap();
        fs::write(hwmon_dir.join("pwm1"), "128\n").unwrap();
        fs::write(hwmon_dir.join("pwm1_enable"), "2\n").unwrap();
        fs::write(hwmon_dir.join("fan1_label"), "FAN_LABEL\n").unwrap();
        fs::write(hwmon_dir.join("pwm1_label"), "PWM_LABEL\n").unwrap();

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers[0].label, "PWM_LABEL");
    }

    // ── AIO header flag + per-channel writability (AIO Phase 1) ───────────

    #[test]
    fn kraken3_headers_flagged_aio_and_writable() {
        // NZXT Kraken3 (Z53): pwm1=pump, pwm2=fan, both writable (0644 in the
        // kernel; tempdir files are writable, mirroring that).
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "z53",
            &[(1, None, true, true), (2, None, true, true)],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 2);
        for h in &headers {
            assert!(h.is_aio, "Kraken header should be flagged AIO: {h:#?}");
            assert!(h.is_writable);
        }
    }

    #[test]
    fn corsair_cpro_writable_but_not_aio() {
        // Commander Pro is a fan hub, not a liquid cooler: writable pwm, NOT AIO.
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "corsaircpro",
            &[(1, None, false, true), (2, None, false, true)],
        );

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 2);
        for h in &headers {
            assert!(!h.is_aio, "a fan hub must not be flagged AIO: {h:#?}");
            assert!(h.is_writable);
        }
    }

    #[test]
    fn ordinary_motherboard_header_not_aio() {
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon0",
            "it8696",
            &[(1, Some("CPU_FAN"), true, true)],
        );
        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert!(!headers[0].is_aio);
    }

    #[test]
    fn readonly_pwm_reports_not_writable() {
        // The monitor-only mechanism: `is_writable` mirrors the sysfs
        // permission bit. A read-only `pwmN` reports `is_writable=false`, so
        // the GUI degrades rather than faking control. (This is a pure mode
        // check, so it holds even when tests run as root.)
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let hwmon_dir = tmp.path().join("hwmon0");
        fs::create_dir_all(&hwmon_dir).unwrap();
        fs::write(hwmon_dir.join("name"), "z53").unwrap();
        let pwm = hwmon_dir.join("pwm1");
        fs::write(&pwm, "128\n").unwrap();
        let mut perms = fs::metadata(&pwm).unwrap().permissions();
        perms.set_mode(0o444);
        fs::set_permissions(&pwm, perms).unwrap();

        let headers = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        assert_eq!(headers.len(), 1);
        assert!(
            !headers[0].is_writable,
            "read-only pwm must report not writable"
        );
        assert!(headers[0].is_aio);
    }

    // ── Board catalogue (DEC-500) ─────────────────────────────────────────

    const HOST_SIV: u32 = 0xA008_090A;

    fn board(cpu_vendor: &str, siv: Option<u32>) -> BoardContext {
        BoardContext {
            cpu_vendor: cpu_vendor.into(),
            siv,
        }
    }

    /// The reference board as it87 v2.0 publishes it: board-suffixed chip
    /// names and no fan or pwm label files.
    fn reference_board(base: &Path, with_secondary: bool) {
        let primary: Vec<(u8, Option<&str>, bool, bool)> =
            (1..=6).map(|i| (i, None, true, true)).collect();
        create_pwm_fixture(base, "hwmon3", "it8696_a008090a", &primary);
        if with_secondary {
            create_pwm_fixture(
                base,
                "hwmon4",
                "it87952_a008090a",
                &[
                    (1, None, true, true),
                    (2, None, true, true),
                    (3, None, true, true),
                ],
            );
        }
    }

    fn header<'a>(
        headers: &'a [PwmHeaderDescriptor],
        chip: &str,
        idx: u8,
    ) -> &'a PwmHeaderDescriptor {
        headers
            .iter()
            .find(|h| h.chip_name == chip && h.pwm_index == idx)
            .unwrap_or_else(|| panic!("{chip} pwm{idx} not discovered"))
    }

    /// [SAFETY] The catalogue's pump name makes an unlabelled header a pump at
    /// discovery — the one place every protection site reads the role from.
    #[test]
    fn unlabelled_headers_take_the_board_catalogue_name_as_role_evidence() {
        use crate::hwmon::roles::{HeaderRole, RoleSource};
        let tmp = tempfile::tempdir().unwrap();
        reference_board(tmp.path(), true);

        let plain = discover_pwm_headers(tmp.path(), &BoardContext::default()).unwrap();
        let headers = discover_pwm_headers(tmp.path(), &board("AMD", Some(HOST_SIV))).unwrap();

        // Without the catalogue: every header is a bare pwmN of unknown role.
        assert!(plain.iter().all(|h| h.board.is_none()
            && h.role == HeaderRole::Unknown
            && h.board_label().is_none()));

        let pump = header(&headers, "it87952", 1);
        assert_eq!(pump.board_label(), Some("SYS_FAN5_PUMP"));
        assert_eq!(
            (pump.role, pump.role_source),
            (HeaderRole::Pump, RoleSource::BoardCatalogue)
        );
        let cpu = header(&headers, "it8696", 1);
        assert_eq!(cpu.board_label(), Some("CPU_FAN"));
        assert_eq!(
            (cpu.role, cpu.role_source),
            (HeaderRole::CpuFan, RoleSource::BoardCatalogue)
        );
        // CPU_OPT is named but infers nothing, as a driver label would not.
        let opt = header(&headers, "it8696", 5);
        assert_eq!(opt.board_label(), Some("CPU_OPT"));
        assert_eq!(
            (opt.role, opt.role_source),
            (HeaderRole::Unknown, RoleSource::None)
        );
        // pwm6 is on the chip but is no header on this board: unmapped, unnamed.
        let spare = header(&headers, "it8696", 6);
        assert_eq!(
            spare.board,
            Some(crate::hwmon::board_catalogue::BoardChannel::Unmapped)
        );
        assert_eq!(spare.board_label(), None);
        assert_eq!(spare.role, HeaderRole::Unknown);

        // The catalogue names a header; it never renames one. Ids and labels are
        // exactly what discovery published before, so no stored id moves.
        let ids = |hs: &[PwmHeaderDescriptor]| -> Vec<(String, String)> {
            hs.iter().map(|h| (h.id.clone(), h.label.clone())).collect()
        };
        assert_eq!(ids(&headers), ids(&plain));
    }

    /// A driver label is the evidence wherever there is one: the catalogue is
    /// not consulted for that header, even where it has an entry.
    #[test]
    fn a_driver_labelled_header_is_never_given_a_board_name() {
        use crate::hwmon::roles::{HeaderRole, RoleSource};
        let tmp = tempfile::tempdir().unwrap();
        create_pwm_fixture(
            tmp.path(),
            "hwmon4",
            "it87952",
            &[(1, Some("CHA_FAN1"), true, true), (2, None, true, true)],
        );
        assert_eq!(
            board("AMD", Some(HOST_SIV)).name(
                "it87952",
                crate::hwmon::board_catalogue::ChannelKind::Pwm,
                1
            ),
            Some("SYS_FAN5_PUMP"),
            "precondition: the catalogue names this header a pump"
        );

        let headers = discover_pwm_headers(tmp.path(), &board("AMD", Some(HOST_SIV))).unwrap();

        let labelled = header(&headers, "it87952", 1);
        assert_eq!(labelled.board, None);
        assert_eq!(
            (labelled.role, labelled.role_source),
            (HeaderRole::ChassisFan, RoleSource::Label)
        );
        assert!(header(&headers, "it87952", 2).board_label().is_some());
    }

    /// The key is platform + chip + SIV; a partial key matches nothing.
    #[test]
    fn a_wrong_platform_or_unknown_layout_names_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        reference_board(tmp.path(), true);
        for ctx in [
            board("Intel", Some(HOST_SIV)),
            board("", Some(HOST_SIV)),
            board("AMD", None),
            board("AMD", Some(0xDEAD_BEEF)),
        ] {
            let headers = discover_pwm_headers(tmp.path(), &ctx).unwrap();
            assert!(
                headers.iter().all(|h| h.board.is_none()),
                "{ctx:?} named a header"
            );
        }
    }

    /// A second chip that is not bound does not cancel the first chip's names.
    #[test]
    fn a_missing_secondary_chip_leaves_the_primary_named() {
        let tmp = tempfile::tempdir().unwrap();
        reference_board(tmp.path(), false);
        let headers = discover_pwm_headers(tmp.path(), &board("AMD", Some(HOST_SIV))).unwrap();
        assert_eq!(headers.len(), 6);
        assert_eq!(header(&headers, "it8696", 1).board_label(), Some("CPU_FAN"));
    }
}
