# Changelog

Ventulus was released as Control-OFC (`control-ofc-daemon`) up to 4.3.0; this log starts with the
rename. Version numbers carry on from Control-OFC.

## [Unreleased]

### Added

- **Gigabyte fan headers and temperatures get their board names.** On a Gigabyte board running the
  out-of-tree it87 driver, the daemon reads the board's firmware layout (its SIV) and names the
  channels the driver leaves unlabelled from frankcrawford/it87's Gigabyte sensor configurations:
  `CPU_FAN`, `SYS_FAN5_PUMP`, `VRM MOS`. `GET /hwmon/headers` carries the name as `board_label`, and
  `GET /diagnostics/hardware` reports the layout and the name of every covered fan header, fan and
  temperature as `board_layout`. A driver's own label always wins, and no id changes.
- **A header the board catalogue names a pump is protected as one.** Its role is `pump` with
  `role_source: "board_catalogue"`: identify moves it instead of stopping it, `stop_permitted` is
  `false`, and a profile holds it at the pump floor (70 % on a DC header). A `CPU_FAN` or `CPU_OPT`
  name earns the 30 % floor. Like every pump rule, this only adds protection; a role you assign can
  add to it, never take it away.
- **The daemon says whether the kernel loads unsigned drivers.** `GET /diagnostics/hardware` reports
  `kernel_security`: Secure Boot, the lockdown level, `module.sig_enforce`, and whether the kernel
  refuses an unsigned module, read from sysfs, securityfs and efivarfs. Where it does and a chip
  needs an out-of-tree driver that is not loaded, the Super-I/O advice and the readiness item say
  so and how to enrol the key DKMS signed the driver with. The service sandbox is unchanged.
- **The board layout lists the chips it expects.** `board_layout.expected_chips` names the chips a
  Gigabyte layout says the board has (the bound primary where a layout has two), so a missing chip
  can be reported on boards the curated list does not include.

### Changed

- **IT8689E support follows the running kernel.** The kernel's own it87 drives the IT8689E from
  Linux 7.1, so on 7.1 or later the chip reads as mainline and its advice calls the out-of-tree
  driver optional, naming what it still adds: the board layout ID and the fixes for boards where
  fan writes have no effect. On older kernels (Debian 13, Ubuntu 26.04), or when the kernel version
  cannot be read, nothing changes.
- **The kernel's own it87 is named as the likely after-update cause.** Where the loaded it87 is the
  kernel's own and cannot drive the chip, the Super-I/O advice says the out-of-tree DKMS build
  probably did not follow a kernel update, with `dkms status` and `sudo dkms autoinstall`.

- **Board voltages: 39 inputs on nine layouts are now shown unnamed.** Where a second, independent
  reading of GIGABYTE's sensor tables disagrees with the it87 configuration, the input is now left
  unnamed rather than guessed. On the five X570S layouts the IT8689E's `in0`–`in6`, shown before as
  not mapped, are now plain unnamed channels. On four layouts (`3008090B`, `3108090B`, `6008090B`,
  `6108090B`) the IT8792E's `in4` loses its `Chipset Core 2.5V` name and divider. The catalogue
  moves to it87 commit `bd7a566`.

## [5.2.0] — 2026-10-16

Pairs with `ventulus` >= v2.23.0, the recommended capability floor; GUI 5.2.0 is the matching release.
Packages for Debian, Ubuntu and Fedora and a tarball, and driver hints named by upstream project. No API,
capability, curve, floor or thermal-safety change.

### Added

- **Packages for Debian 13, Ubuntu 26.04 and Fedora 44, and a tarball.** Each release now carries
  `ventulusd_X.Y.Z-1_amd64.deb`, `ventulusd-X.Y.Z-1.x86_64.rpm` and `ventulusd-X.Y.Z-x86_64.tar.gz`
  beside the Arch package, with a `SHA256SUMS` and a Sigstore provenance attestation for every file.
  They install the same files at the same paths as the Arch package. The Debian and Ubuntu package
  enables and starts the daemon on install, as Debian services do; the Fedora package and the tarball
  leave it for you to enable (`sudo systemctl enable --now ventulusd`). The tarball's `install.sh`
  upgrades an earlier tarball install and refuses to overwrite a packaged one; `sudo
  /usr/lib/ventulus/uninstall` removes it. Every format restarts a running daemon on upgrade and stops
  it on removal through systemd, so the fans are handed back as the daemon found them each time.
  Configuration you edited is kept.

### Changed

- **The module list says what each package manager does with it.** `/etc/modules-load.d/ventulus.conf`
  said pacman puts a deleted copy back. So do dnf and the tarball's installer; apt leaves it deleted.

- **Driver hints name the upstream project, not an Arch package.** The unbound-chip hint and the it87
  risk note say to build the driver from `frankcrawford/it87` or `Fred78290/nct6687d` with DKMS, where
  they named `it87-dkms-git` and `nct6687d-dkms-git`, which exist only in the AUR. The GUI adds the
  install steps for your distribution beside the hint.

- **The documentation covers every supported distribution.** The README, the User Guide and
  `man ventulusd` give the install, upgrade and removal commands for Arch, Debian, Ubuntu, Fedora
  and the tarball, say how each starts and restarts the service and what each keeps of your
  configuration when you remove it, and give the kernel-parameter step for GRUB on Debian, Ubuntu
  and Fedora.

## [5.1.0] — 2026-10-15

Pairs with `ventulus` >= v2.23.0, the recommended capability floor; GUI 5.1.0 is the matching release.
Board, GPU-naming and guidance updates. No API, capability, curve, floor or thermal-safety change.

### Changed

- **More Gigabyte two-chip boards are recognised.** The board table now lists the TRX50 AI TOP and
  TRX50 AERO D, the X870 / B850 AORUS ELITE X3D, X870E EAGLE (X3D) WIFI7, X870E AERO X3D (DARK) WOOD,
  X870 AORUS TACHYON ICE, X870E AORUS XTREME X3D AI TOP and X870 / B850 AORUS STEALTH (ICE). If their
  second ITE chip, which carries pump headers, is missing, the GUI warns, and the Super-I/O guard names
  the board. Only the TRX50 AI TOP has an owner's log; the others come from the it87 driver's board
  catalogue.
- **A missing ITE chip on an Intel Z890 / B860 board is diagnosed correctly.** `it87-dkms-git` builds
  from 2026-09-09 to 2026-10-01 can fail to load there ("Invalid argument") or bind no chip, a driver
  bug fixed upstream on 2026-10-02. The unbound-chip hint and the User Guide now say to update first,
  before the bridge-latch recovery. The User Guide also notes that current builds need kernel 6.4 or
  newer, and which ITE chips MMIO keeps clear of ACPI conflicts.
- **Intel Arc cards are named when the PCI ID database names them.** Besides the B580, the B570,
  Pro B50, B60, B65 and B70 and the A-series A770, A750, A580, A380, A310 and Pro A30M–A60 now
  show their model instead of "Intel D-GPU".
- **GPU guidance updated (checked 2026-10-15).** The daemon's messages, README, User Guide, man
  page and install notes recommend `amdgpu.ppfeaturemask=0xfff7ffff` (any mask with bit 14 still
  works) and explain the kernel's "Overdrive is enabled" taint message. The GPU fan test no longer
  names an SMU version mismatch as a cause of an ignored write, and no longer says resume hands
  the fan back. The NVIDIA notes say `nouveau` gives no sensors on Turing and newer, and that
  NVML's fan duty and RPM are the driver's intended speed, not a measurement.

## [5.0.0] — 2026-10-10

Pairs with `ventulus` >= v2.23.0, the recommended capability floor; GUI 5.0.0 is the matching release.
The first release under the new name. No API, capability, curve, floor or thermal-safety change.

### Changed

- **Control-OFC is now Ventulus.** The daemon is `ventulusd` (package, binary and `ventulusd.service`;
  was `control-ofc-daemon`), the tray is `ventulus-tray`, the hand-back helper is
  `ventulus-restore-auto`. Paths move with it: socket `/run/ventulus/ventulus.sock`, config
  `/etc/ventulus/`, state `/var/lib/ventulus/`, modules-load `/etc/modules-load.d/ventulus.conf`, Super-I/O
  guard `/usr/lib/ventulus/ventulus-superio-guard` (mask with an empty
  `/etc/modprobe.d/ventulus-superio.conf`), the `VENTULUS_CONFIG` environment variable. No API, curve,
  floor or thermal-safety behaviour changes.
- **Upgrading from Control-OFC carries the install across** (DEC-493). `ventulusd` replaces
  `control-ofc-daemon` on `pacman -Syu` (`replaces`/`conflicts`, and `provides control-ofc-daemon` so a 4.x
  GUI still installs). Edited configuration comes across from `/etc/control-ofc` (`daemon.toml`,
  profiles, `modules-load.d`, the Super-I/O guard's off switch, service drop-ins); the packaged defaults
  are kept as `.pacnew`. At its first start the daemon imports `/var/lib/control-ofc` — header roles
  (so a pump role keeps its 30 % floor), the active profile, stored profiles — once, never overwriting.
  If the old daemon was running, `ventulusd` is enabled and started at the end of the transaction, and
  a 4.x daemon still running is stopped and its fans handed back first. For the 5.x series
  `/run/control-ofc/control-ofc.sock` links to the new socket, so 4.x clients keep working, and the
  daemon refuses to start while something still serves on that old path. A tray switched off in
  autostart stays off. The old directories are left in place.
