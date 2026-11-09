//! Guards on `packaging/ventulus-superio-guard` (DEC-332).
//!
//! The guard stops `nct6775`/`w83627ehf` probing 0x2E/0x4E on a board whose
//! Super-I/O is ITE. Both drivers call one `superio_enter()` that writes
//! `outb(0x87); outb(0x87)` UNCONDITIONALLY before reading the device ID, and on
//! a Gigabyte board with an ITE eSPI-to-LPC bridge that write latches the bridge
//! into configuration mode — measured 2026-09-05 on an X870E AORUS MASTER as the
//! loss of 3 of 8 fan headers and 3 of 9 temperatures until a full power cut.
//!
//! Two properties are pinned here, and they fail for different reasons:
//!   * the **decision table** — since DEC-424, suppress on every Gigabyte board
//!     (all use ITE Super-I/O) and on a listed board whose firmware reports no
//!     vendor; load everywhere else;
//!   * **parity** with `GIGABYTE_DUAL_CHIP_BOARDS`, so the shell list — which
//!     now names a board in the journal and decides the no-vendor case — cannot
//!     drift away from the Rust table it was copied from.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

fn guard_path() -> PathBuf {
    repo_root().join("packaging/ventulus-superio-guard")
}

/// Run the guard against a synthetic DMI directory, in dry-run so it prints its
/// decision instead of exec'ing the real `modprobe`.
fn decide(vendor: &str, board: Option<&str>, module: &str) -> String {
    decide_with(Some(vendor), board, module)
}

/// [`decide`], where `vendor: None` models a DMI directory with no
/// `board_vendor` file at all (DEC-424's no-vendor case).
fn decide_with(vendor: Option<&str>, board: Option<&str>, module: &str) -> String {
    // Distinct per CASE, not per board: two cases share the board name
    // "X870E AORUS MASTER" and differ only by vendor, so keying on the board
    // alone gave them one directory. Under cargo's parallel test threads each
    // overwrote the other's `board_vendor` and each called `remove_dir_all` on
    // it — an intermittent red on the two tests that pin the decision table.
    // A static counter makes every call site unique regardless of its inputs.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cofc-guard-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create fake DMI dir");
    if let Some(v) = vendor {
        std::fs::write(dir.join("board_vendor"), v).expect("write vendor");
    }
    match board {
        Some(b) => std::fs::write(dir.join("board_name"), b).expect("write board"),
        // Absent board_name models unreadable DMI (container, exotic firmware).
        None => {
            let _ = std::fs::remove_file(dir.join("board_name"));
        }
    }

    let out = Command::new("sh")
        .arg(guard_path())
        .arg(module)
        .env("VENTULUS_DMI_DIR", &dir)
        .env("VENTULUS_GUARD_DRY_RUN", "1")
        .output()
        .expect("run guard");

    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "the guard must always exit 0 — it runs from a modprobe install rule at \
         early boot, where a non-zero exit is a booting user's problem. \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const GIGABYTE: &str = "Gigabyte Technology Co., Ltd.";

#[test]
fn suppresses_nuvoton_probes_on_the_measured_board() {
    // The exact board and both modules the defect was measured on.
    for module in ["nct6775", "w83627ehf"] {
        let d = decide(GIGABYTE, Some("X870E AORUS MASTER"), module);
        assert!(
            d.starts_with("SUPPRESS"),
            "{module} must be suppressed on the board where the latch was \
             measured, got: {d:?}"
        );
    }
}

#[test]
fn board_matching_is_case_insensitive_and_substring() {
    // chip_db.rs matches case-insensitively as a substring; the guard must agree
    // or the two lists would be "identical" and still behave differently.
    let lower = decide(
        "gigabyte technology co., ltd.",
        Some("x870e aorus master"),
        "nct6775",
    );
    assert!(lower.starts_with("SUPPRESS"), "case-insensitive: {lower:?}");

    // Real DMI carries board revision suffixes (`-CF`, ` -CF`, etc.).
    let suffixed = decide(GIGABYTE, Some("X870E AORUS MASTER-CF"), "nct6775");
    assert!(suffixed.starts_with("SUPPRESS"), "substring: {suffixed:?}");
}

#[test]
fn loads_normally_everywhere_the_module_is_actually_needed() {
    // This is the half that matters most: nct6775 is REQUIRED on most Nuvoton
    // boards. A guard that over-matches silently removes fan control from a much
    // larger population than the one it protects.
    let cases: [(&str, Option<&str>, &str); 5] = [
        // A Nuvoton board — the module's whole purpose.
        (
            "ASUSTeK COMPUTER INC.",
            Some("ROG STRIX X670E-E GAMING WIFI"),
            "a Nuvoton board",
        ),
        // Another vendor that happens to collide on board_name: the vendor gate
        // must stop it.
        (
            "ASRock",
            Some("X870E AORUS MASTER"),
            "a name collision on another vendor",
        ),
        // Gigabyte's server brand reports its own vendor string, which is not
        // "GIGABYTE"; its boards are left to the real modprobe (DEC-424).
        (
            "Giga Computing Technology Co., Ltd.",
            Some("MC13-LE1"),
            "a Giga Computing server board",
        ),
        // A vendor string that is only whitespace is not "no vendor": fail open.
        ("  ", Some("B650M DS3H"), "a blank-but-present vendor"),
        // Unreadable board name: we cannot judge, so we must not suppress.
        (GIGABYTE, None, "unreadable DMI"),
    ];
    for (vendor, board, why) in cases {
        let d = decide(vendor, board, "nct6775");
        assert!(
            d.starts_with("LOAD"),
            "nct6775 must still load for {why}, got: {d:?}"
        );
    }
}

/// DEC-424 (`BRD-n`): every Gigabyte board, listed or not. Before it only the
/// listed boards were covered, and an unlisted Gigabyte board — an AM5 board
/// behind the same eSPI bridge, say — was probed at every boot. The unlisted
/// case is the one that fails if the vendor-wide arm is removed.
#[test]
fn suppresses_on_every_gigabyte_board_listed_or_not() {
    for (board, why) in [
        ("B650M DS3H", "an unlisted single-chip AM5 board"),
        ("X870E AORUS MASTER", "the measured, listed board"),
        ("Z790 UD AC", "an unlisted Intel board"),
    ] {
        for module in ["nct6775", "w83627ehf"] {
            let d = decide(GIGABYTE, Some(board), module);
            assert!(
                d.starts_with(&format!("SUPPRESS {module}")),
                "{module} must be suppressed on {why} ({board}), got: {d:?}"
            );
        }
    }
}

/// DEC-424, by the user's choice: with NO vendor, a listed board name is still
/// Gigabyte's and is suppressed; an unlisted one loads (fail-open), because the
/// name alone says nothing about its Super-I/O.
#[test]
fn with_no_vendor_only_a_listed_board_is_suppressed() {
    for vendor in [Some(""), None] {
        let listed = decide_with(vendor, Some("X870E AORUS MASTER-CF"), "nct6775");
        assert!(
            listed.starts_with("SUPPRESS nct6775"),
            "vendor {vendor:?}, listed board: {listed:?}"
        );
        let unlisted = decide_with(vendor, Some("B650M DS3H"), "nct6775");
        assert!(
            unlisted.starts_with("LOAD nct6775"),
            "vendor {vendor:?}, unlisted board must fail open: {unlisted:?}"
        );
    }
}

/// The reason a suppression gives (DEC-424 review): the dry-run prints the same
/// `why` the journal line carries, so every string and expansion on that path
/// runs here. A listed board is named as known; an unlisted Gigabyte board is not;
/// the no-vendor case says so.
#[test]
fn a_suppression_names_its_reason() {
    let listed = decide(GIGABYTE, Some("X870E AORUS MASTER"), "nct6775");
    assert!(
        listed.contains("is a Gigabyte board")
            && listed.contains("known board: X870E AORUS MASTER"),
        "{listed:?}"
    );
    let unlisted = decide(GIGABYTE, Some("B650M DS3H"), "nct6775");
    assert!(
        unlisted.contains("B650M DS3H is a Gigabyte board") && !unlisted.contains("known board"),
        "{unlisted:?}"
    );
    let no_vendor = decide_with(None, Some("X870E AORUS MASTER"), "nct6775");
    assert!(
        no_vendor.contains("reports no board vendor") && no_vendor.contains("(X870E AORUS MASTER)"),
        "{no_vendor:?}"
    );
}

// ── What the guard does with modprobe's result (`PKG-i`) ─────────────────────

const NO_CHIP: &str = "modprobe: ERROR: could not insert 'nct6775': No such device";

/// Run the guard on a board where it loads (not Gigabyte), with modprobe
/// simulated: it "exits" `rc` after printing `stderr`, and `modinfo` finds the
/// module. Nothing is loaded and nothing reaches the journal; journal lines
/// come back on stdout.
fn load_result(rc: u8, stderr: &str) -> std::process::Output {
    load_result_modinfo(rc, stderr, 0)
}

/// [`load_result`], with `modinfo` simulated as exiting `modinfo_rc` (1: the
/// module is not installed for this kernel).
fn load_result_modinfo(rc: u8, stderr: &str, modinfo_rc: u8) -> std::process::Output {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cofc-guard-load-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create fake DMI dir");
    std::fs::write(dir.join("board_vendor"), "ASUSTeK COMPUTER INC.").unwrap();
    std::fs::write(dir.join("board_name"), "PRIME X670-P").unwrap();
    let out = Command::new("sh")
        .arg(guard_path())
        .arg("nct6775")
        .env("VENTULUS_DMI_DIR", &dir)
        .env("VENTULUS_GUARD_SIM_RC", rc.to_string())
        .env("VENTULUS_GUARD_SIM_STDERR", stderr)
        .env("VENTULUS_GUARD_SIM_MODINFO_RC", modinfo_rc.to_string())
        .output()
        .expect("run guard");
    std::fs::remove_dir_all(&dir).ok();
    out
}

/// `PKG-i`: libkmod reports any non-zero exit of an install command as EINVAL,
/// which systemd-modules-load does not forgive, so a board without the chip
/// failed the unit at every boot. The guard exits 0 for "No such device" — and
/// still shows modprobe's message, so an interactive `modprobe` is not silent,
/// and says so in the journal.
#[test]
fn a_board_without_the_chip_exits_zero_and_still_says_so() {
    let out = load_result(1, NO_CHIP);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "no chip must not fail systemd-modules-load: {out:?}"
    );
    assert_eq!(
        stderr.trim_end(),
        NO_CHIP,
        "modprobe's message is passed on unchanged"
    );
    assert!(
        stdout.starts_with("JOURNAL notice nct6775 found no chip on this board"),
        "the journal says why the module is absent: {stdout:?}"
    );

    // The no-chip line need not be modprobe's last line of output.
    let followed = format!("{NO_CHIP}\nmodprobe: a later line");
    let out = load_result(1, &followed);
    assert!(out.status.success(), "no chip, then another line: {out:?}");
}

/// `PKG-i`, measured on Fedora 45, whose kernel-modules-core has no nct6775:
/// without the install rule systemd-modules-load skips a module that is not
/// installed, but the rule makes it look present and the guard's failure became
/// EINVAL. When modprobe fails and `modinfo` cannot find the module, the guard
/// exits 0, passes the message on and says why in the journal. Measured
/// wording: libkmod's "could not find module by name", then ENOENT's text.
#[test]
fn a_module_not_installed_for_this_kernel_exits_zero() {
    let missing = "modprobe: ERROR: libkmod/libkmod-module.c:699 kmod_module_insert_module() \
                   could not find module by name='nct6775'\n\
                   modprobe: ERROR: could not insert 'nct6775': Unknown symbol in module, \
                   or unknown parameter (see dmesg)";
    let out = load_result_modinfo(1, missing, 1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim_end(), missing);
    assert!(
        stdout.starts_with("JOURNAL notice nct6775 is not installed for this kernel"),
        "{stdout:?}"
    );

    // The same failure with the module installed is a real one (an unknown
    // symbol), and keeps its status.
    let out = load_result_modinfo(1, missing, 0);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// The other half of `PKG-i`: only no chip and no module are settled. A real
/// failure keeps modprobe's own non-zero status and message, and writes no
/// journal line that would misdescribe it. `modinfo` finds the module in every
/// case here.
#[test]
fn a_real_load_failure_keeps_its_non_zero_exit() {
    for (rc, stderr, why) in [
        (
            1,
            "modprobe: FATAL: Module nct6775 not found in directory /lib/modules/6.12.0",
            "a 'not found' that modinfo contradicts",
        ),
        (
            1,
            "modprobe: ERROR: could not insert 'nct6775': Key was rejected by service",
            "a rejected signature",
        ),
        (
            1,
            "modprobe: ERROR: could not insert 'nct6775': Device or resource busy",
            "a resource conflict",
        ),
        // ENXIO's text begins with ENODEV's: the match must end at the line end.
        (
            1,
            "modprobe: ERROR: could not insert 'nct6775': No such device or address",
            "ENXIO",
        ),
        (3, "", "an unexpected status with no message"),
    ] {
        let out = load_result(rc, stderr);
        assert_eq!(
            out.status.code(),
            Some(i32::from(rc)),
            "{why} must keep modprobe's exit status: {out:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr).trim_end(),
            stderr,
            "{why}: modprobe's message is passed on unchanged"
        );
        assert!(
            out.stdout.is_empty(),
            "{why}: no journal line, got {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// A successful load stays quiet: exit 0, nothing added to stderr or the journal.
#[test]
fn a_successful_load_adds_nothing() {
    let out = load_result(0, "");
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
}

#[test]
fn superio_guard_board_list_matches_chip_db() {
    // The drift guard. The shell list is a copy of the Rust table; without this
    // test, adding a board to `GIGABYTE_DUAL_CHIP_BOARDS` would silently leave
    // its owners unprotected.
    let guard = std::fs::read_to_string(guard_path()).expect("read guard");
    let begin = guard
        .find("# ── BEGIN GENERATED BOARD LIST ──")
        .expect("BEGIN marker");
    let end = guard
        .find("# ── END GENERATED BOARD LIST ──")
        .expect("END marker");
    let block = &guard[begin..end];
    let list_start = block.find('\'').expect("opening quote") + 1;
    let list_end = block.rfind('\'').expect("closing quote");
    let mut shell_boards: Vec<&str> = block[list_start..list_end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let chip_db = std::fs::read_to_string(repo_root().join("daemon/src/hwmon/chip_db.rs"))
        .expect("read chip_db.rs");
    let table_start = chip_db
        .find("const GIGABYTE_DUAL_CHIP_BOARDS")
        .expect("table");
    // The table ends at the first `];` after it starts.
    let table_end = table_start + chip_db[table_start..].find("\n];").expect("table end");
    let mut rust_boards: Vec<&str> = chip_db[table_start..table_end]
        .lines()
        .filter_map(|l| l.trim().strip_prefix("board_name: \""))
        .filter_map(|l| l.strip_suffix("\","))
        .collect();

    assert!(
        !rust_boards.is_empty(),
        "parser found no boards in chip_db.rs — the extraction broke, which \
         would make this test pass vacuously"
    );
    shell_boards.sort_unstable();
    rust_boards.sort_unstable();
    assert_eq!(
        shell_boards, rust_boards,
        "packaging/ventulus-superio-guard's board list has drifted from \
         GIGABYTE_DUAL_CHIP_BOARDS in daemon/src/hwmon/chip_db.rs. Update the \
         list between the GENERATED markers in the guard."
    );
}

// ── The modprobe.d `install` lines themselves (DEC-414, `PKG-g`) ─────────────

const INSTALLED_GUARD: &str = "/usr/lib/ventulus/ventulus-superio-guard";

/// Every `install <module> <command>` line in the shipped drop-in, as kmod
/// reads it: the module, then the rest of the line verbatim.
fn install_lines() -> Vec<(String, String)> {
    let conf =
        std::fs::read_to_string(repo_root().join("packaging/modprobe.d-ventulus-superio.conf"))
            .expect("read the modprobe.d drop-in");
    conf.lines()
        .filter_map(|l| l.strip_prefix("install "))
        .map(|rest| {
            let (module, cmd) = rest.split_once(' ').expect("install <module> <command>");
            (module.to_string(), cmd.to_string())
        })
        .collect()
}

/// A unique scratch directory per call, for the same parallel-test reason as
/// [`decide`].
fn scratch(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cofc-install-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Run one install command the way kmod does (`sh -c`, `$CMDLINE_OPTS`
/// substituted), with the guard's installed path pointed at `guard`, and a
/// `modprobe` on PATH that records any call instead of loading anything.
fn run_install(
    cmd: &str,
    guard: &std::path::Path,
    dmi: Option<&std::path::Path>,
) -> (std::process::Output, bool) {
    run_install_env(cmd, guard, dmi, &[])
}

/// [`run_install`], with extra environment for the guard.
fn run_install_env(
    cmd: &str,
    guard: &std::path::Path,
    dmi: Option<&std::path::Path>,
    env: &[(&str, &str)],
) -> (std::process::Output, bool) {
    let bin = scratch("bin");
    let marker = bin.join("modprobe-was-called");
    let fake = bin.join("modprobe");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho \"$@\" > '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let cmd = cmd
        .replace(INSTALLED_GUARD, &guard.display().to_string())
        .replace("$CMDLINE_OPTS", "");
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(&cmd)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    if let Some(dmi) = dmi {
        c.env("VENTULUS_DMI_DIR", dmi)
            .env("VENTULUS_GUARD_DRY_RUN", "1");
    }
    c.envs(env.iter().copied());
    let out = c.output().expect("run the install command");
    let called = marker.exists();
    std::fs::remove_dir_all(&bin).ok();
    (out, called)
}

#[test]
fn both_modules_have_a_guarded_install_line() {
    // Presence first: the tests below iterate over these lines, and an empty
    // list would pass them all.
    let lines = install_lines();
    let modules: Vec<&str> = lines.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(modules, ["nct6775", "w83627ehf"]);
    for (module, cmd) in &lines {
        assert_eq!(
            cmd.matches(INSTALLED_GUARD).count(),
            2,
            "{module}: the line must both check for the guard and exec it, at its \
             installed path: {cmd}"
        );
    }
}

/// [SAFETY] DEC-414 (`PKG-g`): mkinitcpio's `modconf` copies the drop-in into
/// every initramfs but not the guard it names. There, the line must load
/// NOTHING — on an affected ITE board an unguarded load is the very probe the
/// guard exists to stop — and must say why, and must not fail the boot.
#[test]
fn without_the_guard_nothing_is_loaded_and_the_line_says_why() {
    let missing = scratch("noguard").join("ventulus-superio-guard");
    for (module, cmd) in install_lines() {
        let (out, modprobe_called) = run_install(&cmd, &missing, None);
        assert!(
            out.status.success(),
            "{module}: a missing guard must not fail the initramfs load: {out:?}"
        );
        assert!(
            !modprobe_called,
            "{module}: the line loaded the module without its guard — that is the \
             destructive probe on an ITE board"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("{module} is not loaded in the initramfs")),
            "{module}: the line must say why nothing was loaded, got: {stderr:?}"
        );
    }
}

/// With the guard present — every real root — the line hands the decision to
/// the guard, for the module the line names.
#[test]
fn with_the_guard_the_line_hands_the_decision_to_it() {
    for (module, cmd) in install_lines() {
        for (vendor, board, want) in [
            (GIGABYTE, "X870E AORUS MASTER", "SUPPRESS"),
            // DEC-424: an unlisted Gigabyte board through the real install line.
            (GIGABYTE, "B650M DS3H", "SUPPRESS"),
            (
                "ASUSTeK COMPUTER INC.",
                "ROG STRIX X670E-E GAMING WIFI",
                "LOAD",
            ),
        ] {
            let dmi = scratch("dmi");
            std::fs::write(dmi.join("board_vendor"), vendor).unwrap();
            std::fs::write(dmi.join("board_name"), board).unwrap();
            let (out, _) = run_install(&cmd, &guard_path(), Some(&dmi));
            std::fs::remove_dir_all(&dmi).ok();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.trim().starts_with(&format!("{want} {module}")),
                "{module} on {board}: expected {want} from the guard, got {stdout:?} \
                 (stderr {:?})",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// `PKG-i` through the real install line, run as kmod runs it (`sh -c`, the
/// guard `exec`'d): what kmod sees is the line's exit status, and a board
/// without the chip, or a kernel without the module, must give it 0 while a
/// real failure still gives non-zero.
#[test]
fn the_install_line_carries_the_no_chip_exit_to_kmod() {
    for (module, cmd) in install_lines() {
        let dmi = scratch("dmi");
        std::fs::write(dmi.join("board_vendor"), "ASUSTeK COMPUTER INC.").unwrap();
        std::fs::write(dmi.join("board_name"), "PRIME X670-P").unwrap();
        let dmi_s = dmi.display().to_string();
        let no_chip = format!("modprobe: ERROR: could not insert '{module}': No such device");
        for (stderr, modinfo_rc, want_ok) in [
            (no_chip.as_str(), "0", true),
            ("modprobe: FATAL: Module not found", "1", true),
            ("modprobe: FATAL: Module not found", "0", false),
        ] {
            let (out, _) = run_install_env(
                &cmd,
                &guard_path(),
                None,
                &[
                    ("VENTULUS_DMI_DIR", &dmi_s),
                    ("VENTULUS_GUARD_SIM_RC", "1"),
                    ("VENTULUS_GUARD_SIM_STDERR", stderr),
                    ("VENTULUS_GUARD_SIM_MODINFO_RC", modinfo_rc),
                ],
            );
            assert_eq!(
                out.status.success(),
                want_ok,
                "{module} with {stderr:?}, modinfo {modinfo_rc}: {out:?}"
            );
        }
        std::fs::remove_dir_all(&dmi).ok();
    }
}

/// `DC-da` (DEC-468): the daemon's `chip_db::superio_guard_declines` decides
/// what the hardware diagnostics tell a user about loading `nct6775`, so it must
/// say what this script will actually do. Compared case by case against the
/// script's own dry-run, never against a restated table. `None` is a missing DMI
/// file, which the daemon reads as an empty string.
#[test]
fn the_daemon_predicate_matches_the_guard_script() {
    let listed = "X870E AORUS MASTER";
    let cases: [(Option<&str>, Option<&str>); 9] = [
        (Some(GIGABYTE), Some(listed)),
        (Some(GIGABYTE), Some("B650 AORUS ELITE AX")),
        (
            Some("gigabyte technology co., ltd."),
            Some("b650 aorus elite ax"),
        ),
        (None, Some(listed)),
        (None, Some("B650 AORUS ELITE AX")),
        (Some("Micro-Star International Co., Ltd."), Some(listed)),
        (Some("Giga Computing"), Some(listed)),
        (Some(GIGABYTE), None),
        (Some("ASUSTeK COMPUTER INC."), Some("PRIME X670-P")),
    ];
    let mut saw = (false, false);
    for (vendor, board) in cases {
        let script = decide_with(vendor, board, "nct6775");
        let suppress = script.starts_with("SUPPRESS");
        assert!(
            suppress || script.starts_with("LOAD"),
            "unexpected guard output: {script:?}"
        );
        let daemon = ventulusd::hwmon::chip_db::superio_guard_declines(
            vendor.unwrap_or(""),
            board.unwrap_or(""),
        );
        assert_eq!(
            daemon, suppress,
            "vendor {vendor:?}, board {board:?}: the script says {script:?}"
        );
        if suppress {
            saw.0 = true;
        } else {
            saw.1 = true;
        }
    }
    assert!(saw.0 && saw.1, "both decisions must be exercised");
}
