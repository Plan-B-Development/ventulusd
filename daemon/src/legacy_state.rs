//! One-time import of a Control-OFC 4.x state directory (the Ventulus rename).
//!
//! `control-ofc-daemon` kept its state in `/var/lib/control-ofc`; `ventulusd`
//! keeps it in `/var/lib/ventulus`. Without an import, the first start after the
//! upgrade would run on defaults: no active profile, and — the safety-relevant
//! part — no user-assigned header roles from `runtime.toml`, so a pump role's
//! 30 % floor would silently stop applying.
//!
//! The import copies every regular file from the old directory into the new
//! one, never overwriting a file that is already there, and rewrites the old
//! system directory names embedded in `.json` / `.toml` files. Paths into a
//! user's `~/.config/control-ofc` are kept: the daemon starts at the end of the
//! upgrade, before the GUI has copied that directory, and the old one stays.
//!
//! `runtime.toml` (the roles) is copied first and `daemon_state.json` last. A
//! file that fails is reported and the rest are still copied. An in-progress
//! marker is written before the first copy and replaced by the final marker
//! only when every file has arrived, so an import that failed or was killed is
//! resumed at the next start — even though the state directory now holds its
//! own `runtime.toml` or `daemon_state.json` — and one that finished is never
//! repeated. Without the in-progress marker, a state directory holding either
//! file is the user's own and is never imported into. The old directory is
//! read, never changed.

use std::path::{Path, PathBuf};

use crate::atomic_io::{create_dir_private, write_atomic};
use crate::runtime_config::RUNTIME_CONFIG_FILE;
use crate::single_instance::LOCK_FILE_NAME;

/// Where `control-ofc-daemon` 4.x kept its state.
pub const LEGACY_STATE_DIR: &str = "/var/lib/control-ofc";

/// Where `control-ofc-daemon` 4.x served its API. Probed at startup so a 4.x
/// daemon that is still running keeps this one from starting: the two lock
/// files live in different state directories and cannot see each other.
pub const LEGACY_SOCKET_PATH: &str = "/run/control-ofc/control-ofc.sock";

/// Written into the new state directory once an import has finished.
pub const IMPORT_MARKER: &str = ".imported-from-control-ofc";

/// Written before the first copy; present until the import has finished.
pub const IMPORTING_MARKER: &str = ".importing-from-control-ofc";

/// The active-profile record (`daemon_state.rs`'s `STATE_FILE`).
const DAEMON_STATE_FILE: &str = "daemon_state.json";

/// Files never copied: another process's lock, and the preflight's probe.
const SKIP: &[&str] = &[
    LOCK_FILE_NAME,
    ".writable_probe",
    IMPORTING_MARKER,
    IMPORT_MARKER,
];

/// Old → new directory prefixes rewritten inside imported text files.
const PATH_REWRITES: &[(&str, &str)] = &[
    ("/etc/control-ofc/", "/etc/ventulus/"),
    (
        "/run/control-ofc/control-ofc.sock",
        "/run/ventulus/ventulus.sock",
    ),
];

/// What [`import`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    /// There is no old state directory.
    NoLegacyState,
    /// The marker is present: an earlier start imported already.
    AlreadyImported,
    /// The new directory has its own state; nothing was copied.
    StateInUse,
    /// The import finished; `files` were copied.
    Imported { files: usize },
    /// Something failed; whatever was copied stays, and the next start retries.
    Failed(String),
}

/// Import `legacy` into `state` once. See the module docs for the rules.
pub fn import(legacy: &Path, state: &Path) -> ImportOutcome {
    if !legacy.is_dir() {
        return ImportOutcome::NoLegacyState;
    }
    if state.join(IMPORT_MARKER).exists() {
        return ImportOutcome::AlreadyImported;
    }
    let importing = state.join(IMPORTING_MARKER);
    if !importing.exists()
        && (state.join(DAEMON_STATE_FILE).exists() || state.join(RUNTIME_CONFIG_FILE).exists())
    {
        return ImportOutcome::StateInUse;
    }
    let note = format!("importing from {}\n", legacy.display());
    if let Err(e) =
        create_dir_private(state).and_then(|()| write_atomic(&importing, note.as_bytes()))
    {
        return ImportOutcome::Failed(e);
    }
    let (files, errors) = copy_all(legacy, state);
    if !errors.is_empty() {
        return ImportOutcome::Failed(errors.join("; "));
    }
    let note = format!("imported from {}\n", legacy.display());
    if let Err(e) = write_atomic(&state.join(IMPORT_MARKER), note.as_bytes()) {
        return ImportOutcome::Failed(e);
    }
    // Best effort: the final marker already wins over it.
    let _ = std::fs::remove_file(&importing);
    ImportOutcome::Imported { files }
}

/// Whether `legacy` holds header roles that `state` does not — said when an
/// import is skipped as [`ImportOutcome::StateInUse`], so the loss of a pump
/// role's floor is not silent.
pub fn roles_left_behind(legacy: &Path, state: &Path) -> bool {
    let has_roles = |dir: &Path| {
        std::fs::read_to_string(dir.join(RUNTIME_CONFIG_FILE))
            .is_ok_and(|t| t.lines().any(|l| l.trim() == "[header_roles]"))
    };
    has_roles(legacy) && !has_roles(state)
}

/// Copy every file not already present; returns how many were copied and the
/// failures, each of which leaves the rest still copied.
fn copy_all(legacy: &Path, state: &Path) -> (usize, Vec<String>) {
    let mut files = Vec::new();
    if let Err(e) = collect(legacy, Path::new(""), &mut files) {
        return (0, vec![e]);
    }
    files.sort();
    // The roles first, the active profile last (module docs).
    let top =
        |rel: &PathBuf, name: &str| rel.parent() == Some(Path::new("")) && rel.as_os_str() == name;
    files.sort_by_key(|rel| match () {
        () if top(rel, RUNTIME_CONFIG_FILE) => 0,
        () if top(rel, DAEMON_STATE_FILE) => 2,
        () => 1,
    });

    let state_prefix = format!("{}", state.display());
    let (mut copied, mut errors) = (0, Vec::new());
    for rel in &files {
        let dst = state.join(rel);
        if dst.exists() {
            continue;
        }
        match copy_one(&legacy.join(rel), &dst, is_text(rel), &state_prefix) {
            Ok(()) => copied += 1,
            Err(e) => errors.push(e),
        }
    }
    (copied, errors)
}

fn copy_one(src: &Path, dst: &Path, text: bool, state_prefix: &str) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        create_dir_private(parent)?;
    }
    let bytes = std::fs::read(src).map_err(|e| format!("read '{}': {e}", src.display()))?;
    let bytes = if text {
        match String::from_utf8(bytes) {
            Ok(text) => rewrite_paths(&text, state_prefix).into_bytes(),
            Err(e) => e.into_bytes(),
        }
    } else {
        bytes
    };
    write_atomic(dst, &bytes)
}

/// Every regular file under `root/rel`, as paths relative to `root`. Symlinks
/// are not followed: a state directory holds none the daemon wrote.
fn collect(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let dir = root.join(rel);
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("read dir '{}': {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read dir '{}': {e}", dir.display()))?;
        let kind = entry
            .file_type()
            .map_err(|e| format!("stat '{}': {e}", entry.path().display()))?;
        let child = rel.join(entry.file_name());
        if kind.is_dir() {
            collect(root, &child, out)?;
        } else if kind.is_file()
            && !SKIP
                .iter()
                .any(|s| rel.as_os_str().is_empty() && entry.file_name() == *s)
        {
            out.push(child);
        }
    }
    Ok(())
}

fn is_text(rel: &Path) -> bool {
    matches!(
        rel.extension().and_then(|e| e.to_str()),
        Some("json" | "toml")
    )
}

/// Point old directory names inside an imported file at their new homes; the
/// old state directory itself becomes `state_dir`.
fn rewrite_paths(text: &str, state_dir: &str) -> String {
    let mut out = text.replace(LEGACY_STATE_DIR, state_dir);
    for (old, new) in PATH_REWRITES {
        out = out.replace(old, new);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// A 4.x state directory as the old daemon left it, rooted at `legacy`.
    fn legacy_tree(legacy: &Path) {
        write(
            &legacy.join("runtime.toml"),
            "[header_roles]\n\"hwmon:nct6799:pwm2\" = \"pump\"\n",
        );
        write(
            &legacy.join("daemon_state.json"),
            &format!(
                "{{\"active_profile_path\":\"{}/profiles/quiet.json\"}}",
                LEGACY_STATE_DIR
            ),
        );
        write(&legacy.join("profiles/quiet.json"), "{\"name\":\"Quiet\"}");
        write(
            &legacy.join("validation/s1.json"),
            "{\"search\":[\"/etc/control-ofc/profiles\",\"/root/.config/control-ofc/profiles\"]}",
        );
        write(&legacy.join(LOCK_FILE_NAME), "");
    }

    #[test]
    fn imports_roles_profile_and_rewrites_old_system_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, state) = (tmp.path().join("old"), tmp.path().join("new"));
        legacy_tree(&legacy);

        assert_eq!(
            import(&legacy, &state),
            ImportOutcome::Imported { files: 4 }
        );

        // The pump role — the safety-relevant part — arrives verbatim.
        assert_eq!(
            read(&state.join("runtime.toml")),
            read(&legacy.join("runtime.toml"))
        );
        assert_eq!(
            read(&state.join("daemon_state.json")),
            format!(
                "{{\"active_profile_path\":\"{}/profiles/quiet.json\"}}",
                state.display()
            )
        );
        assert_eq!(
            read(&state.join("profiles/quiet.json")),
            "{\"name\":\"Quiet\"}"
        );
        // System paths move; a user's ~/.config path is kept, because the GUI
        // has not copied that directory yet when the daemon first starts.
        assert_eq!(
            read(&state.join("validation/s1.json")),
            "{\"search\":[\"/etc/ventulus/profiles\",\"/root/.config/control-ofc/profiles\"]}"
        );
        assert!(
            !state.join(LOCK_FILE_NAME).exists(),
            "the old lock is not copied"
        );
        assert!(state.join(IMPORT_MARKER).exists());
        assert!(!state.join(IMPORTING_MARKER).exists());
        // The old directory is read, never changed.
        assert!(legacy.join("runtime.toml").exists() && legacy.join(LOCK_FILE_NAME).exists());
    }

    #[test]
    fn runs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, state) = (tmp.path().join("old"), tmp.path().join("new"));
        legacy_tree(&legacy);
        assert!(matches!(
            import(&legacy, &state),
            ImportOutcome::Imported { .. }
        ));

        // A role the user removes after the upgrade must not come back.
        std::fs::remove_file(state.join("runtime.toml")).unwrap();
        assert_eq!(import(&legacy, &state), ImportOutcome::AlreadyImported);
        assert!(!state.join("runtime.toml").exists());
    }

    #[test]
    fn never_imports_into_a_state_dir_in_use() {
        for own in [DAEMON_STATE_FILE, RUNTIME_CONFIG_FILE] {
            let tmp = tempfile::tempdir().unwrap();
            let (legacy, state) = (tmp.path().join("old"), tmp.path().join("new"));
            legacy_tree(&legacy);
            write(&state.join(own), "mine");

            assert_eq!(import(&legacy, &state), ImportOutcome::StateInUse);
            assert_eq!(read(&state.join(own)), "mine");
            assert!(!state.join("profiles/quiet.json").exists());
            assert!(!state.join(IMPORT_MARKER).exists());
            assert!(!state.join(IMPORTING_MARKER).exists());
        }
    }

    #[test]
    fn no_legacy_dir_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("new");
        assert_eq!(
            import(&tmp.path().join("old"), &state),
            ImportOutcome::NoLegacyState
        );
        assert!(!state.exists());
    }

    /// An unreadable file under `validation/` must not cost the roles, and the
    /// import must still finish later although the state dir then holds both
    /// key files — one of them rewritten by the running daemon meanwhile.
    #[test]
    fn a_failed_file_still_lets_the_roles_in_and_the_import_resumes() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, state) = (tmp.path().join("old"), tmp.path().join("new"));
        legacy_tree(&legacy);
        let blocked = legacy.join("validation/s1.json");
        let set_mode =
            |mode| std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(mode));
        set_mode(0o000).unwrap();
        if std::fs::read(&blocked).is_ok() {
            set_mode(0o644).unwrap();
            eprintln!("skipped: running as root, an unreadable file cannot be modelled");
            return;
        }

        let failed = import(&legacy, &state);
        set_mode(0o644).unwrap();
        assert!(matches!(failed, ImportOutcome::Failed(_)), "{failed:?}");
        assert_eq!(
            read(&state.join("runtime.toml")),
            read(&legacy.join("runtime.toml"))
        );
        assert!(state.join("daemon_state.json").exists());
        assert!(state.join(IMPORTING_MARKER).exists());
        assert!(!state.join(IMPORT_MARKER).exists());

        // The daemon ran meanwhile and the user activated a profile.
        write(&state.join("daemon_state.json"), "mine");
        assert_eq!(
            import(&legacy, &state),
            ImportOutcome::Imported { files: 1 }
        );
        assert!(state.join("validation/s1.json").exists());
        assert_eq!(read(&state.join("daemon_state.json")), "mine");
        assert!(!state.join(IMPORTING_MARKER).exists());
    }

    /// Killed after the active profile landed but before the roles: resumed,
    /// not mistaken for a state directory in use.
    #[test]
    fn an_import_killed_part_way_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, state) = (tmp.path().join("old"), tmp.path().join("new"));
        legacy_tree(&legacy);
        write(&state.join(IMPORTING_MARKER), "importing");
        write(&state.join("daemon_state.json"), "{}");

        assert!(matches!(
            import(&legacy, &state),
            ImportOutcome::Imported { .. }
        ));
        assert_eq!(
            read(&state.join("runtime.toml")),
            read(&legacy.join("runtime.toml"))
        );
    }

    #[test]
    fn roles_left_behind_is_said_only_when_the_new_dir_lacks_them() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, state) = (tmp.path().join("old"), tmp.path().join("new"));
        legacy_tree(&legacy);
        write(&state.join("runtime.toml"), "[startup]\ndelay_secs = 0\n");
        assert!(roles_left_behind(&legacy, &state));
        write(
            &state.join("runtime.toml"),
            &read(&legacy.join("runtime.toml")),
        );
        assert!(!roles_left_behind(&legacy, &state));
    }
}
