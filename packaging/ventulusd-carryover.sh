#!/bin/bash
# Installed as /usr/lib/ventulus/ventulusd-carryover; run by
# 90-ventulusd-carryover.hook at the end of the pacman transaction that
# installs ventulusd.
#
# The Ventulus rename. When ventulusd replaces control-ofc-daemon 4.x, the old
# package's pre_remove stops and disables control-ofc-daemon.service, so the
# machine would be left with no fan control until the user noticed. ventulusd's
# post_install records whether the old daemon was in use (it ran during this
# upgrade, or is still running, or is still enabled) in the marker below; this
# script, which runs after systemd has loaded the new unit, then:
#
#   1. stops a control-ofc-daemon that is somehow still running -- its package
#      files are gone, so its own ExecStopPost cannot give the fans back -- and
#      replays its hand-back records with ventulus-restore-auto, which reads the
#      same record format;
#   2. puts the compatibility socket link in place (ventulusd.tmpfiles), which
#      systemd-tmpfiles left alone while a stale 4.x socket file sat there;
#   3. enables and starts ventulusd.service.
#
# Starting a service from a package is not the Arch norm. It is deliberate here
# (DEC-493): this is an upgrade of a service that was already running, not a
# new one, and the alternative is a machine whose fans are silently uncontrolled
# after an ordinary pacman -Syu.
set -u

marker=/run/ventulusd-carryover
[[ -e $marker ]] || exit 0
rm -f -- "$marker"

old=control-ofc-daemon.service
old_runtime=/run/control-ofc
old_socket=$old_runtime/control-ofc.sock

if [[ ! -d /run/systemd/system ]]; then
    # A chroot or a container without systemd: nothing to start here.
    echo ":: ventulusd: enable it on the running system with"
    echo "   sudo systemctl enable --now ventulusd"
    exit 0
fi

# 1. A 4.x daemon still running.
if systemctl is-active --quiet "$old"; then
    echo ":: Stopping the Control-OFC daemon that is still running."
    systemctl stop "$old" || true
    RUNTIME_DIRECTORY=$old_runtime /usr/bin/ventulus-restore-auto || true
fi

# 2. The compatibility link, once nothing serves on the old socket. A 4.x
# daemon started by hand is not the unit, so ask the socket itself: curl exits 7
# only when nothing accepts the connection. A served socket is left in place,
# and ventulusd's startup probe then refuses to run beside that daemon.
if [[ -S $old_socket && ! -L $old_socket ]]; then
    curl -s --max-time 2 --unix-socket "$old_socket" -o /dev/null http://localhost/status
    if (( $? == 7 )); then
        rm -f -- "$old_socket"
    else
        echo ":: Something still serves on $old_socket; it is left in place."
    fi
fi
systemd-tmpfiles --create ventulusd.conf || true

# 3. The new service.
if systemctl enable --now ventulusd.service; then
    echo ":: ventulusd is running and enabled in place of control-ofc-daemon."
    echo "   Your settings, header roles and active profile were carried over."
else
    echo ":: ventulusd could not be started; check:"
    echo "   systemctl status ventulusd"
    echo "   sudo systemctl enable --now ventulusd"
fi
exit 0
