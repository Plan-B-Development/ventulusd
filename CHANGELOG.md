# Changelog

Ventulus was released as Control-OFC (`control-ofc-daemon`) up to 4.3.0; this log starts with the
rename. Version numbers carry on from Control-OFC.

## [Unreleased]

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
