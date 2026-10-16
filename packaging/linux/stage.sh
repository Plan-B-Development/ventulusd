#!/bin/bash
# stage.sh <flavour> <bindir> <mandir> <destroot>
#
# Installs every file of the ventulusd package for Debian/Ubuntu (deb), Fedora
# (rpm) or the tarball into <destroot>, laid out as on the target system. This
# is the one file list for those three formats (DEC-499); it mirrors
# packaging/PKGBUILD's package(), and daemon/tests/packaging_linux.rs holds the
# two together. Run from the repository root.
#
#   flavour  deb | rpm | tarball
#   bindir   holds the built ventulusd and ventulus-tray
#   mandir   holds ventulusd.1 and ventulus-tray.1, rendered from man/*.scd
#
# Not here, by design (DEC-499): the Control-OFC carry-over (pacman hook and
# script) and the compatibility socket link (tmpfiles), which exist only for
# Arch users upgrading from control-ofc-daemon 4.x.
#
# Paths match the Arch package wherever the distribution allows, so the unit,
# the hand-back script it runs (ExecStopPost=/usr/bin/ventulus-restore-auto) and
# every path the documentation names are the same on all four.
set -euo pipefail

flavour=$1 bindir=$2 mandir=$3 dest=$4

case "$flavour" in
    deb)
        # Debian's zsh reads vendor-completions, not site-functions. Debian
        # policy puts the licence in the package's doc directory as `copyright`.
        zsh_dir=/usr/share/zsh/vendor-completions
        license_dir=/usr/share/doc/ventulusd
        license_name=copyright
        ;;
    rpm)
        zsh_dir=/usr/share/zsh/site-functions
        license_dir=/usr/share/licenses/ventulusd
        license_name=LICENSE
        ;;
    tarball)
        # The one directory both distributions' zsh read.
        zsh_dir=/usr/local/share/zsh/site-functions
        license_dir=/usr/share/licenses/ventulusd
        license_name=LICENSE
        ;;
    *)
        echo "stage.sh: unknown flavour '$flavour' (deb, rpm or tarball)" >&2
        exit 2
        ;;
esac

doc=/usr/share/doc/ventulusd
put() { install -D -m "$1" -- "$2" "$dest$3"; }

# Binaries, the hand-back script the unit's ExecStopPost runs (DEC-382), and the
# sleep hook that widens the watchdog across a suspend (DEC-396).
put 755 "$bindir/ventulusd" /usr/bin/ventulusd
put 755 packaging/ventulus-restore-auto.sh /usr/bin/ventulus-restore-auto
put 755 packaging/ventulus-sleep-hook.sh /usr/lib/systemd/system-sleep/ventulusd

# System-tray client and its autostart entry and glyph (DEC-352).
put 755 "$bindir/ventulus-tray" /usr/bin/ventulus-tray
put 644 packaging/ventulus-tray.desktop /etc/xdg/autostart/ventulus-tray.desktop
put 644 packaging/ventulus-symbolic.svg /usr/share/icons/hicolor/scalable/apps/ventulus-symbolic.svg

put 644 packaging/ventulusd.service /usr/lib/systemd/system/ventulusd.service

# Configuration. Every file under /etc is a conffile on Debian (debhelper makes
# it so) and %config(noreplace) on Fedora, and the tarball installer keeps an
# edited one. That is one file more than the PKGBUILD's backup=(): the tray's
# autostart entry, which pacman overwrites.
put 644 packaging/daemon.toml.example /etc/ventulus/daemon.toml
put 644 packaging/profiles/quiet.json /etc/ventulus/profiles/quiet.json
put 644 packaging/modules-load.d/ventulus.conf /etc/modules-load.d/ventulus.conf

# Opt-in examples, documentation only (DEC-203, DEC-204, DEC-483).
put 644 packaging/99-ventulus.rules "$doc/99-ventulus.rules.example"
put 644 packaging/superio-port-probe.conf.example "$doc/superio-port-probe.conf.example"
put 644 packaging/nvidia-telemetry.conf.example "$doc/nvidia-telemetry.conf.example"
put 644 packaging/openfan-firmware-write.conf.example "$doc/openfan-firmware-write.conf.example"

# Super-I/O probe guard (DEC-332, DEC-424): vendor policy, under /usr/lib.
put 755 packaging/ventulus-superio-guard /usr/lib/ventulus/ventulus-superio-guard
put 644 packaging/modprobe.d-ventulus-superio.conf /usr/lib/modprobe.d/ventulus-superio.conf

put 644 "$mandir/ventulusd.1" /usr/share/man/man1/ventulusd.1
put 644 "$mandir/ventulus-tray.1" /usr/share/man/man1/ventulus-tray.1

for prog in ventulusd ventulus-tray; do
    put 644 "completions/$prog.bash" "/usr/share/bash-completion/completions/$prog"
    put 644 "completions/_$prog" "$zsh_dir/_$prog"
    put 644 "completions/$prog.fish" "/usr/share/fish/vendor_completions.d/$prog.fish"
done

put 644 README.md "$doc/README.md"
put 644 CHANGELOG.md "$doc/CHANGELOG.md"
put 644 docs/USER_GUIDE.md "$doc/USER_GUIDE.md"
put 644 docs/ADRs/001-ipc-transport.md "$doc/ADRs/001-ipc-transport.md"
put 644 docs/ADRs/002-runtime-config-split.md "$doc/ADRs/002-runtime-config-split.md"

put 644 LICENSE "$license_dir/$license_name"
put 644 NOTICE.md "$license_dir/NOTICE.md"
