#!/usr/bin/env bash
# Run one headless Zero-K match between skirmish AIs and report the result.
#
# usage: match.sh [options] <AI> <AI> [<AI>...]
#   One AI spec per team slot; slots fill allyteams in order of the layout
#   (-L 2v2 with A B C D: A,B on allyteam 0 and C,D on allyteam 1).
#   AI spec:  ShortName[:Version][@/path/to/libSkirmishAI.so][?key=value&key=value...]
#             (?...: the AI's start-script options)
#             With @path, the library is installed into the match's private
#             write-dir under AI/Skirmish/<ShortName>/<Version>/ together with
#             AIInfo.lua/AIOptions.lua from the library's crate `data/` dir
#             (or generated defaults).
# options:
#   -m MAP       map name (default: "Comet Catcher Redux v3.1")
#   -e DIR       engine dir (default: newest under ~/.spring/engine/linux64)
#   -g GAME      game archive (default: "Zero-K $VERSION")
#   -t SECONDS   wall-clock timeout (default: 900)
#   -f FRAMES    stop after this many sim frames (default: 0 = until game over)
#   -s SPEED     sim speed multiplier (default: 20)
#   -o DIR       output dir for infolog/script (default: mktemp under $TMPDIR)
#   -p PORT      host UDP port (default: derived from the process id; set
#                distinct ports when running matches in parallel)
#   -L LAYOUT    team layout: allyteam sizes joined by "v" (default 1v1;
#                e.g. 2v2, 1v1v1 for a 3-way FFA)
#   -k           keep the write dir after the match
#
# environment:
#   ARENA_MAX_ENGINES (6), ARENA_MIN_FREE_MB (5000): the engine waits to
#                start while that many spring-headless run on the machine (any
#                session's) or less memory is available
#   ARENA_CONF   machine limits, sourced if present (default
#                ~/.config/zkgbai/arena.conf): ARENA_ENGINE_CEILING caps
#                ARENA_MAX_ENGINES (and is its default), ARENA_ENGINE_MEMORY
#                (e.g. 8G) runs each engine in a systemd scope with that
#                MemoryMax and no swap; a killed engine prints ARENA_KILLED
#   ARENA_WIDGETS  more LuaUI widget files to install for the match
#
# Prints a final line: RESULT winner=<A|B|allyN|draw|frames|timeout|crash> ...
#   (A/B = allyteam 0/1)
set -uo pipefail

MAP="Comet Catcher Redux v3.1"
ENGINE_DIR=""
GAME='Zero-K $VERSION'
TIMEOUT=900
MAX_FRAMES=0
SPEED=20
OUT=""
KEEP=0
PORT=$((20000 + $$ % 30000))
LAYOUT=1v1
while getopts "m:e:g:t:f:s:o:p:L:k" opt; do
    case $opt in
        m) MAP=$OPTARG ;;
        e) ENGINE_DIR=$OPTARG ;;
        g) GAME=$OPTARG ;;
        t) TIMEOUT=$OPTARG ;;
        f) MAX_FRAMES=$OPTARG ;;
        s) SPEED=$OPTARG ;;
        o) OUT=$OPTARG ;;
        p) PORT=$OPTARG ;;
        L) LAYOUT=$OPTARG ;;
        k) KEEP=1 ;;
        *) sed -n '2,34p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
IFS=v read -r -a ALLY_SIZES <<< "$LAYOUT"
NUM_TEAMS=0
for n in "${ALLY_SIZES[@]}"; do NUM_TEAMS=$((NUM_TEAMS + n)); done
[ $# -eq "$NUM_TEAMS" ] || { echo "layout $LAYOUT needs $NUM_TEAMS AI specs, got $#" >&2; sed -n '2,34p' "$0"; exit 2; }

if [ -z "$ENGINE_DIR" ]; then
    ENGINE_DIR=$(ls -d "$HOME"/.spring/engine/linux64/20*/ 2>/dev/null | sort | tail -1)
fi
ENGINE="${ENGINE_DIR%/}/spring-headless"
[ -f "$ENGINE" ] || { echo "no spring-headless in $ENGINE_DIR" >&2; exit 2; }
# Lobby-downloaded engines may lack +x (the lobby sets it on launch, too).
[ -x "$ENGINE" ] || chmod +x "$ENGINE"

OUT=${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/zk-match.XXXXXX")}
mkdir -p "$OUT"
# ZKGBAI_JOURNAL=out: ZKGBAI's run journals go with this match's infolog.
if [ "${ZKGBAI_JOURNAL:-}" = out ]; then export ZKGBAI_JOURNAL="$OUT/journal"; fi
WD="$OUT/wd"
mkdir -p "$WD"

# Share one archive cache across matches: a cold cache makes the engine
# rescan every map and game archive (minutes with a big map collection).
CACHE=${ARENA_CACHE:-$(cd "$(dirname "$0")/../.." && pwd)/target/arena-cache}
if [ ! -d "$CACHE" ]; then
    mkdir -p "$CACHE"
    cp "$HOME"/.spring/cache/ArchiveCache*.lua "$CACHE"/ 2>/dev/null || true
fi
ln -s "$CACHE" "$WD/cache"
mkdir -p "$WD/LuaUI/Widgets"
sed "s/@MAX_FRAMES@/$MAX_FRAMES/" "$(dirname "$0")/arena_widget.lua" > "$WD/LuaUI/Widgets/arena_widget.lua"
# ARENA_WIDGETS: more LuaUI widget files to install for the match.
for w in ${ARENA_WIDGETS:-}; do cp "$w" "$WD/LuaUI/Widgets/"; done

# parse "Short[:Ver][@lib][?opts]" -> sets SHORT VER LIB OPTS
parse_ai() {
    local spec=$1
    LIB=""
    OPTS=""
    if [[ $spec == *\?* ]]; then OPTS=${spec#*\?}; spec=${spec%%\?*}; fi
    if [[ $spec == *@* ]]; then LIB=${spec#*@}; spec=${spec%%@*}; fi
    SHORT=${spec%%:*}
    if [[ $spec == *:* ]]; then VER=${spec#*:}; else VER=""; fi
}

install_ai() {
    local short=$1 ver=$2 lib=$3
    local dir="$WD/AI/Skirmish/$short/$ver"
    mkdir -p "$dir"
    cp "$lib" "$dir/libSkirmishAI.so"
    if [ -f "$(dirname "$lib")/AIOptions.lua" ]; then cp "$(dirname "$lib")/AIOptions.lua" "$dir/"; fi
    cat > "$dir/AIInfo.lua" <<EOF
local infos = {
    { key = 'shortName', value = '$short' },
    { key = 'version', value = '$ver' },
    { key = 'name', value = '$short' },
    { key = 'description', value = 'dev build' },
    { key = 'interfaceShortName', value = 'C' },
    { key = 'interfaceVersion', value = '0.1' },
}
return infos
EOF
}

declare -a SHORTS VERS OPTSS
i=0
for spec in "$@"; do
    parse_ai "$spec"
    if [ -n "$LIB" ]; then
        VER=${VER:-dev}
        install_ai "$SHORT" "$VER" "$LIB"
    fi
    SHORTS[$i]=$SHORT
    VERS[$i]=$VER
    OPTSS[$i]=$OPTS
    i=$((i + 1))
done

ai_block() {
    local idx=$1 short=$2 ver=$3 opts=$4
    echo "    [AI$idx]"
    echo "    {"
    echo "        Name=${short}_$idx;"
    echo "        ShortName=$short;"
    [ -n "$ver" ] && echo "        Version=$ver;"
    echo "        Team=$idx;"
    echo "        Host=0;"
    if [ -n "$opts" ]; then
        echo "        [Options]"
        echo "        {"
        local kv
        IFS='&' read -ra kvs <<< "$opts"
        for kv in "${kvs[@]}"; do echo "            $kv;"; done
        echo "        }"
    fi
    echo "    }"
}

SCRIPT="$OUT/script.txt"
{
cat <<EOF
[GAME]
{
    Mapname=$MAP;
    Gametype=$GAME;
    IsHost=1;
    HostIP=127.0.0.1;
    HostPort=$PORT;
    MyPlayerNum=0;
    MyPlayerName=Arena;
    StartPosType=2;
    NumPlayers=1;
    NumUsers=$((NUM_TEAMS + 1));
    NumTeams=$NUM_TEAMS;
    NumAllyTeams=${#ALLY_SIZES[@]};
    [PLAYER0]
    {
        Name=Arena;
        Team=0;
        Spectator=1;
    }
EOF
for ((t = 0; t < NUM_TEAMS; t++)); do
    ai_block "$t" "${SHORTS[$t]}" "${VERS[$t]}" "${OPTSS[$t]}"
done
t=0
for ((a = 0; a < ${#ALLY_SIZES[@]}; a++)); do
    for ((k = 0; k < ALLY_SIZES[a]; k++)); do
        echo "    [TEAM$t] { TeamLeader=0; AllyTeam=$a; }"
        t=$((t + 1))
    done
    echo "    [ALLYTEAM$a] { NumAllies=0; }"
done
cat <<EOF
    [MODOPTIONS]
    {
        MinSpeed=$SPEED;
        MaxSpeed=$SPEED;
    }
}
EOF
} > "$SCRIPT"

LOG="$WD/infolog.txt"
# Engine budget shared by everyone on this machine (other sessions' series
# too): wait while ARENA_MAX_ENGINES headless engines run, or less than
# ARENA_MIN_FREE_MB of memory is available (an engine takes ~3.3 GB). The
# lock makes the check and the launch one step across all match.sh runs.
# The machine's own limits live outside the repo, so they bind every
# branch's series whatever it exports: ARENA_ENGINE_CEILING caps
# ARENA_MAX_ENGINES, ARENA_ENGINE_MEMORY caps each engine.
ARENA_CONF=${ARENA_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/zkgbai/arena.conf}
# shellcheck source=/dev/null
[ -f "$ARENA_CONF" ] && . "$ARENA_CONF"
ARENA_MAX_ENGINES=${ARENA_MAX_ENGINES:-${ARENA_ENGINE_CEILING:-6}}
if [ -n "${ARENA_ENGINE_CEILING:-}" ] && [ "$ARENA_MAX_ENGINES" -gt "$ARENA_ENGINE_CEILING" ]; then
    ARENA_MAX_ENGINES=$ARENA_ENGINE_CEILING
fi
ARENA_MIN_FREE_MB=${ARENA_MIN_FREE_MB:-5000}
# Each engine in its own cgroup without swap: a runaway is killed alone at
# the cap instead of driving the machine into swap (2026-10-23 one engine
# went 3.6 -> 20 GB in seconds, and later the whole box froze).
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
# (systemd-run --scope execs the engine: $! is the engine itself.)
"${launch[@]}" "$ENGINE" --write-dir "$WD" "$SCRIPT" > "$OUT/stdout.txt" 2>&1 &
PID=$!
# Hold the lock until the engine has taken its memory.
sleep 15
flock -u 9
exec 9>&-

start=$(date +%s)
# The arena widget ends the match (quitforce) on game over or at the frame
# cap; we only enforce a wall-clock limit.
while kill -0 "$PID" 2>/dev/null; do
    sleep 1
    if [ $(( $(date +%s) - start )) -ge "$TIMEOUT" ]; then
        kill "$PID" 2>/dev/null; sleep 2; kill -9 "$PID" 2>/dev/null
        break
    fi
done
wait "$PID" 2>/dev/null
status=$?

frame=$(grep -oE '\[f=[0-9]+\]' "$LOG" 2>/dev/null | tail -1 | tr -dc '0-9' | sed 's/^0*//')
over=$(grep -oE "ARENA_GAMEOVER f=[0-9]+ winners=[0-9,]*" "$LOG" 2>/dev/null | tail -1)
if [ -n "$over" ]; then
    w=${over##*=}
    # Zero-K declares a draw by making the Gaia allyteam (index = number of
    # real allyteams) the winner.
    if [ "$w" = "${#ALLY_SIZES[@]}" ] || [ -z "$w" ] || [[ $w == *,* ]]; then winner=draw
    elif [ "$w" = 0 ]; then winner=A
    elif [ "$w" = 1 ]; then winner=B
    else winner=ally$w; fi
elif grep -q "ARENA_TIMEOUT" "$LOG" 2>/dev/null; then
    winner=frames
elif [ $(( $(date +%s) - start )) -ge "$TIMEOUT" ]; then
    winner=timeout
else
    winner=crash
    # SIGKILL that was not our timeout: most likely the memory cap.
    if [ "$status" -eq 137 ]; then
        echo "ARENA_KILLED engine got SIGKILL (memory cap ${ARENA_ENGINE_MEMORY:-none})" | tee -a "$OUT/stdout.txt"
    fi
fi

cp "$LOG" "$OUT/infolog.txt" 2>/dev/null
[ "$KEEP" -eq 1 ] || rm -rf "$WD"
stats=$(grep -oE "ARENA_(FINAL|STATS f=[0-9]+) .*" "$OUT/infolog.txt" 2>/dev/null | tail -1)
panics=$(grep -c "panicked" "$OUT/infolog.txt" 2>/dev/null)
errors=$(grep -cE "Skirmish AI <[^>]*>: error:" "$OUT/infolog.txt" 2>/dev/null)
echo "RESULT winner=$winner frame=${frame:-0} panics=${panics:-0} errors=${errors:-0} dir=$OUT"
[ -n "$stats" ] && echo "STATS ${stats#ARENA_}"
exit 0
