//! Board catalogue: what each unlabelled hwmon channel is on a Gigabyte board
//! (`VOLT-b`, DEC-464; fan headers and temperatures since DEC-500).
//!
//! The it87 driver names only the voltage inputs it measures internally
//! (`3VSB`, `Vbat`, `+3.3V`) and no fan header or temperature at all; every
//! other channel is a pin whose board wiring the driver does not know.
//! lm-sensors names those per board in `/etc/sensors.d`, but only libsensors
//! reads that file — the kernel never does, so no label the daemon reads ever
//! changes because of it. This module carries the same facts for the boards a
//! public catalogue covers: frankcrawford/it87's Gigabyte configs, keyed on
//! what those configs key on.
//!
//! # The key is (CPU platform, chip, SIV)
//!
//! - **SIV** — the board's firmware System Information Vector, the word the
//!   out-of-tree `it87` driver exports at `/sys/class/gigabyte/id/gigabyte_siv`
//!   ([`super::gigabyte_siv::read_siv_word`]). Several boards share one SIV,
//!   and so does their wiring; the configs say so by listing them together.
//!   The kernel's own `it87` publishes no SIV, so without the out-of-tree
//!   driver nothing here applies.
//! - **chip** — the *canonical* name (DEC-442), so an it87 v2.0 rebuild that
//!   appends the SIV to the chip name changes nothing here.
//! - **platform** — measured, not assumed: 38 chip+SIV keys appear in both the
//!   AMD and the Intel file with different rail names (`CPU VCORE SOC` vs
//!   `CPU VAXG` on one input), because the rails are the CPU's. A lookup with
//!   no known CPU vendor finds nothing rather than guessing a platform.
//!
//! A board name is never part of the key: 15 board names appear under two SIVs
//! upstream (board revisions). The names each SIV lists are carried
//! ([`listed_boards`]) so a client can say whether this board is among them.
//!
//! # What an entry means, and what it never overrides
//!
//! [`BoardRail::Named`] gives a rail's name and the divider multiplier that
//! turns the pin voltage into the rail voltage (`compute inN @ * k`).
//! [`BoardChannel::Named`] gives a fan header's or temperature's name.
//! `Unmapped` is a channel the board's config does not map (`ignore` —
//! upstream: "channels not mapped by this SIV configuration"). That is **not**
//! "unconnected": the X299 configs ignore an input for one CPU family that they
//! label `DRAM CH(A/B)` for the other. All that is known is that it is not a
//! named channel.
//!
//! The X299 boards' maps depend on the CPU family, in two files upstream's
//! installer refuses to choose between; the table carries only the channels
//! both files map identically. And where an independent derivation of
//! GIGABYTE's own Windows tables disagrees with the config, the channel is
//! withheld — not named, not `Unmapped` — so a disputed header is never given
//! either side's name (the generated file lists each one).
//!
//! **A channel the driver labelled is never touched** (DEC-464 Q2). For a rail,
//! the driver's label marks an internal input it has already scaled, so a
//! catalogue multiplier would scale it twice; for a header, the driver's label
//! is direct evidence and outranks a table.
//!
//! # A header's catalogue name is a floor input
//!
//! Rails and temperatures named here are display-only. A **fan header's** name
//! is not: discovery hands it to `roles::classify_header_role` beside the
//! driver's label, so a header the catalogue calls `SYS_FAN5_PUMP` is a pump to
//! every pump-protection site, and the engine floors a `CPU`/`PUMP` name as it
//! floors the same driver label (DEC-252). Add-only, like every term of those
//! unions; the user's decision (DEC-500), which amends DEC-496's rule that a
//! floor-relevant board label must rest on a Linux measurement for exactly this
//! source: SIV-keyed upstream names, with disputed channels withheld.

use super::board_catalogue_data::{AMD, AMD_BOARDS, INTEL, INTEL_BOARDS};

/// The it87 commit the table was generated from, published beside every match.
pub const UPSTREAM_COMMIT: &str = super::board_catalogue_data::UPSTREAM_COMMIT;

/// What the catalogue says about one ADC input on one board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoardRail {
    /// The input is wired to the named rail through a divider; the rail
    /// voltage is the pin voltage times `multiplier` (1.0 where the config
    /// carries no `compute` line).
    Named {
        label: &'static str,
        multiplier: f64,
    },
    /// The board's config does not map this input — not a named rail, and not
    /// known to be unconnected either.
    Unmapped,
}

/// What the catalogue says about one fan, PWM or temperature channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardChannel {
    /// The board's name for the header or temperature on this channel.
    Named(&'static str),
    /// The board's config does not map this channel.
    Unmapped,
}

/// Which non-voltage channel family a lookup is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// `fanN` — a tachometer input.
    Fan,
    /// `pwmN` — a header's PWM output; the name a controllable header takes.
    Pwm,
    /// `tempN`.
    Temp,
}

/// Constructor the generated table uses, so each entry stays one short line.
pub(super) const fn named(label: &'static str, multiplier: f64) -> BoardRail {
    BoardRail::Named { label, multiplier }
}

/// One config `chip` block: the (chip, SIV) keys it covers and its channels.
#[derive(Debug)]
pub(super) struct Stanza {
    pub(super) chips: &'static [(&'static str, u32)],
    pub(super) rails: &'static [(u8, BoardRail)],
    pub(super) fans: &'static [(u8, BoardChannel)],
    pub(super) pwms: &'static [(u8, BoardChannel)],
    pub(super) temps: &'static [(u8, BoardChannel)],
}

impl Stanza {
    fn keys(&self, chip: &str, siv: u32) -> bool {
        self.chips.iter().any(|&(c, w)| c == chip && w == siv)
    }

    fn channels(&self, kind: ChannelKind) -> &'static [(u8, BoardChannel)] {
        match kind {
            ChannelKind::Fan => self.fans,
            ChannelKind::Pwm => self.pwms,
            ChannelKind::Temp => self.temps,
        }
    }
}

/// Upstream's board list per SIV: `(siv, board names)`.
type BoardList = &'static [(u32, &'static [&'static str])];

/// The table for a CPU vendor as `chip_db::read_cpu_vendor` spells it
/// (`"AMD"` / `"Intel"`); `None` for anything else, including the empty string
/// it returns when `/proc/cpuinfo` is unreadable or the vendor is unknown.
fn table_for(cpu_vendor: &str) -> Option<(&'static [Stanza], BoardList)> {
    match cpu_vendor {
        "AMD" => Some((AMD, AMD_BOARDS)),
        "Intel" => Some((INTEL, INTEL_BOARDS)),
        _ => None,
    }
}

fn stanza(cpu_vendor: &str, chip: &str, siv: u32) -> Option<&'static Stanza> {
    table_for(cpu_vendor)?.0.iter().find(|s| s.keys(chip, siv))
}

/// Look up one ADC input. `None` when the board, chip or channel is not in the
/// catalogue — the caller then reports the channel exactly as before.
pub fn rail(cpu_vendor: &str, chip: &str, siv: u32, channel: u8) -> Option<BoardRail> {
    stanza(cpu_vendor, chip, siv)?
        .rails
        .iter()
        .find(|(ch, _)| *ch == channel)
        .map(|(_, r)| *r)
}

/// Look up one fan, PWM or temperature channel. `None` when the board, chip or
/// channel is not in the catalogue, or the channel is withheld.
pub fn channel(
    cpu_vendor: &str,
    chip: &str,
    siv: u32,
    kind: ChannelKind,
    index: u8,
) -> Option<BoardChannel> {
    stanza(cpu_vendor, chip, siv)?
        .channels(kind)
        .iter()
        .find(|(ch, _)| *ch == index)
        .map(|(_, c)| *c)
}

/// Whether the catalogue has any block for this SIV on this platform.
pub fn knows_siv(cpu_vendor: &str, siv: u32) -> bool {
    table_for(cpu_vendor).is_some_and(|(stanzas, _)| {
        stanzas
            .iter()
            .any(|s| s.chips.iter().any(|&(_, w)| w == siv))
    })
}

/// The secondary Super-I/O chips — IT8792E/IT8795E (hwmon `it8792`) and
/// IT87952E. A layout with a block for one carries it beside its primary; the
/// primaries are every other chip the catalogue names.
const SECONDARY_CHIPS: &[&str] = &["it8792", "it87952"];

/// The chips this layout expects the board to expose (DEC-500), for the
/// missing-chip warning on a board the DMI dual-chip table does not list.
///
/// Every chip the catalogue has a block for under this platform and SIV, with
/// one correction: several layouts carry blocks for **two primaries** (the
/// X570S layouts' `it8688` and `it8689`, a few Intel layouts' `it8686` and
/// `it8688`) because the boards sharing the SIV are built with either. A board
/// has one, so the primary expected is the one bound; with none of them bound
/// the layout cannot say which is missing, and none is listed — never a guess.
/// A single primary, and every secondary, is expected whatever is bound.
///
/// `bound` are the chip names bound here, canonical or not. Primaries first,
/// then secondaries, each in name order. Empty for an unknown platform or SIV.
///
/// Checked against the DMI table on every board both name (2026-10-19): 101 of
/// 103 agree; on the other two (B450 AORUS PRO and its WIFI, SIV `31050607`)
/// the catalogue has no block for the board's fanless IT8792E, so it expects
/// fewer chips, never more.
pub fn expected_chips(cpu_vendor: &str, siv: u32, bound: &[&str]) -> Vec<String> {
    let Some((stanzas, _)) = table_for(cpu_vendor) else {
        return Vec::new();
    };
    let mut chips: Vec<&'static str> = stanzas
        .iter()
        .flat_map(|s| s.chips.iter())
        .filter(|&&(_, w)| w == siv)
        .map(|&(c, _)| c)
        .collect();
    chips.sort_unstable();
    chips.dedup();
    let is_bound = |chip: &str| {
        bound
            .iter()
            .any(|b| super::chip_name::canonical_chip_name(b.trim()).eq_ignore_ascii_case(chip))
    };
    let (secondaries, primaries): (Vec<&str>, Vec<&str>) =
        chips.into_iter().partition(|c| SECONDARY_CHIPS.contains(c));
    let primaries: Vec<&str> = if primaries.len() > 1 {
        primaries.into_iter().filter(|c| is_bound(c)).collect()
    } else {
        primaries
    };
    primaries
        .into_iter()
        .chain(secondaries)
        .map(str::to_string)
        .collect()
}

/// The board names the configs list for this SIV — empty where upstream says
/// "Names not currently known", and for an unknown platform or SIV.
pub fn listed_boards(cpu_vendor: &str, siv: u32) -> &'static [&'static str] {
    table_for(cpu_vendor)
        .and_then(|(_, boards)| boards.iter().find(|(w, _)| *w == siv))
        .map_or(&[], |(_, names)| names)
}

/// What a board-catalogue lookup is keyed on, read once per discovery.
///
/// [`BoardContext::read`] in production; a test builds one with the vendor and
/// SIV it wants. `Default` is "nothing known" — no vendor, no SIV — under which
/// every lookup finds nothing, so discovery behaves exactly as it did before the
/// catalogue existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardContext {
    /// `chip_db::read_cpu_vendor`'s answer: `"AMD"`, `"Intel"` or `""`.
    pub cpu_vendor: String,
    /// The raw SIV word, `None` where the board publishes none.
    pub siv: Option<u32>,
}

impl BoardContext {
    /// Read the CPU vendor and the SIV from the running system. Two small
    /// read-only files (`/proc/cpuinfo`, the `gigabyte_siv` attribute); no
    /// port I/O, unaffected by the port-probe gate. `main.rs` calls it once and
    /// keeps the value (`AppState::board_context`).
    pub fn read() -> Self {
        Self::read_from(
            std::path::Path::new("/proc/cpuinfo"),
            std::path::Path::new(super::gigabyte_siv::GIGABYTE_SIV_PATH),
        )
    }

    /// [`Self::read`] against other files, for tests. Either file missing or
    /// unreadable gives its field's "did not say" (`""`, `None`), which
    /// matches nothing.
    pub fn read_from(proc_cpuinfo: &std::path::Path, siv: &std::path::Path) -> Self {
        Self {
            cpu_vendor: super::chip_db::read_cpu_vendor_from(proc_cpuinfo),
            siv: super::gigabyte_siv::read_siv_word(siv),
        }
    }

    /// [`channel`] under this context.
    pub fn channel(&self, chip: &str, kind: ChannelKind, index: u8) -> Option<BoardChannel> {
        channel(&self.cpu_vendor, chip, self.siv?, kind, index)
    }

    /// The catalogue's name for a channel, or `None` (unmapped, withheld, or
    /// not in the catalogue).
    pub fn name(&self, chip: &str, kind: ChannelKind, index: u8) -> Option<&'static str> {
        match self.channel(chip, kind, index)? {
            BoardChannel::Named(name) => Some(name),
            BoardChannel::Unmapped => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The reference host: X870E AORUS MASTER, SIV `A008090A`, AMD.
    const HOST_SIV: u32 = 0xA008_090A;

    fn named_parts(rail: Option<BoardRail>) -> (&'static str, f64) {
        match rail {
            Some(BoardRail::Named { label, multiplier }) => (label, multiplier),
            other => panic!("expected a named rail, got {other:?}"),
        }
    }

    fn host() -> BoardContext {
        BoardContext {
            cpu_vendor: "AMD".into(),
            siv: Some(HOST_SIV),
        }
    }

    /// The production key is the two files read together: a vendor and an SIV
    /// that do not both arrive make a context that matches nothing.
    #[test]
    fn read_from_combines_the_cpu_vendor_and_the_siv() {
        let dir = tempfile::tempdir().unwrap();
        let cpuinfo = dir.path().join("cpuinfo");
        let siv = dir.path().join("gigabyte_siv");
        std::fs::write(&cpuinfo, "processor\t: 0\nvendor_id\t: AuthenticAMD\n").unwrap();
        std::fs::write(&siv, "A008090A\n").unwrap();
        let ctx = BoardContext::read_from(&cpuinfo, &siv);
        assert_eq!(ctx, host());
        assert_eq!(
            ctx.name("it87952", ChannelKind::Pwm, 1),
            Some("SYS_FAN5_PUMP"),
            "precondition: the key read from files names the pump header"
        );

        // No SIV file (any non-Gigabyte board, or the in-kernel it87): no key.
        let none = BoardContext::read_from(&cpuinfo, &dir.path().join("absent"));
        assert_eq!(none.siv, None);
        assert_eq!(none.name("it87952", ChannelKind::Pwm, 1), None);
        // An Intel CPU on the same SIV reads the Intel table, not the AMD one.
        std::fs::write(&cpuinfo, "vendor_id\t: GenuineIntel\n").unwrap();
        assert_eq!(BoardContext::read_from(&cpuinfo, &siv).cpu_vendor, "Intel");
    }

    /// [SAFETY] DEC-500: every production header discovery runs under the one
    /// key `main.rs` reads at startup. A call site handed `Default::default()`
    /// (or a fresher read) compiles, passes every other test, and silently
    /// drops — or, on a rescan, publishes without enforcing — every catalogue
    /// pump's protection. So the call sites are enumerated from the source.
    #[test]
    fn production_header_discovery_uses_the_startup_board_key() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut calls = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                // Production code only: a file's test module starts at its
                // first top-level `#[cfg(test)]` module.
                let prod = text.split("\n#[cfg(test)]\nmod ").next().unwrap();
                let rel = path.strip_prefix(&src).unwrap().display().to_string();
                for (at, _) in prod.match_indices("discover_pwm_headers(") {
                    if prod[..at].ends_with("fn ") {
                        continue; // the definition
                    }
                    let args = &prod[at + "discover_pwm_headers(".len()..];
                    let args = &args[..args.find(')').unwrap()];
                    calls.push((rel.clone(), args.to_string()));
                }
            }
        }
        calls.sort();
        assert_eq!(
            calls,
            [
                (
                    "api/handlers/hwmon_ctl.rs".to_string(),
                    "hwmon_root, &state.board_context".to_string()
                ),
                (
                    "main.rs".to_string(),
                    "hwmon_root, &board_context".to_string()
                ),
            ],
            "every production discovery must take the startup key"
        );
        let main = std::fs::read_to_string(src.join("main.rs")).unwrap();
        assert!(
            main.contains(
                "let board_context = ventulusd::hwmon::board_catalogue::BoardContext::read();"
            ),
            "main.rs reads the key once, from the running system"
        );
    }

    /// Pinned against the catalogue as read at the generating commit — and
    /// against this host's measured pins: 1.992 V × 6 = 11.95 V on `+12V`.
    #[test]
    fn the_reference_board_primary_chip_is_named_with_its_dividers() {
        assert_eq!(
            named_parts(rail("AMD", "it8696", HOST_SIV, 0)),
            ("CPU Vcore", 1.0)
        );
        assert_eq!(
            named_parts(rail("AMD", "it8696", HOST_SIV, 1)),
            ("+3.3V", 1.649)
        );
        assert_eq!(
            named_parts(rail("AMD", "it8696", HOST_SIV, 2)),
            ("+12V", 6.0)
        );
        assert_eq!(
            named_parts(rail("AMD", "it8696", HOST_SIV, 3)),
            ("+5V", 2.5)
        );
        assert_eq!(
            named_parts(rail("AMD", "it8696", HOST_SIV, 6)),
            ("CPU VDDIO MEM", 1.0)
        );
        // in7–in9 are the driver's own internal inputs; the config is silent.
        assert_eq!(rail("AMD", "it8696", HOST_SIV, 7), None);
    }

    #[test]
    fn the_reference_board_secondary_chip_has_unmapped_inputs() {
        assert_eq!(
            rail("AMD", "it87952", HOST_SIV, 0),
            Some(BoardRail::Unmapped)
        );
        assert_eq!(
            named_parts(rail("AMD", "it87952", HOST_SIV, 2)),
            ("PM VCC18", 1.0)
        );
        // Not mentioned by the config at all: nothing to say.
        assert_eq!(rail("AMD", "it87952", HOST_SIV, 3), None);
    }

    /// The reference board's headers, against its Linux-verified table
    /// (DEC-444: SYS_FAN4 = `it87952 pwm3`, measured on this host).
    #[test]
    fn the_reference_board_headers_are_named_as_measured() {
        let b = host();
        let pwm = |chip, i| b.name(chip, ChannelKind::Pwm, i);
        assert_eq!(pwm("it8696", 1), Some("CPU_FAN"));
        assert_eq!(pwm("it8696", 5), Some("CPU_OPT"));
        assert_eq!(pwm("it87952", 1), Some("SYS_FAN5_PUMP"));
        assert_eq!(pwm("it87952", 3), Some("SYS_FAN4"));
        // pwm6 exists on the chip but is not a header on this board.
        assert_eq!(
            b.channel("it8696", ChannelKind::Pwm, 6),
            Some(BoardChannel::Unmapped)
        );
        assert_eq!(b.name("it8696", ChannelKind::Pwm, 6), None);
        assert_eq!(b.name("it8696", ChannelKind::Temp, 5), Some("VRM MOS"));
        assert_eq!(b.name("it87952", ChannelKind::Temp, 1), Some("PCIEX4"));
    }

    /// A sister board on the same SIV gets the same names, and the configs list
    /// it — the X870E AORUS PRO shares the MASTER's layout.
    #[test]
    fn a_sister_board_on_the_same_siv_is_listed_and_named() {
        assert!(listed_boards("AMD", HOST_SIV).contains(&"X870E AORUS PRO"));
        assert!(listed_boards("AMD", HOST_SIV).contains(&"X870E AORUS MASTER"));
        assert!(knows_siv("AMD", HOST_SIV));
        assert_eq!(listed_boards("Intel", HOST_SIV), &[] as &[&str]);
    }

    /// The platform is part of the key: the same chip + SIV names input 4
    /// differently on the two platforms (gigabyte-it87-amd.conf:131 vs
    /// gigabyte-it87-intel.conf:114).
    #[test]
    fn the_cpu_platform_selects_between_two_maps_for_one_siv() {
        let amd = named_parts(rail("AMD", "it8686", 0x1004_0607, 4));
        let intel = named_parts(rail("Intel", "it8686", 0x1004_0607, 4));
        assert_eq!(amd.0, "CPU VCORE SOC");
        assert_eq!(intel.0, "CPU VAXG");
    }

    /// SIVs are not vendor-unique: the Intel B860I AORUS PRO ICE reports an
    /// A-series SIV. An AMD lookup must not find the Intel board's names, nor an
    /// Intel lookup the AMD reference board's.
    #[test]
    fn an_a_series_siv_on_the_other_platform_finds_nothing() {
        let intel_siv = 0xA103_0607;
        assert!(listed_boards("Intel", intel_siv).contains(&"B860I AORUS PRO ICE"));
        assert!(channel("Intel", "it8696", intel_siv, ChannelKind::Pwm, 1).is_some());
        assert_eq!(
            channel("AMD", "it8696", intel_siv, ChannelKind::Pwm, 1),
            None
        );
        assert_eq!(
            channel("Intel", "it8696", HOST_SIV, ChannelKind::Pwm, 1),
            None
        );
    }

    /// X299 (DEC-464): only the inputs both CPU-family files map identically.
    /// `in0` is "CPU Vcore" for Kaby Lake-X and "CPU VRIN" for Skylake-X, so it
    /// is left unnamed; `+12V` and its divider agree, so it is carried.
    #[test]
    fn x299_carries_only_the_inputs_both_cpu_families_agree_on() {
        assert_eq!(
            named_parts(rail("Intel", "it8688", 0x5008_090A, 2)),
            ("+12V", 6.0)
        );
        assert_eq!(rail("Intel", "it8688", 0x5008_090A, 0), None);
        // in6 is ignored for one family and DRAM CH(A/B) for the other — the
        // case that shows `ignore` is not "unconnected".
        assert_eq!(rail("Intel", "it8688", 0x5008_090A, 6), None);
    }

    /// Withheld where a second derivation of GIGABYTE's tables disagrees: no
    /// name and no `Unmapped`, while the channels the two agree on stay named.
    #[test]
    fn a_disputed_channel_is_withheld_and_its_neighbours_are_not() {
        // A0040607: the config calls pwm3 SYS_FAN2, GIGABYTE's table a pump
        // header; and pwm5 is ignored by one and CPU OPT in the other.
        let siv = 0xA004_0607;
        let pwm = |i| channel("AMD", "it8696", siv, ChannelKind::Pwm, i);
        assert_eq!(pwm(1), Some(BoardChannel::Named("CPU_FAN")));
        assert_eq!(pwm(2), Some(BoardChannel::Named("SYS_FAN1")));
        assert_eq!(pwm(3), None);
        assert_eq!(pwm(5), None);
        // PCIEX4 vs PCIEX8 on a sister of the reference board's second chip,
        // while the reference board itself (no dispute) keeps PCIEX4.
        assert_eq!(
            channel("AMD", "it87952", 0xA108_090A, ChannelKind::Temp, 1),
            None
        );
        assert_eq!(
            channel("AMD", "it87952", 0xA108_090A, ChannelKind::Temp, 3),
            Some(BoardChannel::Named("System 2"))
        );
        // X570S: the config names the rails on an it8688 block and ignores them
        // on the it8689 actually fitted. Withheld, not "not used by this board".
        assert_eq!(rail("AMD", "it8689", 0x8003_0407, 0), None);
        // A divider the two disagree on (×1.649 vs ×1) is not applied.
        assert_eq!(rail("AMD", "it8792", 0x3008_090B, 4), None);
    }

    #[test]
    fn nothing_matches_without_a_known_platform_chip_or_siv() {
        assert_eq!(rail("", "it8696", HOST_SIV, 2), None, "unknown vendor");
        assert_eq!(rail("AMD", "nct6799", HOST_SIV, 2), None, "other chip");
        assert_eq!(rail("AMD", "it8696", 0xA008_0909, 2), None, "other SIV");
        // The suffixed sysfs spelling is not the key; the canonical name is.
        assert_eq!(rail("AMD", "it8696_a008090a", HOST_SIV, 2), None);
        assert!(!knows_siv("AMD", 0xA008_0909));
        let none = BoardContext::default();
        assert_eq!(none.name("it8696", ChannelKind::Pwm, 1), None);
        let no_siv = BoardContext {
            cpu_vendor: "AMD".into(),
            siv: None,
        };
        assert_eq!(no_siv.name("it8696", ChannelKind::Pwm, 1), None);
    }

    /// Whole-table invariants the generator also enforces — kept here so a
    /// hand edit of the generated file cannot slip one past the build.
    #[test]
    fn every_entry_is_well_formed_and_every_key_is_unique_per_platform() {
        for (name, table, boards) in [("AMD", AMD, AMD_BOARDS), ("INTEL", INTEL, INTEL_BOARDS)] {
            assert!(!table.is_empty(), "{name} table is empty");
            let mut keys = HashSet::new();
            for stanza in table {
                assert!(
                    !(stanza.rails.is_empty()
                        && stanza.fans.is_empty()
                        && stanza.pwms.is_empty()
                        && stanza.temps.is_empty()),
                    "{name}: an empty block"
                );
                for &(chip, siv) in stanza.chips {
                    assert!(
                        keys.insert((chip, siv)),
                        "{name}: ({chip}, {siv:08X}) is keyed twice — the first \
                         match would silently shadow the second"
                    );
                }
                let mut channels = HashSet::new();
                for &(ch, rail) in stanza.rails {
                    assert!(channels.insert(ch), "{name}: in{ch} listed twice");
                    if let BoardRail::Named { label, multiplier } = rail {
                        assert!(!label.trim().is_empty());
                        assert!(multiplier.is_finite() && multiplier >= 1.0);
                    }
                }
                for kind in [ChannelKind::Fan, ChannelKind::Pwm, ChannelKind::Temp] {
                    let mut seen = HashSet::new();
                    for &(ch, c) in stanza.channels(kind) {
                        assert!(seen.insert(ch), "{name}: {kind:?}{ch} listed twice");
                        if let BoardChannel::Named(label) = c {
                            assert!(!label.trim().is_empty());
                        }
                    }
                }
            }
            let mut sivs = HashSet::new();
            for &(siv, names) in boards {
                assert!(sivs.insert(siv), "{name}: board list for {siv:08X} twice");
                assert!(!names.is_empty() && names.iter().all(|n| !n.trim().is_empty()));
            }
        }
    }

    // ── expected_chips (DEC-500) ──────────────────────────────────────────

    #[test]
    fn a_layout_expects_its_primary_and_secondary_whatever_is_bound() {
        let expect = |bound: &[&str]| expected_chips("AMD", HOST_SIV, bound);
        assert_eq!(expect(&["it8696", "it87952"]), ["it8696", "it87952"]);
        // The secondary is expected while it is missing: that is the warning.
        assert_eq!(expect(&["it8696"]), ["it8696", "it87952"]);
        assert_eq!(expect(&[]), ["it8696", "it87952"]);
        // it87 v2.0 names the chip after the SIV; canonical names match.
        assert_eq!(expect(&["it8696_a008090a"]), ["it8696", "it87952"]);
        // A single-chip layout.
        assert_eq!(expected_chips("AMD", 0x9004_0507, &[]), ["it8696"]);
    }

    /// The X570S layouts give one SIV blocks for `it8688` and `it8689`: the
    /// board has one of them. The bound one is expected; with neither bound
    /// the layout names no primary rather than call one missing.
    #[test]
    fn two_primary_blocks_expect_the_bound_one_and_never_guess() {
        let x570s = 0x8008_090A;
        assert_eq!(
            expected_chips("AMD", x570s, &["it8689", "it87952"]),
            ["it8689", "it87952"]
        );
        assert_eq!(
            expected_chips("AMD", x570s, &["it8688_8008090a"]),
            ["it8688", "it87952"]
        );
        assert_eq!(expected_chips("AMD", x570s, &[]), ["it87952"]);
        // The platform is part of the key: the same SIV on Intel is one
        // primary, expected unbound.
        assert_eq!(expected_chips("Intel", x570s, &[]), ["it8689", "it87952"]);
        // Intel 4008090A: two primaries and a secondary.
        assert_eq!(
            expected_chips("Intel", 0x4008_090A, &["it8688"]),
            ["it8688", "it8792"]
        );
        assert_eq!(
            expected_chips("AMD", 0x4008_090A, &[]),
            ["it8688", "it8792"]
        );
    }

    #[test]
    fn an_unknown_layout_or_platform_expects_nothing() {
        assert!(expected_chips("AMD", 0xDEAD_BEEF, &["it8696"]).is_empty());
        assert!(expected_chips("", HOST_SIV, &[]).is_empty());
        assert!(expected_chips("Intel", HOST_SIV, &[]).is_empty());
    }

    /// Every primary group with more than one chip is the either-or case the
    /// rule is for: no layout pairs two primaries with no secondary chip
    /// between them for a reason the rule does not know, and every chip the
    /// catalogue names is either a known primary or a [`SECONDARY_CHIPS`] entry.
    #[test]
    fn every_catalogue_chip_is_a_known_primary_or_secondary() {
        let primaries = ["it8686", "it8688", "it8689", "it8696", "it8698"];
        for (stanzas, _) in [(AMD, AMD_BOARDS), (INTEL, INTEL_BOARDS)] {
            for s in stanzas {
                for &(chip, _) in s.chips {
                    assert!(
                        primaries.contains(&chip) || SECONDARY_CHIPS.contains(&chip),
                        "{chip}: classify it before the expected-chip rule meets it"
                    );
                }
            }
        }
    }
}
