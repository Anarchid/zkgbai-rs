#!/usr/bin/env bash
# Install the release build as the lobby's "ZKGBAI wip" (or another version)
# with the operator API and the run journal ON by default, atomically: each
# file goes in under a temporary name and is renamed over the old one, so a
# running game keeps its mapped library (a plain cp segfaults it).
#
# The lobby wip is the branch `wip` and nothing else. Work meant for the wip
# is merged (or cherry-picked) into `wip`, never stacked on a new wip-* branch,
# and `wip` is installed from a clean checkout of its tip whose release build
# carries that commit's stamp. Other versions install from anything.
#
# usage: install-wip.sh [version=wip] [data-dir=~/.spring]
set -e
version=${1:-wip}
data=${2:-$HOME/.spring}
here="$(cd "$(dirname "$0")/.." && pwd)"
lib="${CARGO_TARGET_DIR:-$here/target}/release/libzkgbai.so"
src="$here/crates/zkgbai/data"
dst="$data/AI/Skirmish/ZKGBAI/$version"
[ -f "$lib" ] || { echo "no $lib: cargo build --release first" >&2; exit 1; }
if pgrep -x cargo > /dev/null; then echo "cargo is running; wait for it" >&2; exit 1; fi
# build.rs stamps the library with `git describe --always --dirty --abbrev=8`.
stamp=$(git -C "$here" describe --always --dirty --abbrev=8 2> /dev/null || echo unknown)
if [ "$version" = wip ]; then
    refuse() { echo "not installing as wip: $1. The lobby wip installs only from branch wip: merge your work into wip, build its tip, install from there." >&2; exit 1; }
    branch=$(git -C "$here" symbolic-ref -q --short HEAD || true)
    [ "$branch" = wip ] || refuse "$here is on '${branch:-detached HEAD}', not wip"
    [ -z "$(git -C "$here" status --porcelain --untracked-files=no)" ] || refuse "$here has uncommitted changes"
    case "$stamp" in *-dirty|unknown) refuse "no clean stamp for $here ($stamp)";; esac
    if ! grep -qaF -- "$stamp" "$lib" || grep -qaF -- "$stamp-dirty" "$lib"; then
        refuse "$lib was not built from $stamp (rebuild it)"
    fi
fi
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
# What went in, for whoever asks later which code the lobby plays.
printf 'stamp %s\nbranch %s\ncommit %s\nfrom %s\nsha256 %s\ninstalled %s\n' "$stamp" \
    "$(git -C "$here" symbolic-ref -q --short HEAD || echo detached)" "$(git -C "$here" rev-parse HEAD 2> /dev/null)" \
    "$here" "$(sha256sum "$lib" | cut -d' ' -f1)" "$(date -Is)" > "$dst/.INSTALLED.tmp"
mv -f "$dst/.INSTALLED.tmp" "$dst/INSTALLED"
echo "installed $stamp as ZKGBAI $version in $dst (operator and journal on)"
grep -n -A4 "key    = 'operator'\|key    = 'journal'" "$dst/AIOptions.lua" | grep "def"
# A Chobby that has played a game keeps the library it loaded first: games
# in it go on with the old build until it is restarted (11-03 and 11-04:
# a night of lobby games ran a build several installs old). Say so.
for pid in $(pgrep -f -- '--menu Chobby' 2> /dev/null); do
    # (the engine names its thread recoil-main: match its binary)
    [[ "$(basename "$(readlink "/proc/$pid/exe" 2> /dev/null)")" == spring* ]] || continue
    started=$(ps -o lstart= -p "$pid" 2> /dev/null) || continue
    echo "WARNING: Chobby (pid $pid, running since $started) keeps the library it already loaded;" \
         "restart it to play $stamp (check a game's journal: \"k\":\"run\" -> build)" >&2
done
