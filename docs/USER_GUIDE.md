# User Guide

`ventulusd` is the fan control daemon for Ventulus. It drives:

- an **OpenFAN controller**, a USB fan controller with up to 10 channels;
- **motherboard fan headers**, and other fan controllers with a kernel hwmon
  driver, through `/sys/class/hwmon`;
- **AMD GPU fans**, through the firmware fan curve of an RX 7000 or RX 9000.

The daemon is the only thing that writes fan speeds. While a profile is active,
its profile engine evaluates the profile's fan curves once a second and writes
every fan itself, so the fans stay controlled when the GUI is closed or crashes
and across sleep. With no profile active it evaluates no curves: fans stay
wherever their hardware left them, and only the safety rules (§ Safety) act.

Clients use a local HTTP API to read temperatures and fan speeds and to ask for
things: activate a profile, take a short manual override, identify a fan, run a
diagnostic. The GUI (`ventulus`) and the tray (`ventulus-tray`) are clients, and
so can your scripts be (§ Scripting the daemon). No client writes a fan.

> **New to Ventulus?** The GUI manual walks through setup step by step. Start
> with the [Setup Checklist](https://github.com/Plan-B-Development/ventulus/blob/main/manual/setup-checklist.md);
> [Understanding Motherboard Fan Control](https://github.com/Plan-B-Development/ventulus/blob/main/manual/understanding-fan-control.md)
> explains the background, and [OpenFAN Controller](https://github.com/Plan-B-Development/ventulus/blob/main/manual/openfan-controller.md)
> covers that device.

## Supported hardware

| Device | Read | Write |
|---|---|---|
| CPU temperature (`k10temp`, `coretemp`) | Yes | — |
| GPU temperature (AMD `amdgpu`, Intel Arc `xe` / `i915`, NVIDIA `nouveau` or opt-in NVML) | Yes | — |
| Disk temperature (NVMe) | Yes | — |
| Motherboard temperatures (ITE, Nuvoton) | Yes | — |
| OpenFAN controller fans | Yes | Yes |
| Motherboard fan headers (hwmon) | Yes | Yes |
| AMD GPU fans, RX 7000 / RX 9000 | Yes | Yes, through the firmware (PMFW) fan curve |
| AMD GPU fans, RX 6000 and older | Yes | Fan test and reset only. No profile drives them, so the card's own curve stays in charge |
| Intel Arc GPU fans | RPM | No: the firmware manages them and the kernel exposes no PWM control |
| NVIDIA GPU fans | RPM, plus the firmware-measured duty through NVML | No: there is no kernel write path |
| ARCTIC Fan Controller (`arctic_fan`, Linux 7.2 and later) | Yes | Yes. Every write carries all ten channels, so the daemon first sets each channel reading 0 that it did not choose to 100 %. A channel your profile does not control therefore runs at full speed; control all ten to keep them quiet |
| Liquid coolers with an hwmon driver | Coolant temperature and pump RPM | Pump speed, fixed or on a curve, never below 30 %; the GUI's **Configure AIO** sets it up. A pump on a motherboard header is set up the same way once the header has the `pump` role (§ Identifying a fan) |

## Installation

Install the package from the signed Ventulus pacman repository: the GUI manual's
[Getting Started](https://github.com/Plan-B-Development/ventulus/blob/main/manual/getting-started.md#installation)
has the steps. A headless machine needs only `ventulusd`:

```bash
sudo pacman -Syu ventulusd
sudo systemctl enable --now ventulusd
```

Do not install by copying the binary and unit file in by hand. The unit runs
`/usr/bin/ventulusd` and, after every stop, `/usr/bin/ventulus-restore-auto`,
which gives each fan header back to what it was doing before the daemon took it.
The package also installs the sleep hook, the Super-I/O guard and
`/etc/modules-load.d/ventulus.conf`; a hand-copied binary gets none of these.
To build the package yourself or run your own build, see
[`daemon/README.md`](https://github.com/Plan-B-Development/ventulusd/blob/main/daemon/README.md).

## Hardware sensor modules

The daemon finds sensors and fan headers by scanning `/sys/class/hwmon/`. A
device appears there only once its kernel driver is loaded.

**Loaded for you.** The package installs `/etc/modules-load.d/ventulus.conf`,
which loads the common Super-I/O drivers at boot. The kernel cannot load them by
itself, because the chips sit on ISA ports:

| Module | Chipset | Common boards |
|--------|---------|---------------|
| `nct6775` | Nuvoton NCT6775–NCT6799 (incl. NCT5585D and the ASUS NCT6701D, which it reports as `nct6799`) | ASUS (AM4 500-series onward, Intel), ASRock, older MSI (AM4 300/400, the original X570 boards, Intel ≤ 300-series) |
| `it87` | ITE IT87xx/IT86xx: the in-kernel driver covers only older parts plus IT8689E (7.1+) and IT87952E (6.3+); IT8686E/IT8688E/IT8696E/IT8665E need `it87-dkms-git` | Gigabyte; ASUS AM4 300/400-series (IT8665E) |
| `w83627ehf` | Winbond W83627EHF/DHG | Older boards |
| `drivetemp` | SATA/SAS drive temperature | All SATA drives |

To stop loading one, comment out its line and run
`sudo systemctl restart systemd-modules-load` (`start` does nothing once the unit
has run) or reboot. Do not rename or delete the file: pacman restores a missing
one at the next upgrade, and keeps an edited one, delivering a new version as
`ventulus.conf.pacnew`.

CPU temperature modules (`coretemp` for Intel, `k10temp` for AMD) and SMBus
adapter modules (`i2c-i801`, `i2c-piix4`) load automatically through PCI/ACPI
matching.

**The Super-I/O guard.** On a Gigabyte board the package stops `nct6775` and
`w83627ehf` from loading, even though the file above lists them. Gigabyte boards
use ITE Super-I/O chips, which those Nuvoton/Winbond drivers can never bind, and
their probe writes a config-mode unlock that can latch the ITE bridge in front of
a board's second fan chip. That hides the chip until a reboot, and on some boards
until the machine is powered down at the wall. The guard covers every Gigabyte
board, and any board it lists whose firmware reports no vendor. It is
`/usr/lib/modprobe.d/ventulus-superio.conf`, with its helper
`/usr/lib/ventulus/ventulus-superio-guard`; on any other board it loads the module
unchanged. Each time it declines a module it says so in the journal:
`journalctl -b -t ventulus-superio-guard`. To turn it off, create an **empty**
`/etc/modprobe.d/ventulus-superio.conf`: the same name masks it. A file of any
other name does not reliably win, because modprobe reads every directory's files
in one name order and, for a module named twice, the first `install` line read
takes effect. The guard prevents the latch; it does not clear one already set.
For that, see the GUI manual's
[Hardware Troubleshooting](https://github.com/Plan-B-Development/ventulus/blob/main/manual/hardware-troubleshooting.md#some-of-my-fan-headers-are-missing--only-5-of-8-show-up).

**If your hardware is not detected,** check the GUI first: the **Hardware** page's
readiness checklist and Super-I/O section identify your board's chips and the
exact module or package needed, **without probing the hardware**. As a last
resort, install `lm_sensors` and run:

```bash
sudo sensors-detect
```

This probes for further sensor chips and saves what it finds. Probing is at your
own risk: it "can access chips in a way these chips do not like, causing problems
ranging from SMBus lockup to permanent hardware damage (a rare case, thankfully)"
([sensors-detect(8)](https://man.archlinux.org/man/extra/lm_sensors/sensors-detect.8.en)).
Accept the conservative defaults, and never run it on a Gigabyte board: it writes
a Super-I/O unlock to 0x4E that can wedge the ITE bridge in front of a second
chip, which then vanishes, sometimes until the machine is powered down at the
wall, because the bridge runs on standby power and survives a reboot. The guard
cannot stop it, because `sensors-detect` probes the ports itself rather than
loading a module. Then restart the daemon:

```bash
sudo systemctl restart ventulusd
```

**ACPI conflicts.** If a Super-I/O module fails to bind with `ACPI resource
conflict` or *Device or resource busy* in the kernel log, update the driver first:
current `it87-dkms-git` builds sidestep most conflicts through MMIO. For `it87`,
prefer the driver-local `options it87 ignore_resource_conflict=1` in
`/etc/modprobe.d/it87.conf`; the it87 documentation notes that the system-wide
`acpi_enforce_resources=lax` can cause boot failures on some systems. The Nuvoton
drivers (`nct6775`, `nct6687`) have no driver-local option, so for them
`acpi_enforce_resources=lax` on the kernel command line is the only kernel-side
remedy.

**Out-of-tree modules.** Newer boards need drivers that are not in the mainline
kernel, built with DKMS from the AUR: `it87-dkms-git` for newer ITE chips,
`nct6687d-dkms-git` for MSI NCT6687D boards and some ASRock NCT6686D boards. The
GUI manual's [Driver Setup](https://github.com/Plan-B-Development/ventulus/blob/main/manual/driver-setup.md)
covers installing, signing and verifying them. Two things to know:

- `it87-dkms-git` builds from 2026-09-09 (it87 v2.0) give Gigabyte chips longer
  names, such as `it8696_a008090a`. The daemon strips that suffix, so fan header,
  sensor and voltage ids, and the pump roles, fan names and profiles that use
  them, do not change.
- Never load `nct6687` with `force=1` on a board whose chip is an NCT679x.

### What the daemon detects, and what it deliberately doesn't

The daemon publishes a **read-only** view of your cooling hardware for the GUI
(`GET /inventory/hwmon` and `GET /inventory/hardware-readiness`):

- **Detected automatically:** CPU and motherboard temperature sensors, each
  classified (*CPU Tctl*, *VRM*, *chipset* and so on) with a confidence and a
  plain-English reason; a recommended default CPU sensor; every controllable PWM
  fan header; and **monitor-only fan tachometers**, fans whose RPM can be read but
  not controlled. From these it builds a readiness checklist that says what works,
  what is missing, what is read-only, and what to do about it.
- **Never automated:** the daemon never runs `sensors-detect`, never loads kernel
  modules, never edits your bootloader, initramfs or udev rules, and **never writes
  to a fan during discovery**. Discovery is safe to run at any time.
- **"Control unverified":** a writable PWM header only *appears* controllable until
  a fan-control test confirms that a write changes the fan's speed. Until then the
  readiness list marks it unverified.
- **Read-only fans:** some drivers expose a PWM channel read-only. Those fans can
  be monitored but not controlled, and the readiness list says so.
- **Reboot may be required:** loading a missing Super-I/O or DKMS driver usually
  needs a reboot or a module reload, and the readiness item says so.
- **GPUs are handled separately:** GPU fan discovery and control are not part of
  this hwmon path (§ GPU fan control).

## Configuration

Configuration is optional. The package installs `/etc/ventulus/daemon.toml` with
every key commented out, so the defaults apply until you uncomment one, and pacman
keeps your edits on upgrade. With no file at all the daemon also runs on defaults.

The file's location can be changed with `--config <path>` or `VENTULUS_CONFIG`
(the flag wins). Under systemd, set either in a drop-in
(`sudo systemctl edit ventulusd`), as § Loading a profile shows for
`OPENFAN_PROFILE`.

**Read this before editing.** Inside a section the file is strict: **an unknown or
misspelt key, a wrongly typed value or an out-of-range value makes the daemon exit
at startup**, and systemd restarts it on a back-off (3 s, then up to one start a
minute) until the file is fixed. For all that time nothing controls the fans and
the thermal emergency cannot run. `journalctl -u ventulusd` names the offending
key. An unknown top-level section or key is different: the daemon logs a warning
naming it and ignores it, so a section a newer release added does not stop an
older one starting. That also means **a misspelt section name such as `[saftey]`
is ignored, and that section's defaults apply**, as is a key written above its
section header. Check the journal for `ignoring unknown top-level section` after
an edit.

The keys, with their defaults:

```toml
[serial]
# port = "/dev/serial/by-id/usb-..."   # no default: auto-detect when unset
# timeout_ms = 500                      # 50 or more

[polling]
# poll_interval_ms = 1000   # 100 or more; slower than 6000 is clamped to 6000

[ipc]
# socket_path = "/run/ventulus/ventulus.sock"

[state]
# state_dir = "/var/lib/ventulus"

[startup]
# delay_secs = 0            # 0-30
# record_startup = false    # record a short validation session at every start.
#   daemon.toml only: runtime.toml's [startup] takes delay_secs alone and rejects anything else

[profiles]
# search_dirs = ["/etc/ventulus/profiles", "/root/.config/ventulus/profiles"]
#   The default shown is what the service gets (it sets HOME=/root); see
#   § Profile search directories. The daemon's own store is always searched first.

[detection]
# allow_port_probe = false          # also needs a systemd drop-in
# enable_nvidia_telemetry = false   # also needs a systemd drop-in

[shutdown]
# exit_floor_pct = 50       # 0-100; the exit minimum for fans with no firmware fallback (0 = off)

[safety]
# coolant_limit_c = 60      # 40-70 whole C; a coolant sensor at or above it forces every fan to 100 %
```

After an edit, `sudo systemctl reload ventulusd` applies the profile search
directories, the exit minimum and the coolant limit at once; every other key
needs `sudo systemctl restart ventulusd`. A reload that finds the file invalid
logs the error and keeps the running values.

**Moving `state_dir` or `socket_path` needs a drop-in as well.** The service runs
with `ProtectSystem=strict`, so outside its private `/tmp` it can write files
only under `/run/ventulus`, `/var/lib/ventulus` and `/sys/devices`. Add the new
directory with `ReadWritePaths=` in `sudo systemctl edit ventulusd`, or the
daemon cannot create its socket or save its state there. Put the socket in a
directory only root can write: the daemon refuses to start (exit 1) if another
user can write the directory it goes in, sticky bit or not, owns any directory or
symlink on the way to it, or can write a directory further up that is not sticky
(`/tmp`-style `1777` is fine there), because they could put a socket there first
and keep the daemon from starting.

**Before installing an older release,** comment out any key you uncommented that
the older daemon does not know. A key a newer release added inside a section the
older one knows stops it starting; only whole unknown sections are tolerated. If
you forget, the journal names the key. Settings made from the GUI live in
`runtime.toml` and never stop an older daemon starting.

### Changing settings without editing the file

Most keys can also be set through the API, which is what the GUI's
**Settings ▸ Daemon Configuration** card does. `GET /config` reports every key
with its value on disk (`value`), the value the daemon is running with
(`running_value`), where it came from (`source`: `runtime`, `admin` or
`default`), whether the API can change it (`mutable`), whether a change waits for
a restart (`requires_restart`), and whether one is waiting now
(`restart_pending`).

| Key | API | Notes |
|---|---|---|
| `profiles.search_dirs` | `POST /config/profile-search-dirs` | Add and remove directories (§ Profile search directories). Applies at once |
| `startup.delay_secs` | `POST /config/startup-delay` | 0–30. After a restart |
| `polling.poll_interval_ms` | `POST /config/poll-interval` | 250–2000 through the API, after a restart. The cap bounds how stale a temperature the thermal emergency can act on |
| `serial.port` | `POST /config/serial-port` | A `/dev/ttyS*`, `ttyUSB*`, `ttyACM*`, `ttyAMA*` or `/dev/serial/` path of up to 256 characters, or `null` to auto-detect. After a restart. The service itself can open only `ttyACM` and `ttyUSB` nodes (§ Serial permissions). A port that does not answer as an OpenFAN controller falls back to auto-detection |
| `serial.timeout_ms` | `POST /config/serial-timeout` | 50–1000 through the API, after a restart |
| `detection.allow_port_probe` | `POST /config/allow-port-probe` | Also needs the `superio-port-probe.conf.example` drop-in |
| `detection.enable_nvidia_telemetry` | `POST /config/nvidia-telemetry` | Also needs the `nvidia-telemetry.conf.example` drop-in |
| `shutdown.exit_floor_pct` | `POST /config/exit-floor` | 0–100. Applies at once |
| `safety.coolant_limit_c` | `POST /config/coolant-limit` | 40–70. Applies at once; there is no off switch |
| `ipc.socket_path`, `state.state_dir` | Read-only | A bad socket path would lock every client out; moving the state directory would orphan `runtime.toml` and the profile store |

The two `[detection]` example drop-ins ship in `/usr/share/doc/ventulusd/`; the
flag alone does not turn either feature on.

### Runtime configuration

Configuration is split between two files (see
[`docs/ADRs/002-runtime-config-split.md`](ADRs/002-runtime-config-split.md)):

- **`/etc/ventulus/daemon.toml`** is yours. The daemon never writes it.
- **`/var/lib/ventulus/runtime.toml`** is the daemon's, written with 0600
  permissions through an atomic rename. It holds everything set through the API:
  - **the fan header roles you assign** (`[hardware] header_roles`,
    `POST /config/header-role`). On a board whose Super-I/O publishes no fan
    labels, a `pump` assignment here is the **only** evidence that a header drives
    a pump, so it is what gives that header the 30 % floor and keeps fan identify
    from stopping it;
  - the cooling devices (`[[cooling_devices]]`, `POST /config/cooling-device`);
  - the preferred CPU and motherboard temperature sensors and the exit minimum;
  - any key from the table above changed through the API.

  **Do not delete this file or edit it by hand.** Back it up with `daemon.toml`:
  a restore that copies only `daemon.toml` loses your pump roles.

At startup the daemon loads `daemon.toml`, then overlays `runtime.toml` (runtime
values win). If a runtime value is shadowing an edit you made to `daemon.toml`,
the daemon says so once at startup in an `info` log, and `GET /config` reports
`source: "runtime"` for that key.

#### When `runtime.toml` cannot be read

The daemon still starts, because a damaged settings file must never leave the
fans with no controller, but it runs on **defaults, with no header roles**: a pump
you assigned by hand has no 30 % floor and can be stopped by fan identify. It
says so on `GET /status` and `GET /poll` as
`runtime_config_degraded = {reason, path, detail, phase, kept_as?}`, and the GUI
shows a banner. `phase` says what the failure cost:

| `phase` | What happened | What to do |
|---|---|---|
| `startup` | The file could not be read at start. Every setting in it, header roles included, is **not in effect**. | Repair the file (or restore it from a backup), then restart the daemon. Saving a setting does not bring the old ones back. If the record also carries `kept_as`, a setting was saved since: the file at `path` is now a new one with no roles, and your old settings are only in the `kept_as` copy. Stop the daemon, repair that copy and move it back over `path`, then start it. |
| `reload` | A `SIGHUP` reload could not read it. Nothing changes: header roles, the profile search directories, the exit minimum and the coolant limit keep the values the daemon was running with, and an edit to them in either file waits for a reload that can read it. | Repair the file, then reload or restart. |
| `update` | A setting was saved while the file could not be read. The daemon kept the original as `runtime.toml.invalid-<unix-time>` beside it and replaced it with a new file carrying **the header roles and cooling devices it is running with**, plus the new setting. | Copy any other setting you need back from the `.invalid-` copy, then restart the daemon. |

`reason` is `unreadable` (an I/O error, or a file over 4 MiB) or `malformed` (read,
but not valid for this daemon version). The daemon's full error is in
`journalctl -u ventulusd`. A **missing** file is not a failure: that is a first boot.

#### Startup delay

The daemon can wait before it starts looking for devices, for hardware that is
slow to appear after boot. Set it through the API (it is saved to `runtime.toml`
and takes effect at the next start) or as `[startup] delay_secs`; the limit is 30
seconds:

```bash
curl --unix-socket /run/ventulus/ventulus.sock \
  -X POST -H "Content-Type: application/json" \
  -d '{"delay_secs": 3}' \
  http://localhost/config/startup-delay | jq .
```

## Running the service

```bash
sudo systemctl status ventulusd
journalctl -u ventulusd -f
curl --unix-socket /run/ventulus/ventulus.sock http://localhost/status | jq .
```

The package's systemd unit supervises the daemon so that fan control does not
quietly stop. The unit file's own comments
(`/usr/lib/systemd/system/ventulusd.service`) explain each value.

- **Start.** The unit is `Type=notify`: `systemctl start` returns only once the
  profile engine is ticking and the API is answering. `TimeoutStartSec=120` is
  set in the unit because some distributions default to 15 s; the boot-time
  serial probe extends it for each device it tries, so the probe does not count
  against those 120 s.
- **Watchdog.** If the control loop stops completing its once-a-second tick for
  15 s (`WatchdogSec=15`), a deadlock say, systemd sends `SIGTERM`, so the normal
  stop runs (exit minimum, then every header and GPU fan given back). It sends
  `SIGKILL` if that has not finished within 10 s (`TimeoutAbortSec=10`), runs
  `ventulus-restore-auto`, and restarts the daemon. The journal shows
  `Watchdog timeout`. A slow or wedged *device* does not trip it: the loop keeps
  ticking past a write that has not returned.
- **Restarts.** A crash, a non-zero exit or a watchdog timeout restarts the
  daemon after 3 s, then about 5.5, 10, 18 and 33 s, then once a minute for as
  long as it keeps failing. There is no start limit, so it never gives up and
  never needs `systemctl reset-failed`. After five minutes of normal running the
  daemon resets the back-off, so a later, unrelated fault restarts fast again
  (systemd 258 or newer). **The back-off needs systemd 254 or newer.** Older
  releases (Debian 12 ships 252) ignore `RestartSteps=` and `RestartMaxDelaySec=`
  with a warning, and restart every 3 s.
- **Stop.** A stop is allowed 40 s (`TimeoutStopSec=40`); the daemon bounds each
  hardware step itself so that it can always exit. After every stop, whatever
  ended it, `ExecStopPost` runs `ventulus-restore-auto`, which gives back each
  header and GPU fan the daemon's records name (§ Safety). The one exception is a
  start refused because another daemon is running: those records belong to the
  daemon that is running.
- **Reload.** `systemctl reload` sends `SIGHUP`, which applies the profile search
  directories, the exit minimum and the coolant limit (§ Configuration).
- **Sleep.** The package installs a system-sleep hook,
  `/usr/lib/systemd/system-sleep/ventulusd`, because the watchdog keeps counting
  while user space is frozen for a suspend and resume. Before a sleep it sends
  the daemon `SIGUSR1`, which widens the watchdog to 120 s, and waits up to 2 s
  for the daemon to acknowledge; after the resume it sends `SIGUSR2`, which puts
  the 15 s back. The daemon does that itself after 120 s if no resume signal
  arrives. The handshake uses `/run/ventulus/sleep-hook.pid` and
  `/run/ventulus/sleep-hook.ack`. In the journal a normal sleep shows
  `system sleep: systemd watchdog widened for the suspend and resume` and then
  `system sleep: systemd watchdog restored`. The hook logs
  `ventulus: the daemon could not widen its watchdog for this sleep` or
  `ventulus: the daemon did not acknowledge the sleep within 2 s` when that went
  wrong, and a slow resume may then restart the daemon.
- **After a resume or an OpenFAN controller reconnect,** the daemon cannot tell
  whether each OpenFAN channel it had written still runs at that speed. The
  controller keeps its speeds when only USB drops, but restarts every fan at
  about 1000 rpm when its 12 V supply went off. So the daemon remembers the
  speed each channel had and writes it again: on the next tick for a channel
  whose control cannot run (its curve writes it otherwise), and wherever the
  daemon decides what to leave a fan at. The no-CPU-sensor fallback (§ Safety)
  holds it at no less than that speed; a clean stop, a profile that stops naming
  it and the end of a thermal emergency leave it there (or at the exit minimum,
  whichever is higher); and the end of a calibration restores it exactly. A
  channel whose speed was already unknown before the loss stays unknown: a stop
  leaves it at 100 %, and the fallback gives it 40 %.

## The system tray

The package installs a second program, `ventulus-tray`, alongside the daemon.
It puts a Ventulus icon in the KDE Plasma system tray and starts automatically
when you log in.

- **Left click** opens the GUI.
- **Right click** shows the running daemon's version, the list of profiles with
  the active one marked, an action to stop profile control, and an action to open
  the GUI.
- If the daemon reports a thermal event, the menu says so. Switching to a quieter
  profile during one will not quieten the fans: the forced duties are floors over
  whatever the profile asks for.

It is purely a convenience. It never controls fans itself, it asks the daemon
exactly as the GUI does, and fan control does not depend on it. If you close it,
or it never starts, nothing about cooling changes.

It starts only if `ventulus` is installed, so a headless install never runs it.
To turn it off permanently, use **System Settings → Autostart**. To start, stop
or read its logs by hand:

```bash
systemctl --user status  'app-ventulus\x2dtray@autostart.service'
systemctl --user restart 'app-ventulus\x2dtray@autostart.service'
journalctl --user -u     'app-ventulus\x2dtray@autostart.service'
```

Full detail: `man ventulus-tray`.

## Fan profiles

A profile names groups of fans (controls), the curve each follows, and the
sensor each curve reads. Profiles use the **v7** schema. The GUI writes and
upgrades them; the daemon reads them forward-compatibly, accepting fields it does
not know and defaulting missing ones, so a profile from a newer GUI still loads.
It logs a warning only for profiles older than v3. An example ships at
`/etc/ventulus/profiles/quiet.json`. **It controls no fan as shipped**: its one
control has an empty `members` list, so activating it unchanged drives nothing.
The GUI's three starter profiles (Quiet, Balanced, Performance) are the same: one
*All Fans* control with no members and a curve with no sensor. Copy one, add your
fans and a sensor, then activate it.

**Slow-down band.** When a temperature falls, the engine holds a curve's output
until the reading is a set distance below where the output last changed, so fans
do not step down at every small dip. The distance is 2 °C unless a `graph`,
`stepped` or `linear` curve sets its own `hysteresis_c` (0–10 °C; 0 turns the band
off). A rise always re-evaluates at once, a reading that stays inside the band is
released after about 30 s, and a hold never keeps a fan slower than the curve asks
for at the current temperature. `POST`/`PUT /profiles` reject a value outside the
range, and so does loading one from disk. `GET /capabilities` advertises the field
as `control.curve_hysteresis`.

### Loading a profile

Activate a profile through the API (the GUI and the tray do this for you); the
choice is saved and used at the next start too:

```bash
curl --unix-socket /run/ventulus/ventulus.sock \
  -X POST -H "Content-Type: application/json" \
  -d '{"profile_id": "quiet"}' \
  http://localhost/profile/activate | jq .

# Which profile is active?
curl --unix-socket /run/ventulus/ventulus.sock \
  http://localhost/profile/active | jq .
```

At startup the daemon uses the first of these that loads:

1. `--profile <name>` or `--profile-file <path>`, whichever comes first on the
   command line.
2. `OPENFAN_PROFILE`.
3. The profile last activated through the API (the GUI, the tray, or
   `POST /profile/activate`), saved in `/var/lib/ventulus/daemon_state.json`.

A source that names no file, or a file that will not load, is logged and the next
one is tried, so a mistyped `--profile` falls back to your saved profile instead of
starting with none. `<name>` is the **file name without `.json`** (`quiet` for
`quiet.json`) in a profile search directory, not the name the GUI shows, which can
differ. With none of the three, no curve runs until a profile is activated; the
safety rules still act.

`--profile` and `OPENFAN_PROFILE` are **not saved**. While either is set it wins on
every start, including every restart the watchdog or a crash triggers, over a
profile you activated from the GUI; remove it and the next start uses the saved
profile again. Activating a profile from the GUI while the daemon runs still takes
effect at once and is saved.

Under systemd, set either in a drop-in. The environment variable is the simpler one:

```bash
sudo systemctl edit ventulusd
# add:
#   [Service]
#   Environment=OPENFAN_PROFILE=quiet
sudo systemctl restart ventulusd
```

To pass `--profile` instead, the drop-in must clear the unit's command line first
(`ExecStart=` on its own line, then `ExecStart=/usr/bin/ventulusd --profile quiet`).
Do not run a second `ventulusd` by hand for this: it refuses to start while the
service is running.

`POST /profile/deactivate` clears the active profile. It is idempotent, and the
cleared state is saved, so a restart does not bring the profile back. Activating or
deactivating clears every manual override. On the next tick each motherboard header
and AMD GPU fan the old profile drove and the new one does not name is given back
(§ Safety); an OpenFAN channel, or a header with no automatic mode, is left at its
last duty or the exit minimum, whichever is higher, or 100 % if its last duty is
unknown.

### Profile storage

The daemon is the profile **store of record**: stored profiles live under
`/var/lib/ventulus/profiles/`, and the full document is served and edited over the
API. The GUI uses this surface; scripts can too.

```bash
SOCK="/run/ventulus/ventulus.sock"

# List stored profiles (id / name / description summaries only)
curl -s --unix-socket $SOCK http://localhost/profiles | jq .

# Fetch one full profile document
curl -s --unix-socket $SOCK http://localhost/profiles/quiet | jq .

# Create a profile from a local JSON file
curl -s --unix-socket $SOCK \
  -X POST -H "Content-Type: application/json" \
  --data @my-profile.json \
  http://localhost/profiles | jq .

# Validate a document without storing it (?validate_only=true)
curl -s --unix-socket $SOCK \
  -X POST -H "Content-Type: application/json" \
  --data @my-profile.json \
  'http://localhost/profiles?validate_only=true' | jq .

# Replace a stored profile (re-activate afterwards to apply; there is no hot reload)
curl -s --unix-socket $SOCK \
  -X PUT -H "Content-Type: application/json" \
  --data @my-profile.json \
  http://localhost/profiles/my-profile | jq .

# Delete a stored profile (fails 409 profile_in_use if it is active)
curl -s --unix-socket $SOCK \
  -X DELETE http://localhost/profiles/my-profile | jq .
```

Profile ids are filesystem-safe stems: non-empty, at most 128 bytes, with no `/`,
`\`, `..` or control characters. Validation returns hard `errors`, which reject the
profile, and soft `warnings`, which are accepted. An unknown `sensor_id` is a
warning, not an error, so profiles stay portable across machines.

### Profile search directories

The daemon searches for profiles in this order:

1. `/var/lib/ventulus/profiles`, the daemon's own store, always first;
2. `/etc/ventulus/profiles` (always included);
3. `$XDG_CONFIG_HOME/ventulus/profiles` if that is set, else
   `$HOME/.config/ventulus/profiles`, which is `/root/.config/ventulus/profiles`
   under the service because it sets `HOME=/root`.

Items 2 and 3 are the default for `[profiles] search_dirs` in `daemon.toml`;
setting that key replaces them, and the store is still searched first. More
directories can be registered at runtime through the API; the GUI registers its own
profile folder this way every time it connects:

```bash
curl --unix-socket /run/ventulus/ventulus.sock \
  -X POST -H "Content-Type: application/json" \
  -d '{"add": ["/home/user/.config/ventulus/profiles"]}' \
  http://localhost/config/profile-search-dirs | jq .
```

A stale directory can be removed the same way, and the two combine into a single
atomic move, with removals applied first:

```bash
curl --unix-socket /run/ventulus/ventulus.sock \
  -X POST -H "Content-Type: application/json" \
  -d '{"add": ["/home/user/profiles-new"], "remove": ["/home/user/profiles-old"]}' \
  http://localhost/config/profile-search-dirs | jq .
```

`/etc/ventulus/profiles` cannot be removed, and neither can the last remaining
entry: activation resolves against this list, so an empty one would leave the
daemon unable to find any profile. Both are `400 validation_error`. A non-root
caller may only add or remove directories under its own home; removal does
**not** need the directory to still exist, since a stale entry usually no longer
does.

A directory is stored by its **real path** (symlinks resolved), and different
spellings of one directory (`/./`, `//`, a trailing `/`) count as one entry. The
list holds at most 32 directories, at most 4 of them inside one user's home, each
path at most 4096 bytes. When reading profiles the daemon skips, with a warning in
the journal:

- a directory whose real path is no longer the path registered, such as one
  replaced by a symlink after it was added (add it again to store its real path;
  the GUI does this for its own folder every time it connects);
- a profile file that is a symlink, a FIFO or anything else but a regular file, or
  whose owner is not the directory's owner;
- a user's directory that has stopped answering (a hung network or FUSE mount),
  until a read of it returns. The request that first meets it gets a retryable
  error after 2 s; root can always remove the entry. Root's directories (the store
  and the presets) are never skipped: a request that cannot read them in time gets
  the retryable error instead.

So a profile directory cannot be used to read files the caller could not read
themselves.

### Who writes the fans

While a profile is active the profile engine is the only writer of every fan the
daemon drives. It cannot stop *another program* writing the same fan (fancontrol,
CoolerControl, a vendor tool): when a duty it set moves, it writes it again, and
after three corrections that do not hold it stops and flags the header
`duty_not_holding` (§ Troubleshooting). A manual override replaces the curve for
the control it targets until it is released or expires; everything else keeps
following its curve. The safety rules take priority over both, as **floors**
rather than replacements: each fan receives `max(commanded, forced)`. A fan no
control commands receives the forced duty only in the 100 % emergency; the 40 %
no-CPU-sensor floor reaches the active profile's fans alone (§ Safety).

## Setting fan speeds

The daemon, not the client, sets fan speeds. There is no endpoint that writes a
fan directly. To change what the fans do you have two levers:

- **Persistent:** author a profile (the GUI is the easiest way) and activate it
  (§ Fan profiles).
- **Temporary:** pin one control to a fixed speed with a **manual override**, an
  expiring overlay on the active profile.

### Manual override (temporary, per control)

A manual override pins every fan in one of the active profile's controls to a
fixed duty. It **reverts to the curve** if you stop renewing it, on the daemon's
clock, and a superseded token cannot pin it again. The duty is still raised to the
pump/CPU floor (30 %, or 70 % for a pump on a DC-mode header), and the safety rules
still win.

```bash
SOCK="/run/ventulus/ventulus.sock"

# 1. Take an override on a control (control_id comes from the active profile).
#    Returns an override_token plus the TTL (15 s) and advised renew interval (~5 s).
TOKEN=$(curl -s --unix-socket $SOCK \
  -X POST -H "Content-Type: application/json" \
  -d '{"pwm_percent": 60}' \
  http://localhost/control/cpu_fans/override | jq -r .override_token)

# 2. Renew before the TTL lapses (repeat roughly every 5 s to hold it).
curl -s --unix-socket $SOCK \
  -X POST -H "Content-Type: application/json" \
  -d "{\"override_token\": $TOKEN}" \
  http://localhost/control/cpu_fans/override/renew | jq .

# 3. Release when done (reverts to the curve immediately).
curl -s --unix-socket $SOCK \
  -X DELETE -H "Content-Type: application/json" \
  -d "{\"override_token\": $TOKEN}" \
  http://localhost/control/cpu_fans/override | jq .
```

`ttl_secs` in the first request is clamped to 1–15 (default 15). Active overrides
also appear in `GET /status` (`overrides[]` of
`{control_id, pwm_percent, expires_in_secs}`), but those entries carry no token, so
only the client that created an override can renew or release it.

### Identifying a fan

To find which physical fan is which, the identify API changes a single fan briefly
so you can spot the one that responded. It restores the fan by itself when its
timer runs out, so a crashed client can never leave a fan held.

**A pump is never stopped.** You always send `action: "stop"`, and the daemon
decides what that means from the header's role. An ordinary fan is driven to 0. A
header the daemon holds as a pump, because its role or its own label says so or
the active profile's name for it contains `pump` or `aio`, is moved instead: about
25 points clear of its current duty, upward wherever there is room, and never
below its pump floor. The response's `mode` field says which happened (`"stop"` or
`"pump_perturb"`), with `identify_pwm_percent` and `baseline_pwm_percent`.

If your pump is on a header the daemon cannot classify (common on boards whose
Super-I/O publishes no fan labels, where every header reads `role: "unknown"`),
tell it first:

```bash
curl -s --unix-socket /run/ventulus/ventulus.sock \
  -X POST -H 'Content-Type: application/json' \
  -d '{"header_id":"hwmon:it8696:it87.2624:pwm5:pwm5","role":"pump"}' \
  http://localhost/config/header-role
```

That assignment is saved in `runtime.toml`, takes effect at once, and also earns
the header the 30 % pump floor and the stop-snap exemption. Assigning `pump` to a
header an identify is holding stopped releases that stop. The roles are `pump`,
`cpu_fan`, `radiator_fan`, `chassis_fan`, `no_fan` (nothing plugged in) and
`unknown`; `null` clears an assignment. An OpenFAN channel (`openfan:ch00` to
`openfan:ch09`) takes a role too, and `pump` is the only way the daemon can tell
that a channel drives a pump.

`cpu_fan` gives a header the same 30 % floor and stop-snap exemption in the active
profile, as a `CPU_FAN` label does, but it is not a pump: identify still stops it,
and `stop_permitted` stays `true`.

```bash
SOCK="/run/ventulus/ventulus.sock"

# Stop one fan (fan_id from GET /fans). It is restored when the timer runs out.
curl -s --unix-socket $SOCK \
  -X POST -H "Content-Type: application/json" \
  -d '{"action": "stop"}' \
  http://localhost/fans/amd_gpu:0000:03:00.0/identify | jq .

# Restore it now (the engine resumes the fan's curve value).
curl -s --unix-socket $SOCK \
  -X POST -H "Content-Type: application/json" \
  -d '{"action": "restore"}' \
  http://localhost/fans/amd_gpu:0000:03:00.0/identify | jq .
```

## Serial device setup (OpenFanController)

**The OpenFAN controller is optional.** If you do not have one, nothing here
applies and nothing is wrong: the daemon notes once at `info` that it found none,
and motherboard and GPU fan control are unaffected.

At startup the daemon lists the `/dev/ttyACM*` and `/dev/ttyUSB*` devices that
exist, then opens each one in turn and asks it to identify itself. Only a device
that answers as an OpenFAN controller is adopted; a configured `[serial] port` is
tried first, but never alone, so a wrong value cannot remove OpenFAN control. The
API and the profile engine start whether or not a controller was found.

If you have **other** USB-serial hardware attached (an Arduino, a 3D printer, a
modem), three things are worth knowing:

- Identifying a device means opening it, and on Linux opening a serial port
  asserts DTR, which **resets Arduino-class boards**. The daemon therefore opens
  each candidate at most once per attempt, and enumerates without opening wherever
  it can, but it cannot identify a device without opening it.
- Startup makes **one** attempt, then gets out of the way. The search continues in
  the background once the daemon is up and answering, for **60 seconds** by
  default or **180** if you have named a `[serial] port`. Nothing waits for it.
- The background search costs nothing when nothing changes. It compares the list
  of serial devices each time, and opens anything only when that list changes, so
  plugging the controller in during the window is picked up within a few seconds,
  while a machine whose devices never change is never probed again.

If a controller that was working drops off, unplugged or reset, the daemon keeps
trying to get it back for as long as it runs, about every 30 seconds at the
default poll interval. Each try opens only the configured port, the controller's
own device node, and a serial device that has appeared since the controller
dropped off: on every try for its first minute, then once every five minutes while
the controller stays away. Other USB-serial devices that were already attached are
left alone, so an Arduino-class board beside the controller is not reset while it
is away. A controller that stops answering without dropping off the USB bus is not
recovered this way; restart the daemon (`systemctl restart ventulusd`).

If a controller is attached but was not detected, because it appeared after the
window say, use **Rescan Hardware** in the GUI (`POST /fans/openfan/rescan`)
rather than restarting, or pin the port as below. For reliable detection across
reboots, use a stable device path:

```bash
# Find your device's stable path
ls -la /dev/serial/by-id/

# Example output:
# lrwxrwxrwx 1 root root ... usb-Karanovic_Research_OpenFan_...-if00 -> ../../ttyACM0

# Set the stable path in daemon.toml
# [serial]
# port = "/dev/serial/by-id/usb-Karanovic_Research_OpenFan_...-if00"
```

### Updating the OpenFAN firmware

The GUI's **Update OpenFAN Firmware…** (Hardware page) installs a `.uf2` file you
downloaded. The daemon sets every OpenFAN channel to 100 %, sends the board into
its USB bootloader, waits while the file is copied onto the `RPI-RP2` drive, then
checks the board and gives the fans back to the profile. While an update runs,
OpenFAN writes are skipped rather than failed, calibrations and the other
diagnostics are refused, and the `openfan` health entry says what is happening. A
run that leaves the board in its bootloader keeps that entry critical, and OpenFAN
writes off, until the board answers again. The run is recorded in
`/var/lib/ventulus/openfan-maintenance.json`, so a daemon restart reports it
rather than forgets it.

**A board that does not answer.** An OpenFAN board can be on USB and still not
answer: the 2023 FW_01 build, a firmware with other commands, a hung one. Once a
probe has opened it and had no answer, the daemon names it in `/status`
(`openfan_silent_board`), and the same window updates it. Nothing is parked,
because the board takes no commands; the daemon sends the 1200-baud signal and,
if that does not move the board, waits up to 10 minutes for you to hold BOOT and
press RESET. Once the board answers, the daemon takes it over as the controller.
This path has not yet been tried on real hardware.

**Letting the daemon write the firmware itself (opt-in).** For a published OpenFAN
release it knows by fingerprint, the daemon can write the file through the
bootloader's PICOBOOT USB interface. It reads the flash's unique id first and
erases nothing unless that is your board's serial number, writes only the sectors
the file covers, reads every byte back, and restarts the board. The GUI then
reports the exact build as verified, unless the board's reports point at the old
firmware instead. Anything that stops that write before the restart gives the
drive back to you, and the update waits for the copy by hand. It needs read-write
access to the USB device nodes, which the shipped unit does not grant; install
the opt-in drop-in once:

```bash
sudo install -Dm644 \
  /usr/share/doc/ventulusd/openfan-firmware-write.conf.example \
  /etc/systemd/system/ventulusd.service.d/openfan-firmware-write.conf
sudo systemctl daemon-reload
sudo systemctl restart ventulusd
```

The file says what it grants. With it installed, any local account can start an
update that writes one of those releases, never another file; without it, the
update works as before. The 2023 FW_01 binary is refused in every case.

The steps, results, recovery, and updating by hand with the daemon stopped are in
the GUI manual's
[Updating the OpenFAN firmware](https://github.com/Plan-B-Development/ventulus/blob/main/manual/openfan-controller.md#updating-the-openfan-firmware).

### Serial permissions

The daemon runs as root, so no group membership is needed for it to open the
serial device, and Debian/Ubuntu need no `dialout` drop-in. What does limit it is
the unit's device allow-list, `DeviceAllow=char-ttyACM rw` and
`DeviceAllow=char-ttyUSB rw`: the service can open only `/dev/ttyACM*` and
`/dev/ttyUSB*` nodes (a `/dev/serial/by-id/` link to one of them is fine). A
controller on any other kind of node needs a drop-in adding its device class:

```bash
sudo systemctl edit ventulusd
# Add, for example:
#   [Service]
#   DeviceAllow=char-ttyS rw
```

A udev rule is **not required**: the daemon finds the controller on
`/dev/ttyACM*` and `/dev/ttyUSB*` by itself. Use one only if you want a specific
group or mode on the device node. For a stable path, use the `/dev/serial/by-id/`
link above: the daemon opens only `/dev/ttyS*`, `ttyUSB*`, `ttyACM*`, `ttyAMA*`
and `/dev/serial/` paths, so a custom udev symlink such as
`/dev/ventulus-controller` is refused.

The package installs an example rule, as documentation only, at
`/usr/share/doc/ventulusd/99-ventulus.rules.example`. To use it, copy it into
`/etc/udev/rules.d/` and edit it there (pacman overwrites the shipped example on
upgrade):

```bash
sudo install -m644 \
  /usr/share/doc/ventulusd/99-ventulus.rules.example \
  /etc/udev/rules.d/99-ventulus.rules

# Find VID/PID for your device:
udevadm info --attribute-walk --name=/dev/ttyACM0 | grep -E "idVendor|idProduct"

# Edit /etc/udev/rules.d/99-ventulus.rules and replace XXXX/YYYY, then:
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=tty
```

## GPU fan control

The control method depends on the AMD GPU's generation:

- **RX 7000 / RX 9000 (RDNA3 and later)** use the firmware's `fan_curve` sysfs
  interface. It needs the `amdgpu.ppfeaturemask` kernel parameter with bit 14 set
  (`0x4000`, `PP_OVERDRIVE_MASK`). The recommended value enables every flag;
  narrower masks work as long as bit 14 is set, but `0xffffffff` is what the
  daemon's diagnostics and the GUI suggest, what most distributions document, and
  what the daemon's error message points at:
  ```
  amdgpu.ppfeaturemask=0xffffffff
  ```
  The firmware curve is the **only** path the daemon writes on these cards. An
  RX 7000 also exposes `pwm1` and `pwm1_enable`, but a write there can succeed and
  change nothing, so without a `fan_curve` the daemon reports the fan as
  `read_only`. `man ventulusd` has the steps for each bootloader.
- **RX 6000 and older** use the traditional `pwm1_enable=1` + `pwm1` pair, which
  the daemon writes only from the GPU fan test (`POST /gpu/{id}/fan/verify`) and
  reset. No profile drives these fans: `/capabilities` reports
  `fan_control_method: "hwmon_pwm"` with `fan_write_supported: false`, and a
  profile control bound only to one is listed in `skipped_controls` as
  `backend_unavailable`.

When a profile names a GPU fan, the engine drives it like any other fan; for a
short manual change use the override API, and to find it the identify API
(§ Setting fan speeds). GPU writes need no lease, and the daemon skips a change
smaller than 5 % to avoid churning the GPU's firmware. When the daemon stops, each
GPU fan curve it drove is put back to automatic; a card it never drove is left
alone (§ Safety).

To hand a GPU fan back to its firmware, reset it:

```bash
# Restore GPU fan to firmware automatic (re-enables zero-RPM)
curl --unix-socket /run/ventulus/ventulus.sock \
  -X POST http://localhost/gpu/0000:03:00.0/fan/reset | jq .
```

The GPU id is the card's bare PCI address, its entry's `pci_bdf` in
`devices.amd_gpus` of `GET /capabilities` (`devices.amd_gpu.pci_bdf` is the
primary card only). The fan id (`amd_gpu:0000:03:00.0`), or anything else with a
prefix, answers `404`.

**A reset hands the fan to the firmware until the next profile activation.** The
daemon stops writing that fan, even while the active profile names it, until a
profile is activated again (the same one included); deactivating does not return
it, and a daemon restart does. The GUI's *Restore GPU Fan to Automatic* is
disabled while the active profile drives the card.

**Known kernel regressions.** The daemon matches the running kernel against a
short list of published amdgpu regressions and reports any that apply in
`GET /capabilities` (`devices.amd_gpu.kernel_warnings`); the GUI shows a high or
critical one as a popup once per session. The list holds one entry,
`rdna_mes_hang_drm_amd_4765` (critical): on Linux 6.17.9–6.17.13 and
6.18.0–6.18.6, an RDNA3, RDNA3.5 or RDNA4 GPU can hang when a compute job runs
beside a 3D workload, and nothing can change a fan's speed while the system is
hung. Every AMD GPU in the machine is checked, an integrated one beside a discrete
card too, and the message names the cards it applies to. It is fixed in 6.18.7 and
6.19: update to the latest 6.18 longterm or a current 7.x kernel. The check uses
the version number only, so a distribution kernel that already carries the fix may
still be flagged.

## Scripting the daemon

The API is HTTP over the Unix socket `/run/ventulus/ventulus.sock`, which every
local user can read and write. `curl` and `jq` are enough:

```bash
SOCK=/run/ventulus/ventulus.sock
curl -s --unix-socket $SOCK http://localhost/status | jq .        # health, thermal state, overrides
curl -s --unix-socket $SOCK http://localhost/capabilities | jq .  # devices, feature flags, limits
curl -s --unix-socket $SOCK http://localhost/sensors | jq .
curl -s --unix-socket $SOCK http://localhost/fans | jq .
curl -s --unix-socket $SOCK http://localhost/diagnostics/hardware | jq .  # start here when something looks wrong
```

The [API contract](https://github.com/Plan-B-Development/ventulus/blob/main/docs/08_API_Integration_Contract.md)
is the reference for every endpoint, payload and error code. Errors share one
envelope, `{"error": {"code", "message", "retryable", "source", "details"}}`. A
client should decide what to offer from the feature flags in `GET /capabilities`,
not from the daemon's version number.

| To | Use |
|---|---|
| Read state | `GET /status`, `GET /poll` (status, sensors and fans in one call), `GET /sensors`, `GET /sensors/history`, `GET /fans`, `GET /hwmon/headers` |
| Diagnose the hardware | `GET /diagnostics/hardware`, `GET /inventory/hardware-readiness`, `GET /inventory/hwmon` |
| Manage profiles | `GET`, `POST`, `PUT`, `DELETE /profiles`; `POST /profile/activate`, `/profile/deactivate`; `GET /profile/active` (§ Fan profiles) |
| Change a fan for a moment | `POST /control/{control_id}/override` and its renew and release; `POST /fans/{fan_id}/identify` (§ Setting fan speeds) |
| Change settings | `GET /config`, `POST /config/*` (§ Configuration), `POST /config/header-role` |
| Describe a cooler and record it | `GET /inventory/cooling-devices`, `POST`/`DELETE /config/cooling-device`, `/validation/session` and `/validation/sessions` |
| Test the hardware | `POST /hwmon/{header}/verify`, `/characterize`, `/discover-control-path` and `/stall-probe`; `POST /fans/openfan/{ch}/calibration`; `POST /gpu/{id}/fan/verify` and `/fan/reset`; `GET /diagnostics/preflight` first |
| Look after the OpenFAN controller | `POST /fans/openfan/rescan`, `GET /fans/openfan/device`, `PUT /fans/openfan/firmware`, `/fans/openfan/maintenance` |
| Pick up new hardware | `POST /hwmon/rescan`, `POST /fans/openfan/rescan` |

The hardware tests move fans on purpose. Each one puts the fan back when it ends,
and the contract says exactly how each treats a pump, a stale temperature and a
cancel. Read its section before scripting one.

**A validation session records until you stop it.** Finishing its diagnostics does
not end it, and neither does closing the GUI. Left alone it stops at the daemon's
7200-sample cap, two hours at one sample a second. End one with
`POST /validation/session/stop`, or with `DELETE /validation/session`, which keeps
the same evidence but marks the session `cancelled`; neither discards anything.
Starting it with `"stop_when_diagnostics_complete": true` ends it as soon as its
diagnostics have run. A session interrupted by a stop or a reboot is kept as
`interrupted` at its last real sample.

## Troubleshooting

Start with the journal (`journalctl -u ventulusd`) and
`GET /diagnostics/hardware`, or the GUI's **System State** and **Hardware** pages,
which show the same things.

### Conditions the daemon reports

`GET /status` (and `GET /poll`) and `GET /fans` report four conditions the daemon
cannot fix by itself. The GUI shows each one; the fields are listed here for
scripts and for reading a support bundle.

| Field | What it means | What to do |
|---|---|---|
| `duty_not_holding: true` on an hwmon fan in `GET /fans` | Something else is writing this header. The daemon rewrote the duty it had set three times and the header did not keep it, so it has stopped fighting. It tries again when its curve asks for a different speed, and the flag clears when the header keeps a duty again. `duty_corrections` counts the rewrites since the daemon started. | Stop the other fan-control program (`fancontrol`, CoolerControl, CoreCtrl, `fan2go`, a vendor tool), or the BIOS/EC feature that drives the header. |
| `skipped_controls[]` on `/status` | A control in the active profile has not been commanding its fans for at least 3 ticks, so they hold their last speed. `reason` says why: `curve_not_found` (the curve it names is gone), `sensor_unavailable` (its sensor is missing or has stopped updating), `mix_unresolvable` / `sync_unresolvable` (a combined curve cannot be worked out), or `backend_unavailable` (none of its fans can be reached, for example an OpenFAN fan with no controller attached, or an AMD GPU the daemon cannot drive: an RX 6000 or older, or an RX 7000/9000 without its firmware fan curve). | Fix the profile or the sensor. A frozen sensor is the usual cause: see § Safety, "Stalled sensors stop driving curves". |
| `unavailable_sensors[]` on `/status` | A sensor the daemon found fails every read (a Wi-Fi card's temperature while the radio is off is the usual one) or reports an impossible temperature. It is logged once and left out of `GET /sensors`. | Nothing, unless a curve needs that sensor; then pick another. |
| `runtime_config_degraded` on `/status` | The daemon could not read `runtime.toml`, which holds your header roles. | See § Runtime configuration, "When `runtime.toml` cannot be read". |

### The daemon does not start

`sudo systemctl status ventulusd` and `journalctl -u ventulusd` say why. The usual
causes are a bad edit to `daemon.toml`, which the journal names (§ Configuration),
and another `ventulusd` already running, which exits with status 75 and
*another ventulusd is already running*.

### A client cannot connect

The GUI shows itself as disconnected, or `curl` cannot reach the socket:

- `systemctl is-active ventulusd` should say `active`;
- `ls -la /run/ventulus/ventulus.sock` should show a socket that everyone can read
  and write (`srw-rw-rw-`);
- a client started with a different socket path (`ventulus --socket <path>`) has
  to match `[ipc] socket_path`.

### No OpenFAN controller is found

See § Serial device setup. `journalctl -u ventulusd | grep -i openfan` shows
`OpenFanController connected on <port>` once one is adopted; on a machine with
none, the absence is logged at `info`, not as a warning.

### Motherboard fan headers are missing

- Check that the driver is loaded and the files exist:
  `ls /sys/class/hwmon/hwmon*/pwm[0-9]`. (A plain
  `find /sys/class/hwmon -name 'pwm[0-9]'` finds nothing, because each `hwmonN`
  entry is a symlink, which `find` does not follow.)
- A sensor chip whose driver you have just loaded appears after a rescan:
  **Rescan Hardware** in the GUI footer, or
  `curl -X POST --unix-socket /run/ventulus/ventulus.sock http://localhost/hwmon/rescan`.
  **New fan headers need a daemon restart**
  (`sudo systemctl restart ventulusd`): the rescan lists them but does not add
  them to the running controller.
- § Hardware sensor modules covers drivers, and the GUI manual's
  [Hardware Troubleshooting](https://github.com/Plan-B-Development/ventulus/blob/main/manual/hardware-troubleshooting.md)
  covers a second chip that has vanished.

### Fans are found but never change speed

If the journal repeats a line like

```
[WARN] hwmon write failed for hwmon:it8696:pwm1: … /sys/class/hwmon/hwmonN/pwm1_enable: Read-only file system (os error 30)
```

the service's sandbox does not let it write the device tree. The packaged unit
sets `ReadWritePaths=/sys/devices`, so look for a drop-in that overrides it
(`systemctl cat ventulusd` shows every file in effect), remove it, then
`sudo systemctl daemon-reload && sudo systemctl restart ventulusd`.

Without that line, the cause is a hardware prerequisite: the GUI's **Hardware**
page detects a missing Super-I/O driver, an ACPI conflict or a missing
`amdgpu.ppfeaturemask`, and shows the fix. If the header is flagged
`duty_not_holding`, another program is writing it (see the table above).

### The profile does not come back after a reboot

- Check § Loading a profile: a `--profile` or `OPENFAN_PROFILE` in a drop-in wins
  over the profile you activated in the GUI.
- `sudo cat /var/lib/ventulus/daemon_state.json` shows the saved choice (the state
  directory is `0700`, readable by root only).
- The journal says why a profile did not load at startup.

## Uninstall

```bash
sudo pacman -R ventulusd
```

The package stops and disables the service before it is removed. Stopping the
daemon gives every motherboard fan header it took back to what it was doing
before, usually its BIOS fan mode, both in-process as it exits and again through
`ExecStopPost`. OpenFAN channels and headers with no mode switch are left at the
exit minimum (§ Safety).

pacman leaves behind what it did not install or what you changed:

- `/var/lib/ventulus/`, the daemon's state: `runtime.toml` (your header roles and
  API-set settings), the saved active profile, the profile store and diagnostic
  records;
- an edited `/etc/ventulus/daemon.toml`, `/etc/ventulus/profiles/quiet.json` or
  `/etc/modules-load.d/ventulus.conf`, kept with a `.pacsave` suffix, and any
  profile you added under `/etc/ventulus/profiles/`;
- anything you installed by hand: a udev rule in `/etc/udev/rules.d/`, or a
  drop-in under `/etc/systemd/system/ventulusd.service.d/`.

Remove those by hand if you no longer want them:

```bash
sudo rm -rf /var/lib/ventulus/ /etc/ventulus/
sudo rm -f /etc/udev/rules.d/99-ventulus.rules && sudo udevadm control --reload-rules
sudo rm -rf /etc/systemd/system/ventulusd.service.d/ && sudo systemctl daemon-reload
```

## Safety

The daemon enforces these rules whatever the GUI or the active profile asks for.
None can be turned off; the only setting is the coolant limit.

- **Thermal emergency.** If the hottest CPU temperature sensor reaches the
  emergency limit, every OpenFAN channel and writable motherboard fan header the
  machine has is driven to 100 %. **The limit is at least 105 °C and is set per
  machine:** where the kernel reports the CPU's own design ceiling, the daemon
  raises the limit to `min(ceiling + 5 °C, 115 °C)`, because a modern part is
  *meant* to sit at its ceiling under sustained load. A limit set at the ceiling
  would fire on a healthy machine and then latch, since release needs a reading
  at or below 80 °C that such a part never produces. `GET /diagnostics/hardware`
  reports the limit in use as `emergency_threshold_c`, and the release point as
  `release_threshold_c`. The emergency holds until a *fresh* CPU reading is at or
  below 80 °C (a sensor that stops updating or disappears keeps it at 100 %), and
  then control returns to the active profile at once. One reading at or above the
  limit is enough to start it, and the daemon never decides afterwards that the
  reading was wrong: as IEC 61511-1 expects of a safety action, once it has fired
  it stays in force until its reset. A faulty sensor stuck at or above the limit
  therefore keeps those fans at 100 % until it reads 80 °C or below. If your fans
  stay at full speed with no real heat, look for the CPU sensor reporting the
  limit, and fix or report its driver. Every other fan the emergency took is
  given back at that point: a motherboard header to what it was doing before, an
  OpenFAN channel to its duty from before the emergency. **Both duties are floors
  over the active profile's output, never replacements for it**, so the rules can
  only raise a fan, never lower one. GPU fans are deliberately excluded: there is
  no GPU emergency threshold, because AMD's firmware protects the GPU itself by
  throttling its clocks on junction temperature. It does not speed a fan up past
  a curve the daemon has set, so while the daemon drives a GPU fan the throttling
  is what protects the card; the fan returns to the firmware's own control when
  the daemon stops. A GPU fan in your profile keeps following its own curve
  throughout an emergency, as it does the rest of the time.
- **The CPU emergency is a backstop, not a cooling-failure detector.** A modern CPU
  protects itself by throttling at its own temperature ceiling, and the emergency
  limit sits above that ceiling on purpose, so a stopped pump or stalled fans
  usually show up as a CPU pinned at its ceiling and running slower rather than as
  an emergency. Three cooling-failure checks cover that gap:
  - **Coolant emergency.** When the hottest *current* coolant reading (an AIO or
    custom-loop coolant sensor the daemon recognises) is at or above the coolant
    limit (`[safety] coolant_limit_c`, whole °C, 40–70, default 60, also settable
    from the GUI; no off switch), every fan is forced to 100 % exactly as in the CPU
    emergency, until a current reading is 5 °C below the limit. A stale or vanished
    coolant sensor holds a latched emergency and otherwise does nothing (most
    machines have no coolant sensor). `thermal_state` is `emergency` for either
    rule, and `emergency_causes` on `GET /status` says which fired.
  - **Pump stall response.** A pump in the active profile that the daemon has seen
    spinning, and that then reads 0 RPM for 10 s while commanded to 30 % or more, is
    driven to 100 % for 30 s. If it turns it returns to its curve; if not, it stays
    at 100 %. A second stall in one run holds it at 100 % until the daemon restarts
    or a profile is activated. Listed on `GET /status` as `pump_stalls[]`.
  - **Cooling advisory.** A CPU held at or above its ceiling (its own `crit`, else
    85 °C) for a minute while no fan or pump is commanded at 50 % or more raises
    `cpu_at_ceiling_low_cooling` in `advisories[]` on `GET /status`. It forces
    nothing.

  A CPU that sits at its ceiling under a load it used to handle is still the sign
  to check the pump and fans.
- **DC pump floor.** A pump on a motherboard header that the kernel reports in DC
  (voltage) mode is never driven below 70 %, rather than the 30 % pump floor,
  because a voltage-driven pump stalls much higher in its range. This applies
  wherever the pump floor does: curves, manual overrides, identify, and every
  diagnostic. A header whose driver does not report its mode keeps the 30 % floor.
- **Missing sensor fallback.** If no CPU temperature sensor reports for 5
  consecutive polling cycles and no emergency is under way, the fans your active
  profile controls are held at 40 % or more (GPU fans excluded, as above); a fan
  whose own curve can no longer be read keeps the speed it had. Fans no profile
  controls are left to their own firmware curve, which reads its own CPU sensor: a
  flat 40 % there could run a fan *slower* than the BIOS would under load, and with
  no profile active nothing is forced at all. An ARCTIC Fan Controller is the
  exception: it has no firmware curve, and every write to it carries all ten
  channels, so the daemon sets each channel reading 0 that it did not choose to
  100 % before writing any of them. Control all ten channels to keep them quiet.
  Only the 100 % emergency takes every fan. A sensor that is still *listed* but has
  **stopped updating** counts as missing: a reading older than five polling
  intervals is not treated as current, because a frozen temperature can never rise
  and would otherwise hide a real emergency indefinitely. The exceptions all follow
  one rule, that losing sight of a sensor must never *reduce* cooling. If a thermal
  emergency is already active the 100 % force continues, whether the sensor went
  quiet or disappeared; and if the last reading before the sensor went quiet was at
  or above the 80 °C release temperature, fan curves simply keep running on it. The
  40 % fallback applies when the last thing the daemon knew was that the system was
  *cool*, which is the case it was written for.
- **Stalled sensors stop driving curves.** The rule above concerns the CPU. Every
  other sensor that drives a fan curve (GPU, coolant, VRM, drive) is also checked
  for freshness: if it stops updating, its curve stops running and the fans it
  controls **hold at their current speed** rather than tracking a temperature that
  is no longer real. They are never dropped to zero, and never quietly lowered. A
  curve that combines several sensors keeps running on the ones it can still see,
  but may not command *less* than it already was until they are all back, so losing
  one input can make your fans stay high, never fall. If one of the sensors a
  combined curve names does not exist on your machine at all, the curve simply runs
  on the others. If your fans stop responding to a rising GPU or coolant
  temperature, check that sensor: a frozen reading is the likely cause, and the
  daemon has deliberately stopped trusting it. Sensors that disappear entirely (a
  driver unloaded, hardware removed) are dropped from `GET /sensors` instead of
  lingering at their last value.
- **Override visibility.** The current state is reported as `thermal_state` in
  `GET /status` (`normal`, `emergency` or `no_sensor_fallback`), and the GUI shows
  a banner from it. The GUI has no fan-control loop of its own to pause; the daemon
  owns control throughout.
- **OpenFAN controller stops are not time-limited.** A channel commanded to 0 %
  stays stopped for as long as it is commanded; the daemon does not restart it
  after a fixed time. The 8-second stop timer (advertised as
  `limits.openfan_stop_timeout_s`) refuses only a 0 % command that would reach the
  device while an earlier stop is still timed as running, which no normal sequence
  of commands produces, because a repeated 0 % is never re-sent. A profile never
  stops a pump: pump and CPU members are held at or above the 30 % floor, whatever
  the curve asks for. An OpenFAN channel counts as a pump only once you assign it
  the `pump` role (`POST /config/header-role`); the daemon has no other way to tell.
- **Per-member minimum floors.** The daemon reports no per-*header* floor
  (`min_pwm_percent: 0` for every hwmon header), but it **does** enforce the
  role-aware minimum the GUI writes into each control's `minimum_pct`. A profile
  whose pump/CPU control drops below the hard 30 % floor is rejected at validation
  with `400 validation_error` (`FLOOR_TOO_LOW`), and the profile engine re-clamps
  every member to its effective floor on each tick. Floor safety is enforced by
  the daemon, not merely by the GUI.
- **GPU fan curves and motherboard headers are given back on shutdown.** Each AMD
  GPU whose fan curve the daemon wrote (a profile's curve, or a fan test) goes back
  to the firmware's own curve with zero-RPM idle stop re-enabled, and each
  motherboard header the daemon took goes back to exactly what it was doing
  before: its recorded `pwm_enable`, or its duty if it was already in manual mode.
  A GPU the daemon never wrote a curve to is not touched, so a curve LACT or
  CoreCtrl put on a card your profile does not name survives a stop or a restart;
  so does one put on a card after **Restore GPU Fan to Automatic** handed it back.
  The daemon does not wait for a stop to give a GPU back: once the active profile
  no longer names a card it drove (the profile deactivated, or switched to one
  without that card), it puts the card back on the firmware's own curve on the next
  tick, and leaves it alone until a profile names it again. An older AMD card (RX
  6000 or earlier) that the daemon was testing when it died goes back to the fan
  mode it had before the test. A header whose mode cannot be restored is set to
  full speed instead, as lm-sensors `fancontrol` does. The daemon never writes a
  fixed "automatic" value: `2` is automatic on `it87` but Thermal Cruise on
  `nct6775`, and on an NZXT Kraken it applies an empty curve. The one exception is
  a Dell machine whose `dell_smm` driver has a single BIOS fan-control switch for
  every fan: that switch can be set but not read, so the daemon gives it `2`, which
  on that driver hands the fans back to the BIOS. On such a machine a profile
  should control all of its fans or none of them. The GUI enforces that, and will
  not save or activate a profile that controls only some of them; the daemon does
  not, so an imported profile, or one written by another client, is still started
  by the tray or at boot. While the daemon holds the switch, a fan no profile
  controls has nothing driving it. When the daemon gives the switch back, the BIOS
  takes every fan, including any a profile still controls. Two mechanisms cover the
  hand-back: the daemon does it **in-process** as it shuts down, and `ExecStopPost`
  in the systemd unit repeats it once the daemon has exited, whatever ended it: a
  normal stop, a crash, a SIGKILL the daemon could not respond to, or the watchdog
  restarting a daemon that stopped responding. The in-process one is bounded, so a
  restore that hangs cannot keep the daemon from exiting, and `ExecStopPost` from
  running.
- **OpenFAN fans, and motherboard headers with no mode switch, are left at a
  minimum on stop.** These have no firmware behaviour to go back to (an OpenFAN
  channel holds whatever it was last told, indefinitely), so on a clean stop
  (including `systemctl restart`, a reboot, or the watchdog restarting a daemon
  that stopped responding) each one the daemon drove is left at its last speed or
  the **exit minimum**, whichever is higher: 50 % unless you change it in the GUI
  (Settings → Daemon Configuration → Exit minimum) or with
  `[shutdown] exit_floor_pct`. A fan whose last speed the daemon lost track of is
  left at 100 %; a fan it never drove is not touched, except an ARCTIC Fan
  Controller channel set to 100 % as above; 0 turns the minimum off. A crash or
  SIGKILL cannot run this, because `ExecStopPost` cannot reach the OpenFAN
  controller, so after one those fans keep their last speed until the daemon is
  back.
- **Neither guarantees the hardware actually came back.** Each restore step gives
  up after a few seconds so the daemon can always exit. If a chip or card has
  stopped accepting writes, nothing can restore it, and those fans hold their last
  speed until something takes them over again.
