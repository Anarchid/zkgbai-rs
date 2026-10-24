#!/usr/bin/env bash
# Resume a saved headless match (made by savematch.sh) from its engine
# save: same write-dir (AI install, Saves/), a start script with only the
# SaveFile block, and the arena widget's frame cap re-set.
#
# usage: loadmatch.sh [-f FRAMES] [-t SECONDS] [-e DIR] [-n SAVE] [-s SPEED]
#                     [-l LABEL] <match out dir>
#   -f FRAMES   absolute frame at which the arena widget quits (default 0 =
#               game over); the saved game's frame counter continues
#   -t SECONDS  wall-clock timeout (default 900)
#   -e DIR      engine dir (default: the one match.sh picks, newest)
#   -n SAVE     save name without .ssf (default: newest in wd/Saves)
#   -s SPEED    sim speed set after the load (default 20; a loaded game
#               otherwise runs at 1x, the saved MODOPTIONS are not applied)
#   -l LABEL    output subdir name under the match dir (default load)
# Output: <out>/<label>/{script.txt,stdout.txt,infolog.txt,journal/}.
# Environment as match.sh (ARENA_CONF ceiling/memory, ZKGBAI_* switches).
# Prints: RESULT winner=... frame=... panics=... errors=... dir=...
set -uo pipefail
MAX_FRAMES=0
TIMEOUT=900
ENGINE_DIR=""
SAVE=""
LABEL=load
SPEED=20
while getopts "f:t:e:n:s:l:" opt; do
    case $opt in
        f) MAX_FRAMES=$OPTARG ;;
        t) TIMEOUT=$OPTARG ;;
        e) ENGINE_DIR=$OPTARG ;;
        n) SAVE=$OPTARG ;;
        s) SPEED=$OPTARG ;;
        l) LABEL=$OPTARG ;;
        *) sed -n '2,17p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || { sed -n '2,17p' "$0"; exit 2; }
MATCH=$1
WD="$MATCH/wd"
[ -d "$WD/Saves" ] || { echo "no $WD/Saves (run savematch.sh with -o $MATCH first)" >&2; exit 2; }
if [ -z "$SAVE" ]; then
    SAVE=$(ls -t "$WD"/Saves/*.ssf 2>/dev/null | head -1)
    SAVE=$(basename "${SAVE%.ssf}")
fi
[ -f "$WD/Saves/$SAVE.ssf" ] || { echo "no $WD/Saves/$SAVE.ssf" >&2; exit 2; }

if [ -z "$ENGINE_DIR" ]; then
    ENGINE_DIR=$(ls -d "$HOME"/.spring/engine/linux64/20*/ 2>/dev/null | sort | tail -1)
fi
ENGINE="${ENGINE_DIR%/}/${ARENA_ENGINE_BIN:-spring-headless}"
[ -x "$ENGINE" ] || { echo "no ${ARENA_ENGINE_BIN:-spring-headless} in $ENGINE_DIR" >&2; exit 2; }

OUT="$MATCH/$LABEL"
mkdir -p "$OUT"
if [ "${ZKGBAI_JOURNAL:-}" = out ]; then export ZKGBAI_JOURNAL="$OUT/journal"; fi
ARENA_CONF=${ARENA_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/zkgbai/arena.conf}
# shellcheck source=/dev/null
[ -f "$ARENA_CONF" ] && . "$ARENA_CONF"
# Re-cap the arena widget for the resumed run; the saver must not fire again.
sed "s/@MAX_FRAMES@/$MAX_FRAMES/" "$(dirname "$0")/arena_widget.lua" > "$WD/LuaUI/Widgets/arena_widget.lua"
rm -f "$WD/LuaUI/Widgets/save_widget.lua" "$WD/LuaUI/Widgets/reload_widget.lua"
sed -e "s/@SPEED@/$SPEED/" -e "s/@MAX_FRAMES@/$MAX_FRAMES/" "$(dirname "$0")/load_widget.lua" > "$WD/LuaUI/Widgets/load_widget.lua"
# The saved game's host player is match.sh's [PLAYER0] (Arena).
PLAYER=$(sed -n '/\[PLAYER0\]/,/}/ s/^ *Name=\(.*\);/\1/p' "$MATCH/script.txt" | head -1)
PLAYER=${PLAYER:-Arena}
SCRIPT="$OUT/script.txt"
cat > "$SCRIPT" <<EOT
[GAME]
{
    SaveFile=$SAVE.ssf;
    IsHost=1;
    OnlyLocal=1;
    MyPlayerName=$PLAYER;
}
EOT
# Keep the first run's infolog aside; the engine rewrites wd/infolog.txt.
LOG="$WD/infolog.txt"
[ -f "$LOG" ] && mv "$LOG" "$WD/infolog.pre-$LABEL.txt"

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
"${launch[@]}" "$ENGINE" --write-dir "$WD" "$SCRIPT" > "$OUT/stdout.txt" 2>&1 &
PID=$!
sleep 15
flock -u 9
exec 9>&-

start=$(date +%s)
# LuaUI widgets get no GameFrame in a resumed game (observed 2026-10-24), so
# the frame cap is enforced here from the infolog's [f=N] stamps as well.
while kill -0 "$PID" 2>/dev/null; do
    sleep 2
    now=$(grep -oE '\[f=[0-9]+\]' "$LOG" 2>/dev/null | tail -1 | tr -dc '0-9' | sed 's/^0*//')
    if [ "$MAX_FRAMES" -gt 0 ] && [ "${now:-0}" -ge "$MAX_FRAMES" ]; then
        echo "ARENA_TIMEOUT f=$now (loadmatch.sh frame cap)" >> "$OUT/stdout.txt"
        capped=1
        kill "$PID" 2>/dev/null; sleep 3; kill -9 "$PID" 2>/dev/null
        break
    fi
    if [ $(( $(date +%s) - start )) -ge "$TIMEOUT" ]; then
        kill "$PID" 2>/dev/null; sleep 2; kill -9 "$PID" 2>/dev/null
        break
    fi
done
wait "$PID" 2>/dev/null
status=$?
elapsed=$(( $(date +%s) - start ))

frame=$(grep -oE '\[f=[0-9]+\]' "$LOG" 2>/dev/null | tail -1 | tr -dc '0-9' | sed 's/^0*//')
over=$(grep -oE "ARENA_GAMEOVER f=[0-9]+ winners=[0-9,]*" "$LOG" 2>/dev/null | tail -1)
if [ -n "$over" ]; then
    w=${over##*=}
    case $w in 0) winner=A ;; 1) winner=B ;; "") winner=draw ;; *,*) winner=draw ;; *) winner=ally$w ;; esac
elif [ "${capped:-0}" = 1 ] || grep -q "ARENA_TIMEOUT" "$LOG" 2>/dev/null; then winner=frames
elif [ "$elapsed" -ge "$TIMEOUT" ]; then winner=timeout
else
    winner=crash
    [ "$status" -eq 137 ] && echo "ARENA_KILLED engine got SIGKILL (memory cap ${ARENA_ENGINE_MEMORY:-none})" | tee -a "$OUT/stdout.txt"
fi
cp "$LOG" "$OUT/infolog.txt" 2>/dev/null
for d in "$WD"/demos/*.sdfz; do [ -f "$d" ] && [ ! -f "$MATCH/$(basename "$d")" ] && cp "$d" "$OUT/"; done
panics=$(grep -c "panicked" "$OUT/infolog.txt" 2>/dev/null)
errors=$(grep -cE "Skirmish AI <[^>]*>: error:" "$OUT/infolog.txt" 2>/dev/null)
echo "RESULT winner=$winner frame=${frame:-0} panics=${panics:-0} errors=${errors:-0} seconds=$elapsed dir=$OUT"
exit 0
