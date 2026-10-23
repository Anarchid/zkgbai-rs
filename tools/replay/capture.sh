#!/usr/bin/env bash
# Play a demo (.sdfz) headless with the replay capture widget and keep its
# JSON lines dump (see replay_capture_widget.lua).
#
# usage: capture.sh [-e ENGINE_DIR] [-t SECONDS] [-S FRAMES] [-I N] [-D DIR]
#                   <demo.sdfz> <out dir>
#   writes <out dir>/replaycap.jsonl and <out dir>/infolog.txt
#   -S f1,f2,...  also write full engine saves (creg, like ZK's save menu) at
#                 those frames to <out dir>/saves/replay_f<frame>.ssf
#   -I N          ... and every N frames (30 = every game second, ~7 MB each)
#   -D DIR        write the saves straight to DIR instead (e.g. a big disk)
#
# Waits for the machine-wide engine budget like tools/arena/match.sh
# (ARENA_MAX_ENGINES, ARENA_MIN_FREE_MB, and the limits in ARENA_CONF).
set -uo pipefail

ENGINE_DIR=""
TIMEOUT=1800
SAVES=""
SAVE_EVERY=0
SAVE_DIR=""
while getopts "e:t:S:I:D:" opt; do
    case $opt in
        e) ENGINE_DIR=$OPTARG ;;
        t) TIMEOUT=$OPTARG ;;
        S) SAVES=$OPTARG ;;
        I) SAVE_EVERY=$OPTARG ;;
        D) SAVE_DIR=$(realpath -m "$OPTARG") ;;
        *) sed -n '2,15p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 2 ] || { sed -n '2,15p' "$0"; exit 2; }
DEMO=$(realpath "$1")
OUT=$(realpath -m "$2")

if [ -z "$ENGINE_DIR" ]; then
    # The demo's engine version is in its file name: <date>_<map>_<engine>.sdfz
    ver=$(basename "$DEMO" .sdfz); ver=${ver##*_}
    ENGINE_DIR="$HOME/.spring/engine/linux64/$ver"
    [ -d "$ENGINE_DIR" ] || ENGINE_DIR=$(ls -d "$HOME"/.spring/engine/linux64/20*/ | sort | tail -1)
fi
ENGINE="${ENGINE_DIR%/}/spring-headless"
[ -x "$ENGINE" ] || { echo "no spring-headless in $ENGINE_DIR" >&2; exit 2; }

mkdir -p "$OUT"
rm -f "$OUT/replaycap.jsonl"
WD="$OUT/wd"
rm -rf "$WD"
mkdir -p "$WD/LuaUI/Widgets" "$WD/demos"
CACHE=${ARENA_CACHE:-$(cd "$(dirname "$0")/../.." && pwd)/target/arena-cache}
[ -d "$CACHE" ] && ln -s "$CACHE" "$WD/cache"
sed "s/@SAVE_FRAMES@/$SAVES/; s/@SAVE_EVERY@/$SAVE_EVERY/" "$(dirname "$0")/replay_capture_widget.lua" \
    > "$WD/LuaUI/Widgets/replay_capture_widget.lua"
if [ -n "$SAVE_DIR" ]; then mkdir -p "$SAVE_DIR" && ln -s "$SAVE_DIR" "$WD/Saves"; fi
cp "$DEMO" "$WD/demos/replay.sdfz"
# The playback's local server binds HostPortDefault (8452): one port per run.
echo "HostPortDefault = $((20000 + $$ % 30000))" > "$WD/springsettings.cfg"

# The machine's limits, as in tools/arena/match.sh (ARENA_CONF).
ARENA_CONF=${ARENA_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/zkgbai/arena.conf}
# shellcheck source=/dev/null
[ -f "$ARENA_CONF" ] && . "$ARENA_CONF"
ARENA_MAX_ENGINES=${ARENA_MAX_ENGINES:-${ARENA_ENGINE_CEILING:-6}}
if [ -n "${ARENA_ENGINE_CEILING:-}" ] && [ "$ARENA_MAX_ENGINES" -gt "$ARENA_ENGINE_CEILING" ]; then
    ARENA_MAX_ENGINES=$ARENA_ENGINE_CEILING
fi
ARENA_MIN_FREE_MB=${ARENA_MIN_FREE_MB:-5000}
launch=()
if [ -n "${ARENA_ENGINE_MEMORY:-}" ] && systemd-run --user --scope -q true 2>/dev/null; then
    launch=(systemd-run --user --scope -q -p MemoryMax="$ARENA_ENGINE_MEMORY" -p MemorySwapMax=0 --)
fi
exec 9> "${TMPDIR:-/tmp}/zkgbai-arena.lock"
while :; do
    flock 9
    engines=$(pgrep -c -f '[s]pring-headless --write-dir')
    free_mb=$(awk '/MemAvailable/ { print int($2 / 1024) }' /proc/meminfo)
    if [ "$engines" -lt "$ARENA_MAX_ENGINES" ] && [ "$free_mb" -ge "$ARENA_MIN_FREE_MB" ]; then break; fi
    flock -u 9
    sleep 10
done
"${launch[@]}" "$ENGINE" --write-dir "$WD" "$WD/demos/replay.sdfz" > "$OUT/stdout.txt" 2>&1 &
PID=$!
sleep 15
flock -u 9
exec 9>&-

start=$(date +%s)
while kill -0 "$PID" 2>/dev/null; do
    sleep 1
    if [ $(( $(date +%s) - start )) -ge "$TIMEOUT" ]; then
        kill "$PID" 2>/dev/null; sleep 2; kill -9 "$PID" 2>/dev/null
        echo "timeout" >&2
        break
    fi
done
wait "$PID" 2>/dev/null

cp "$WD/infolog.txt" "$OUT/infolog.txt" 2>/dev/null
mv "$WD/replaycap.jsonl" "$OUT/replaycap.jsonl" 2>/dev/null
if [ -z "$SAVE_DIR" ] && [ -d "$WD/Saves" ]; then mkdir -p "$OUT/saves" && cp "$WD"/Saves/* "$OUT/saves/" 2>/dev/null; fi
rm -rf "$WD"
if [ -s "$OUT/replaycap.jsonl" ]; then
    echo "CAPTURE $(wc -l < "$OUT/replaycap.jsonl") lines, last: $(tail -c 200 "$OUT/replaycap.jsonl")"
else
    echo "CAPTURE failed; see $OUT/infolog.txt" >&2
    exit 1
fi
