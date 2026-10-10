//! Carry a Control-OFC tray autostart override across the Ventulus rename.
//!
//! A desktop disables a system autostart entry by writing a copy of it under
//! the same name in `~/.config/autostart/` with `Hidden=true` (KDE's Autostart
//! page does exactly that). The rename gave the entry a new name, so a user who
//! had turned `control-ofc-tray` off would see `ventulus-tray` come back. At the
//! first start after the upgrade this copies the override to the new name; if it
//! disabled the tray, that first start exits, and every later login honours the
//! copied override before the tray is ever launched.

use std::path::{Path, PathBuf};

const LEGACY_ENTRY: &str = "control-ofc-tray.desktop";
const ENTRY: &str = "ventulus-tray.desktop";

const RENAMES: &[(&str, &str)] = &[
    ("control-ofc-tray", "ventulus-tray"),
    ("control-ofc-gui", "ventulus"),
    ("control-ofc-symbolic", "ventulus-symbolic"),
    ("Control-OFC", "Ventulus"),
];

/// What [`carry_over`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Carry {
    /// No old override, or the new one already exists.
    Nothing,
    /// The old override was copied; `disabled` if it turned the tray off.
    Copied { disabled: bool },
}

/// `$XDG_CONFIG_HOME/autostart`, else `$HOME/.config/autostart`.
pub fn autostart_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart"))
}

/// Copy `dir/control-ofc-tray.desktop` to `dir/ventulus-tray.desktop` once.
pub fn carry_over(dir: &Path) -> std::io::Result<Carry> {
    let (legacy, entry) = (dir.join(LEGACY_ENTRY), dir.join(ENTRY));
    if !legacy.is_file() || entry.exists() {
        return Ok(Carry::Nothing);
    }
    let mut text = std::fs::read_to_string(&legacy)?;
    for (old, new) in RENAMES {
        text = text.replace(old, new);
    }
    std::fs::write(&entry, &text)?;
    Ok(Carry::Copied {
        disabled: disables(&text),
    })
}

/// The two keys desktops use to switch an autostart entry off.
fn disables(entry: &str) -> bool {
    entry.lines().map(str::trim).any(|line| {
        line.eq_ignore_ascii_case("Hidden=true")
            || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KDE_DISABLED: &str = "[Desktop Entry]\nType=Application\nName=Control-OFC Tray\n\
        Exec=control-ofc-tray\nTryExec=control-ofc-gui\nIcon=control-ofc-symbolic\nHidden=true\n";

    #[test]
    fn a_disabled_override_is_copied_renamed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LEGACY_ENTRY), KDE_DISABLED).unwrap();

        assert_eq!(
            carry_over(dir.path()).unwrap(),
            Carry::Copied { disabled: true }
        );
        let copied = std::fs::read_to_string(dir.path().join(ENTRY)).unwrap();
        assert!(copied.contains("Exec=ventulus-tray\n"));
        assert!(copied.contains("TryExec=ventulus\n"));
        assert!(copied.contains("Icon=ventulus-symbolic\n"));
        assert!(!copied.contains("control-ofc"));
        assert!(
            dir.path().join(LEGACY_ENTRY).exists(),
            "the old file is left alone"
        );
    }

    #[test]
    fn an_enabled_override_is_copied_and_not_reported_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let enabled = KDE_DISABLED.replace("Hidden=true\n", "X-GNOME-Autostart-enabled=true\n");
        std::fs::write(dir.path().join(LEGACY_ENTRY), enabled).unwrap();
        assert_eq!(
            carry_over(dir.path()).unwrap(),
            Carry::Copied { disabled: false }
        );
    }

    #[test]
    fn gnome_off_switch_counts_as_disabled() {
        assert!(disables("X-GNOME-Autostart-enabled=false"));
        assert!(!disables("Hidden=false"));
    }

    #[test]
    fn runs_once_and_never_overwrites_the_new_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LEGACY_ENTRY), KDE_DISABLED).unwrap();
        std::fs::write(dir.path().join(ENTRY), "mine").unwrap();
        assert_eq!(carry_over(dir.path()).unwrap(), Carry::Nothing);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(ENTRY)).unwrap(),
            "mine"
        );
    }

    #[test]
    fn no_old_override_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(carry_over(dir.path()).unwrap(), Carry::Nothing);
        assert!(!dir.path().join(ENTRY).exists());
    }
}
