#!/bin/sh
# Runs the aarch64 Linux build in Debian 13 (Docker): screenshots of the
# test pages and of a web page, and the windowed browser on a virtual X11
# display (Xvfb), captured with xwd. Images go to target/linux/.
set -e
cd "$(dirname "$0")/../.."
bin=target/aarch64-unknown-linux-gnu/release/rubrowser
[ -x "$bin" ] || { echo "run scripts/linux/build.sh first"; exit 1; }
mkdir -p target/linux
docker build -q -t rubrowser-linux scripts/linux >/dev/null
docker run --rm -v "$PWD:/w" -w /w rubrowser-linux sh -c "
  set -e
  for p in page js; do
    $bin --screenshot target/linux/docker-\$p.png --timeout 5 file:///w/scripts/linux/\$p.html
  done
  $bin --screenshot target/linux/docker-web.png --timeout 15 https://en.wikipedia.org/wiki/Linux
  Xvfb :1 -screen 0 1280x900x24 >/dev/null 2>&1 &
  sleep 2
  DISPLAY=:1 $bin file:///w/scripts/linux/page.html >/dev/null 2>&1 &
  sleep 8
  DISPLAY=:1 xwd -root -silent | xwdtopnm 2>/dev/null | pnmtopng > target/linux/docker-x11.png
  echo 'wrote target/linux/docker-x11.png (the window on X11)'"
