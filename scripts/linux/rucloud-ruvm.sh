#!/bin/sh
# Runs rucloud (https://github.com/ccc-claude-opus/rucloud) on Debian 13 + Node 24 in ruvm with
# hardware virtualization, and forwards it to http://127.0.0.1:18787/ on this machine, so that
# rubrowser here can use the app served from the VM:
#   scripts/linux/rucloud-ruvm.sh <ruvm checkout> <rucloud checkout> [hvf|kvm|tcg]
#   rubrowser http://127.0.0.1:18787/
# The root (target/linux/rucloud.ext4) is the node:24-trixie-slim image with rucloud, ruvm's static
# busybox (for ifconfig/route) and scripts/linux/rucloud-init as /sbin/init; it is built once and
# booted with -snapshot. Stop with Ctrl-C.
set -e
cd "$(dirname "$0")/../.."
ruvm=${1:?usage: scripts/linux/rucloud-ruvm.sh <ruvm checkout> <rucloud checkout> [hvf|kvm|tcg]}
rucloud=${2:?usage: scripts/linux/rucloud-ruvm.sh <ruvm checkout> <rucloud checkout> [hvf|kvm|tcg]}
accel=${3:-hvf}
disk=target/linux/rucloud.ext4
if [ ! -f "$disk" ]; then
    (cd "$rucloud" && cargo zigbuild --release --target aarch64-unknown-linux-gnu)
    mkdir -p target/linux/rucloud-root
    cp "$rucloud/target/aarch64-unknown-linux-gnu/release/rucloud" target/linux/rucloud-root/
    gunzip -c "$ruvm/tests/guest/out/linux/root/arm64/initramfs.cpio.gz" | (cd target/linux/rucloud-root && cpio -id --quiet bin/busybox)
    cid=$(docker create --platform linux/arm64 node:24-trixie-slim)
    docker export "$cid" > target/linux/rucloud-root/rootfs.tar
    docker rm "$cid" >/dev/null
    docker run --rm -v "$PWD:/w" debian:trixie-slim sh -c "
      set -e
      apt-get update -qq && apt-get install -y -qq e2fsprogs >/dev/null 2>&1
      mkdir /rootfs && tar -xf /w/target/linux/rucloud-root/rootfs.tar -C /rootfs
      install -m 755 /w/target/linux/rucloud-root/rucloud /rootfs/usr/local/bin/rucloud
      mkdir -p /rootfs/opt && install -m 755 /w/target/linux/rucloud-root/bin/busybox /rootfs/opt/busybox
      install -m 755 /w/scripts/linux/rucloud-init /rootfs/sbin/init
      mke2fs -q -t ext4 -L rucloud -d /rootfs /w/$disk 1024M"
fi
# shellcheck disable=SC2046
exec "$ruvm/target/release/ruvm" -M aarch64 -m 2048 $( [ "$accel" = tcg ] || echo "-accel $accel -smp 4" ) \
    -kernel "$ruvm/tests/guest/out/linux/debian/arm64/linux" \
    -initrd "$ruvm/tests/guest/out/linux/root/arm64/initramfs.cpio.gz" \
    -drive file="$disk" -snapshot -rng -nic user,hostfwd=tcp::18787-:8787 \
    -append "console=ttyAMA0 rdinit=/init"
