#!/usr/bin/env bash
# Install the release build as the lobby's "ZKGBAI wip" (or another version)
# with the operator API and the run journal ON by default, atomically: each
# file goes in under a temporary name and is renamed over the old one, so a
# running game keeps its mapped library (a plain cp segfaults it).
#
# usage: install-wip.sh [version=wip] [data-dir=~/.spring]
set -e
version=${1:-wip}
data=${2:-$HOME/.spring}
here="$(cd "$(dirname "$0")/.." && pwd)"
lib="$here/target/release/libzkgbai.so"
src="$here/crates/zkgbai/data"
dst="$data/AI/Skirmish/ZKGBAI/$version"
[ -f "$lib" ] || { echo "no $lib: cargo build --release first" >&2; exit 1; }
if pgrep -x cargo > /dev/null; then echo "cargo is running; wait for it" >&2; exit 1; fi
mkdir -p "$dst"
put() { cp "$1" "$dst/.$2.tmp" && mv -f "$dst/.$2.tmp" "$dst/$2"; }
put "$lib" libSkirmishAI.so
sed "s/@VERSION@/$version/" "$src/AIInfo.lua" > "$dst/.AIInfo.lua.tmp" && mv -f "$dst/.AIInfo.lua.tmp" "$dst/AIInfo.lua"
# The lobby build serves the operator and keeps a journal unless told not to.
python3 - "$src/AIOptions.lua" "$dst/.AIOptions.lua.tmp" <<'PY'
import re, sys
s = open(sys.argv[1]).read()
for key in ("operator", "journal"):
    s, n = re.subn(r"(key\s*=\s*'%s',(?:(?!\},)[\s\S])*?def\s*=\s*)''" % key, r"\1'on'", s)
    assert n == 1, key
open(sys.argv[2], "w").write(s)
PY
mv -f "$dst/.AIOptions.lua.tmp" "$dst/AIOptions.lua"
echo "installed $(git -C "$here" rev-parse --short HEAD 2>/dev/null) as ZKGBAI $version in $dst (operator and journal on)"
grep -n -A4 "key    = 'operator'\|key    = 'journal'" "$dst/AIOptions.lua" | grep "def"
