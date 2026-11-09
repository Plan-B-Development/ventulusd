# Changelog

Ventulus was released as Control-OFC (`control-ofc-daemon`) up to 4.3.0; this log starts with the
rename. Version numbers carry on from Control-OFC.

## [Unreleased]

### Fixed

- **A fan diagnostic stopped because the CPU temperature was lost no longer leaves a fan low.** When
  the CPU sensor stops reporting, the daemon raises the active profile's fans to 40 %, and a running
  fan test (verify, characterisation, control-path discovery, stall probe or OpenFAN calibration)
  stops. The test then skipped putting its fan back, assuming the daemon was driving it, which was
  true only for a fan in the active profile; and when the daemon had taken the fan over, putting it
  back failed. A fan outside the profile was left at whatever duty the test had reached, possibly a
  stop, until the daemon exited. Now a motherboard fan with no automatic mode is raised once the test
  ends: to the exit floor, and to no less than 40 % while the CPU still cannot be seen, and an OpenFAN channel is put back no
  lower than 40 % (restarted first if the test stopped it). A fan in the profile, a motherboard fan
  with an automatic mode, and every fan during a temperature emergency are left to the daemon as
  before.
- **No local user can stop the thermal emergency by tying up the daemon's socket.** The socket is
  open to every local user, and the daemon used to accept connections without limit and keep idle
  ones open for ever. About a thousand idle connections used up its open-file limit, after which
  every motherboard fan write failed, the thermal emergency's included. The daemon now serves at
  most 64 connections at once (a further client waits until one closes), closes a connection that
  sends no complete request head within 10 s, and raises its own open-file limit to 65536 at start.
  Request bodies are limited to 64 KiB, except profile uploads (4 MiB) and firmware files (1 MiB).
  Each kind of upload is handled one at a time: another waits up to 4 s and then answers `503`
  (retryable), and a body that has not arrived 10 s after its turn answers `408`. The unit sets
  `OOMScoreAdjust=-900`. A client can still make the API slow; it can no longer use up the file
  handles the fan writes need.
- **A flood of requests can no longer hold up the daemon's fan writes.** The daemon's fan writes and
  its sensor reads run on the same pool of background threads as the work API requests start, and
  a client that sends a request and hangs up leaves that work running. A local client repeating
  that on a route that scans the hardware queued work faster than it drained; a fan write, the
  thermal emergency's included, then waited behind the queue indefinitely. Request work now uses
  at most 64 of those threads at once, so the fan writes and sensor reads always find one free.
  - A request that cannot get one of those 64 within 2 s while starting a motherboard fan
    diagnostic is refused with `503`, `details.reason: "request_pool_busy"` (retryable).
  - `/diagnostics/hardware` no longer hangs while a motherboard fan write has not returned. It
    answers after 2 s without the BIOS reclaim counts, and says so with
    `enable_revert_counts_unavailable`.
- **Upgrading from Control-OFC now warns when its header roles were left behind.** When the
  Ventulus state directory already held its own settings, the daemon does not import the
  Control-OFC one, and is meant to log a warning that pump roles assigned under Control-OFC are not
  in effect. The check looked for the roles in the wrong place in `runtime.toml`, so the warning
  never appeared and a pump role (and its 30 % floor) could be lost without notice. It now finds
  them.
- **Dependency updates.** tokio 1.54.1 fixes RUSTSEC-2026-0386 (unsound `AsyncFd` constructors, a
  part of tokio the daemon and tray do not use), and hyper 1.12.0 drops the unmaintained
  `pin-utils` (RUSTSEC-2026-0441).

## [5.5.0] — 2026-11-03

Pairs with `ventulus` >= v2.23.0; ventulus 6.0.0 is the matching GUI release, and its
`ventulus-diag` starts the tests this release now names in the journal.

### Added

- **The journal names every test that changes fan speeds, and who asked for it.** A request
  that starts a PWM verify, characterisation, control-path discovery, the stall probe, OpenFAN
  calibration, a GPU fan test, a validation session or a Super-I/O port probe is logged when it
  arrives, with the caller's user id and process id, and again with the daemon's answer.
  Before this, only the OpenFAN firmware update logged its start, so a test's fan changes could
  not be told apart from the profile's. `journalctl -u ventulusd | grep 'hardware diagnostic'`
  lists them.

### Fixed

- **A stop no longer leaves a GPU fan test's card at its test speed.** A GPU fan test waits six
  seconds before it restores the card, but a stop waits only three for requests to finish. A
  stop during the test gave the card back first, and then the test put its own setting back on
  top. A pre-RDNA3 card stayed in manual mode at 75 or 100 %; an RDNA3 or newer card got a fixed
  curve with zero-RPM idle off. Only the unit's `ExecStopPost` corrected it, and a daemon run by
  hand has none. The test now wakes when the stop begins and gives the card back at once, and
  from then on the daemon writes no GPU fan curve but a reset. A test started during a stop is
  refused with a retryable `503`.
- **A stop signal that arrives as the daemon finishes starting takes the normal stop.** The
  `SIGTERM` handler was installed just after the fan control loop started, and the `SIGINT` one
  only once the daemon began waiting for a stop. A signal in between killed the process without
  its hand-back. Both are now installed before the first fan write.
- **`ventulus-restore-auto` no longer hands back fans a running daemon controls.** A service
  start that fails before it checks for another daemon, for example with a broken
  `/etc/ventulus/daemon.toml`, exits with an ordinary error. `ExecStopPost` then replayed the
  hand-back records in `/run/ventulus`, which belonged to a daemon started by hand that was
  still running, and gave its headers back underneath it. The daemon that writes the records
  now holds a lock on `/run/ventulus/handback.lock` until its own hand-back has run, and the
  script leaves the records alone while anyone else holds it. A second daemon that cannot take
  the lock keeps no records rather than overwriting the first one's. Without `flock` (util-linux)
  the script replays the records as before.

The script stays as the backstop for exits no daemon code survives: a crash, `SIGKILL`, an OOM
kill, and a watchdog stop cut off after 10 s. No configuration or unit change and no new package
dependency; the `ExecStopPost` script gains the lock check above.

### Changed

- **Installing, removing and running the daemon are documented once, in the GUI manual's
  Administration section.** The User Guide is now the reference for configuration, the
  service unit, profiles and the API, the conditions the daemon reports and the safety rules,
  and links to that section for the rest; the README, `SECURITY.md`, the man page and the
  install messages point to it too. The User Guide and man page now say the missing-CPU-sensor
  floor starts after 5 seconds with no current reading (they said 5 poll cycles). On Arch,
  removing the package now also names a leftover OpenFAN firmware-write drop-in, and the
  example udev rule says which serial group to use on each distribution.


## [5.4.0] — 2026-10-26

Pairs with `ventulus` >= v2.23.0; ventulus 5.5.0 is the first GUI that shows the two new
`GET /fans` fields.

### Added

- **`GET /fans` reports a fan whose writes keep failing.** Every hwmon and OpenFAN entry
  carries `write_failing`, `true` once the daemon's last three or more writes to that fan have
  failed, the latest within 30 seconds. The thermal emergency's and the no-sensor floor's
  writes count as well as the profile's, so the flag stays up during an emergency. The fan is
  then not at the speed the daemon chose, and the emergency cannot raise it either. It clears
  when a write lands, and 30 seconds after the daemon stops writing the fan. Until now the failure was only in the
  journal (`hwmon write failed for …`). The error itself stays in the journal, because it
  names sysfs paths.
- **`GET /fans` reports a header that was already in manual mode when the daemon first took
  it.** Every hwmon entry carries `found_in_manual_mode`. Such a header is given back in manual
  mode at the duty it had, not to the BIOS, because the daemon gives back exactly what it
  found; the usual cause is another fan program still holding it. It is never cleared while
  the daemon runs, and a restart finds the header in manual mode again; a reboot clears it
  unless the board itself starts that header in manual mode.

Both fields are optional and additive: no capability flag, since a client that does not see
them has nothing to show. No curve, floor, hand-back or thermal-ladder change: the forced
writes report their outcome to the state the API reads, and write and log exactly as before.

### Fixed

- **The Super-I/O report's signing note now says that enrolling a key needs the shim boot
  loader.** It told you to enroll the DKMS key with `mokutil` without saying that the MOK
  Manager screen comes from shim, which Debian, Ubuntu and Fedora boot through with Secure Boot
  on. A key enrolled in the firmware with `sbctl` does not count for modules, so without shim
  the kernel loads only modules signed with a key built into it.

## [5.3.1] — 2026-10-22

Pairs with `ventulus` >= v2.23.0, the recommended capability floor; GUI 5.4.1 is the matching release
and drops the ACPI I/O port conflict card on its side too. `acpi_conflicts` stays on the wire, now
always empty; no capability flag, curve, floor or thermal-ladder change.

### Fixed

- **MSI and ASRock boards on the out-of-tree nct6687 or nct6686 driver no longer show fake RPM
  thresholds.** Builds of those drivers that publish `fanN_min` and `fanN_max` fill them with the
  slowest and fastest speeds seen since the module loaded, and the daemon passed them on as
  `rpm_min_threshold` and `rpm_max_threshold`, which the Hardware page shows as "RPM thresholds".
  The daemon now leaves both out for those two drivers, read from the driver actually bound to the
  chip. The in-kernel nct6683 driver, which uses the same chip names, keeps its real minimum.
- **No more false ACPI I/O-port conflict reports.** The daemon looked for them in `/proc/ioports`,
  where the kernel never lists the ACPI regions that cause a real conflict, so it could not see one
  and could report only harmless firmware ranges. The check is removed: `acpi_conflicts` in
  `GET /diagnostics/hardware` is now always empty (the field stays for older GUIs), the Super-I/O
  report no longer sends `acpi_conflict_drivers` or an ACPI risk note, and the
  `superio_acpi_conflict` readiness item is gone. A real conflict shows in the kernel log
  (`sudo dmesg | grep -i "conflicts with OpRegion"`), which the daemon's sandbox does not let it
  read.

## [5.3.0] — 2026-10-19

Pairs with `ventulus` >= v2.23.0, the recommended capability floor; GUI 5.4.0 is the matching release
and shows the board names, layout and driver-readiness detail below. The new fields are additive and
optional; no capability flag, curve or thermal-ladder change. Floor values are unchanged; a board
name can only add a floor to a header (below), never remove one.

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
- **Packages offer the driver tools, never install them.** The Fedora package now suggests
  `lm_sensors` and `dkms`, as the Debian package already did. The Arch `it87-dkms-git` optional
  dependency says which chips need it and what it adds where the kernel already drives the chip
  (IT87952E, and IT8689E from Linux 7.1). The tarball installer points to the GUI's Hardware page
  to check which driver your board needs. No package installs DKMS or kernel headers by default,
  and a test keeps it that way.
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
