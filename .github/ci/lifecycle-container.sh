#!/bin/bash
# lifecycle-container.sh <image> <deb|rpm|tarball> <dir>: run lifecycle.sh in
# <image> with systemd as PID 1, in a privileged podman container. ci.yml's
# `lifecycle` job runs it as root on an ubuntu-24.04 runner (DEC-499 phase 0
# proved this harness runs the shipped, hardened unit on all three
# distributions). <dir> holds the packages and is mounted read-only.
set -euo pipefail

image=${1:?usage: lifecycle-container.sh <image> <format> <dir>}
format=${2:?usage: lifecycle-container.sh <image> <format> <dir>}
dir=$(realpath "${3:?usage: lifecycle-container.sh <image> <format> <dir>}")
here=$(dirname "$(realpath "$0")")
name=lifecycle

case "$image" in
    debian:* | ubuntu:*)
        setup='export DEBIAN_FRONTEND=noninteractive && apt-get update -q && apt-get install -y -q --no-install-recommends systemd systemd-sysv dbus kmod curl ca-certificates procps'
        ;;
    fedora:*)
        setup='dnf -y -q install systemd kmod curl procps-ng tar gzip gawk'
        ;;
    *)
        echo "no setup for $image" >&2
        exit 2
        ;;
esac

ctx=$(mktemp -d)
cat > "$ctx/Containerfile" << EOF
FROM $image
RUN $setup
CMD ["/sbin/init"]
EOF
podman build -q -t ventulus-lifecycle "$ctx"
podman run -d --name "$name" --privileged --systemd=always \
    -v "$dir:/packages:ro" -v "$here:/ci:ro" ventulus-lifecycle > /dev/null

booted=
for _ in $(seq 120); do
    state=$(podman exec "$name" systemctl is-system-running 2> /dev/null || true)
    case "$state" in
        running | degraded) booted=$state; break ;;
    esac
    sleep 1
done
if [ -z "$booted" ]; then
    echo "systemd did not finish booting in $image (state: ${state:-none})" >&2
    podman logs "$name" | tail -40
    exit 1
fi

status=0
podman exec "$name" bash /ci/lifecycle.sh "$format" /packages || status=$?
podman rm -f "$name" > /dev/null
exit "$status"
