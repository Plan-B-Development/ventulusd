//! Passive Super-I/O hardware-monitor chip detection (DEC-202).
//!
//! Report-only. This module answers "which motherboard Super-I/O sensor/fan
//! chip does this machine have, is its kernel driver bound, and — if not —
//! which allowlisted module should the user load, with what caveats". It is
//! the daemon-owned half of the Super-I/O feature; the GUI only displays the
//! result and the copy-paste remediation.
//!
//! ## Safety posture (non-negotiable)
//! - **Passive only.** Detection composes read-only evidence the kernel already
//!   exposes (DMI board table, bound hwmon chips, `/proc/modules`, `/dev/kmsg`,
//!   `/proc/ioports`) via [`crate::hwmon::chip_db`]. It never probes an I/O
//!   port, never loads a module, never writes any sysfs attribute, and never
//!   touches PWM. (Active `/dev/port` probing and module loading are separate,
//!   opt-in, later phases — not here.)
//! - **Detection proves "chip present", never "fan control available".** A
//!   detected chip may have unrouted PWM pins or a BIOS/EC/ACPI that overrides
//!   writes; the report says so and never claims control works.
//! - **x86 only.** On non-x86 the detector returns `arch_supported = false`.
//! - **Allowlisted recommendations.** A "load this driver" suggestion is only
//!   ever emitted for a module in [`SUPERIO_ALLOWLIST`], and never suggests a
//!   risky module parameter (`force_id`, `ignore_resource_conflict`).
//!
//! The detector is dependency-injected via [`SuperIoEvidence`] so it is a pure,
//! deterministic function over primitive facts — tested with fake hardware, no
//! real sysfs. [`SysfsSuperIoEvidence`] is the production adapter that gathers
//! the real evidence from `chip_db`.

use crate::hwmon::chip_db;
use crate::hwmon::classify::Confidence;
use std::collections::BTreeMap;
use std::path::Path;

/// Whether Super-I/O detection is supported on the build target. Detection
/// relies on x86 Super-I/O / ISA semantics; on any other architecture the
/// detector short-circuits to an empty, `arch_supported = false` report.
pub const SUPERIO_ARCH_SUPPORTED: bool = cfg!(any(target_arch = "x86", target_arch = "x86_64"));

/// Super-I/O silicon vendor (display grouping only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperIoVendor {
    Ite,
    Nuvoton,
    Winbond,
    Smsc,
    National,
    Fintek,
    Unknown,
}

impl std::fmt::Display for SuperIoVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ite => "ite",
            Self::Nuvoton => "nuvoton",
            Self::Winbond => "winbond",
            Self::Smsc => "smsc",
            Self::National => "national",
            Self::Fintek => "fintek",
            Self::Unknown => "unknown",
        })
    }
}

/// Where a chip's presence was observed. A chip may carry more than one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Listed for this board in the DMI dual-chip table (DEC-101).
    DmiBoardTable,
    /// The kernel logged it during its Super-I/O scan (`/dev/kmsg`).
    KernelLog,
    /// Currently bound and exposing an hwmon device.
    BoundHwmon,
    /// Identified by the opt-in active `/dev/port` probe (DEC-203).
    PortProbe,
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DmiBoardTable => "dmi_board_table",
            Self::KernelLog => "kernel_log",
            Self::BoundHwmon => "bound_hwmon",
            Self::PortProbe => "port_probe",
        })
    }
}

/// A concrete "load this driver" suggestion for an unbound chip. Only ever
/// constructed for an allowlisted module and never names a risky parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperIoRecommendation {
    /// The allowlisted kernel module to load.
    pub module: String,
    /// Whether mainline supports this specific chip (false ⇒ needs the DKMS
    /// build; delegated to [`chip_db::chip_driver_in_mainline`], DEC-144).
    pub in_mainline: bool,
    /// Copy-paste remediation text (modules-load.d / modprobe, or DKMS steps).
    pub load_hint: String,
    /// Why this is being suggested (which evidence, and that it is unbound).
    pub reason: String,
    /// Caveats the user must weigh before loading (ACPI conflict, DEC-106
    /// collision, DKMS-required, per-driver risk).
    pub risk_notes: Vec<String>,
}

/// One Super-I/O chip the detector concluded is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperIoChip {
    /// hwmon-style chip name, e.g. `"it8696"`, `"nct6799"`.
    pub chip_name: String,
    pub vendor: SuperIoVendor,
    /// Evidence sources, deduplicated and in a stable order.
    pub evidence: Vec<Evidence>,
    /// Confidence the chip is physically present (bound/kernel-logged ⇒ High;
    /// board-table-only ⇒ Medium).
    pub confidence: Confidence,
    /// The kernel driver bound to this chip, as observed from its hwmon
    /// device's `device/driver` link (`BRD-g`, DEC-469) — `Some` only when the
    /// chip is bound and the link was read; never inferred from the name.
    pub bound_driver: Option<String>,
    /// The kernel module expected to drive it (`"unknown"` if unrecognized).
    pub expected_module: String,
    /// Whether the chip's driver is loaded: `true` whenever `bound_driver` is
    /// observed (a bound driver is loaded or built in, whatever the name
    /// suggests), otherwise whether `expected_module` is loaded.
    pub module_loaded: bool,
    /// Whether the chip is currently exposing an hwmon device.
    pub hwmon_present: bool,
    /// A load recommendation, present only when the chip is unbound and its
    /// module is allowlisted.
    pub recommendation: Option<SuperIoRecommendation>,
    /// Non-actionable observations (e.g. "unrecognized chip").
    pub caveats: Vec<String>,
}

/// The full passive detection report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperIoReport {
    /// False on non-x86 targets (detector returns empty).
    pub arch_supported: bool,
    /// Detected chips, ordered by chip name.
    pub chips: Vec<SuperIoChip>,
    /// Driver names whose I/O range collides with an ACPI OperationRegion.
    pub acpi_conflict_drivers: Vec<String>,
    /// Report-level notes (always includes the "present ≠ controllable" caveat).
    pub notes: Vec<String>,
    /// Whether this kernel refuses unsigned modules (DEC-500,
    /// [`crate::hwmon::kernel_security::KernelSecurity::signed_modules_only`]);
    /// `None` when that could not be read, and off x86.
    pub signed_modules_only: Option<bool>,
}

/// One hwmon chip currently bound, as seen by the daemon's live cache. Passed
/// into the detector so it need not read the sensor/pwm cache itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundChip {
    /// hwmon `name` (e.g. `"nct6799"`).
    pub chip_name: String,
    /// Stable device identifier (used for DEC-106 dual-chip disambiguation).
    pub device_id: String,
    /// The kernel driver bound to it, observed from sysfs
    /// ([`crate::hwmon::bound_driver`]); `None` when not read.
    pub bound_driver: Option<String>,
}

/// Read-only evidence the detector composes. Injected so the detector is a
/// pure function testable without real hardware.
pub trait SuperIoEvidence {
    /// DMI `(board_vendor, board_name)`.
    fn board(&self) -> (String, String);
    /// hwmon chips currently bound (from the daemon's live cache).
    fn bound_chips(&self) -> Vec<BoundChip>;
    /// Names of currently-loaded known hwmon modules.
    fn loaded_modules(&self) -> Vec<String>;
    /// Chip names the kernel logged during its Super-I/O scan. NOTE: the
    /// production reader (`read_kernel_detected_chips`) parses only ITE `it87:`
    /// log lines, so in practice only ITE chips surface here; non-ITE chips
    /// reach the detector via `bound_chips` (or, for ITE boards, the DMI table).
    /// The trait itself is source-agnostic — tests may inject any chip name.
    fn kmsg_chips(&self) -> Vec<String>;
    /// Driver names with an ACPI I/O-port conflict.
    fn acpi_conflict_drivers(&self) -> Vec<String>;
    /// Is the currently-loaded `module` an out-of-tree build? (`HOST-d`.)
    ///
    /// On the trait rather than read inline, because `detect_superio` is
    /// documented deterministic and side-effect free and every other input
    /// already arrives this way. The production implementation reads
    /// `/sys/module/<name>/taint`; see [`module_is_out_of_tree`] for why the
    /// module's *path* cannot be used. `None` when the flag could not be read
    /// (DEC-500: then the loaded build is neither, never "the kernel's own").
    fn module_out_of_tree(&self, module: &str) -> Option<bool>;
    /// The running kernel's `(major, minor)`, `None` when unreadable (DEC-500):
    /// whether a chip with a [`chip_db::mainline_since`] release is mainline
    /// depends on it.
    fn kernel_version(&self) -> Option<chip_db::KernelVersion>;
    /// Whether this kernel refuses unsigned modules (DEC-500), `None` when
    /// unknown.
    fn signed_modules_only(&self) -> Option<bool>;
}

// ── Module allowlist ────────────────────────────────────────────────
//
// The daemon must never *recommend* (nor, in later phases, load) a module
// outside this vetted set. Entries are the Super-I/O sensor/fan drivers this
// project supports; every module `expected_driver_for_chip` can return has an
// entry here so a recognized chip always has a rationale, and an unrecognized
// one is safely dropped. Mainline-vs-DKMS truth is NOT duplicated here — it is
// delegated to `chip_db::chip_driver_in_mainline` (DEC-144). No entry ever
// carries a module parameter.

struct AllowlistEntry {
    module: &'static str,
    vendor: SuperIoVendor,
    /// Baseline per-driver risk note, appended to a recommendation's
    /// `risk_notes` (empty string ⇒ none).
    risk_note: &'static str,
}

const SUPERIO_ALLOWLIST: &[AllowlistEntry] = &[
    AllowlistEntry {
        module: "it87",
        vendor: SuperIoVendor::Ite,
        risk_note: "Newer ITE chips (e.g. IT8686E/IT8688E/IT8696E) are supported only by the \
                    out-of-tree it87 driver (frankcrawford/it87); mainline it87 will not bind \
                    them. Never use force_id — it misroutes every register access.",
    },
    AllowlistEntry {
        module: "nct6775",
        vendor: SuperIoVendor::Nuvoton,
        risk_note: "Do not load nct6775 alongside the out-of-tree nct6687 on a board whose only \
                    Nuvoton chip is an NCT679x — an nct6687 that claims it (old builds, or any \
                    build loaded with force=1) can brick a fan header (DEC-106). Boards with two \
                    genuine Nuvoton chips (e.g. ASRock Taichi: NCT6686D + NCT6796D-S) need both.",
    },
    AllowlistEntry {
        module: "nct6687",
        vendor: SuperIoVendor::Nuvoton,
        risk_note: "Out-of-tree nct6687d (DKMS), common on MSI boards. Blacklist the in-kernel \
                    nct6683 (both can bind the same chip and garble readings). Do not load \
                    alongside in-kernel nct6775 (DEC-106 brick risk); use a build post-Fred78290 \
                    PR #164, and never load it with force=1 on an NCT679x board — since PR #174 \
                    force=1 attaches to any 0xDxxx chip ID.",
    },
    AllowlistEntry {
        module: "nct6683",
        vendor: SuperIoVendor::Nuvoton,
        risk_note: "Mainline nct6683 is monitoring-only: the driver publishes pwm read-only on \
                    every board except Mitac OEM systems (writes are refused even as root) and \
                    exposes no pwm_enable. For fan control on MSI NCT6687D boards use the \
                    out-of-tree nct6687d (module `nct6687`); ASRock NCT6686D boards need \
                    asrock-nct6683 (listed boards) or nct6687d.",
    },
    AllowlistEntry {
        module: "f71882fg",
        vendor: SuperIoVendor::Fintek,
        risk_note: "",
    },
    AllowlistEntry {
        module: "f71805f",
        vendor: SuperIoVendor::Fintek,
        risk_note: "",
    },
    AllowlistEntry {
        module: "w83627ehf",
        vendor: SuperIoVendor::Winbond,
        risk_note: "",
    },
    AllowlistEntry {
        module: "w83627hf",
        vendor: SuperIoVendor::Winbond,
        risk_note: "Older Winbond family; monitoring-focused.",
    },
    AllowlistEntry {
        module: "smsc47m1",
        vendor: SuperIoVendor::Smsc,
        risk_note: "",
    },
    AllowlistEntry {
        module: "smsc47b397",
        vendor: SuperIoVendor::Smsc,
        risk_note: "Monitoring-only driver — exposes temperatures and fan tachometers but no PWM \
                    output.",
    },
    // `DC-db` (DEC-473): `expected_driver_for_chip` returns this module since it
    // stopped folding `smsc47m192` into `smsc47m1`, so it needs an entry here.
    AllowlistEntry {
        module: "smsc47m192",
        vendor: SuperIoVendor::Smsc,
        risk_note: "Monitoring-only (I2C): voltages and temperatures, no fan or PWM. Fan control \
                    on these chips is the smsc47m1 driver.",
    },
    AllowlistEntry {
        module: "dme1737",
        vendor: SuperIoVendor::Smsc,
        risk_note: "",
    },
    AllowlistEntry {
        module: "sch5627",
        vendor: SuperIoVendor::Smsc,
        risk_note: "",
    },
    AllowlistEntry {
        module: "sch5636",
        vendor: SuperIoVendor::Smsc,
        risk_note: "",
    },
    AllowlistEntry {
        module: "pc87360",
        vendor: SuperIoVendor::National,
        risk_note: "The kernel driver warns that careless PWM values can stop a fan and cause \
                    irreversible damage. (This tool only detects — it never writes PWM.)",
    },
    AllowlistEntry {
        module: "pc87427",
        vendor: SuperIoVendor::National,
        risk_note: "",
    },
];

fn allowlist_entry(module: &str) -> Option<&'static AllowlistEntry> {
    SUPERIO_ALLOWLIST.iter().find(|e| e.module == module)
}

/// Vendor for an allowlisted module (Unknown if not allowlisted).
fn vendor_for_module(module: &str) -> SuperIoVendor {
    allowlist_entry(module)
        .map(|e| e.vendor)
        .unwrap_or(SuperIoVendor::Unknown)
}

/// Per-chip accumulator while unioning evidence sources.
#[derive(Default)]
struct EvidenceAcc {
    dmi: bool,
    kmsg: bool,
    bound: bool,
    /// The observed bound driver, from the first bound-chip entry that had one.
    bound_driver: Option<String>,
}

/// Run passive Super-I/O detection over the injected evidence.
///
/// Deterministic and side-effect free. On non-x86 targets it returns an empty,
/// `arch_supported = false` report.
pub fn detect_superio(ev: &dyn SuperIoEvidence) -> SuperIoReport {
    // Always-present honesty note (AREA-8 / DEC-201): detection ≠ controllability.
    let present_not_control =
        "Detection confirms a chip is present; it does not prove fan control is available. \
         Whether pwmN control works depends on routed PWM pins and the BIOS/EC/ACPI not \
         overriding the chip — verify after the driver is bound."
            .to_string();

    if !SUPERIO_ARCH_SUPPORTED {
        return SuperIoReport {
            arch_supported: false,
            chips: Vec::new(),
            acpi_conflict_drivers: Vec::new(),
            notes: vec!["Super-I/O detection is only supported on x86 / x86_64.".to_string()],
            signed_modules_only: None,
        };
    }

    let loaded = ev.loaded_modules();
    let acpi_conflict_drivers = ev.acpi_conflict_drivers();
    let kernel = KernelContext {
        version: ev.kernel_version(),
        signed_modules_only: ev.signed_modules_only(),
    };

    // Union evidence into a name-keyed map (BTreeMap ⇒ deterministic ordering).
    let mut acc: BTreeMap<String, EvidenceAcc> = BTreeMap::new();
    let bound_chips = ev.bound_chips();
    for b in bound_chips.iter().cloned() {
        // Only BOUND chips that are recognised Super-I/O monitoring chips become
        // candidates. An ordinary sensor chip (k10temp/coretemp/amdgpu/nvme/
        // spd5118/…) is a legitimate hwmon device but NOT a Super-I/O chip, and
        // must not be reported as an "Unrecognized Super-I/O" card (DEC-207). The
        // DMI/kmsg evidence sources below are already Super-I/O-scoped, so only
        // the bound-hwmon source needs this gate.
        if !chip_db::is_known_superio_chip(&b.chip_name) {
            continue;
        }
        let entry = acc.entry(normalize(&b.chip_name)).or_default();
        entry.bound = true;
        if entry.bound_driver.is_none() {
            entry.bound_driver = b.bound_driver;
        }
    }
    for c in ev.kmsg_chips() {
        acc.entry(normalize(&c)).or_default().kmsg = true;
    }
    let (board_vendor, board_name) = ev.board();
    let bound_names: Vec<&str> = bound_chips.iter().map(|b| b.chip_name.as_str()).collect();
    for c in chip_db::expected_chips_for_board(&board_vendor, &board_name, &bound_names) {
        acc.entry(normalize(&c)).or_default().dmi = true;
    }
    // `DC-da`: the board is known here, so the per-chip advice can be exact
    // where the aggregate readiness item (which carries no vendor) cannot.
    let guard_declines = chip_db::superio_guard_declines(&board_vendor, &board_name);

    let mut chips = Vec::with_capacity(acc.len());
    for (chip_name, ev_acc) in acc {
        chips.push(build_chip(
            &chip_name,
            &ev_acc,
            &loaded,
            &acpi_conflict_drivers,
            &|m| ev.module_out_of_tree(m),
            guard_declines,
            kernel,
        ));
    }

    let mut notes = vec![present_not_control];
    if chips.is_empty() {
        notes.push(
            "No Super-I/O monitoring chip was detected from the board table, loaded drivers, or \
             kernel logs. On boards whose Super-I/O driver is not yet loaded, the chip is not \
             visible passively."
                .to_string(),
        );
    }

    SuperIoReport {
        arch_supported: true,
        chips,
        acpi_conflict_drivers,
        notes,
        signed_modules_only: kernel.signed_modules_only,
    }
}

fn normalize(chip: &str) -> String {
    chip.trim().to_lowercase()
}

/// Build one [`SuperIoChip`] from its accumulated evidence.
fn build_chip(
    chip_name: &str,
    ev: &EvidenceAcc,
    loaded: &[String],
    acpi_conflict_drivers: &[String],
    // `HOST-d`: "is the loaded build out-of-tree?", taken from the evidence
    // trait rather than read here, so `build_chip` stays a pure function of its
    // inputs and the recommendation path is testable without a real `/sys`.
    out_of_tree: &dyn Fn(&str) -> Option<bool>,
    // `DC-da`: whether the package's Super-I/O guard declines its modules on
    // this board ([`chip_db::superio_guard_declines`]).
    guard_declines: bool,
    kernel: KernelContext,
) -> SuperIoChip {
    let expected_module = chip_db::expected_driver(chip_name).to_string();
    let vendor = vendor_for_module(&expected_module);
    let hwmon_present = ev.bound;
    // Observed, never inferred (`BRD-g`): the hwmon name is shared by both
    // nct668x drivers, so only the sysfs link says which one bound the chip.
    let bound_driver = if hwmon_present {
        ev.bound_driver.clone()
    } else {
        None
    };
    let module_loaded = bound_driver.is_some() || loaded.iter().any(|m| m == &expected_module);

    // Confidence: bound or kernel-logged ⇒ definitely present (High); a
    // board-table-only expectation is a strong-but-unconfirmed prior (Medium).
    let confidence = if ev.bound || ev.kmsg {
        Confidence::High
    } else if ev.dmi {
        Confidence::Medium
    } else {
        // Defensive: every candidate enters the accumulator via at least one
        // evidence source, so this arm is unreachable from the production
        // accumulator. Kept for total coverage of the enum.
        Confidence::Low
    };

    let mut evidence = Vec::new();
    if ev.dmi {
        evidence.push(Evidence::DmiBoardTable);
    }
    if ev.kmsg {
        evidence.push(Evidence::KernelLog);
    }
    if ev.bound {
        evidence.push(Evidence::BoundHwmon);
    }

    let mut caveats = Vec::new();
    let recommendation = if expected_module == "unknown" {
        caveats.push(
            "Unrecognized Super-I/O chip — no known in-kernel driver maps to this chip name."
                .to_string(),
        );
        None
    } else if hwmon_present {
        // Already bound and working — nothing to recommend.
        None
    } else {
        build_recommendation(
            chip_name,
            &expected_module,
            ev,
            if !module_loaded {
                DriverState::NotLoaded
            } else {
                match out_of_tree(&expected_module) {
                    Some(true) => DriverState::LoadedOutOfTree,
                    Some(false) => DriverState::LoadedInTree,
                    None => DriverState::LoadedUnread,
                }
            },
            loaded,
            acpi_conflict_drivers,
            guard_declines,
            kernel,
            &mut caveats,
        )
    };

    SuperIoChip {
        chip_name: chip_name.to_string(),
        vendor,
        evidence,
        confidence,
        bound_driver,
        expected_module,
        module_loaded,
        hwmon_present,
        recommendation,
        caveats,
    }
}

/// Production root for per-module kernel metadata.
///
/// `/sys` is mounted read-only-but-readable in the daemon's namespace, so this
/// is reachable. **`/lib/modules` is NOT** — see [`module_is_out_of_tree_at`].
const SYS_MODULE_ROOT: &str = "/sys/module";

/// Is the currently-loaded `module` an **out-of-tree** build rather than the
/// in-tree one? (`HOST-d`, DEC-327.)
///
/// Reads `/sys/module/<name>/taint` and looks for the `O` flag, which the kernel
/// sets on any module built outside the tree. Measured on the dev host:
/// `it87` (DKMS build) reads `OE`, while in-tree `kvm` and `nvme` read empty.
///
/// # Why not the module's path, which is the obvious answer
///
/// `HOST-d` proposed `/lib/modules/*/updates/dkms/<mod>.ko*`. **The daemon
/// cannot read that.** `ProtectKernelModules=true` in the unit file mounts an
/// inaccessible directory over `/usr/lib/modules` — verified in the running
/// daemon's own mount namespace, not inferred from documentation:
///
/// ```text
/// /systemd/inaccessible/dir  /usr/lib/modules  ro,nosuid,nodev,noexec
/// ```
///
/// (`/lib` symlinks to `/usr/lib`, so both spellings hit the same mount.)
/// `dkms status` is likewise out of reach. The taint flag is the only signal
/// that survives the sandbox, and it is arguably the better one anyway: it
/// describes what is **loaded**, not what happens to be installed on disk.
///
/// # What it does and does not tell you
///
/// `O` means "built out-of-tree" — **not** "installed by DKMS" specifically. A
/// hand-compiled module reads `O` too. For the one decision this drives that is
/// correct rather than approximate: the advice being suppressed is *"install the
/// DKMS build"*, and somebody already running any out-of-tree build is in
/// exactly the same position as somebody running the DKMS one.
///
/// Returns `false` when the file is missing or unreadable, which is the
/// conservative direction — the caller then keeps the install recommendation
/// rather than suppressing it on a failed read.
pub fn module_is_out_of_tree(module: &str) -> bool {
    module_is_out_of_tree_at(Path::new(SYS_MODULE_ROOT), module)
}

/// Internal: the same check against an injectable root (test seam), matching
/// the `amdgpu_module_loaded` / `amdgpu_module_loaded_at` pair in `gpu_detect`.
pub fn module_is_out_of_tree_at(module_root: &Path, module: &str) -> bool {
    module_taint_out_of_tree_at(module_root, module).unwrap_or(false)
}

/// [`module_taint_out_of_tree_at`] against the production root: `Some(true)`
/// only for a loaded out-of-tree build (an unloaded module has no taint file).
pub fn module_taint_out_of_tree(module: &str) -> Option<bool> {
    module_taint_out_of_tree_at(Path::new(SYS_MODULE_ROOT), module)
}

/// The `O` flag as read, `None` when the taint file could not be read (DEC-500)
/// — the reading `kernel_modules[].out_of_tree` publishes. A claim about which
/// build is loaded ("the kernel's own") needs a flag that was read.
pub fn module_taint_out_of_tree_at(module_root: &Path, module: &str) -> Option<bool> {
    std::fs::read_to_string(module_root.join(module).join("taint"))
        .ok()
        .map(|t| t.contains('O'))
}

/// How the expected driver stands for a present-but-unbound chip.
///
/// Replaces the pair `(module_loaded: bool, out_of_tree: bool)`. Two booleans
/// that must agree is the shape the project rulebook § Hard-won lessons records as
/// "two arguments to the same call, derived from different sources, will
/// eventually disagree" (DEC-325) — here `(false, true)`, "not loaded but
/// out-of-tree", is nonsense that the type system can simply forbid. It also
/// keeps `build_recommendation` inside clippy's argument budget, which is what
/// surfaced the coupling in the first place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverState {
    /// The expected module is not loaded at all.
    NotLoaded,
    /// Loaded, and it is the in-tree build.
    LoadedInTree,
    /// Loaded, and it is an out-of-tree build (`HOST-d`, DEC-327).
    LoadedOutOfTree,
    /// Loaded, with its taint unread (DEC-500). Advised as the in-tree build
    /// is — the install advice stays, the conservative direction — except that
    /// nothing says it is the kernel's own.
    LoadedUnread,
}

impl DriverState {
    fn loaded(self) -> bool {
        !matches!(self, Self::NotLoaded)
    }
}

/// What the running kernel decides about a recommendation (DEC-500), read once
/// per report from the evidence.
#[derive(Debug, Clone, Copy, Default)]
struct KernelContext {
    version: Option<chip_db::KernelVersion>,
    signed_modules_only: Option<bool>,
}

/// The upstream project an out-of-tree module is built from (DEC-499).
///
/// Hints name the project, never a distribution's package for it: the daemon
/// does not know the distribution, and the Arch package names hints used to give
/// told Debian and Fedora users to install something that does not exist there.
/// The GUI, which reads the machine's os-release, adds that distribution's
/// install steps beside the hint.
pub(crate) fn out_of_tree_source(module: &str) -> &'static str {
    match module {
        "it87" => "frankcrawford/it87",
        "nct6687" => "Fred78290/nct6687d",
        _ => "its upstream project",
    }
}

/// The ITE-specific tail of the "loaded but did not bind" hint.
///
/// Extracted so the branch is testable on its own — the surrounding
/// [`build_recommendation`] already branches on the driver state, and layering a
/// second special case inside it is what this project's rewrite-vs-patch rule
/// calls a broken abstraction.
fn ite_unbound_tail(out_of_tree: bool) -> &'static str {
    if out_of_tree {
        // `HOST-d`: the whole point. Telling somebody to install a build they
        // are demonstrably already running is what made this hint a loop.
        //
        // But the taint flag proves the build is OUT-OF-TREE, not that it is
        // CURRENT — so this must not send a user on a stale build to a dead end
        // while the GUI beside it correctly tells them to update. "Reinstalling
        // the same build" and "updating to a current one" are different actions
        // and only the first is futile.
        // DEC-421: one recovery ladder, not a DEVID branch the user cannot see —
        // `Unsupported chip (DEVID=…)` is `pr_debug`, and a 0xFFFF read prints
        // nothing at all. A rebuild past it87 v2.0 renames the chips, which
        // this daemon absorbs (DEC-442) — so the text says so, rather than
        // telling the user to re-check ids that no longer change.
        " You are already running an out-of-tree `it87` build, so reinstalling \
         the same build will not change anything — the plain 'install the \
         out-of-tree driver' advice is for somebody still on the in-tree driver. \
         If yours is old, updating is still worth doing: rebuilding it from \
         frankcrawford/it87's current source picks up several secondary-chip \
         fixes that landed in 2026-03 and later. \
         Builds from 2026-09-09 (it87 v2.0) on name Gigabyte chips with a board \
         suffix (e.g. it8696_a008090a); this daemon strips it, so fan header \
         ids, pump roles and profile members stay the same across the \
         rebuild. On Intel 800-series boards (Z890 / B860), builds from \
         2026-09-09 to 2026-10-01 can fail outright — `modprobe it87` reports \
         'Invalid argument' after an 'H2RAM set_slot … failed' line, or the chip \
         is found but never registers — a driver bug fixed on 2026-10-02 (it87 \
         PR #140), so update first. Do not \
         pass force_id, and do not add `mmio=on` (it is already the driver \
         default). If the chip still does not bind, the usual cause on these \
         boards is an ITE eSPI-to-LPC bridge latched in configuration mode in \
         front of it (the kernel log shows 0x8883 for it only with it87 dynamic \
         debug on). It is recoverable: something wrote a Super-I/O unlock to \
         0x2E/0x4E — nct6775, w83627ehf or sensors-detect — and this package \
         ships a guard that stops the two modules on every Gigabyte board. Stop the \
         trigger and reboot; if the chip is still missing, the machine must be \
         powered down at the wall, because the bridge runs on standby power and \
         a reboot does not always clear it. Full steps in the Hardware \
         Troubleshooting guide."
    } else {
        " For newer Gigabyte ITE boards the in-tree it87 often cannot drive the \
         chip — install the out-of-tree it87 driver from frankcrawford/it87 \
         (builds from 2026-09-09 name \
         Gigabyte chips with a board suffix, e.g. it8696_a008090a, which this \
         daemon strips, so ids do not change). On Intel 800-series boards \
         (Z890 / B860) use a build from 2026-10-02 or later: builds from \
         2026-09-09 to 2026-10-01 can fail to load ('Invalid argument') or bind \
         nothing there (it87 PR #140). (Do not add \
         `mmio=on`: it is already the driver default, DEC-326.) Do not pass \
         force_id. If the chip still does not bind, the usual cause on these \
         boards is an ITE eSPI-to-LPC bridge latched in configuration mode in \
         front of it (0x8883 in the kernel log with it87 dynamic debug on): keep \
         nct6775/w83627ehf off the board and do not run sensors-detect, reboot, \
         and if it is still missing, power fully down at the wall. Full steps in \
         the Hardware Troubleshooting guide."
    }
}

/// The likely cause when the kernel's own `it87` is loaded but cannot drive the
/// chip (DEC-500): DKMS builds a module per kernel, so a kernel whose rebuild
/// failed, or whose headers were never installed, boots with its own driver.
/// Prefixed to [`ite_unbound_tail`]'s in-tree branch. It gives no `modprobe -r`:
/// unloading `it87` under a running daemon drops every header it drives.
pub(crate) const IN_TREE_REBUILD_CAUSE: &str = " The `it87` loaded is the kernel's own, and it \
     cannot drive this chip. If you had installed the out-of-tree it87, the usual cause is a \
     kernel update its DKMS build did not follow: the rebuild for the new kernel failed, or \
     that kernel's headers were not installed, so it kept its own driver. `dkms status` lists \
     the kernels the build is installed for, and `sudo dkms autoinstall` builds it for the \
     running kernel once that kernel's headers are installed; then reboot.";

/// The cause named where a kernel refuses unsigned modules (DEC-500), from
/// `module_sig_check`'s two refusals: `sig_enforce` (`EKEYREJECTED`) and
/// lockdown (`EPERM`).
/// The recommendation notes the running kernel decides (DEC-500), shared by the
/// passive report and the port probe so the two cannot advise differently:
///
/// - a kernel that refuses unsigned modules refuses an out-of-tree build unless
///   it is signed with an enrolled key — named wherever such a build is needed
///   and is not the one loaded (`out_of_tree_loaded`);
/// - a chip in mainline only because this kernel is new enough gets the
///   out-of-tree build called optional, with what it still adds.
pub(crate) fn kernel_risk_notes(
    chip_name: &str,
    module: &str,
    in_mainline: bool,
    signed_modules_only: Option<bool>,
    out_of_tree_loaded: bool,
) -> Vec<String> {
    let mut notes = Vec::new();
    if !in_mainline && signed_modules_only == Some(true) && !out_of_tree_loaded {
        notes.push(signed_modules_note(module));
    }
    if in_mainline && module == "it87" {
        if let Some((major, minor)) = chip_db::mainline_since(chip_name) {
            notes.push(format!(
                "The kernel's own it87 drives this chip from Linux {major}.{minor}, so this \
                 kernel needs no out-of-tree driver for it. frankcrawford/it87 is optional \
                 here: it adds the board's GIGABYTE layout ID, which board header names \
                 need, and the fixes for boards where fan writes have no effect (its PR \
                 #128)."
            ));
        }
    }
    notes
}

pub(crate) fn signed_modules_note(module: &str) -> String {
    format!(
        "This kernel loads only signed modules (module signature enforcement or lockdown is \
         on, as distributions usually set it under Secure Boot), so an out-of-tree `{module}` \
         that is unsigned, or signed with a key this machine has not enrolled, is refused: \
         `modprobe {module}` reports 'Key was rejected by service' or 'Operation not \
         permitted'. DKMS signs the module as it builds it and prints the certificate it \
         used; enrol that file once with `sudo mokutil --import <file>`, reboot, and choose \
         'Enroll MOK' in the MOK Manager screen."
    )
}

/// Build a load recommendation for a present-but-unbound chip whose module is
/// allowlisted. Returns `None` (with a caveat) if the module is not on the
/// allowlist — the safety gate that stops the daemon ever recommending an
/// unvetted module.
#[allow(clippy::too_many_arguments)]
fn build_recommendation(
    chip_name: &str,
    module: &str,
    ev: &EvidenceAcc,
    driver: DriverState,
    loaded: &[String],
    acpi_conflict_drivers: &[String],
    guard_declines: bool,
    kernel: KernelContext,
    caveats: &mut Vec<String>,
) -> Option<SuperIoRecommendation> {
    let Some(entry) = allowlist_entry(module) else {
        caveats.push(format!(
            "{module} is not in the vetted Super-I/O module allowlist; no load recommendation is \
             offered."
        ));
        return None;
    };

    let in_mainline = chip_db::chip_driver_in_mainline(chip_name, kernel.version);

    let load_hint = if driver.loaded() {
        // The driver is already loaded but the chip produced no hwmon device —
        // telling the user to "load it" would be wrong. Point at the real
        // causes of a failed bind instead. The it87-dkms / mmio / force_id
        // notes are ITE-specific, so only add them for the it87 driver.
        let ite_tail = if module == "it87" {
            // DEC-500: the kernel's own it87, loaded, with a chip it cannot
            // drive — after an update, the out-of-tree build that should be
            // there was not rebuilt for this kernel.
            let rebuild = if driver == DriverState::LoadedInTree && !in_mainline {
                IN_TREE_REBUILD_CAUSE
            } else {
                ""
            };
            format!(
                "{rebuild}{}",
                ite_unbound_tail(driver == DriverState::LoadedOutOfTree)
            )
        } else {
            String::new()
        };
        format!(
            "The `{module}` driver is already loaded but no hwmon device appeared — the chip did \
             not bind. Common causes: an ACPI resource conflict (see caveats) or a reboot being \
             needed.{ite_tail}"
        )
    } else if guard_declines && chip_db::SUPERIO_GUARD_MODULES.contains(&module) {
        // `DC-da`: "load it" would be advice that does nothing here.
        superio_guard_load_hint(module)
    } else if in_mainline {
        format!(
            "Enable it at boot: `echo {module} | sudo tee /etc/modules-load.d/{module}.conf`, or \
             load it now with `sudo modprobe {module}`. A reboot or module reload may be needed \
             before the fans/sensors appear."
        )
    } else {
        let source = out_of_tree_source(module);
        format!(
            "This chip needs an out-of-tree driver: build `{module}` from {source} with DKMS \
             (your distribution's `dkms` package and the headers for your running kernel), then \
             load `{module}`. Do not pass force_id to bind an unrecognized chip ID — it misroutes \
             every register access."
        )
    };

    let reason = if driver.loaded() {
        format!(
            "The `{module}` driver is loaded but no hwmon device appeared for this chip — it did \
             not bind."
        )
    } else {
        match (ev.dmi, ev.kmsg) {
            (true, true) => format!(
                "Your board's chip table lists this chip and the kernel logged it, but no {module} \
                 driver is bound."
            ),
            (true, false) => {
                format!("Your board's chip table lists this chip, but no {module} driver is bound.")
            }
            (false, true) => format!(
                "The kernel logged this chip during its Super-I/O scan, but no {module} driver is \
                 bound."
            ),
            // Defensive: build_recommendation is only reached for an unbound
            // chip that entered the accumulator, so at least one of dmi/kmsg is
            // set here. Unreachable in practice; kept for a total match.
            (false, false) => {
                format!("This chip appears present, but no {module} driver is bound.")
            }
        }
    };

    let mut risk_notes = Vec::new();
    if !entry.risk_note.is_empty() {
        risk_notes.push(entry.risk_note.to_string());
    }
    if acpi_conflict_drivers.iter().any(|d| d == module) {
        risk_notes.push(format!(
            "ACPI firmware claims this chip's I/O ports; under acpi_enforce_resources=strict (the \
             default) the {module} driver may refuse to bind. Do not switch to lax or force the \
             driver without understanding the race it reintroduces."
        ));
    }
    risk_notes.extend(kernel_risk_notes(
        chip_name,
        module,
        in_mainline,
        kernel.signed_modules_only,
        driver == DriverState::LoadedOutOfTree,
    ));
    if let Some(other) = chip_db::conflicting_loaded_module(module, loaded) {
        if driver.loaded() {
            // Both drivers are loaded right now — the collision is ACTIVE, not
            // a future risk of loading.
            risk_notes.push(format!(
                "Both `{module}` and the conflicting driver `{other}` are currently loaded — this \
                 is exactly the chip-ID clash that can brick a fan header (DEC-106). Blacklist one \
                 driver and reboot now; do not leave both loaded."
            ));
        } else {
            risk_notes.push(format!(
                "The conflicting driver `{other}` is already loaded — also loading `{module}` \
                 risks a chip-ID clash that can brick a fan header (DEC-106). Identify the chip \
                 first and blacklist the wrong driver; do not load both."
            ));
        }
    }

    Some(SuperIoRecommendation {
        module: module.to_string(),
        in_mainline,
        load_hint,
        reason,
        risk_notes,
    })
}

/// The load hint for a module the package's Super-I/O guard declines on this
/// board (`DC-da`, DEC-468), shared by the passive detector and the port probe.
///
/// The usual "load it with `sudo modprobe`" would be advice that does nothing:
/// the guard's `install` rule intercepts the load, reports success and binds
/// nothing. Says so, why, and where the guard logs each refusal — the same facts
/// as the aggregate `superio_driver_unloaded` item, stated for this board rather
/// than conditionally.
pub(crate) fn superio_guard_load_hint(module: &str) -> String {
    format!(
        "On this Gigabyte board, this package's Super-I/O guard declines `{module}`: \
         Gigabyte boards carry their fans on ITE chips, and this module's probe can hide a \
         second one until the machine is powered down at the wall. Loading it reports \
         success and binds nothing; `sudo journalctl -b -t ventulus-superio-guard` \
         shows each module the guard declined. If fan headers are missing, the it87 driver \
         is the one to pursue."
    )
}

// ── Production evidence adapter ─────────────────────────────────────

/// Gathers real Super-I/O evidence from `chip_db` (passive `/proc`, `/sys`,
/// `/dev/kmsg` reads). The bound-chip list comes from the daemon's live cache
/// and is supplied by the caller (the endpoint handler in a later phase), so
/// this adapter stays free of any cache coupling and remains a thin, honest
/// mapping over `chip_db`.
pub struct SysfsSuperIoEvidence {
    bound: Vec<BoundChip>,
}

impl SysfsSuperIoEvidence {
    /// Construct from the caller-supplied bound-chip list (from the sensor/PWM
    /// cache). All other evidence is read from `chip_db` on demand.
    pub fn new(bound_chips: Vec<BoundChip>) -> Self {
        Self { bound: bound_chips }
    }
}

impl SuperIoEvidence for SysfsSuperIoEvidence {
    fn module_out_of_tree(&self, module: &str) -> Option<bool> {
        module_taint_out_of_tree(module)
    }

    fn kernel_version(&self) -> Option<chip_db::KernelVersion> {
        chip_db::kernel_version(crate::hwmon::kernel_warnings::read_kernel_release().as_deref())
    }

    fn signed_modules_only(&self) -> Option<bool> {
        crate::hwmon::kernel_security::KernelSecurity::read().signed_modules_only()
    }

    fn board(&self) -> (String, String) {
        let b = chip_db::read_board_info();
        (b.vendor, b.name)
    }

    fn bound_chips(&self) -> Vec<BoundChip> {
        self.bound.clone()
    }

    fn loaded_modules(&self) -> Vec<String> {
        chip_db::detect_loaded_modules()
            .into_iter()
            .filter(|m| m.loaded)
            .map(|m| m.name)
            .collect()
    }

    fn kmsg_chips(&self) -> Vec<String> {
        chip_db::read_kernel_detected_chips()
    }

    fn acpi_conflict_drivers(&self) -> Vec<String> {
        let mut v: Vec<String> = chip_db::detect_acpi_conflicts()
            .into_iter()
            .map(|c| c.conflicts_with_driver)
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake evidence for deterministic, hardware-free tests.
    #[derive(Default)]
    struct FakeEvidence {
        board: (String, String),
        bound: Vec<BoundChip>,
        loaded: Vec<String>,
        kmsg: Vec<String>,
        acpi: Vec<String>,
        /// Modules the fake reports as out-of-tree builds (`HOST-d`).
        out_of_tree: Vec<String>,
        /// Modules whose taint the fake cannot read (DEC-500).
        taint_unread: Vec<String>,
        /// The running kernel (DEC-500); `None` = unreadable.
        kernel: Option<chip_db::KernelVersion>,
        /// Whether the kernel refuses unsigned modules (DEC-500).
        signed_only: Option<bool>,
    }

    impl SuperIoEvidence for FakeEvidence {
        fn module_out_of_tree(&self, module: &str) -> Option<bool> {
            if self.taint_unread.iter().any(|m| m == module) {
                return None;
            }
            Some(self.out_of_tree.iter().any(|m| m == module))
        }
        fn kernel_version(&self) -> Option<chip_db::KernelVersion> {
            self.kernel
        }
        fn signed_modules_only(&self) -> Option<bool> {
            self.signed_only
        }
        fn board(&self) -> (String, String) {
            self.board.clone()
        }
        fn bound_chips(&self) -> Vec<BoundChip> {
            self.bound.clone()
        }
        fn loaded_modules(&self) -> Vec<String> {
            self.loaded.clone()
        }
        fn kmsg_chips(&self) -> Vec<String> {
            self.kmsg.clone()
        }
        fn acpi_conflict_drivers(&self) -> Vec<String> {
            self.acpi.clone()
        }
    }

    fn bound(name: &str, dev: &str) -> BoundChip {
        BoundChip {
            chip_name: name.to_string(),
            device_id: dev.to_string(),
            bound_driver: None,
        }
    }

    fn bound_by(name: &str, dev: &str, driver: &str) -> BoundChip {
        BoundChip {
            bound_driver: Some(driver.to_string()),
            ..bound(name, dev)
        }
    }

    fn find<'a>(r: &'a SuperIoReport, chip: &str) -> &'a SuperIoChip {
        r.chips
            .iter()
            .find(|c| c.chip_name == chip)
            .unwrap_or_else(|| panic!("chip {chip} not in report: {:?}", r.chips))
    }

    /// `BRD-a`: on it87 v2.0 the bound chips publish as `it8696_a008090a` /
    /// `it87952_a008090a` while the board table names `it8696` / `it87952`.
    /// Driven from real discovery output, the report must hold ONE card per
    /// chip — not a bound suffixed card plus a phantom "loaded but no hwmon
    /// device appeared" card for the same chip.
    #[test]
    fn it87_v2_suffixed_chips_do_not_raise_phantom_unbound_cards() {
        let root = tempfile::tempdir().unwrap();
        for (dir, name, dev) in [
            ("hwmon3", "it8696_a008090a", "it87.2624"),
            ("hwmon4", "it87952_a008090a", "it87.2640"),
        ] {
            let platform = root.path().join(format!("devices/{dev}"));
            std::fs::create_dir_all(&platform).unwrap();
            let d = root.path().join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::os::unix::fs::symlink(&platform, d.join("device")).unwrap();
            std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(d.join("pwm1"), "128\n").unwrap();
        }
        let bound_chips: Vec<BoundChip> =
            crate::hwmon::pwm_discovery::discover_pwm_headers(root.path(), &Default::default())
                .unwrap()
                .into_iter()
                .map(|h| bound(&h.chip_name, &h.device_id))
                .collect();
        assert_eq!(bound_chips.len(), 2, "precondition: both chips discovered");

        let ev = FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".into(),
                "X870E AORUS MASTER".into(),
            ),
            bound: bound_chips,
            loaded: vec!["it87".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let names: Vec<&str> = r.chips.iter().map(|c| c.chip_name.as_str()).collect();
        assert_eq!(names, vec!["it8696", "it87952"], "{:?}", r.chips);
        for c in &r.chips {
            assert!(c.hwmon_present, "{c:?}");
            assert!(
                c.recommendation.is_none(),
                "a bound chip needs no fix: {c:?}"
            );
        }
    }

    /// `DC-cp`: a Z390 board whose primary is bound as `it8686` gets one card
    /// for it, carrying the board-table evidence — not an unbound `it8688`
    /// card recommending a driver for a chip the board does not have. With
    /// `it8688` bound the listed primary is the board-table chip as before.
    #[test]
    fn a_bound_alternative_primary_takes_the_board_table_evidence() {
        let board = (
            "Gigabyte Technology Co., Ltd.".to_string(),
            "Z390 AORUS PRO WIFI".to_string(),
        );
        for primary in ["it8686", "it8688"] {
            let ev = FakeEvidence {
                board: board.clone(),
                bound: vec![bound(primary, "it87.2624"), bound("it8792", "it87.2656")],
                loaded: vec!["it87".into()],
                ..Default::default()
            };
            let r = detect_superio(&ev);
            let names: Vec<&str> = r.chips.iter().map(|c| c.chip_name.as_str()).collect();
            assert_eq!(names, vec![primary, "it8792"], "{:?}", r.chips);
            for c in &r.chips {
                assert!(c.evidence.contains(&Evidence::DmiBoardTable), "{c:?}");
                assert!(c.recommendation.is_none(), "{c:?}");
            }
        }
    }

    #[test]
    fn report_always_carries_present_not_control_note() {
        let r = detect_superio(&FakeEvidence::default());
        assert!(r.arch_supported); // test host is x86
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("does not prove fan control")),
            "every report must carry the present≠control honesty note"
        );
    }

    #[test]
    fn empty_evidence_yields_no_chips_and_a_note() {
        let r = detect_superio(&FakeEvidence::default());
        assert!(r.chips.is_empty());
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("No Super-I/O monitoring chip")));
    }

    #[test]
    fn gigabyte_board_unbound_ite_recommends_dkms() {
        // X570 AORUS MASTER's DMI entry expects it8688 + it8792 (both DKMS-only
        // ITE chips). Nothing is bound/loaded → expect a DKMS recommendation.
        let ev = FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".into(),
                "X570 AORUS MASTER".into(),
            ),
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "it8688");
        assert_eq!(chip.expected_module, "it87");
        assert_eq!(chip.vendor, SuperIoVendor::Ite);
        assert_eq!(chip.confidence, Confidence::Medium); // DMI-only
        assert!(!chip.hwmon_present);
        assert_eq!(chip.evidence, vec![Evidence::DmiBoardTable]);
        let rec = chip
            .recommendation
            .as_ref()
            .expect("unbound chip → recommendation");
        assert_eq!(rec.module, "it87");
        assert!(!rec.in_mainline, "it8688 is DKMS-only (DEC-144)");
        assert!(rec.load_hint.contains("DKMS"));
        assert!(rec.load_hint.contains("frankcrawford/it87"));
        assert!(rec
            .risk_notes
            .iter()
            .any(|n| n.contains("frankcrawford/it87")));
    }

    #[test]
    fn bound_nuvoton_chip_needs_no_recommendation() {
        let ev = FakeEvidence {
            bound: vec![bound_by("nct6799", "isa-0290", "nct6775")],
            loaded: vec!["nct6775".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6799");
        assert!(chip.hwmon_present);
        assert_eq!(chip.confidence, Confidence::High);
        assert_eq!(chip.bound_driver.as_deref(), Some("nct6775"));
        assert!(chip.module_loaded);
        assert!(
            chip.recommendation.is_none(),
            "a bound, working chip should not be told to load anything"
        );
        assert_eq!(chip.evidence, vec![Evidence::BoundHwmon]);
    }

    /// `BRD-g`: the hwmon name is shared by both nct668x drivers, so the
    /// observed driver is reported — an MSI NCT6687D the in-kernel nct6683
    /// bound is not reported as the out-of-tree nct6687 its name suggests.
    #[test]
    fn bound_driver_is_the_observed_one_not_the_name_guess() {
        let ev = FakeEvidence {
            bound: vec![bound_by("nct6687", "nct6683.2592", "nct6683")],
            loaded: vec!["nct6683".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6687");
        assert_eq!(chip.expected_module, "nct6687", "the name still guesses");
        assert_eq!(chip.bound_driver.as_deref(), Some("nct6683"));
        assert!(chip.recommendation.is_none());
    }

    /// `BRD-g`: `module_loaded` follows the observed driver. An `nct6683`
    /// chip the out-of-tree nct6687 bound has its driver loaded although the
    /// module its name suggests is not — the arm the name-keyed check could
    /// never report.
    #[test]
    fn module_loaded_follows_the_observed_driver() {
        let ev = FakeEvidence {
            bound: vec![bound_by("nct6683", "nct6687.2592", "nct6687")],
            loaded: vec!["nct6687".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6683");
        assert_eq!(chip.expected_module, "nct6683");
        assert!(
            !ev.loaded.contains(&chip.expected_module),
            "precondition: the name-suggested module is not loaded"
        );
        assert_eq!(chip.bound_driver.as_deref(), Some("nct6687"));
        assert!(chip.module_loaded);
    }

    /// A bound chip whose driver link was not read reports no driver — never
    /// the name's guess — and `module_loaded` keeps its name-keyed meaning.
    #[test]
    fn an_unobserved_driver_is_absent_not_guessed() {
        let ev = FakeEvidence {
            bound: vec![bound("nct6799", "isa-0290")],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6799");
        assert!(chip.hwmon_present);
        assert_eq!(chip.bound_driver, None);
        assert!(!chip.module_loaded);
    }

    #[test]
    fn unbound_chip_with_conflicting_loaded_driver_warns_dec106() {
        // Exercises the recommender's collision path: an unbound Nuvoton chip
        // whose module (nct6775) is NOT loaded, but the conflicting nct6687 IS
        // → the recommendation must carry the DEC-106 brick warning. We inject
        // it via kmsg because the trait is source-agnostic; in production a
        // Nuvoton chip only surfaces this way once bound (the kmsg reader is
        // ITE-only), so this path is chiefly a guard for the future active
        // probe. `module_loaded` is false here → the "hypothetical" phrasing.
        let ev = FakeEvidence {
            kmsg: vec!["nct6799".into()],
            loaded: vec!["nct6687".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6799");
        assert_eq!(chip.confidence, Confidence::High); // kernel-logged
        let rec = chip
            .recommendation
            .as_ref()
            .expect("unbound → recommendation");
        assert_eq!(rec.module, "nct6775");
        assert!(
            rec.risk_notes
                .iter()
                .any(|n| n.contains("DEC-106") && n.contains("nct6687")),
            "must warn that nct6687 is already loaded, risk_notes={:?}",
            rec.risk_notes
        );
    }

    /// `BRD-ac`: an unbound NCT6686D (an ASRock part) is pointed at the
    /// in-kernel nct6683, as the GUI's guidance does — not at the out-of-tree
    /// nct6687, whose recommendation beside a loaded nct6775 (the ASRock
    /// Taichi's second Nuvoton chip) carried the DEC-106 brick warning, which
    /// nct6683 does not share.
    #[test]
    fn unbound_nct6686_is_pointed_at_the_in_kernel_nct6683() {
        let ev = FakeEvidence {
            kmsg: vec!["nct6686".into()],
            loaded: vec!["nct6775".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6686");
        assert!(!chip.hwmon_present);
        assert_eq!(chip.expected_module, "nct6683");
        let rec = chip
            .recommendation
            .as_ref()
            .expect("unbound → recommendation");
        assert_eq!(rec.module, "nct6683");
        assert!(
            !rec.risk_notes.iter().any(|n| n.contains("DEC-106")),
            "nct6683 collides with nothing loaded here, risk_notes={:?}",
            rec.risk_notes
        );
    }

    #[test]
    fn active_collision_when_both_drivers_loaded_is_flagged_as_current() {
        // nct6775 IS loaded AND the conflicting nct6687 IS loaded, but the chip
        // did not bind → the DEC-106 note must say the collision is happening
        // NOW (blacklist + reboot), not frame it as a risk of a future load.
        let ev = FakeEvidence {
            kmsg: vec!["nct6799".into()],
            loaded: vec!["nct6775".into(), "nct6687".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "nct6799");
        assert!(chip.module_loaded);
        assert!(!chip.hwmon_present);
        let rec = chip
            .recommendation
            .as_ref()
            .expect("loaded-but-unbound colliding chip still needs guidance");
        assert!(
            rec.risk_notes.iter().any(|n| n.contains("currently loaded")
                && n.contains("DEC-106")
                && n.contains("reboot now")),
            "active collision must be framed as current, got {:?}",
            rec.risk_notes
        );
        assert!(
            !rec.risk_notes.iter().any(|n| n.contains("also loading")),
            "must not frame an already-active collision as a hypothetical future load"
        );
    }

    #[test]
    fn acpi_conflict_is_surfaced_in_recommendation() {
        let ev = FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".into(),
                "X570 AORUS MASTER".into(),
            ),
            acpi: vec!["it87".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        assert_eq!(r.acpi_conflict_drivers, vec!["it87".to_string()]);
        let rec = find(&r, "it8688").recommendation.as_ref().unwrap();
        assert!(
            rec.risk_notes
                .iter()
                .any(|n| n.contains("acpi_enforce_resources")),
            "ACPI conflict must appear as a caveat, got {:?}",
            rec.risk_notes
        );
    }

    #[test]
    fn unrecognized_chip_gets_no_recommendation() {
        let ev = FakeEvidence {
            kmsg: vec!["xyz9000".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "xyz9000");
        assert_eq!(chip.expected_module, "unknown");
        assert_eq!(chip.vendor, SuperIoVendor::Unknown);
        assert!(chip.recommendation.is_none());
        assert!(chip.caveats.iter().any(|c| c.contains("Unrecognized")));
    }

    // ── [HOST-d / DEC-327] out-of-tree detection ────────────────────

    /// The DETECTOR, against a fake `/sys/module` tree — never real hardware
    /// (the project rulebook § Testing policy).
    #[test]
    fn out_of_tree_is_read_from_the_module_taint_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Shapes measured on the dev host 2026-09-04.
        for (module, taint) in [("it87", "OE\n"), ("kvm", "\n"), ("nvme", "")] {
            std::fs::create_dir_all(root.join(module)).unwrap();
            std::fs::write(root.join(module).join("taint"), taint).unwrap();
        }
        assert!(
            module_is_out_of_tree_at(root, "it87"),
            "`OE` carries the out-of-tree flag"
        );
        assert!(
            !module_is_out_of_tree_at(root, "kvm"),
            "in-tree: empty taint"
        );
        assert!(
            !module_is_out_of_tree_at(root, "nvme"),
            "in-tree: empty taint"
        );
        // A module with no taint file, and one absent entirely, must both be
        // FALSE — the conservative direction, which keeps the install advice
        // rather than suppressing it on a failed read.
        std::fs::create_dir_all(root.join("notaint")).unwrap();
        assert!(!module_is_out_of_tree_at(root, "notaint"));
        assert!(!module_is_out_of_tree_at(root, "absent"));
    }

    /// The rule itself, asserted as a RELATIONSHIP between the two branches
    /// rather than against a literal string — a substring check on "install"
    /// would pass against a sentence saying installing does not help.
    #[test]
    fn the_unbound_tail_stops_recommending_a_build_already_loaded() {
        let already = ite_unbound_tail(true);
        let not_yet = ite_unbound_tail(false);

        assert!(
            already.contains("reinstalling the same build will not change anything"),
            "the out-of-tree branch must say REINSTALLING is futile"
        );
        // [security-reviewer finding 1] ...but the taint flag proves the build
        // is out-of-tree, NOT that it is current. A user on a stale out-of-tree
        // build must still be told updating is worth doing — otherwise this
        // sends them to a dead end while the GUI beside it says the opposite.
        assert!(
            already.contains("updating is still worth doing"),
            "an out-of-tree build may still be STALE; the branch must not \
             present a dead end. got: {already}"
        );
        // The 0x8883 advice must stay gated on the DEVID condition rather than
        // applying to every out-of-tree build — but it is no longer terminal.
        // DEC-332 measured the recovery, so a hint that still said "no local
        // fix" would be telling a user to give up on a chip they can get back.
        assert!(
            already.contains("0x8883"),
            "the bridge advice must stay scoped to the DEVID that identifies it"
        );
        assert!(
            !already.contains("no local fix"),
            "retracted by DEC-332: the latch IS clearable (suppress the \
             Nuvoton/Winbond probe, then a full power cut). Telling a user \
             otherwise costs them 3 of 8 fan headers permanently."
        );
        assert!(
            already.contains("powered down at the wall"),
            "a reboot does NOT clear the latch — the hint has to say so, \
             because trying a reboot and failing is exactly what makes users \
             conclude the chip is dead. got: {already}"
        );
        // And it must not cite a CLOSED upstream issue as the live thread —
        // it87 #64 is recorded closed 2025-12 (docs/19:705). Same defect as
        // HOST-c, which cited #81 as a resolution where it records a failure.
        assert!(
            !already.contains("#64"),
            "do not point users at a closed issue as the tracking thread"
        );
        assert!(
            not_yet.contains("install the out-of-tree it87 driver"),
            "the in-tree branch must still offer the install that DOES help"
        );
        // The opposite branch, or a predicate stuck on one answer passes above.
        assert_ne!(already, not_yet, "the branches must actually differ");
        // `HOST-d` is explicit that this half stays, on BOTH branches. It is
        // correct, well-tested, and independently reconfirmed by DEC-326.
        assert!(already.contains("force_id") && not_yet.contains("force_id"));
    }

    /// `BLD-01` / `BRD-m` (it87 #139, #138): on Intel 800-series boards the fork's
    /// builds from 2026-09-09 to 2026-10-01 fail to load or bind nothing — a driver
    /// bug PR #140 fixed — so before the bridge-latch recovery, both branches must
    /// tell the user which builds to avoid, and the out-of-tree branch to update.
    #[test]
    fn the_unbound_tail_names_the_intel_800_series_build_window() {
        for (oot, tail) in [
            (true, ite_unbound_tail(true)),
            (false, ite_unbound_tail(false)),
        ] {
            assert!(tail.contains("Z890 / B860"), "oot={oot}: {tail}");
            assert!(
                tail.contains("2026-09-09 to 2026-10-01"),
                "oot={oot}: {tail}"
            );
            assert!(tail.contains("PR #140"), "oot={oot}: {tail}");
            // The build-window advice comes BEFORE the latch recovery, so a user
            // on a broken build is not sent to power-cycle first.
            let window = tail.find("Z890 / B860").unwrap();
            let latch = tail.find("eSPI-to-LPC bridge").unwrap();
            assert!(window < latch, "oot={oot}: {tail}");
        }
        assert!(ite_unbound_tail(true).contains("update first"));
    }

    /// The CALL-SITE test. The detector and the tail each have their own
    /// coverage above; this drives `detect_superio` and asserts the rendered
    /// recommendation changes — because "extracting a rule into a testable
    /// function does NOT test the call site" is this project's most-repeated
    /// lesson, and the whole defect was a call site rendering the wrong string.
    #[test]
    fn an_unbound_chip_on_an_out_of_tree_build_is_not_told_to_install_it() {
        let board = (
            "Gigabyte Technology Co., Ltd.".to_string(),
            "X870E AORUS MASTER".to_string(),
        );
        // Loaded, but nothing bound — the exact state this host is in.
        let mk = |oot: Vec<String>| FakeEvidence {
            board: board.clone(),
            loaded: vec!["it87".to_string()],
            kmsg: vec!["it8696".to_string()],
            out_of_tree: oot,
            ..Default::default()
        };

        let dkms = detect_superio(&mk(vec!["it87".to_string()]));
        let in_tree = detect_superio(&mk(vec![]));

        let hint = |r: &SuperIoReport| {
            r.chips
                .iter()
                .find_map(|c| c.recommendation.as_ref().map(|x| x.load_hint.clone()))
                .expect("an unbound allowlisted chip must produce a recommendation")
        };
        let (dkms_hint, in_tree_hint) = (hint(&dkms), hint(&in_tree));

        assert!(
            !dkms_hint.contains("install the out-of-tree it87 driver"),
            "the user is already running an out-of-tree build; telling them to \
             install one is the loop HOST-d exists to break. got: {dkms_hint}"
        );
        assert!(
            in_tree_hint.contains("install the out-of-tree it87 driver"),
            "somebody still on the in-tree driver must still be told to install \
             it. got: {in_tree_hint}"
        );
        // Asserted as a relationship against the extracted rule, not a literal.
        assert!(dkms_hint.ends_with(ite_unbound_tail(true)));
        assert!(in_tree_hint.ends_with(ite_unbound_tail(false)));
    }

    /// `DC-da`, the call site: `detect_superio` reads the board and the per-chip
    /// advice for a guarded module follows the guard. Driven with the same
    /// unbound Nuvoton chip on a Gigabyte and an MSI board; the hint is the
    /// guard's on the first and the `modprobe` on the second, so a flag stuck
    /// either way fails one arm.
    #[test]
    fn an_unbound_nuvoton_chip_on_a_gigabyte_board_is_told_about_the_guard() {
        let hint = |vendor: &str, name: &str| {
            let r = detect_superio(&FakeEvidence {
                board: (vendor.to_string(), name.to_string()),
                kmsg: vec!["nct6798".to_string()],
                ..Default::default()
            });
            r.chips
                .iter()
                .find(|c| c.expected_module == "nct6775")
                .and_then(|c| c.recommendation.as_ref())
                .map(|x| x.load_hint.clone())
                .expect("an unbound nct6798 must produce a recommendation")
        };
        let gigabyte = hint("Gigabyte Technology Co., Ltd.", "B650 AORUS ELITE AX");
        let msi = hint("Micro-Star International Co., Ltd.", "MAG B650 TOMAHAWK");
        assert_eq!(gigabyte, superio_guard_load_hint("nct6775"));
        assert!(!gigabyte.contains("modprobe"), "{gigabyte}");
        assert!(msi.contains("sudo modprobe nct6775"), "{msi}");
    }

    /// `DriverState` exists so "not loaded but out-of-tree" cannot be
    /// represented. That is a type-level guarantee, but its *behavioural*
    /// consequence is testable: an unloaded module's advice must be identical
    /// whether or not the taint probe would say out-of-tree, because the flag
    /// must never reach that branch.
    #[test]
    fn the_out_of_tree_flag_cannot_leak_into_the_not_loaded_branch() {
        let mk = |oot: Vec<String>| FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".to_string(),
                "X870E AORUS MASTER".to_string(),
            ),
            loaded: vec![], // <- the module is NOT loaded
            kmsg: vec!["it8696".to_string()],
            out_of_tree: oot,
            ..Default::default()
        };
        let claims_oot = detect_superio(&mk(vec!["it87".to_string()]));
        let clean = detect_superio(&mk(vec![]));
        assert_eq!(
            format!("{:?}", claims_oot.chips),
            format!("{:?}", clean.chips),
            "an unloaded module has no build to be out-of-tree; the flag must \
             not change its advice"
        );
        // Precondition: the case really is the not-loaded one, or this asserts
        // nothing — a recommendation must exist and must offer the install.
        let hint = clean
            .chips
            .iter()
            .find_map(|c| c.recommendation.as_ref().map(|r| r.load_hint.clone()))
            .expect("an unbound allowlisted chip must produce a recommendation");
        assert!(hint.contains("Enable it at boot") || hint.contains("DKMS"));
    }

    /// The detection must not leak to other families: only the ITE tail branches
    /// on it, so a Nuvoton chip's advice is byte-identical either way.
    #[test]
    fn out_of_tree_detection_does_not_alter_non_ite_advice() {
        let mk = |oot: Vec<String>| FakeEvidence {
            board: ("MSI".to_string(), "MAG B650".to_string()),
            loaded: vec!["nct6687".to_string()],
            kmsg: vec!["nct6687".to_string()],
            out_of_tree: oot,
            ..Default::default()
        };
        let a = detect_superio(&mk(vec!["nct6687".to_string()]));
        let b = detect_superio(&mk(vec![]));
        assert_eq!(
            format!("{:?}", a.chips),
            format!("{:?}", b.chips),
            "only the ITE tail may branch on the out-of-tree flag"
        );
    }

    #[test]
    fn recommendations_never_suggest_forbidden_parameters() {
        // Sweep unbound-chip scenarios across every recommendation-producing
        // family (ITE via DMI, plus Winbond/SMSC/National/Nuvoton/Fintek via
        // kmsg, which the trait treats source-agnostically). No recommendation
        // string — load_hint, reason, OR any risk_note — may propose a risky
        // parameter as an action.
        let mut evs: Vec<FakeEvidence> = vec![
            FakeEvidence {
                board: (
                    "Gigabyte Technology Co., Ltd.".into(),
                    "X570 AORUS MASTER".into(),
                ),
                ..Default::default()
            },
            FakeEvidence {
                board: (
                    "Gigabyte Technology Co., Ltd.".into(),
                    "X870E AORUS MASTER".into(),
                ),
                ..Default::default()
            },
            // [security-reviewer finding 5] Every fixture above leaves `loaded`
            // empty, so `module_loaded` is false and the ITE tail is `""` — this
            // sweep has never reached either branch of `ite_unbound_tail`, and
            // a future edit adding `force_id=0x8696` there would ship green.
            // These two put both branches under the guard.
            FakeEvidence {
                board: (
                    "Gigabyte Technology Co., Ltd.".into(),
                    "X870E AORUS MASTER".into(),
                ),
                loaded: vec!["it87".into()],
                ..Default::default()
            },
            FakeEvidence {
                board: (
                    "Gigabyte Technology Co., Ltd.".into(),
                    "X870E AORUS MASTER".into(),
                ),
                loaded: vec!["it87".into()],
                out_of_tree: vec!["it87".into()],
                ..Default::default()
            },
        ];
        // One unbound chip per non-ITE family, surfaced via kmsg.
        for chip in [
            "nct6799",
            "w83627ehf",
            "smsc47m1",
            "pc87360",
            "f71805f",
            "dme1737",
        ] {
            evs.push(FakeEvidence {
                kmsg: vec![chip.to_string()],
                ..Default::default()
            });
        }
        let mut saw_recommendation = false;
        for ev in &evs {
            let r = detect_superio(ev);
            for chip in &r.chips {
                if let Some(rec) = &chip.recommendation {
                    saw_recommendation = true;
                    let mut fields = vec![rec.load_hint.clone(), rec.reason.clone()];
                    fields.extend(rec.risk_notes.iter().cloned());
                    for f in &fields {
                        assert!(
                            !f.contains("force_id=") && !f.contains("ignore_resource_conflict="),
                            "no recommendation field may propose a forbidden parameter, got: {f}"
                        );
                    }
                }
            }
        }
        assert!(
            saw_recommendation,
            "the sweep must actually exercise recommendations"
        );
    }

    #[test]
    fn loaded_but_unbound_chip_advises_binding_fix_not_a_reload() {
        // The common "DKMS installed, module loaded, but no hwmon device" state:
        // the recommendation must NOT tell the user to load an already-loaded
        // module — it must point at the real bind failure (ACPI/reboot/mmio).
        let ev = FakeEvidence {
            kmsg: vec!["it8696".into()],
            loaded: vec!["it87".into()], // module IS loaded
            bound: vec![],               // but the chip did not bind
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "it8696");
        assert!(chip.module_loaded);
        assert!(!chip.hwmon_present);
        let rec = chip
            .recommendation
            .as_ref()
            .expect("loaded-but-unbound chip still needs guidance");
        assert!(
            rec.load_hint.contains("already loaded") && rec.load_hint.contains("did not bind"),
            "hint must address the failed bind, not say 'load it', got: {}",
            rec.load_hint
        );
        assert!(
            !rec.load_hint.contains("modprobe it87"),
            "must not tell the user to load an already-loaded module"
        );
        assert!(rec.reason.contains("did not bind"));
    }

    #[test]
    fn both_dmi_and_kmsg_unbound_uses_combined_reason() {
        // Exercises the (dmi=true, kmsg=true) reason branch: board table lists
        // the chip AND the kernel logged it, but nothing is bound.
        let ev = FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".into(),
                "X570 AORUS MASTER".into(),
            ),
            kmsg: vec!["it8688".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "it8688");
        assert_eq!(
            chip.evidence,
            vec![Evidence::DmiBoardTable, Evidence::KernelLog]
        );
        let rec = chip.recommendation.as_ref().unwrap();
        assert!(
            rec.reason
                .contains("lists this chip and the kernel logged it"),
            "combined-source reason expected, got: {}",
            rec.reason
        );
    }

    #[test]
    fn same_chip_from_different_sources_and_cases_deduplicates() {
        // A chip seen as "NCT6799" in kmsg and "nct6799" when bound must collapse
        // to ONE entry (guards normalize()), carrying both evidence sources.
        let ev = FakeEvidence {
            kmsg: vec!["NCT6799".into()],
            bound: vec![bound("nct6799", "isa-0290")],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        assert_eq!(
            r.chips.len(),
            1,
            "case-different names must dedupe: {:?}",
            r.chips
        );
        let chip = &r.chips[0];
        assert_eq!(chip.chip_name, "nct6799");
        assert_eq!(
            chip.evidence,
            vec![Evidence::KernelLog, Evidence::BoundHwmon]
        );
        assert!(chip.hwmon_present);
    }

    #[test]
    fn bound_non_superio_sensor_chips_are_never_cards() {
        // DEC-207: ordinary sensor chips are legitimate hwmon devices but NOT
        // Super-I/O chips — none may appear as an "Unrecognized Super-I/O" card.
        let ev = FakeEvidence {
            bound: vec![
                bound("amdgpu", "pci-0000:03:00.0"),
                bound("k10temp", "pci-0000:00:18.3"),
                bound("nvme", "pci-0000:01:00.0"),
                bound("spd5118", "i2c-0-0050"),
                bound("coretemp", "isa-0000"),
            ],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        assert!(
            r.chips.is_empty(),
            "no non-Super-I/O sensor chip may become a card: {:?}",
            r.chips
        );
    }

    #[test]
    fn bound_genuine_superio_chips_are_cards() {
        for name in ["nct6799", "it8688"] {
            let ev = FakeEvidence {
                bound: vec![bound(name, "isa-0290")],
                ..Default::default()
            };
            let r = detect_superio(&ev);
            assert_eq!(r.chips.len(), 1, "{name} should be one card: {:?}", r.chips);
            assert_eq!(r.chips[0].chip_name, name);
            assert!(r.chips[0].hwmon_present);
        }
    }

    #[test]
    fn bound_mixed_keeps_only_superio_chips() {
        // A realistic host: a Super-I/O chip bound alongside ordinary sensor
        // chips. Only the Super-I/O chips are cards.
        let ev = FakeEvidence {
            bound: vec![
                bound("k10temp", "pci-0000:00:18.3"),
                bound("nct6799", "isa-0290"),
                bound("amdgpu", "pci-0000:03:00.0"),
                bound("it8688", "isa-0a40"),
            ],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let mut names: Vec<&str> = r.chips.iter().map(|c| c.chip_name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["it8688", "nct6799"]);
    }

    #[test]
    fn same_chip_via_bound_dmi_and_kmsg_is_one_card_with_all_evidence() {
        // The X570 AORUS MASTER DMI table expects it8688; the same chip is also
        // kernel-logged and bound. All three evidence sources must collapse into
        // ONE it8688 card (DEC-207 "combine evidence, don't duplicate").
        let ev = FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".into(),
                "X570 AORUS MASTER".into(),
            ),
            kmsg: vec!["IT8688".into()],
            bound: vec![bound("it8688", "isa-0a40")],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        assert_eq!(
            r.chips.iter().filter(|c| c.chip_name == "it8688").count(),
            1,
            "it8688 must be a single card, not one per source: {:?}",
            r.chips
        );
        let chip = find(&r, "it8688");
        assert_eq!(
            chip.evidence,
            vec![
                Evidence::DmiBoardTable,
                Evidence::KernelLog,
                Evidence::BoundHwmon
            ]
        );
        assert!(chip.hwmon_present);
    }

    #[test]
    fn allowlist_covers_every_recognized_module() {
        // Safety-gate completeness: every non-"unknown" module that
        // chip_db::expected_driver can return must have an allowlist entry, so
        // a recognized chip always has a rationale and is never silently
        // dropped for lack of an entry.
        let sample_chips = [
            "it8688",
            "nct6799",
            "nct6687",
            "nct6683",
            "f71882fg",
            "f71805f",
            "w83627ehf",
            "w83627hf",
            "smsc47m1",
            "smsc47m192",
            "smsc47b397",
            "dme1737",
            "sch5627",
            "sch5636",
            "pc87360",
            "pc87427",
            // `DC-db` (DEC-473): names the mapping newly recognises.
            "f81768d",
        ];
        for chip in sample_chips {
            let module = chip_db::expected_driver(chip);
            assert_ne!(module, "unknown", "test chip {chip} should be recognized");
            assert!(
                allowlist_entry(module).is_some(),
                "module {module} (for chip {chip}) must be in SUPERIO_ALLOWLIST"
            );
        }
    }

    #[test]
    fn multi_source_evidence_reinforces_high_confidence() {
        // A chip that is both board-listed AND bound is High confidence and
        // records both evidence sources (deterministic order dmi, kmsg, bound).
        let ev = FakeEvidence {
            board: (
                "Gigabyte Technology Co., Ltd.".into(),
                "X570 AORUS MASTER".into(),
            ),
            bound: vec![bound("it8688", "isa-0a40")],
            loaded: vec!["it87".into()],
            ..Default::default()
        };
        let r = detect_superio(&ev);
        let chip = find(&r, "it8688");
        assert_eq!(chip.confidence, Confidence::High);
        assert_eq!(
            chip.evidence,
            vec![Evidence::DmiBoardTable, Evidence::BoundHwmon]
        );
        assert!(chip.hwmon_present);
        assert!(chip.recommendation.is_none());
    }

    #[test]
    fn arch_gate_reflects_build_target() {
        // On the x86_64 CI/build host the detector reports arch_supported=true;
        // this pins the gate to the real build target (comparing a runtime
        // field, not a bare const) rather than hard-coding `true`.
        let r = detect_superio(&FakeEvidence::default());
        assert_eq!(r.arch_supported, SUPERIO_ARCH_SUPPORTED);
    }

    /// DEC-499: every out-of-tree recommendation names the upstream project the
    /// module is built from, on every driver state that reaches the install text.
    #[test]
    fn out_of_tree_hints_name_the_upstream_project() {
        for (module, project) in [
            ("it87", "frankcrawford/it87"),
            ("nct6687", "Fred78290/nct6687d"),
        ] {
            assert_eq!(out_of_tree_source(module), project);
            let mut caveats = Vec::new();
            let ev = EvidenceAcc {
                dmi: true,
                ..Default::default()
            };
            // A chip whose driver is not in mainline, with nothing loaded.
            let chip = if module == "it87" {
                "it8696"
            } else {
                "nct6687"
            };
            let rec = build_recommendation(
                chip,
                module,
                &ev,
                DriverState::NotLoaded,
                &[],
                &[],
                false,
                KernelContext::default(),
                &mut caveats,
            )
            .expect("allowlisted module → recommendation");
            assert!(!rec.in_mainline, "{chip} precondition: out-of-tree");
            assert!(
                rec.load_hint.contains(project),
                "{module}: {}",
                rec.load_hint
            );
        }
    }

    /// DEC-499 (`XD-1`): the daemon does not know the distribution, so no text it
    /// produces may name a distribution's package for a driver. The Arch package
    /// names it used to give do not exist on Debian or Fedora. Walks the source trees of
    /// both crates rather than listing files, so a new module is covered; the
    /// needles are assembled at run time so this test does not match itself.
    #[test]
    fn no_source_names_an_aur_package() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("source dir is readable") {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        walk(&root.join("src"), &mut files);
        walk(&root.join("../tray/src"), &mut files);
        assert!(
            files.iter().any(|f| f.ends_with("hwmon/superio.rs")),
            "precondition: the walk found this file"
        );
        let package_suffix = ["-dkms", "-git"].concat();
        let aur = ["A", "UR"].concat();
        let names_aur = |line: &str| {
            line.match_indices(aur.as_str()).any(|(i, _)| {
                let before = line[..i].chars().next_back();
                let after = line[i + aur.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        };
        let mut offenders = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            for (n, line) in text.lines().enumerate() {
                if line.contains(package_suffix.as_str()) || names_aur(line) {
                    offenders.push(format!("{}:{}", file.display(), n + 1));
                }
            }
        }
        assert!(offenders.is_empty(), "{offenders:#?}");
    }

    // ── DEC-500: the running kernel decides ─────────────────────────────

    const GIGABYTE: &str = "Gigabyte Technology Co., Ltd.";

    /// X870E AORUS MASTER: the board table expects it8696 + it87952. Here the
    /// IT87952E is bound and `it87` is loaded, so the it8696 is the unbound
    /// chip, with `it87` in-tree unless `it87_out_of_tree`.
    fn x870e_with_it87_loaded(it87_out_of_tree: bool) -> FakeEvidence {
        FakeEvidence {
            board: (GIGABYTE.into(), "X870E AORUS MASTER".into()),
            bound: vec![bound_by("it87952", "it87.2656", "it87")],
            loaded: vec!["it87".into()],
            out_of_tree: if it87_out_of_tree {
                vec!["it87".into()]
            } else {
                Vec::new()
            },
            kernel: Some((7, 2)),
            ..Default::default()
        }
    }

    fn rec_for<'a>(r: &'a SuperIoReport, chip: &str) -> &'a SuperIoRecommendation {
        find(r, chip)
            .recommendation
            .as_ref()
            .expect("unbound chip → recommendation")
    }

    /// The kernel's own it87 is loaded and cannot drive the chip: the hint
    /// names the missed DKMS rebuild and the two commands — only on that
    /// branch, and never a `modprobe -r` under a running daemon.
    #[test]
    fn the_kernels_own_it87_names_the_missed_dkms_rebuild() {
        let in_tree = detect_superio(&x870e_with_it87_loaded(false));
        let hint = &rec_for(&in_tree, "it8696").load_hint;
        assert!(hint.contains(IN_TREE_REBUILD_CAUSE), "{hint}");
        assert!(hint.contains("`dkms status`") && hint.contains("`sudo dkms autoinstall`"));
        assert!(!hint.contains("modprobe -r"), "{hint}");

        let out_of_tree = detect_superio(&x870e_with_it87_loaded(true));
        let hint = &rec_for(&out_of_tree, "it8696").load_hint;
        assert!(!hint.contains(IN_TREE_REBUILD_CAUSE), "{hint}");

        // A taint that could not be read says nothing about which build is
        // loaded: no "kernel's own", and the in-tree install advice stays.
        let unread = detect_superio(&FakeEvidence {
            taint_unread: vec!["it87".into()],
            ..x870e_with_it87_loaded(false)
        });
        let hint = &rec_for(&unread, "it8696").load_hint;
        assert!(!hint.contains(IN_TREE_REBUILD_CAUSE), "{hint}");
        assert_eq!(
            hint.replace(IN_TREE_REBUILD_CAUSE, ""),
            rec_for(&in_tree, "it8696")
                .load_hint
                .replace(IN_TREE_REBUILD_CAUSE, ""),
            "otherwise advised as the in-tree build"
        );

        // Not loaded at all: the install hint, not the rebuild cause.
        let none = detect_superio(&FakeEvidence {
            board: (GIGABYTE.into(), "X870E AORUS MASTER".into()),
            ..Default::default()
        });
        assert!(!rec_for(&none, "it8696")
            .load_hint
            .contains(IN_TREE_REBUILD_CAUSE));
    }

    /// The in-kernel it87 drives an IT8689E from 7.1, so there an unbound one
    /// is not "a missed rebuild" — the cause would be false.
    #[test]
    fn the_rebuild_cause_is_not_given_for_a_chip_the_kernel_drives() {
        let ev = |kernel| FakeEvidence {
            board: (GIGABYTE.into(), "Z790 AORUS ELITE AX".into()),
            loaded: vec!["it87".into()],
            kernel,
            ..Default::default()
        };
        let old = detect_superio(&ev(Some((7, 0))));
        assert!(rec_for(&old, "it8689")
            .load_hint
            .contains(IN_TREE_REBUILD_CAUSE));
        let new = detect_superio(&ev(Some((7, 1))));
        assert!(!rec_for(&new, "it8689")
            .load_hint
            .contains(IN_TREE_REBUILD_CAUSE));
    }

    /// DEC-500 "Kernel-aware": an unbound IT8689E with nothing loaded is told
    /// to load the kernel's own it87 on 7.1, with the out-of-tree build named
    /// as optional, and keeps the DKMS hint on 7.0 or an unknown kernel.
    #[test]
    fn an_it8689_is_mainline_on_7_1_and_the_out_of_tree_build_is_optional_there() {
        let ev = |kernel| FakeEvidence {
            board: (GIGABYTE.into(), "Z790 AORUS ELITE AX".into()),
            kernel,
            ..Default::default()
        };
        let optional = |r: &SuperIoRecommendation| {
            r.risk_notes
                .iter()
                .any(|n| n.contains("frankcrawford/it87 is optional here"))
        };

        let new = detect_superio(&ev(Some((7, 1))));
        let rec = rec_for(&new, "it8689");
        assert!(rec.in_mainline);
        assert!(
            rec.load_hint.contains("sudo modprobe it87"),
            "{}",
            rec.load_hint
        );
        assert!(optional(rec), "{:?}", rec.risk_notes);
        assert!(rec
            .risk_notes
            .iter()
            .any(|n| n.contains("from Linux 7.1") && n.contains("GIGABYTE layout ID")));

        for kernel in [Some((7, 0)), None] {
            let old = detect_superio(&ev(kernel));
            let rec = rec_for(&old, "it8689");
            assert!(!rec.in_mainline, "{kernel:?}");
            assert!(rec.load_hint.contains("DKMS"), "{kernel:?}");
            assert!(!optional(rec), "{kernel:?}");
        }

        // A chip mainline on every kernel never gets the "optional" note.
        let it8628 = detect_superio(&FakeEvidence {
            kmsg: vec!["it8628".into()],
            kernel: Some((7, 2)),
            ..Default::default()
        });
        assert!(!optional(rec_for(&it8628, "it8628")));
    }

    /// A kernel that refuses unsigned modules is named where an out-of-tree
    /// build is needed and is not the one running — and nowhere else, so the
    /// note never fires on a verdict it was not given.
    #[test]
    fn the_signing_note_fires_only_where_an_out_of_tree_build_is_needed_and_absent() {
        let note = signed_modules_note("it87");
        let has = |r: &SuperIoReport, chip: &str| rec_for(r, chip).risk_notes.contains(&note);
        let with =
            |signed_only, ev: FakeEvidence| detect_superio(&FakeEvidence { signed_only, ..ev });
        let unloaded = || FakeEvidence {
            board: (GIGABYTE.into(), "X870E AORUS MASTER".into()),
            ..Default::default()
        };

        let r = with(Some(true), unloaded());
        assert!(has(&r, "it8696"));
        // The IT87952E is in every kernel this supports: no build to sign.
        assert!(rec_for(&r, "it87952").in_mainline && !has(&r, "it87952"));
        assert_eq!(r.signed_modules_only, Some(true), "the report carries it");
        assert!(has(
            &with(Some(true), x870e_with_it87_loaded(false)),
            "it8696"
        ));
        // The out-of-tree build is running, so it was accepted.
        assert!(!has(
            &with(Some(true), x870e_with_it87_loaded(true)),
            "it8696"
        ));
        // Not refused, or not known: nothing said.
        assert!(!has(&with(Some(false), unloaded()), "it8696"));
        assert!(!has(&with(None, unloaded()), "it8696"));
        // A chip the kernel drives needs no out-of-tree build to be signed.
        let it8689 = with(
            Some(true),
            FakeEvidence {
                board: (GIGABYTE.into(), "Z790 AORUS ELITE AX".into()),
                kernel: Some((7, 1)),
                ..Default::default()
            },
        );
        assert!(!rec_for(&it8689, "it8689")
            .risk_notes
            .iter()
            .any(|n| n.contains("loads only signed modules")));
        assert!(note.contains("Key was rejected by service") && note.contains("mokutil"));
    }
}
