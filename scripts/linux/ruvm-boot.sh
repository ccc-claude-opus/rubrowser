#!/bin/sh
# Boots target/linux/debian.ext4 in ruvm (https://github.com/ccc-claude-opus/ruvm, an
# AArch64 interpreter): Debian's arm64 kernel and ruvm's initramfs, which
# mounts the disk and runs its /sbin/init (scripts/linux/init). That runs
# rubrowser on the test pages and powers off; the screenshots are copied
# out to target/linux/ruvm-*.png.
#   scripts/linux/ruvm-boot.sh <ruvm checkout> [hvf|kvm]
# The ruvm checkout needs `cargo build --release` and
# `tools/linux/fetch-debian.sh arm64 && tools/linux/mkroot.sh arm64`.
# hvf / kvm (ruvm 1.2) run the guest on the host CPU instead of the interpreter, with 4 CPUs;
# hvf needs `tools/hvf-sign.sh` in the ruvm checkout after building.
set -e
cd "$(dirname "$0")/../.."
ruvm=${1:?usage: scripts/linux/ruvm-boot.sh <ruvm checkout> [hvf|kvm]}
accel=${2:+-accel $2 -smp 4}
disk=target/linux/debian.ext4
[ -f "$disk" ] || { echo "run scripts/linux/ruvm-root.sh first"; exit 1; }
# shellcheck disable=SC2086
"$ruvm/target/release/ruvm" -M aarch64 -m 2048 $accel \
    -kernel "$ruvm/tests/guest/out/linux/debian/arm64/linux" \
    -initrd "$ruvm/tests/guest/out/linux/root/arm64/initramfs.cpio.gz" \
    -drive file="$disk" -rng -no-reboot \
    -append "console=ttyAMA0 rdinit=/init" | tee target/linux/ruvm.log
debugfs=$(command -v debugfs || echo /opt/homebrew/opt/e2fsprogs/sbin/debugfs)
for p in page js; do
    "$debugfs" -R "dump /root/$p.png target/linux/ruvm-$p.png" "$disk" 2>/dev/null
done
ls -l target/linux/ruvm-*.png
