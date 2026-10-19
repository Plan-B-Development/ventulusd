//! The Debian/Ubuntu, Fedora and tarball packages (DEC-499), run for real where
//! that needs no root: `packaging/linux/stage.sh` (their one file list), the
//! tarball that `build.sh` assembles, and its `install.sh` and `uninstall`
//! under `--root`. The lifecycle under systemd (stop, SIGKILL, upgrade and
//! removal each replaying the hand-back) is `.github/ci/lifecycle.sh`, which
//! ci.yml runs per distribution; the last tests here hold that job and
//! release.yml to their shape.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read_repo(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every regular file under `root`, as its absolute path on the target system
/// mapped to its permission bits.
fn tree(root: &Path) -> BTreeMap<String, u32> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, u32>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_str().unwrap();
                out.insert(format!("/{rel}"), meta.permissions().mode() & 0o7777);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed ({}):\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Stand-ins for the two binaries and the two rendered man pages.
fn fake_build(dir: &Path) -> (PathBuf, PathBuf) {
    let bin = dir.join("bin");
    let man = dir.join("man");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&man).unwrap();
    for name in ["ventulusd", "ventulus-tray"] {
        std::fs::write(bin.join(name), format!("#!/bin/sh\necho {name}\n")).unwrap();
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(man.join(format!("{name}.1")), ".TH test\n").unwrap();
    }
    (bin, man)
}

fn stage(flavour: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, man) = fake_build(tmp.path());
    let dest = tmp.path().join("stage");
    let out = Command::new("bash")
        .arg("packaging/linux/stage.sh")
        .args([flavour, bin.to_str().unwrap(), man.to_str().unwrap()])
        .arg(&dest)
        .current_dir(repo())
        .output()
        .unwrap();
    ok(&out, &format!("stage.sh {flavour}"));
    (tmp, dest)
}

/// The files `packaging/PKGBUILD`'s `package()` installs, with their modes.
fn pkgbuild_files() -> BTreeMap<String, u32> {
    let pkgbuild = read_repo("packaging/PKGBUILD");
    let body = pkgbuild
        .split("\npackage() {")
        .nth(1)
        .expect("PKGBUILD has a package() function");
    let body = body.split("\n}").next().unwrap().replace("\\\n", " ");
    let mut out = BTreeMap::new();
    for line in body.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() != Some(&"install") {
            continue;
        }
        let Some(mode) = words.iter().find_map(|w| w.strip_prefix("-Dm")) else {
            continue; // `install -dm755 <dir>`: a directory, which holds files listed anyway
        };
        let dest = words.last().unwrap().trim_matches('"');
        let dest = dest
            .strip_prefix("$pkgdir")
            .unwrap_or_else(|| panic!("PKGBUILD install target outside $pkgdir: {line}"))
            .replace("$pkgname", "ventulusd");
        out.insert(dest, u32::from_str_radix(mode, 8).unwrap());
    }
    assert!(out.len() > 30, "parsed too few PKGBUILD installs: {out:?}");
    out
}

/// What exists only for Arch users upgrading from control-ofc-daemon 4.x
/// (DEC-493): the carry-over hook and script and the compatibility socket link.
const ARCH_ONLY: [&str; 3] = [
    "/usr/lib/tmpfiles.d/ventulusd.conf",
    "/usr/share/libalpm/hooks/90-ventulusd-carryover.hook",
    "/usr/lib/ventulus/ventulusd-carryover",
];

/// The PKGBUILD's file set as `flavour` lays it out: the Arch-only items left
/// out, zsh completions and the licence where that distribution looks for them.
fn expected_for(flavour: &str) -> BTreeMap<String, u32> {
    let zsh = match flavour {
        "deb" => "/usr/share/zsh/vendor-completions",
        "rpm" => "/usr/share/zsh/site-functions",
        "tarball" => "/usr/local/share/zsh/site-functions",
        _ => unreachable!(),
    };
    pkgbuild_files()
        .into_iter()
        .filter(|(path, _)| !ARCH_ONLY.contains(&path.as_str()))
        .map(|(path, mode)| {
            let path = if let Some(file) = path.strip_prefix("/usr/share/zsh/site-functions/") {
                format!("{zsh}/{file}")
            } else if flavour == "deb" && path == "/usr/share/licenses/ventulusd/LICENSE" {
                "/usr/share/doc/ventulusd/copyright".to_string()
            } else if flavour == "deb" && path == "/usr/share/licenses/ventulusd/NOTICE.md" {
                "/usr/share/doc/ventulusd/NOTICE.md".to_string()
            } else {
                path
            };
            (path, mode)
        })
        .collect()
}

/// stage.sh is the one file list for three formats and the PKGBUILD is a
/// second, independent one. A file added to one and not the other would ship
/// on Arch and be missing elsewhere, or the reverse; a mode that differs would
/// leave a script unexecutable on one distribution.
#[test]
fn stage_installs_what_the_pkgbuild_installs_for_every_format() {
    for flavour in ["deb", "rpm", "tarball"] {
        let (_tmp, dest) = stage(flavour);
        let staged = tree(&dest);
        let expected = expected_for(flavour);
        let missing: Vec<_> = expected
            .iter()
            .filter(|(p, _)| !staged.contains_key(*p))
            .collect();
        let extra: Vec<_> = staged
            .iter()
            .filter(|(p, _)| !expected.contains_key(*p))
            .collect();
        let wrong_mode: Vec<_> = expected
            .iter()
            .filter_map(|(p, m)| staged.get(p).filter(|s| *s != m).map(|s| (p, *m, *s)))
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty() && wrong_mode.is_empty(),
            "{flavour}: stage.sh and the PKGBUILD disagree\n  missing: {missing:?}\n  \
             extra: {extra:?}\n  (path, PKGBUILD mode, staged mode): {wrong_mode:?}"
        );
    }
}

/// The program a unit's `ExecStopPost=` runs, with systemd's prefixes removed.
fn exec_stop_post(unit: &str) -> Vec<String> {
    unit.lines()
        .filter_map(|l| l.trim().strip_prefix("ExecStopPost="))
        .map(|v| {
            v.trim_start_matches(['-', '+', '!', '@', ':'])
                .split_whitespace()
                .next()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// [SAFETY] DEC-382: the unit's ExecStopPost gives every fan header back on
/// every stop path, SIGKILL included. In a package whose unit names a script
/// the package does not ship, or ships unexecutable, systemd logs a failed
/// ExecStopPost and the fans stay where the dead daemon left them.
#[test]
fn every_format_ships_the_hand_back_its_unit_runs() {
    for flavour in ["deb", "rpm", "tarball"] {
        let (_tmp, dest) = stage(flavour);
        let unit = std::fs::read_to_string(dest.join("usr/lib/systemd/system/ventulusd.service"))
            .unwrap_or_else(|e| panic!("{flavour}: no unit staged: {e}"));
        let programs = exec_stop_post(&unit);
        assert!(
            programs.contains(&"/usr/bin/ventulus-restore-auto".to_string()),
            "{flavour}: the staged unit must run the hand-back as ExecStopPost; found {programs:?}"
        );
        for program in &programs {
            let staged = dest.join(program.trim_start_matches('/'));
            let mode = std::fs::metadata(&staged)
                .unwrap_or_else(|_| {
                    panic!("{flavour}: ExecStopPost {program} is not in the package")
                })
                .permissions()
                .mode();
            assert!(
                mode & 0o111 != 0,
                "{flavour}: ExecStopPost {program} is not executable"
            );
        }
        let shipped = std::fs::read(dest.join("usr/bin/ventulus-restore-auto")).unwrap();
        assert_eq!(
            shipped,
            std::fs::read(repo().join("packaging/ventulus-restore-auto.sh")).unwrap(),
            "{flavour}: the staged hand-back must be the script restore_auto_script.rs tests"
        );
    }
}

/// The tarball build.sh assembles from a fake build, extracted.
fn build_tarball() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    fake_build(&tmp.path().join("work"));
    let out_dir = tmp.path().join("out");
    let out = Command::new("bash")
        .args(["packaging/linux/build.sh", "tarball"])
        .arg(tmp.path().join("work"))
        .arg("9.9.9")
        .arg(&out_dir)
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .current_dir(repo())
        .output()
        .unwrap();
    ok(&out, "build.sh tarball");
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(out_dir.join("ventulusd-9.9.9-x86_64.tar.gz"))
        .arg("-C")
        .arg(tmp.path())
        .output()
        .unwrap();
    ok(&out, "tar -x");
    let pkg = tmp.path().join("ventulusd-9.9.9-x86_64");
    (tmp, pkg)
}

/// `sha256 mode /path` per line, as build.sh writes it.
fn manifest(pkg: &Path) -> BTreeMap<String, (String, u32)> {
    std::fs::read_to_string(pkg.join("MANIFEST"))
        .unwrap()
        .lines()
        .map(|l| {
            let mut w = l.split(' ');
            let (sum, mode, path) = (w.next().unwrap(), w.next().unwrap(), w.next().unwrap());
            (
                path.to_string(),
                (sum.to_string(), u32::from_str_radix(mode, 8).unwrap()),
            )
        })
        .collect()
}

/// Rewrite MANIFEST from the files now in the package, as build.sh would.
fn rewrite_manifest(pkg: &Path) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            r#"cd files && find . -type f -printf '%P\n' | LC_ALL=C sort | while read -r f; do
                printf '%s %s /%s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c %a "$f")" "$f"
            done > ../MANIFEST"#,
        )
        .current_dir(pkg)
        .output()
        .unwrap();
    ok(&out, "rewrite MANIFEST");
}

fn run(script: &Path, root: &Path) -> Output {
    Command::new("bash")
        .arg(script)
        .arg("--root")
        .arg(root)
        .output()
        .unwrap()
}

/// The tarball is the staged tree plus its uninstaller, every file listed in
/// MANIFEST with the sum and mode it really has.
#[test]
fn the_tarball_lists_every_file_it_carries() {
    let (_tmp, pkg) = build_tarball();
    let files = tree(&pkg.join("files"));
    let (_s, staged) = stage("tarball");
    let mut expected = tree(&staged);
    expected.insert("/usr/lib/ventulus/uninstall".into(), 0o755);
    assert_eq!(
        files, expected,
        "the tarball's files/ must be the staged tree plus the uninstaller"
    );

    let listed = manifest(&pkg);
    assert_eq!(
        listed.keys().collect::<Vec<_>>(),
        files.keys().collect::<Vec<_>>(),
        "MANIFEST must list exactly the files in files/"
    );
    for (path, (_, mode)) in &listed {
        assert_eq!(*mode, files[path], "MANIFEST mode for {path}");
    }
    assert!(
        std::fs::metadata(pkg.join("install.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111
            != 0,
        "install.sh must be executable"
    );
}

/// A fresh install puts every file at its path and mode and records MANIFEST,
/// which is what the next upgrade and the uninstaller read.
#[test]
fn install_places_every_file_and_records_the_manifest() {
    let (tmp, pkg) = build_tarball();
    let root = tmp.path().join("root");
    ok(&run(&pkg.join("install.sh"), &root), "install.sh");
    let mut installed = tree(&root);
    let recorded = installed.remove("/usr/lib/ventulus/tarball-manifest");
    assert!(recorded.is_some(), "install.sh must record the manifest");
    assert_eq!(installed, tree(&pkg.join("files")));
    assert_eq!(
        std::fs::read(root.join("usr/lib/ventulus/tarball-manifest")).unwrap(),
        std::fs::read(pkg.join("MANIFEST")).unwrap()
    );
}

/// An upgrade replaces what the user never touched, keeps what they edited
/// (writing the new default beside it as .new), and removes what the new
/// version no longer ships, unless edited.
#[test]
fn an_upgrade_keeps_edited_configuration_and_removes_what_is_gone() {
    let (tmp, pkg) = build_tarball();
    let root = tmp.path().join("root");
    ok(&run(&pkg.join("install.sh"), &root), "first install");

    let edited = root.join("etc/ventulus/daemon.toml");
    let mut text = std::fs::read_to_string(&edited).unwrap();
    text.push_str("# mine\n");
    std::fs::write(&edited, &text).unwrap();
    let untouched = root.join("etc/modules-load.d/ventulus.conf");
    std::fs::remove_file(root.join("etc/xdg/autostart/ventulus-tray.desktop")).unwrap();

    // The next version: two new defaults, a dropped config file and a dropped
    // doc, and a new binary.
    let files = pkg.join("files");
    std::fs::write(files.join("etc/ventulus/daemon.toml"), "# new default\n").unwrap();
    std::fs::write(
        files.join("etc/modules-load.d/ventulus.conf"),
        "# new modules\n",
    )
    .unwrap();
    std::fs::remove_file(files.join("etc/ventulus/profiles/quiet.json")).unwrap();
    std::fs::remove_file(files.join("usr/share/doc/ventulusd/README.md")).unwrap();
    std::fs::write(files.join("usr/bin/ventulusd"), "#!/bin/sh\necho v2\n").unwrap();
    rewrite_manifest(&pkg);
    let mut quiet = std::fs::read_to_string(root.join("etc/ventulus/profiles/quiet.json")).unwrap();
    quiet.push(' ');
    std::fs::write(root.join("etc/ventulus/profiles/quiet.json"), &quiet).unwrap();

    let out = run(&pkg.join("install.sh"), &root);
    ok(&out, "upgrade");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        text,
        "an edited config is kept"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("etc/ventulus/daemon.toml.new")).unwrap(),
        "# new default\n",
        "the new default arrives beside it"
    );
    assert!(
        stdout.contains("Kept your edited /etc/ventulus/daemon.toml"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&untouched).unwrap(),
        "# new modules\n",
        "an unedited config takes the new default"
    );
    assert!(!root.join("etc/modules-load.d/ventulus.conf.new").exists());
    assert!(
        root.join("etc/xdg/autostart/ventulus-tray.desktop")
            .exists(),
        "a config the user deleted comes back, as the file itself says"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("usr/bin/ventulusd")).unwrap(),
        "#!/bin/sh\necho v2\n"
    );
    assert!(
        !root.join("usr/share/doc/ventulusd/README.md").exists(),
        "a file the new version dropped is removed"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("etc/ventulus/profiles/quiet.json")).unwrap(),
        quiet,
        "a dropped config file the user edited is kept"
    );
    let leftovers: Vec<_> = tree(&root)
        .into_keys()
        .filter(|p| p.contains(".ventulus-new"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary files left behind: {leftovers:?}"
    );
}

/// A tarball whose files do not match its MANIFEST (a truncated download or a
/// damaged extraction) installs nothing. This catches corruption only:
/// MANIFEST travels in the same tarball, so authenticity comes from SHA256SUMS
/// and the attestation on the .tar.gz.
#[test]
fn install_refuses_a_tarball_that_does_not_match_its_manifest() {
    let (tmp, pkg) = build_tarball();
    std::fs::write(
        pkg.join("files/usr/bin/ventulus-restore-auto"),
        "#!/bin/sh\n",
    )
    .unwrap();
    let root = tmp.path().join("root");
    let out = run(&pkg.join("install.sh"), &root);
    assert!(
        !out.status.success(),
        "install.sh accepted a file not matching MANIFEST"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("/usr/bin/ventulus-restore-auto does not match MANIFEST"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !root.exists() || tree(&root).is_empty(),
        "nothing may be installed"
    );
}

/// The uninstaller removes what the install placed, keeps edited
/// configuration, and leaves no empty ventulus directory behind.
#[test]
fn uninstall_removes_the_install_and_keeps_edited_configuration() {
    let (tmp, pkg) = build_tarball();
    let root = tmp.path().join("root");
    ok(&run(&pkg.join("install.sh"), &root), "install");
    let edited = root.join("etc/ventulus/daemon.toml");
    let mut text = std::fs::read_to_string(&edited).unwrap();
    text.push_str("# mine\n");
    std::fs::write(&edited, &text).unwrap();

    let out = run(&root.join("usr/lib/ventulus/uninstall"), &root);
    ok(&out, "uninstall");
    assert_eq!(
        tree(&root).into_keys().collect::<Vec<_>>(),
        vec!["/etc/ventulus/daemon.toml".to_string()],
        "only the edited config may remain"
    );
    assert_eq!(std::fs::read_to_string(&edited).unwrap(), text);
    for dir in [
        "usr/lib/ventulus",
        "etc/ventulus/profiles",
        "usr/share/doc/ventulusd",
    ] {
        assert!(!root.join(dir).exists(), "{dir} was left behind");
    }

    let again = run(&root.join("usr/lib/ventulus/uninstall"), &root);
    assert!(
        !again.status.success(),
        "a second uninstall has nothing to remove"
    );
}

fn workflow(name: &str) -> String {
    read_repo(&format!(".github/workflows/{name}"))
}

/// The jobs at two-space depth of a workflow and their bodies.
fn job(wf: &str, name: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in wf.lines() {
        if line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':') {
            inside = line.trim() == format!("{name}:");
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(!out.is_empty(), "no `{name}:` job");
    out
}

/// [SAFETY] Every format the Release ships has a lifecycle leg on every
/// distribution it supports. A format without one could ship a stop, upgrade
/// or removal path that skips the hand-back, and nothing would notice.
#[test]
fn ci_runs_the_lifecycle_for_every_format_and_distribution() {
    let lifecycle = job(&workflow("ci.yml"), "lifecycle");
    let legs: Vec<(String, String)> = lifecycle
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- {")?.strip_suffix('}')?;
            let mut image = None;
            let mut format = None;
            for kv in l.split(',') {
                let (k, v) = kv.split_once(':')?;
                let v = v.trim().trim_matches('\'').to_string();
                match k.trim() {
                    "image" => image = Some(v),
                    "format" => format = Some(v),
                    _ => {}
                }
            }
            Some((image?, format?))
        })
        .collect();
    for want in [
        ("debian:13", "deb"),
        ("ubuntu:26.04", "deb"),
        ("fedora:44", "rpm"),
        ("fedora:45", "rpm"),
        ("debian:13", "tarball"),
        ("ubuntu:26.04", "tarball"),
        ("fedora:44", "tarball"),
        ("fedora:45", "tarball"),
    ] {
        assert!(
            legs.contains(&(want.0.to_string(), want.1.to_string())),
            "ci.yml lifecycle has no {} leg on {}; legs: {legs:?}",
            want.1,
            want.0
        );
    }
    assert!(
        lifecycle.contains(".github/ci/lifecycle-container.sh"),
        "the lifecycle job must run lifecycle-container.sh"
    );
}

/// The Release waits for every package, attests all of them, and stays a draft
/// until the uploaded assets read back as built: a Release published with a
/// format missing cannot have it added later once immutable releases are on.
#[test]
fn the_release_ships_every_format_attested_and_checked_before_publishing() {
    let wf = workflow("release.yml");
    let release = job(&wf, "github-release");
    let needs = release
        .lines()
        .find_map(|l| l.trim().strip_prefix("needs:"))
        .expect("github-release has needs:");
    for dep in ["build-test", "linux-packages", "rpm-package", "ci-green"] {
        assert!(
            needs.contains(dep),
            "github-release must need {dep}: {needs}"
        );
    }
    let key = |k: &str| {
        release
            .lines()
            .filter_map(|l| l.trim().strip_prefix(k).map(str::trim))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        key("subject-path:"),
        ["dist/*"],
        "every asset must be attested"
    );
    assert_eq!(key("files:"), ["dist/*"], "every asset must be attached");
    assert_eq!(
        key("draft:"),
        ["true"],
        "the Release must be created as a draft"
    );
    let sums = release.find("> SHA256SUMS").expect("SHA256SUMS is written");
    let attest = release.find("actions/attest-build-provenance@").unwrap();
    let create = release.find("softprops/action-gh-release@").unwrap();
    let check = release
        .find("sha256sum -c SHA256SUMS")
        .expect("the draft is checked");
    let publish = release
        .find("releases/$id\" -F draft=false")
        .expect("the checked draft is published by its id");
    assert!(
        sums < attest && attest < create && create < check && check < publish,
        "order must be SHA256SUMS, attest, draft, check, publish"
    );
    for asset in [
        "ventulusd-$v-1-x86_64.pkg.tar.zst",
        "ventulusd_$v-1_amd64.deb",
        "ventulusd-$v-1.x86_64.rpm",
        "ventulusd-$v-x86_64.tar.gz",
    ] {
        assert!(release.contains(asset), "the Release must require {asset}");
    }
    for name in ["linux-packages", "rpm-package"] {
        assert!(
            job(&wf, name).contains("packaging/linux/build.sh"),
            "{name} must build with packaging/linux/build.sh, as ci.yml's tested jobs do"
        );
    }
}

/// The package names in a PKGBUILD array (`depends=(...)`), quotes and any
/// `: description` or version constraint removed.
fn pkgbuild_array(pkgbuild: &str, name: &str) -> Vec<String> {
    let start = pkgbuild
        .find(&format!("\n{name}=("))
        .unwrap_or_else(|| panic!("PKGBUILD has no {name}=( array"))
        + name.len()
        + 3;
    let (mut out, mut word, mut quote) = (Vec::new(), String::new(), None);
    for ch in pkgbuild[start..].chars() {
        match (quote, ch) {
            (None, ')') => break,
            (None, '\'' | '"') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => out.push(std::mem::take(&mut word)),
            (_, c) => word.push(c),
        }
    }
    out.push(word);
    out.into_iter()
        .filter(|w| !w.is_empty())
        .map(|w| {
            let w = w.split(':').next().unwrap();
            w.split(['<', '>', '=']).next().unwrap().to_string()
        })
        .collect()
}

/// The package names in a Debian control field, alternatives included, with
/// continuation lines joined and version constraints removed.
fn deb_field(control: &str, field: &str) -> Vec<String> {
    let mut value = String::new();
    let mut lines = control.lines();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            value.push_str(rest);
            for more in lines.by_ref() {
                if !more.starts_with([' ', '\t']) {
                    break;
                }
                value.push_str(more);
            }
        }
    }
    value
        .split([',', '|'])
        .filter_map(|alt| alt.split_whitespace().next())
        .map(|w| w.split('(').next().unwrap().to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The package names an rpm spec tag lists (`Requires`, `Requires(post)`, …),
/// version constraints removed.
fn rpm_tag(spec: &str, tag: &str) -> Vec<String> {
    spec.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            (key == tag || key.starts_with(&format!("{tag}("))).then_some(value)
        })
        .flat_map(|v| {
            v.split([',', ' ', '\t'])
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic()))
        .collect()
}

/// A package that builds out-of-tree modules: DKMS, a `*-dkms` driver, kernel
/// headers or akmods.
fn builds_modules(name: &str) -> bool {
    name.contains("dkms")
        || name.contains("headers")
        || name.starts_with("kernel-devel")
        || name.starts_with("akmod")
}

/// DEC-500 Phase 3: an out-of-tree driver, and the DKMS and kernel headers it
/// builds with, stay the user's choice in every format. None may become a
/// dependency the package manager installs by default — Arch `depends`, a
/// `.deb`'s `Depends`, `Pre-Depends` or `Recommends` (apt installs those), an
/// rpm's `Requires` or `Recommends` (dnf installs weak dependencies) — and the
/// tarball installer installs no package at all. They are offered where each
/// format offers optional software: `optdepends` and `Suggests`.
#[test]
fn no_format_installs_dkms_or_kernel_headers_on_its_own() {
    let pkgbuild = read_repo("packaging/PKGBUILD");
    let control = read_repo("packaging/debian/control");
    let spec = read_repo("packaging/rpm/ventulusd.spec");

    // Each parser reads what it should, so an empty list cannot pass.
    let arch_depends = pkgbuild_array(&pkgbuild, "depends");
    assert!(
        arch_depends.contains(&"glibc".to_string()),
        "{arch_depends:?}"
    );
    let deb_depends = deb_field(&control, "Depends");
    assert!(deb_depends.contains(&"kmod".to_string()), "{deb_depends:?}");
    let rpm_requires = rpm_tag(&spec, "Requires");
    assert!(
        rpm_requires.contains(&"kmod".to_string()),
        "{rpm_requires:?}"
    );

    let installed: Vec<(&str, Vec<String>)> = vec![
        ("PKGBUILD depends", arch_depends),
        ("debian Depends", deb_depends),
        ("debian Pre-Depends", deb_field(&control, "Pre-Depends")),
        ("debian Recommends", deb_field(&control, "Recommends")),
        ("rpm Requires", rpm_requires),
        ("rpm Recommends", rpm_tag(&spec, "Recommends")),
    ];
    for (what, names) in &installed {
        let bad: Vec<_> = names.iter().filter(|n| builds_modules(n)).collect();
        assert!(bad.is_empty(), "{what} installs {bad:?} by default");
    }

    // Offered, never installed: the optional lists name them.
    let optdepends = pkgbuild_array(&pkgbuild, "optdepends");
    for name in ["dkms", "linux-headers", "it87-dkms-git", "lm_sensors"] {
        assert!(
            optdepends.contains(&name.to_string()),
            "optdepends lacks {name}: {optdepends:?}"
        );
    }
    assert_eq!(deb_field(&control, "Suggests"), ["lm-sensors", "dkms"]);
    assert_eq!(rpm_tag(&spec, "Suggests"), ["lm_sensors", "dkms"]);

    // The tarball has no dependency list; its installer installs nothing.
    let install = read_repo("packaging/linux/install.sh");
    let commands: Vec<&str> = install
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    assert!(
        commands.iter().any(|l| l.contains("dpkg -S")),
        "the scan reads the installer's commands"
    );
    for verb in [
        "apt install",
        "apt-get install",
        "dnf install",
        "pacman -S",
        "dkms ",
    ] {
        assert!(
            !commands.iter().any(|l| l.contains(verb)),
            "install.sh runs `{verb}`"
        );
    }
}
