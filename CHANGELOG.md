# Changelog

Ventulus was released as Control-OFC (`control-ofc-daemon`) up to 4.3.0; this log starts with the
rename, keeping 4.3.0 as the last release under the old name.

## [Unreleased]

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

## [4.3.0] — 2026-10-09

Pairs with `control-ofc-gui` >= v2.23.0, the recommended capability floor; GUI 4.3.0 is the matching
release and the first to show memory temperatures as memory. No capability flag or endpoint changes;
`GET /inventory/hwmon` adds the `memory_temp` classification, and memory-module sensor ids change form
(old-form profile curves keep working).

### Added

- **Memory temperatures are recognised as memory** (DEC-491). `GET /inventory/hwmon` classifies a
  memory-module sensor as `memory_temp`: the DDR5 SPD hub (`spd5118`) at high confidence, and at medium
  the JEDEC DDR4 sensor (`jc42`, whose driver also serves standalone thermometers) and the board channels
  labelled for memory (nct6683 `DIMM n` / `PECI DIMM n`, nct6776–6792 `PCH_DIM0_TEMP`…, nct6793+
  `Agent0 Dimm0`…, Dell `SODIMM`). Their `kind` stays
  `mb_temp`, so nothing about fan control or the thermal ladder changes, and a memory sensor still counts
  as a board reading in the CPU plausibility check.
- **Memory-module sensor ids no longer change when the kernel renumbers i2c buses** (DEC-492). An
  `spd5118` or `jc42` sensor is now named by its SMBus controller, the adapter's port (`p<N>`) or mux
  channel (`ch<K>`), and its SPD address — `hwmon:spd5118:0000:00:14.0-p0-0051:temp1` instead of
  `hwmon:spd5118:21-0051:temp1`. Where the topology cannot be named unambiguously the bus-numbered id is
  kept (logged once). A profile curve saved against the other form keeps driving its fans: the engine
  follows it to the single module with the same chip, address and label in the other form, and skips the
  control as `sensor_unavailable` if any second module matches, even an unreadable (quarantined) one. `validate` no longer warns `UNKNOWN_SENSOR` for such a curve. No fan,
  header or other sensor id changes.

### Fixed

- **`PCH_DIM0_TEMP`…`PCH_DIM3_TEMP` were classified as the chipset.** The `pch` in the label matched the
  chipset rule; they are memory channels and now read `memory_temp`.
- **A switched-off DDR5 memory sensor no longer reports a frozen value.** With `temp1_enable` at 0 the
  `spd5118` driver keeps returning the last temperature with no error. The daemon now treats that sensor
  as unreadable: it is quarantined and listed in `unavailable_sensors` with the reason, and a curve bound
  to it is reported as not controlled (`sensor_unavailable`) instead of following a stuck number. It
  recovers by itself when the sensor is switched back on through the driver. The check reads the driver's
  own enable state, so a change made over raw SMBus, bypassing the driver, is not seen.
