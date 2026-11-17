#!/bin/sh
# Builds target/linux/debian.ext4: the Debian 13 root file system of the
# Docker image, with rubrowser, the test pages and scripts/linux/init as
# /sbin/init. Made as root inside a Linux container, so file owners are
# kept and names stay case-sensitive.
set -e
cd "$(dirname "$0")/../.."
bin=target/aarch64-unknown-linux-gnu/release/rubrowser
[ -x "$bin" ] || { echo "run scripts/linux/build.sh first"; exit 1; }
mkdir -p target/linux
docker build -q -t rubrowser-linux scripts/linux >/dev/null
cid=$(docker create rubrowser-linux)
docker export "$cid" > target/linux/rootfs.tar
docker rm "$cid" >/dev/null
docker run --rm -v "$PWD:/w" debian:trixie-slim sh -c "
  set -e
  apt-get update -qq && apt-get install -y -qq e2fsprogs >/dev/null 2>&1
  mkdir /rootfs && tar -xf /w/target/linux/rootfs.tar -C /rootfs
  install -m 755 /w/$bin /rootfs/usr/local/bin/rubrowser
  cp /w/scripts/linux/page.html /w/scripts/linux/js.html /rootfs/root/
  rm -f /rootfs/sbin/init && install -m 755 /w/scripts/linux/init /rootfs/sbin/init
  rm -f /w/target/linux/debian.ext4
  mke2fs -q -t ext4 -L debian -d /rootfs /w/target/linux/debian.ext4 1024M"
rm target/linux/rootfs.tar
ls -l target/linux/debian.ext4
