//! Whether this kernel loads unsigned modules (DEC-500).
//!
//! An out-of-tree Super-I/O driver is a module the distribution did not sign.
//! On a kernel that refuses unsigned modules, a DKMS build that is unsigned, or
//! signed with a key the firmware never enrolled, fails to load, and the fan
//! headers it would have brought stay missing. This module reads the three
//! facts that decide it, so readiness can name that cause instead of leaving
//! the user to guess.
//!
//! # What refuses an unsigned module
//!
//! `kernel/module/signing.c::module_sig_check` (read at v7.3): a module with no
//! signature, or one signed with a key the kernel does not hold, is refused
//! when `sig_enforce` is set (`-EKEYREJECTED`: modprobe's "Key was rejected by
//! service"), and otherwise exactly when lockdown restricts
//! `LOCKDOWN_MODULE_SIGNATURE` (`-EPERM`, "Operation not permitted", with
//! "Lockdown: modprobe: unsigned module loading is restricted" in the kernel
//! log). That reason sits below `LOCKDOWN_INTEGRITY_MAX`, so the `integrity` and
//! `confidentiality` levels both refuse.
//!
//! Secure Boot itself refuses nothing; distributions turn it into one of the
//! two (Debian, Ubuntu and Fedora kernels lock down, or set `sig_enforce`, when
//! booted with it; Arch kernels do neither). It is reported because it is the
//! setting the user can see and change, and because it is what the key has to
//! be enrolled into.
//!
//! # Reads
//!
//! Three small read-only files: the `SecureBoot` EFI variable (efivarfs), the
//! lockdown level (securityfs) and the `module.sig_enforce` parameter (sysfs).
//! No port I/O, no subprocess. All three mounts are present, read-only, in the
//! packaged unit's namespace (`ProtectKernelTunables=true`; measured against the
//! running daemon's `mountinfo`, 2026-10-19). A fact that cannot be read is
//! `None`, never a guess, and [`KernelSecurity::signed_modules_only`] answers
//! only from facts that were read.

use std::path::Path;

/// The production sysfs root every path below hangs off.
pub const SYS_ROOT: &str = "/sys";

/// The UEFI global-variable GUID that `SecureBoot` lives under.
const EFI_GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// The kernel lockdown level (`/sys/kernel/security/lockdown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lockdown {
    None,
    Integrity,
    Confidentiality,
}

impl Lockdown {
    /// The token the kernel prints for this level, which is also the wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Integrity => "integrity",
            Self::Confidentiality => "confidentiality",
        }
    }
}

/// The module-signing facts, each `None` where it could not be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KernelSecurity {
    /// Whether the firmware booted with Secure Boot on. `Some(false)` on a
    /// machine with no UEFI firmware (no Secure Boot to have), and where the
    /// firmware has no `SecureBoot` variable.
    pub secure_boot: Option<bool>,
    /// The lockdown level in force. `Some(None)` where securityfs is mounted
    /// but the lockdown LSM is not active (no `lockdown` file), which locks
    /// nothing down.
    pub lockdown: Option<Lockdown>,
    /// The `module.sig_enforce` parameter. `None` where it cannot be read,
    /// including a kernel built without module signing.
    pub module_sig_enforce: Option<bool>,
}

impl KernelSecurity {
    /// Read the running kernel's facts.
    pub fn read() -> Self {
        Self::read_from(Path::new(SYS_ROOT))
    }

    /// [`Self::read`] against another sysfs root, for tests.
    pub fn read_from(sys: &Path) -> Self {
        Self {
            secure_boot: read_secure_boot(sys),
            lockdown: read_lockdown(sys),
            module_sig_enforce: read_sig_enforce(sys),
        }
    }

    /// Whether every fact is unknown — the case a client is not told about.
    pub fn is_unknown(&self) -> bool {
        self.secure_boot.is_none() && self.lockdown.is_none() && self.module_sig_enforce.is_none()
    }

    /// Does this kernel refuse a module that is unsigned or signed with a key it
    /// does not hold?
    ///
    /// `Some(true)` when `sig_enforce` is set, or lockdown is at `integrity` or
    /// above on a kernel that checks signatures at all (the `sig_enforce`
    /// parameter exists only where module signing is built in, and without it
    /// lockdown has no signature to check). `Some(false)` only when both facts
    /// were read and neither refuses. Otherwise `None`: a client then says
    /// nothing about signing, as it did before these facts were read.
    pub fn signed_modules_only(&self) -> Option<bool> {
        let locked = matches!(
            self.lockdown,
            Some(Lockdown::Integrity | Lockdown::Confidentiality)
        );
        match (self.module_sig_enforce, self.lockdown) {
            (Some(true), _) => Some(true),
            (Some(false), Some(_)) => Some(locked),
            _ => None,
        }
    }
}

/// The `SecureBoot` variable's one data byte, after efivarfs's four attribute
/// bytes. With no `/sys/firmware/efi` the machine booted without UEFI, so
/// Secure Boot is off; with efivarfs mounted (it lists variables) but no
/// `SecureBoot` among them the firmware does not implement it. An empty or
/// unreadable efivars directory is `None`: the variable cannot be told apart
/// from an unmounted filesystem.
fn read_secure_boot(sys: &Path) -> Option<bool> {
    let efi = sys.join("firmware/efi");
    if !efi.exists() {
        return Some(false);
    }
    let efivars = efi.join("efivars");
    match std::fs::read(efivars.join(format!("SecureBoot-{EFI_GLOBAL_GUID}"))) {
        Ok(bytes) => match bytes.get(4) {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::read_dir(&efivars)
            .ok()
            .and_then(|mut entries| entries.next())
            .map(|_| false),
        Err(_) => None,
    }
}

/// The bracketed token of `none [integrity] confidentiality`. With securityfs
/// mounted (it lists entries) but no `lockdown` file, the LSM is not active.
fn read_lockdown(sys: &Path) -> Option<Lockdown> {
    let securityfs = sys.join("kernel/security");
    match std::fs::read_to_string(securityfs.join("lockdown")) {
        Ok(text) => text
            .split_whitespace()
            .find_map(|t| t.strip_prefix('[')?.strip_suffix(']'))
            .and_then(|level| match level {
                "none" => Some(Lockdown::None),
                "integrity" => Some(Lockdown::Integrity),
                "confidentiality" => Some(Lockdown::Confidentiality),
                _ => None,
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::read_dir(&securityfs)
            .ok()
            .and_then(|mut entries| entries.next())
            .map(|_| Lockdown::None),
        Err(_) => None,
    }
}

/// `Y` or `N` (a kernel `bool` parameter).
fn read_sig_enforce(sys: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(sys.join("module/module/parameters/sig_enforce")).ok()?;
    match text.trim() {
        "Y" | "y" | "1" => Some(true),
        "N" | "n" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake sysfs root. Each fact is written only when given.
    struct Tree {
        dir: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> &Self {
            let path = self.dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            self
        }

        fn dir(&self, rel: &str) -> &Self {
            std::fs::create_dir_all(self.dir.path().join(rel)).unwrap();
            self
        }

        fn secure_boot(&self, byte: u8) -> &Self {
            // Attributes NV|BS|RT (0x06), little-endian, then the data byte —
            // the bytes this host's own variable reads.
            self.write(
                &format!("firmware/efi/efivars/SecureBoot-{EFI_GLOBAL_GUID}"),
                &[0x06, 0, 0, 0, byte],
            )
        }

        fn read(&self) -> KernelSecurity {
            KernelSecurity::read_from(self.dir.path())
        }
    }

    #[test]
    fn this_hosts_files_read_as_secure_boot_off_lockdown_none_and_not_enforced() {
        let t = Tree::new();
        t.secure_boot(0)
            .write(
                "kernel/security/lockdown",
                b"[none] integrity confidentiality\n",
            )
            .write("module/module/parameters/sig_enforce", b"N\n");
        let ks = t.read();
        assert_eq!(ks.secure_boot, Some(false));
        assert_eq!(ks.lockdown, Some(Lockdown::None));
        assert_eq!(ks.module_sig_enforce, Some(false));
        assert_eq!(ks.signed_modules_only(), Some(false));
        assert!(!ks.is_unknown());
    }

    #[test]
    fn secure_boot_reads_the_data_byte_and_tells_no_uefi_from_an_unmounted_efivarfs() {
        let on = Tree::new();
        on.secure_boot(1);
        assert_eq!(on.read().secure_boot, Some(true));

        // No /sys/firmware/efi: a BIOS boot, which has no Secure Boot.
        assert_eq!(Tree::new().read().secure_boot, Some(false));

        // UEFI, efivarfs mounted (it lists a variable), no SecureBoot variable.
        let unsupported = Tree::new();
        unsupported.write(
            &format!("firmware/efi/efivars/SetupMode-{EFI_GLOBAL_GUID}"),
            &[0x06, 0, 0, 0, 0],
        );
        assert_eq!(unsupported.read().secure_boot, Some(false));

        // UEFI, efivars directory empty: not mounted, so unknown.
        let unmounted = Tree::new();
        unmounted.dir("firmware/efi/efivars");
        assert_eq!(unmounted.read().secure_boot, None);

        // A truncated variable is not read as either answer.
        let short = Tree::new();
        short.write(
            &format!("firmware/efi/efivars/SecureBoot-{EFI_GLOBAL_GUID}"),
            &[0x06, 0, 0, 0],
        );
        assert_eq!(short.read().secure_boot, None);
    }

    #[test]
    fn lockdown_reads_the_bracketed_level() {
        for (text, level) in [
            ("[none] integrity confidentiality\n", Lockdown::None),
            ("none [integrity] confidentiality\n", Lockdown::Integrity),
            (
                "none integrity [confidentiality]\n",
                Lockdown::Confidentiality,
            ),
        ] {
            let t = Tree::new();
            t.write("kernel/security/lockdown", text.as_bytes());
            assert_eq!(t.read().lockdown, Some(level), "{text}");
        }

        // securityfs mounted (another LSM's entry), lockdown LSM inactive.
        let inactive = Tree::new();
        inactive.dir("kernel/security/apparmor");
        assert_eq!(inactive.read().lockdown, Some(Lockdown::None));

        // securityfs not mounted: unknown.
        let unmounted = Tree::new();
        unmounted.dir("kernel/security");
        assert_eq!(unmounted.read().lockdown, None);

        let garbled = Tree::new();
        garbled.write("kernel/security/lockdown", b"none integrity\n");
        assert_eq!(garbled.read().lockdown, None);
    }

    #[test]
    fn sig_enforce_reads_the_bool_parameter() {
        for (text, value) in [("Y\n", Some(true)), ("N\n", Some(false)), ("?\n", None)] {
            let t = Tree::new();
            t.write("module/module/parameters/sig_enforce", text.as_bytes());
            assert_eq!(t.read().module_sig_enforce, value, "{text}");
        }
        assert_eq!(Tree::new().read().module_sig_enforce, None);
    }

    /// The decision is `module_sig_check`'s: enforcement, or lockdown at
    /// integrity or above where signing is built in. Every combination of the
    /// two facts, so a stuck or swapped arm fails.
    #[test]
    fn signed_modules_only_follows_module_sig_check() {
        let ks = |enforce: Option<bool>, lockdown: Option<Lockdown>| KernelSecurity {
            secure_boot: None,
            lockdown,
            module_sig_enforce: enforce,
        };
        let levels = [
            None,
            Some(Lockdown::None),
            Some(Lockdown::Integrity),
            Some(Lockdown::Confidentiality),
        ];
        for lockdown in levels {
            assert_eq!(
                ks(Some(true), lockdown).signed_modules_only(),
                Some(true),
                "enforcement refuses whatever the lockdown: {lockdown:?}"
            );
        }
        assert_eq!(
            ks(Some(false), Some(Lockdown::None)).signed_modules_only(),
            Some(false)
        );
        assert_eq!(
            ks(Some(false), Some(Lockdown::Integrity)).signed_modules_only(),
            Some(true)
        );
        assert_eq!(
            ks(Some(false), Some(Lockdown::Confidentiality)).signed_modules_only(),
            Some(true)
        );
        // An unread fact leaves the answer open, never "no".
        assert_eq!(ks(Some(false), None).signed_modules_only(), None);
        for lockdown in levels {
            assert_eq!(
                ks(None, lockdown).signed_modules_only(),
                None,
                "{lockdown:?}"
            );
        }
    }

    #[test]
    fn secure_boot_alone_decides_nothing() {
        // Arch: Secure Boot on, nothing enforced — unsigned modules load.
        let arch = KernelSecurity {
            secure_boot: Some(true),
            lockdown: Some(Lockdown::None),
            module_sig_enforce: Some(false),
        };
        assert_eq!(arch.signed_modules_only(), Some(false));
    }

    #[test]
    fn nothing_readable_is_unknown() {
        let t = Tree::new();
        t.dir("firmware/efi/efivars").dir("kernel/security");
        let ks = t.read();
        assert!(ks.is_unknown(), "{ks:?}");
        assert_eq!(ks.signed_modules_only(), None);
    }
}
