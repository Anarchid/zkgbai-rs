#!/usr/bin/env bash
# Replay a match's demo headless in the match's own write-dir and save the
# game at a frame (save_widget.lua): tests /save during demo playback.
#
# usage: ARENA_SAVE_FRAME=12000 [ARENA_SAVE_NAME=name] demosave.sh [-e DIR] [-t SECONDS] <match out dir> [demo.sdfz]
#   The demo defaults to the newest *.sdfz in the match dir. Output in
#   <match dir>/demosave/{infolog.txt,stdout.txt}; the save in wd/Saves/.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
ENGINE_DIR=""
TIMEOUT=600
while getopts "e:t:" opt; do
    case $opt in
        e) ENGINE_DIR=$OPTARG ;;
        t) TIMEOUT=$OPTARG ;;
        *) sed -n '2,7p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -ge 1 ] || { sed -n '2,7p' "$0"; exit 2; }
MATCH=$1
DEMO=${2:-$(ls -t "$MATCH"/*.sdfz | head -1)}
WD="$MATCH/wd"
[ -d "$WD" ] && [ -f "$DEMO" ] || { echo "need $WD and a demo" >&2; exit 2; }
if [ -z "$ENGINE_DIR" ]; then
    ENGINE_DIR=$(ls -d "$HOME"/.spring/engine/linux64/20*/ 2>/dev/null | sort | tail -1)
fi
ENGINE="${ENGINE_DIR%/}/${ARENA_ENGINE_BIN:-spring-headless}"
FRAME=${ARENA_SAVE_FRAME:-0}
NAME=${ARENA_SAVE_NAME:-demo_$FRAME}
OUT="$MATCH/demosave"
mkdir -p "$OUT"
sed -e "s/@SAVE_FRAME@/$FRAME/" -e "s/@SAVE_NAME@/$NAME/" "$here/save_widget.lua" > "$WD/LuaUI/Widgets/save_widget.lua"
rm -f "$WD/LuaUI/Widgets/reload_widget.lua"
# The loader widget asks for the arena speed (a replay otherwise plays at 1x).
sed -e "s/@SPEED@/${ARENA_SPEED:-20}/" -e "s/@MAX_FRAMES@/0/" "$here/load_widget.lua" > "$WD/LuaUI/Widgets/load_widget.lua"
LOG="$WD/infolog.txt"
[ -f "$LOG" ] && mv "$LOG" "$WD/infolog.pre-demosave.txt"
ARENA_CONF=${ARENA_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/zkgbai/arena.conf}
# shellcheck source=/dev/null
[ -f "$ARENA_CONF" ] && . "$ARENA_CONF"
launch=()
if [ -n "${ARENA_ENGINE_MEMORY:-}" ] && systemd-run --user --scope -q true 2>/dev/null; then
    launch=(systemd-run --user --scope -q -p MemoryMax="$ARENA_ENGINE_MEMORY" -p MemorySwapMax=0 --)
fi
# The demo path must be absolute: the engine resolves relative ones in the write-dir.
timeout "$TIMEOUT" "${launch[@]}" "$ENGINE" --write-dir "$WD" "$(readlink -f "$DEMO")" > "$OUT/stdout.txt" 2>&1
status=$?
cp "$LOG" "$OUT/infolog.txt" 2>/dev/null
rm -f "$WD/LuaUI/Widgets/save_widget.lua" "$WD/LuaUI/Widgets/load_widget.lua"
echo "DEMOSAVE status=$status save=$WD/Saves/$NAME.ssf $(ls -la "$WD/Saves/$NAME.ssf" 2>/dev/null | awk '{print $5" bytes"}' || echo missing)"
grep -n "ARENA_SAVE\|LSH::\|ILoadSaveHandler\|Segmentation\|demo\b.*\(end\|finish\)\|Demo" "$OUT/infolog.txt" | head -8
