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
sed "s/@VERSION@/$version/" "$src/AIInfo.lua" > "$dst/.AIInfo.lua.tmp"
# The lobby build serves the operator and keeps a journal unless told not to.
python3 - "$src/AIOptions.lua" "$dst/.AIOptions.lua.tmp" <<'PY'
import re, sys
s = open(sys.argv[1]).read()
for key in ("operator", "journal"):
    s, n = re.subn(r"(key\s*=\s*'%s',(?:(?!\},)[\s\S])*?def\s*=\s*)''" % key, r"\1'on'", s)
    assert n == 1, key
open(sys.argv[2], "w").write(s)
PY
# A Lua file that does not parse (one stray quote in a desc does it) breaks
# the lobby's whole AI list, and the lobby logs nothing about it. Both files
# must parse before anything goes in. Line numbers match the sources.
luaparse() {
    if command -v luac > /dev/null; then luac -p "$1"
    elif command -v lua > /dev/null; then lua - "$1" <<< 'assert(loadfile(arg[1]))'
    else echo "no luac or lua to check $1 with" >&2; return 1; fi
}
for f in AIInfo.lua AIOptions.lua; do
    if ! luaparse "$dst/.$f.tmp"; then
        rm -f "$dst/.AIInfo.lua.tmp" "$dst/.AIOptions.lua.tmp"
        rmdir "$dst" 2> /dev/null || true
        echo "$src/$f does not parse; nothing installed" >&2
        exit 1
    fi
done
put "$lib" libSkirmishAI.so
# The built-in banter lexicon, for tuning in ../banter.toml (see its header).
put "$src/banter.toml" banter.example.toml
mv -f "$dst/.AIInfo.lua.tmp" "$dst/AIInfo.lua"
mv -f "$dst/.AIOptions.lua.tmp" "$dst/AIOptions.lua"
echo "installed $(git -C "$here" rev-parse --short HEAD 2>/dev/null) as ZKGBAI $version in $dst (operator and journal on)"
grep -n -A4 "key    = 'operator'\|key    = 'journal'" "$dst/AIOptions.lua" | grep "def"
