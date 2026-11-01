#!/bin/sh
# Renders a set of real sites with `rubrowser --screenshot` (in parallel)
# and writes an index.html showing them side by side, for comparing with
# another browser. Usage: scripts/survey.sh [OUT_DIR] [extra rubrowser args]
set -e
cd "$(dirname "$0")/.."
out="${1:-target/survey}"
[ $# -gt 0 ] && shift
mkdir -p "$out"
cargo build --release --quiet

sites="
wikipedia https://en.wikipedia.org/wiki/Rust_(programming_language)
zhwiki https://zh.wikipedia.org/wiki/Rust
github https://github.com/rust-lang/rust
x https://x.com/rustlang
mdn https://developer.mozilla.org/en-US/docs/Web/HTML
docsrs https://docs.rs/serde/latest/serde/
python https://www.python.org/
rust https://www.rust-lang.org/
bbc https://www.bbc.com/news
hn https://news.ycombinator.com/
reddit https://old.reddit.com/r/rust/
so https://stackoverflow.com/questions
ddg https://html.duckduckgo.com/html/?q=rust
apple https://www.apple.com/
example https://example.com/
"

echo "$sites" | while read -r name url; do
  [ -z "$name" ] && continue
  (
    start=$(date +%s)
    if ./target/release/rubrowser --screenshot "$out/$name.png" --width 1280 \
      --height 822 --timeout 40 "$@" "$url" >"$out/$name.log" 2>&1; then
      echo "$name: $(($(date +%s) - start))s"
    else
      echo "$name: FAILED (see $out/$name.log)"
    fi
  ) &
done
wait

{
  echo '<!doctype html><meta charset="utf-8"><title>rubrowser survey</title>'
  echo '<style>body{font:14px sans-serif;margin:16px}div{display:grid;grid-template-columns:repeat(auto-fill,minmax(420px,1fr));gap:16px}img{width:100%;border:1px solid #ccc}</style><div>'
  echo "$sites" | while read -r name url; do
    [ -z "$name" ] && continue
    echo "<figure><a href=\"$url\">$name</a><img src=\"$name.png\"></figure>"
  done
  echo '</div>'
} >"$out/index.html"
echo "wrote $out/index.html"
