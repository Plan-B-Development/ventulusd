#!/bin/bash
# /usr/lib/ventulus/uninstall: remove a ventulusd installed from the tarball
# (DEC-499).
#
# Stops the daemon through systemd first, while the hand-back script is still
# installed, so its ExecStopPost gives every fan header back (DEC-382). A
# configuration file you edited is kept; the daemon's state in /var/lib/ventulus
# (header roles, the active profile) and any drop-in you added under
# /etc/systemd/system/ventulusd.service.d are left alone.
set -euo pipefail

die() {
    echo "uninstall: $*" >&2
    exit 1
}

# --root DIR removes the install under DIR and asks systemd nothing. It exists
# for the test suite.
root=
if [ "${1:-}" = --root ]; then
    [ -n "${2:-}" ] || die "--root needs a directory"
    root=${2%/}
    shift 2
fi
[ $# = 0 ] || die "usage: $0 [--root DIR]"
manifest=/usr/lib/ventulus/tarball-manifest

[ -r "$root$manifest" ] || die "no tarball install of ventulusd here ($manifest is missing)"

if [ -z "$root" ]; then
    [ "$(id -u)" = 0 ] || die "run it as root: sudo $0"
    if [ -d /run/systemd/system ]; then
        # Decided by what is running, not by the unit's load state: a masked
        # unit or one with a broken drop-in can still have a daemon running.
        # A restart pending after a crash counts as running.
        state=$(systemctl show -p ActiveState --value ventulusd.service)
        if [ "$state" != inactive ] && [ "$state" != failed ]; then
            systemctl stop ventulusd.service
        fi
        # Nothing is removed while a daemon runs: the hand-back script must stay.
        state=$(systemctl show -p ActiveState --value ventulusd.service)
        if [ "$state" != inactive ] && [ "$state" != failed ]; then
            die "ventulusd is still $state; nothing was removed"
        fi
        # Disabled while its unit file still exists to say what to unlink.
        systemctl disable --quiet ventulusd.service 2> /dev/null || true
    fi
fi

kept=()
while read -r sum _mode path; do
    dst=$root$path
    [ -e "$dst" ] || continue
    if [[ $path == /etc/* ]] && [ "$(sha256sum < "$dst" | cut -d' ' -f1)" != "$sum" ]; then
        kept+=("$path")
        continue
    fi
    rm -f -- "$dst"
done < "$root$manifest"
rm -f -- "$root$manifest"

for dir in /etc/ventulus/profiles /etc/ventulus /usr/lib/ventulus \
    /usr/share/doc/ventulusd/ADRs /usr/share/doc/ventulusd /usr/share/licenses/ventulusd; do
    rmdir -- "$root$dir" 2> /dev/null || true
done

if [ -z "$root" ] && [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi

for path in "${kept[@]}"; do
    echo "Kept your edited $path"
done
echo "ventulusd is removed. Its state is left in /var/lib/ventulus."
