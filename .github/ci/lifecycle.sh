#!/bin/bash
# lifecycle.sh <deb|rpm|tarball> <dir>: the package lifecycle of ventulusd on a
# system booted with systemd, as root (DEC-499, XD-3). ci.yml's `lifecycle` job
# runs it in a privileged container per distribution; it runs unchanged in a VM.
#
# <dir> holds two builds of the same version, revision 1 and 2:
#   deb      ventulusd_*-1_amd64.deb and ventulusd_*-2_amd64.deb
#   rpm      ventulusd-*-1.x86_64.rpm and ventulusd-*-2.x86_64.rpm
#   tarball  ventulusd-*-x86_64.tar.gz (installed twice: the upgrade path)
#
# [SAFETY] What it proves: no way of stopping, upgrading or removing the package
# skips the hand-back. Each step plants a record saying a fake hwmon header was
# found in automatic mode (2), leaves that header in manual mode at duty 77, does
# the step, and requires mode 2 back and the record consumed. That is
# ExecStopPost (/usr/bin/ventulus-restore-auto) running on that path (DEC-382).
# A test-only drop-in points it at the fake tree (VENTULUS_SYSFS_ROOT); no profile
# is active, so the daemon writes nothing to the machine's own /sys.
#
# It also checks each platform's service policy: Debian and Ubuntu enable and
# start on install, Fedora's preset and the tarball leave the unit disabled, an
# upgrade restarts a running daemon, removal stops and disables it, and an edited
# configuration file survives both.
set -uo pipefail

format=${1:?usage: lifecycle.sh deb|rpm|tarball <dir>}
dir=$(realpath "${2:?usage: lifecycle.sh deb|rpm|tarball <dir>}")
fake=/srv/ventulus-fakesys
header=$fake/devices/platform/ventulus-lifecycle.0/hwmon/hwmon9
record=/run/ventulus/hwmon-handback
conf=/etc/ventulus/daemon.toml
failures=0
steps=0

pass() { steps=$((steps + 1)); echo "PASS  $*"; }
fail() { steps=$((steps + 1)); failures=$((failures + 1)); echo "FAIL  $*"; }
check() {
    local label=$1
    shift
    if "$@"; then pass "$label"; else fail "$label"; fi
}

unit_is() { [ "$(systemctl "$1" ventulusd.service 2> /dev/null)" = "$2" ]; }

wait_active() {
    local _
    for _ in $(seq 60); do
        unit_is is-active active && return 0
        sleep 1
    done
    systemctl status ventulusd.service --no-pager
    return 1
}

pkg_version() {
    case "$format" in
        deb) dpkg-query -W -f '${Version}' ventulusd 2> /dev/null ;;
        rpm) rpm -q --qf '%{VERSION}-%{RELEASE}' ventulusd 2> /dev/null ;;
        tarball) test -e /usr/lib/ventulus/tarball-manifest && echo tarball ;;
    esac
}

pkg_install() {
    case "$format" in
        deb) DEBIAN_FRONTEND=noninteractive apt-get install -y -q "$1" ;;
        rpm) dnf -y -q install "$1" ;;
        tarball) "$1/install.sh" ;;
    esac
}

pkg_upgrade() {
    case "$format" in
        deb) DEBIAN_FRONTEND=noninteractive apt-get install -y -q "$1" ;;
        rpm) dnf -y -q upgrade "$1" ;;
        tarball) "$1/install.sh" ;;
    esac
}

pkg_remove() {
    case "$format" in
        deb) DEBIAN_FRONTEND=noninteractive apt-get remove -y -q ventulusd ;;
        rpm) dnf -y -q remove ventulusd ;;
        tarball) /usr/lib/ventulus/uninstall ;;
    esac
}

# The fake header's mode, polled while ExecStopPost may still be running.
header_back_in_auto() {
    local _
    for _ in $(seq 40); do
        [ "$(cat "$header/pwm1_enable")" = 2 ] && [ ! -e "$record" ] && return 0
        sleep 0.25
    done
    echo "pwm1_enable=$(cat "$header/pwm1_enable") record=$(test -e "$record" && echo present || echo consumed)"
    return 1
}

plant() {
    echo 1 > "$header/pwm1_enable"
    echo 77 > "$header/pwm1"
    install -d -m 755 /run/ventulus
    printf '# planted by lifecycle.sh\n%s\t%s\tmode\t2\n' "$header/pwm1_enable" "$header/pwm1" > "$record"
}

# replayed <label> <command...>: with the daemon running, plant a record, run
# the command, and require the record replayed and consumed.
replayed() {
    local label=$1
    shift
    if ! unit_is is-active active; then
        systemctl start ventulusd.service
        wait_active || { fail "$label (the daemon would not start first)"; return; }
    fi
    plant
    if ! "$@"; then
        fail "$label (the command failed)"
        return
    fi
    check "$label: the fans were handed back" header_back_in_auto
}

# shellcheck source=/dev/null
echo "== ventulusd lifecycle: $format ($(. /etc/os-release && echo "$PRETTY_NAME"), $(systemctl --version | head -1))"
[ -d /run/systemd/system ] || { echo "systemd is not running"; exit 1; }

case "$format" in
    deb)
        old=$(ls "$dir"/ventulusd_*-1_amd64.deb)
        new=$(ls "$dir"/ventulusd_*-2_amd64.deb)
        ;;
    rpm)
        old=$(ls "$dir"/ventulusd-*-1.x86_64.rpm)
        new=$(ls "$dir"/ventulusd-*-2.x86_64.rpm)
        ;;
    tarball)
        tar -xzf "$(ls "$dir"/ventulusd-*-x86_64.tar.gz)" -C /tmp
        old=$(ls -d /tmp/ventulusd-*-x86_64)
        new=$old
        ;;
    *) echo "unknown format $format"; exit 2 ;;
esac

# The fake tree exists before the first start, so the drop-in's ReadWritePaths
# can bind it.
mkdir -p "$header"
echo 2 > "$header/pwm1_enable"
echo 128 > "$header/pwm1"
mkdir -p /etc/systemd/system/ventulusd.service.d
printf '[Service]\nEnvironment=VENTULUS_SYSFS_ROOT=%s\nReadWritePaths=%s\n' "$fake" "$fake" \
    > /etc/systemd/system/ventulusd.service.d/lifecycle-test.conf

check "install revision 1" pkg_install "$old"
case "$format" in
    deb)
        check "Debian/Ubuntu policy: enabled on install" unit_is is-enabled enabled
        check "Debian/Ubuntu policy: started on install" wait_active
        ;;
    rpm | tarball)
        check "$format policy: left disabled on install" unit_is is-enabled disabled
        check "$format policy: not started on install" unit_is is-active inactive
        check "enable --now starts it" systemctl enable --now ventulusd.service
        check "it reaches active" wait_active
        ;;
esac
check "GET /status answers on the socket" \
    curl -sf --unix-socket /run/ventulus/ventulus.sock -o /dev/null http://localhost/status

replayed "systemctl stop" systemctl stop ventulusd.service
replayed "systemctl restart" systemctl restart ventulusd.service
# The main process only, as a crash or the OOM killer would. `systemctl kill`
# signals the whole cgroup and would kill ExecStopPost too.
# shellcheck disable=SC2016 # expanded by the inner shell, after the plant
replayed "SIGKILL of the main process" \
    bash -c 'kill -KILL "$(systemctl show -p MainPID --value ventulusd.service)"'
check "Restart=on-failure brings it back after the SIGKILL" wait_active

echo "# edited by lifecycle.sh" >> "$conf"
before=$(pkg_version)
replayed "upgrade to revision 2 while running" pkg_upgrade "$new"
check "the upgrade restarted it onto the new version" wait_active
[ "$format" = tarball ] || check "the package version moved ($before -> $(pkg_version))" \
    test "$(pkg_version)" != "$before"
check "the edited $conf survived the upgrade" grep -q '^# edited by lifecycle.sh' "$conf"

replayed "removal while running" pkg_remove
check "removal stopped it" unit_is is-active inactive
check "removal left no unit file" test ! -e /usr/lib/systemd/system/ventulusd.service
check "removal left no daemon binary" test ! -e /usr/bin/ventulusd
case "$format" in
    deb | tarball) check "the edited $conf was kept" grep -q '^# edited by lifecycle.sh' "$conf" ;;
    rpm) check "the edited $conf was kept as .rpmsave" grep -q '^# edited by lifecycle.sh' "$conf.rpmsave" ;;
esac
if [ "$format" = deb ]; then
    check "purge succeeds" env DEBIAN_FRONTEND=noninteractive apt-get purge -y -q ventulusd
fi

# The second install of the same package is not a fresh system any more: the
# state survived, and the hand-back must still run on its stop.
check "reinstall after removal" pkg_install "$old"
[ "$format" = deb ] || systemctl enable --now ventulusd.service
check "it runs again" wait_active
replayed "removal after reinstall" pkg_remove

if [ "$format" = tarball ]; then
    # Masked while it runs (`systemctl mask` without --now does not stop it).
    # systemd then holds an empty unit and runs no ExecStopPost on any stop, so
    # a planted record cannot be replayed here, on any format. The daemon's own
    # SIGTERM shutdown still gives back what it took; the uninstaller must stop
    # it rather than delete the files under a running daemon.
    pkg_install "$old" > /dev/null
    systemctl enable --now --quiet ventulusd.service
    wait_active > /dev/null
    systemctl mask --runtime --quiet ventulusd.service
    check "uninstall with the unit masked while running" pkg_remove
    check "it stopped the masked daemon" unit_is is-active inactive
    systemctl unmask --runtime --quiet ventulusd.service
fi

# A container cannot load modules, so systemd-modules-load fails there by design.
failed=$(systemctl --failed --plain --no-legend | awk '{print $1}' | grep -v '^systemd-modules-load\.service$' | tr '\n' ' ')
check "no failed units${failed:+ (failed: $failed)}" test -z "$failed"

echo "== $((steps - failures))/$steps passed"
journalctl -u ventulusd.service --no-pager -n 40 -o short-precise || true
[ "$failures" = 0 ]
