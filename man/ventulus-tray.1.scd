ventulus-tray(1)

# NAME

ventulus-tray - system-tray client for the Ventulus daemon

# SYNOPSIS

*ventulus-tray* [OPTIONS]

# DESCRIPTION

A small StatusNotifierItem process that places a Ventulus icon in the KDE
Plasma system tray (and any other desktop implementing the freedesktop
StatusNotifierItem specification). It provides quick access to profile
switching and to the GUI without keeping the full GUI running.

It is a *client*, not a controller. It calls the daemon's HTTP API over the
Unix socket and nothing else: it holds no lease, writes no PWM, evaluates no
fan curve, and reads no hardware. The daemon has no knowledge of the tray and
no dependency on it: stopping the tray, or never starting it, changes nothing
about fan control, thermal safety, or profile execution.

Written in Rust and deliberately toolkit-free: no Qt, GTK, OpenGL, Vulkan or
Mesa library is loaded into the process, so it cannot interfere with a
GPU-heavy workload. It runs no timer and no polling loop: it reads the daemon
only when its menu is about to be shown, and is otherwise asleep.

# OPTIONS

*--socket* _PATH_
	Path to the daemon's Unix socket.
	Default: _/run/ventulus/ventulus.sock_.

*--version*
	Print the tray's own version and exit. This is the version of the tray
	binary; the *running daemon's* version is what the menu displays.

*-h*, *--help*
	Print usage and exit.

# THE MENU

*Left click* opens ventulus(1).

*Right click* opens the menu, which shows:

- a greyed header line: *Ventulus · daemon* followed by the running
  daemon's version, or *daemon version unknown* when the daemon did not
  report one, or *daemon unavailable* when it could not be reached;
- a greyed warning line when the daemon reports a thermal state other than
  normal: *⚠ Thermal emergency — fans forced to maximum*, *⚠ No CPU
  temperature — fans held at a safety floor*, or, for a state this tray
  does not know, *⚠ Thermal state:* and the daemon's own name for it;
- a greyed line when the last profile change you made did not happen (see
  below);
- *Profile*, listing every profile the daemon knows, with the active one
  checked, plus *Stop profile control*, which is greyed while no profile is
  active. In place of the list it shows a greyed *Daemon unavailable*,
  *Profiles could not be read*, or *No profiles available*;
- *Open Ventulus*, the same action as a left click;
- *Quit tray*.

Profiles that share a name are shown with their id appended. This is normal:
the daemon's profile list is a union across all of its search directories,
deduplicated by id rather than by name.

Selecting a profile sends the change to the daemon and then re-reads it, so
what the menu shows afterwards is what the daemon actually applied, not what
was requested. If the change did not happen, the menu says so on a greyed
line below the header and any thermal line: *⚠ Profile not switched — <profile> is still active*,
*⚠ Not stopped — <profile> is still controlling the fans*, or, when the daemon
did not answer the re-read either, *⚠ Profile switch failed, and the daemon is
not answering* or *⚠ Stop failed, and the daemon is not answering*. The line
stays until a later change succeeds.

# BEHAVIOUR WORTH KNOWING

## Double-click

The StatusNotifierItem protocol has no double-click. Plasma routes a left
click straight to _Activate_, so a double click launches the GUI twice. The
GUI is single-instance, so the second launch raises the first window instead
of starting a second copy.

On Wayland, raising an existing window is subject to the compositor's
focus-stealing prevention. With KWin's default setting this normally succeeds,
but the window may instead be marked as demanding attention in the task bar.
The tray cannot influence this: _Activate_ carries no xdg-activation token.

## Switching profiles during a thermal event

Activating a profile clears any manual control overrides that are in effect.

When the daemon reports a thermal state other than normal, its forced fan
duties are *floors over* the active profile's output, not replacements for it.
Selecting a quieter profile during a thermal event will therefore not quieten
the fans. The menu does not say this; its warning line says only that the
fans are forced to maximum or held at a safety floor.

## When the daemon is not running

The menu reports that the daemon is unavailable and keeps working; opening the
GUI still functions. The tray does not retry in a loop and does not exit.

## Startup

The tray starts automatically at login via
_/etc/xdg/autostart/ventulus-tray.desktop_. Only one tray runs per user.

The autostart entry names ventulus in its _TryExec_ field, so on a
headless or daemon-only installation, where the GUI is not installed, the
tray is not started at all.

# TROUBLESHOOTING

On a systemd session, the autostart entry becomes a user unit:

```
systemctl --user status 'app-ventulus\x2dtray@autostart.service'
systemctl --user restart 'app-ventulus\x2dtray@autostart.service'
journalctl --user -u 'app-ventulus\x2dtray@autostart.service'
```

By default the tray logs its own messages at _info_ and everything from its
libraries at _warn_. That is deliberate: the D-Bus stack it uses logs every
message it dispatches at _info_, which buries the tray's own lines in a journal
where they are the ones worth reading.

To run it by hand with verbose logging, after stopping the unit:

```
RUST_LOG=debug ventulus-tray
```

*RUST_LOG* overrides the default completely, so it is also how to get the D-Bus
internals back when diagnosing a registration problem:

```
RUST_LOG=ventulus_tray=debug,zbus=info ventulus-tray
```

To disable it permanently, use System Settings → Autostart, which writes a
per-user override. Do not edit the file in _/etc/xdg/autostart/_; it is owned
by the package and is replaced on upgrade.

If the icon does not appear, check that the daemon socket exists and that the
desktop provides a StatusNotifierItem host. The tray waits for the host rather
than exiting, so it survives *plasmashell --replace* and a Plasma restart.

# FILES

_/run/ventulus/ventulus.sock_
	The daemon socket the tray reads.

_$XDG_RUNTIME_DIR/ventulus-tray.lock_
	Advisory lock holding the one-tray-per-user guarantee. Created on
	demand; the kernel releases it however the process exits, so a file
	left behind by a crash is harmless and needs no cleaning up. If
	*$XDG_RUNTIME_DIR* is unset or is not private to you, the tray runs
	without the guard and logs the reason.

_/etc/xdg/autostart/ventulus-tray.desktop_
	Autostart entry. Package-owned.

_/usr/share/icons/hicolor/scalable/apps/ventulus-symbolic.svg_
	The panel glyph.

# SEE ALSO

ventulusd(1), ventulus(1)
