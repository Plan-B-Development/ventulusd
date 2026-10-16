#!/bin/bash
# build.sh: the Debian/Ubuntu, Fedora and tarball packages of ventulusd (DEC-499).
# Run from the repository root.
#
#   build.sh binaries <work>                      Debian 13: cargo build, strip and
#                                                 man pages into <work>/{bin,man}
#   build.sh deb <work> <version> <rev> <out>     Debian 13: ventulusd_<version>-<rev>_amd64.deb
#   build.sh tarball <work> <version> <out>       ventulusd-<version>-x86_64.tar.gz
#   build.sh rpm <work> <version> <rev> <out>     Fedora: ventulusd-<version>-<rev>.x86_64.rpm
#
# One build serves all three formats: the binaries are built once on Debian 13,
# the oldest glibc of the supported distributions, and every package carries the
# same bytes, laid out by packaging/linux/stage.sh.
set -euo pipefail

die() {
    echo "build.sh: $*" >&2
    exit 1
}

# The commit's time, so a rebuild of the same commit stamps the same dates.
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}

binaries() {
    local work=$1
    cargo build --release --locked
    install -d "$work/bin" "$work/man"
    # Stripped once here rather than by each format's tooling, so the three
    # packages carry identical binaries.
    local target=${CARGO_TARGET_DIR:-target}/release
    install -m 755 -s "$target/ventulusd" "$target/ventulus-tray" "$work/bin/"
    scdoc < man/ventulusd.1.scd > "$work/man/ventulusd.1"
    scdoc < man/ventulus-tray.1.scd > "$work/man/ventulus-tray.1"
}

deb() {
    local work=$1 version=$2 rev=$3 out=$4 tmp
    work=$(realpath "$work")
    tmp=$(mktemp -d)
    mkdir -p "$tmp/src"
    cp -r packaging/debian "$tmp/src/debian"
    cat > "$tmp/src/debian/changelog" <<EOF
ventulusd ($version-$rev) unstable; urgency=medium

  * Ventulus daemon $version; see /usr/share/doc/ventulusd/CHANGELOG.md.

 -- Plan-B-Development <https://github.com/Plan-B-Development>  $(date -R -u -d "@$SOURCE_DATE_EPOCH")
EOF
    # -d: the toolchain comes from rustup, not Build-Depends; nothing is
    # compiled here anyway (debian/rules packs <work>/bin).
    (cd "$tmp/src" && VENTULUS_REPO=$OLDPWD VENTULUS_WORK=$work \
        dpkg-buildpackage -b -us -uc -d)
    install -d "$out"
    cp "$tmp/ventulusd_${version}-${rev}_amd64.deb" "$out/"
    rm -rf "$tmp"
}

tarball() {
    local work=$1 version=$2 out=$3 tmp name
    name=ventulusd-$version-x86_64
    tmp=$(mktemp -d)
    packaging/linux/stage.sh tarball "$work/bin" "$work/man" "$tmp/$name/files"
    install -D -m 755 packaging/linux/uninstall.sh "$tmp/$name/files/usr/lib/ventulus/uninstall"
    install -m 755 packaging/linux/install.sh "$tmp/$name/install.sh"
    # install.sh reads this: each file's sha256, mode and path. It tells an
    # edited configuration file from a shipped one, and lists what to remove.
    (cd "$tmp/$name/files" && find . -type f -printf '%P\n' | LC_ALL=C sort |
        while read -r f; do
            printf '%s %s /%s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c %a "$f")" "$f"
        done) > "$tmp/$name/MANIFEST"
    install -d "$out"
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$SOURCE_DATE_EPOCH" \
        -C "$tmp" -czf "$out/$name.tar.gz" "$name"
    rm -rf "$tmp"
}

rpm_pkg() {
    local work=$1 version=$2 rev=$3 out=$4 tmp stage list
    tmp=$(mktemp -d)
    stage=$tmp/stage
    packaging/linux/stage.sh rpm "$work/bin" "$work/man" "$stage"
    # The %files list, generated from the staged tree so it cannot drift from
    # it: /etc files are configuration an upgrade must not overwrite, the
    # licences are %license, man pages carry a glob for rpm's compression, and
    # the directories that belong to this package alone are owned.
    list=$tmp/files.list
    (cd "$stage" && find . -type f -printf '/%P\n' | LC_ALL=C sort | while read -r f; do
        case "$f" in
            /etc/*) echo "%config(noreplace) $f" ;;
            /usr/share/licenses/*) echo "%license $f" ;;
            /usr/share/man/*) echo "$f*" ;;
            *) echo "$f" ;;
        esac
    done
    find . -mindepth 1 -type d -printf '/%P\n' | LC_ALL=C sort | grep -E '/ventulus(d)?(/|$)' |
        sed 's/^/%dir /') > "$list"
    rpmbuild -bb \
        --define "_topdir $tmp/rpmbuild" \
        --define "ventulus_version $version" \
        --define "ventulus_release $rev" \
        --define "ventulus_stage $stage" \
        --define "ventulus_filelist $list" \
        packaging/rpm/ventulusd.spec
    install -d "$out"
    cp "$tmp/rpmbuild/RPMS/x86_64/ventulusd-$version-$rev.x86_64.rpm" "$out/"
    rm -rf "$tmp"
}

[ -f packaging/linux/stage.sh ] || die "run from the repository root"
cmd=${1:-}
shift || true
case "$cmd" in
    binaries) [ $# = 1 ] || die "usage: build.sh binaries <work>"; binaries "$@" ;;
    deb) [ $# = 4 ] || die "usage: build.sh deb <work> <version> <rev> <out>"; deb "$@" ;;
    tarball) [ $# = 3 ] || die "usage: build.sh tarball <work> <version> <out>"; tarball "$@" ;;
    rpm) [ $# = 4 ] || die "usage: build.sh rpm <work> <version> <rev> <out>"; rpm_pkg "$@" ;;
    *) die "usage: build.sh binaries|deb|tarball|rpm ..." ;;
esac
