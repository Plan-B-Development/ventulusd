#!/bin/bash
# install.sh: install ventulusd from this tarball, or upgrade an earlier tarball
# install (DEC-499).
#
#   sudo ./install.sh
#
# Files go to the same paths as the Debian, Fedora and Arch packages. It refuses
# when a package manager owns ventulusd; use that package manager instead.
# A configuration file you edited is kept, with the new default written beside it
# as <file>.new. Files an earlier version installed and this one no longer ships
# are removed. A running daemon is restarted through systemd, so its ExecStopPost
# gives every fan header back before the new version starts (DEC-382). A fresh
# install is left for you to enable.
#
# Uninstall with: sudo /usr/lib/ventulus/uninstall
set -euo pipefail

die() {
    echo "install.sh: $*" >&2
    exit 1
}

here=$(cd "$(dirname "$0")" && pwd)
# --root DIR installs under DIR and asks neither systemd nor a package manager
# anything. It exists for the test suite.
root=
if [ "${1:-}" = --root ]; then
    [ -n "${2:-}" ] || die "--root needs a directory"
    root=${2%/}
    shift 2
fi
[ $# = 0 ] || die "usage: $0 [--root DIR]"
manifest=/usr/lib/ventulus/tarball-manifest

sha() { sha256sum < "$1" | cut -d' ' -f1; }

# Replace $2 with $1 in one rename, so a running daemon, or an ExecStopPost
# starting at that moment, sees either the old file or the new one.
place() {
    local src=$1 dst=$2 mode=$3 tmp
    install -d -m 755 "${dst%/*}"
    tmp="${dst%/*}/.${dst##*/}.ventulus-new"
    install -m "$mode" "$src" "$tmp"
    mv -f -- "$tmp" "$dst"
}

if [ -z "$root" ]; then
    [ "$(id -u)" = 0 ] || die "run it as root: sudo $0"
    for path in /usr/bin/ventulusd /usr/lib/systemd/system/ventulusd.service; do
        if command -v dpkg > /dev/null && dpkg -S "$path" > /dev/null 2>&1; then
            die "$path belongs to a Debian package; upgrade it with apt"
        fi
        if command -v rpm > /dev/null && rpm -qf "$path" > /dev/null 2>&1; then
            die "$path belongs to an RPM package; upgrade it with dnf"
        fi
        if command -v pacman > /dev/null && pacman -Qqo "$path" > /dev/null 2>&1; then
            die "$path belongs to a pacman package; upgrade it with pacman"
        fi
    done
fi

[ -r "$here/MANIFEST" ] || die "no MANIFEST beside $0; is the tarball complete?"
while read -r sum _mode path; do
    [ "$(sha "$here/files$path")" = "$sum" ] || die "$path does not match MANIFEST; download the tarball again"
done < "$here/MANIFEST"

# What the previous tarball install shipped: an /etc file still matching it was
# never edited, so it can be replaced.
declare -A shipped=()
if [ -r "$root$manifest" ]; then
    while read -r sum _mode path; do shipped[$path]=$sum; done < "$root$manifest"
fi
declare -A current=()
kept=()

while read -r sum mode path; do
    current[$path]=1
    dst=$root$path
    if [[ $path == /etc/* ]] && [ -e "$dst" ]; then
        have=$(sha "$dst")
        [ "$have" = "$sum" ] && continue
        if [ "${shipped[$path]:-}" != "$have" ]; then
            place "$here/files$path" "$dst.new" "$mode"
            kept+=("$path")
            continue
        fi
    fi
    place "$here/files$path" "$dst" "$mode"
done < "$here/MANIFEST"

if [ -z "$root" ] && [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    if systemctl is-active --quiet ventulusd.service; then
        echo "Restarting ventulusd onto the new version."
    fi
    # Only a running daemon is restarted. The stop runs ExecStopPost, which
    # gives every fan header back as the old daemon found it.
    systemctl try-restart ventulusd.service
fi

# Files the previous version shipped and this one does not, unless edited.
# Removed only now: until the reload and restart above, systemd's loaded unit
# may still name an old file as its ExecStopPost.
for path in "${!shipped[@]}"; do
    [ -n "${current[$path]:-}" ] && continue
    dst=$root$path
    [ -e "$dst" ] || continue
    if [[ $path == /etc/* ]] && [ "$(sha "$dst")" != "${shipped[$path]}" ]; then
        kept+=("$path")
        continue
    fi
    rm -f -- "$dst"
done

install -m 644 "$here/MANIFEST" "$root$manifest"

for path in "${kept[@]}"; do
    echo "Kept your edited $path; the new default is $path.new"
done

if [ -z "$root" ] && [ -d /run/systemd/system ] &&
    ! systemctl is-enabled --quiet ventulusd.service 2> /dev/null; then
    cat << 'EOF'
ventulusd is installed.
  Start it now and at every boot:  sudo systemctl enable --now ventulusd
  Load the sensor modules:         sudo systemctl restart systemd-modules-load   (or reboot)
  Setup guide:                     /usr/share/doc/ventulusd/USER_GUIDE.md
  Check your fan hardware:         the Ventulus GUI's Hardware page names any driver your board needs
  Uninstall:                       sudo /usr/lib/ventulus/uninstall
EOF
fi
