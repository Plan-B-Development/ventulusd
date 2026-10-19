# ventulusd

*Formerly Control-OFC (`control-ofc-daemon`, `control-ofc-tray`).*

**Latest release:** v5.3.0 — 2026-10-19. Pairs with `ventulus` ≥ v2.23.0 (the recommended capability floor; the package itself only hard-blocks GUIs < 2.0.0, the sole-writer cutover); version numbers carry on from Control-OFC.

`ventulusd` is the fan control daemon behind [Ventulus](https://github.com/Plan-B-Development/ventulus).
It runs as a systemd service, reads your temperature sensors and drives your fans
from the active profile once a second, whether or not the desktop app is open. The
app, `ventulus`, edits profiles and shows what the daemon is doing; it never touches
hardware itself. The package also installs `ventulus-tray`, a KDE system-tray icon.

## What it drives

| Hardware | Support |
|---|---|
| OpenFAN controller (USB, 10 channels) | Full control |
| Motherboard fan headers, and other fan controllers with a kernel hwmon driver (such as the ARCTIC Fan Controller) | Full control on most boards; newer Gigabyte, MSI and some ASRock boards need an out-of-tree driver first |
| Liquid coolers with a kernel driver (NZXT Kraken, Aquacomputer) | Coolant temperature, and pump and fan control where the driver allows it |
| AMD GPU fans, RX 7000 / RX 9000 | Full control through the card's firmware fan curve; needs one kernel parameter |
| AMD GPU fans, RX 6000 and older | Monitored; the fan test and reset work, but no profile drives them |
| NVIDIA and Intel Arc GPU fans | Monitor only |

It also reads CPU, GPU, drive, memory and motherboard temperatures. The
[Hardware Compatibility](https://github.com/Plan-B-Development/ventulus/blob/main/docs/19_Hardware_Compatibility.md)
guide has the chip-by-chip and board-by-board detail.

## Install

`ventulusd` is packaged for x86_64 Linux in signed repositories, from which it
upgrades with the rest of your system:

| Distribution | Install | Upgrades with |
|---|---|---|
| Arch Linux, CachyOS, EndeavourOS, Garuda | `sudo pacman -Syu ventulusd` | `sudo pacman -Syu` |
| Debian 13, Ubuntu 26.04 LTS | `sudo apt install ventulusd` | `sudo apt update && sudo apt upgrade` |
| Fedora 44, Fedora 45 | `sudo dnf install ventulusd` | `sudo dnf upgrade` |

Add the repository first: the
[pacman-repo README](https://github.com/Plan-B-Development/pacman-repo#readme) and
the GUI's [Getting Started](https://github.com/Plan-B-Development/ventulus/blob/main/manual/getting-started.md#installation)
have the steps (the key, its fingerprint and the repository entry), the signed
bootstrap script for Arch, a tarball for Debian, Ubuntu and Fedora without a
repository, one-off installs from a release with their Sigstore check, and the
distributions that are not supported. Install `ventulus` instead for the desktop
app; it pulls in the daemon.

On Debian and Ubuntu the service is enabled and started as it installs. On Arch
and Fedora, and from the tarball, enable it yourself:

```bash
sudo systemctl enable --now ventulusd
```

A headless machine needs only `ventulusd`; the tray starts only where `ventulus`
is installed. Install a package rather than copying the binary: the unit, the
hand-back script that runs after every stop, the sleep hook and the Super-I/O
guard all come with it. To build the packages yourself or run your own build, see
[`daemon/README.md`](https://github.com/Plan-B-Development/ventulusd/blob/main/daemon/README.md).

Check that the daemon is answering:

```bash
curl --unix-socket /run/ventulus/ventulus.sock http://localhost/status
```

## Prerequisites

The package loads the common Super-I/O drivers at boot
(`/etc/modules-load.d/ventulus.conf`), so many machines need nothing more. The
GUI's **Hardware** page reports what yours needs. The usual extras:

| You need | When | How |
|---|---|---|
| An out-of-tree driver (`frankcrawford/it87`, `Fred78290/nct6687d`), built with DKMS | Most 2022-and-later Gigabyte and MSI boards, many ASRock boards, ASUS AM4 300/400-series | [Driver Setup][driver-setup], per distribution |
| Signed DKMS modules, or Secure Boot off | An out-of-tree driver with Secure Boot on | [Secure Boot and DKMS modules][secure-boot] |
| Sensible BIOS fan settings | Every board: the BIOS still runs the fans at boot, so never give its curve a 0 % point | [BIOS settings][bios] |
| `amdgpu.ppfeaturemask=0xfff7ffff` (any mask with bit `0x4000`) | Fan control on an RX 7000 or RX 9000 | [AMD GPU prerequisite][amd-gpu] |
| A driver option for an ACPI resource conflict | A driver that fails with *Device or resource busy* | [Module parameters][module-params] |
| A full power cut | A dual-chip Gigabyte board whose second fan chip has vanished | [Missing fan headers][missing-headers] |

Stop any other fan tool (fancontrol, CoolerControl, CoreCtrl, fan2go) before the
daemon takes over: two programs writing one fan header fight each other. The
[Setup Checklist][setup-checklist] walks through all of this in order.

[driver-setup]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/driver-setup.md
[secure-boot]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/driver-setup.md#secure-boot-and-dkms-modules
[bios]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/driver-setup.md#step-5--bios-settings-the-half-people-skip
[amd-gpu]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/driver-setup.md#amd-gpu-fan-control-prerequisite-rdna3
[module-params]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/driver-setup.md#module-parameters-you-may-actually-need
[missing-headers]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/hardware-troubleshooting.md#some-of-my-fan-headers-are-missing--only-5-of-8-show-up
[setup-checklist]: https://github.com/Plan-B-Development/ventulus/blob/main/manual/setup-checklist.md

## How it works

The daemon serves an HTTP API on the Unix socket `/run/ventulus/ventulus.sock`.
The GUI, the tray and your own scripts use it to activate profiles, take short
manual overrides, identify fans and run diagnostics. The socket is open to every
local user and has no authentication. Only the daemon writes fan speeds.

It also owns thermal safety, and none of it can be turned off:

- At the CPU emergency limit, every OpenFAN channel and writable motherboard
  header runs at 100 % until a current reading is 80 °C or lower. The limit is at
  least 105 °C, and up to 115 °C on a CPU that reports its own ceiling.
- A coolant sensor at or above `[safety] coolant_limit_c` (60 °C by default) does
  the same until it is 5 °C cooler.
- With no CPU reading for five polls, the active profile's fans are held at 40 %
  or more.
- A pump is never stopped and never driven below 30 %, or 70 % on a header in DC
  mode. A pump that stalls is driven to 100 %.
- When the daemon stops, every motherboard header and GPU fan curve it took goes
  back to what it was doing before. OpenFAN channels, which have nothing to go
  back to, stay at their last speed or the exit floor (50 % by default),
  whichever is higher.

These are floors over the profile, so they can only speed a fan up. GPU fans are
outside them, because the card's firmware protects the GPU itself. The full rules
are in the [User Guide § Safety](https://github.com/Plan-B-Development/ventulusd/blob/main/docs/USER_GUIDE.md#safety).

## Documentation

| Document | For | Covers |
|---|---|---|
| [User Guide](https://github.com/Plan-B-Development/ventulusd/blob/main/docs/USER_GUIDE.md) | operators | Sensor drivers, configuration, the service, profiles, the OpenFAN controller, GPU fans, safety, troubleshooting, uninstalling |
| `man ventulusd`, `man ventulus-tray` | operators | Options, files, environment and signals |
| [GUI manual](https://github.com/Plan-B-Development/ventulus/blob/main/manual/README.md) | everyone | Setting up and using Ventulus, driver setup, hardware troubleshooting |
| [API contract](https://github.com/Plan-B-Development/ventulus/blob/main/docs/08_API_Integration_Contract.md) | script and client authors | Every endpoint, payload, error code and capability flag |
| [`daemon/README.md`](https://github.com/Plan-B-Development/ventulusd/blob/main/daemon/README.md) | contributors | Building, testing, running your own build, repository layout |
| [`docs/ADRs/`](https://github.com/Plan-B-Development/ventulusd/tree/main/docs/ADRs) | contributors | Architecture decision records |
| [CHANGELOG](CHANGELOG.md) | everyone | Release history |

## License

MIT. See [`LICENSE`](https://github.com/Plan-B-Development/ventulusd/blob/main/LICENSE).
