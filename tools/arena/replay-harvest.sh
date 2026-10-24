#!/usr/bin/env bash
# Replay a lobby demo headless with the combat log widget (spectator full
# view: UnitDamaged carries projectile and attacker, so shots match exactly).
#
# usage: replay-harvest.sh [-s SPEED] [-t SECONDS] <demo.sdfz> <out dir>
#   The engine version comes from the demo file name (…_<version>.sdfz) and
#   must be installed under ~/.spring/engine/linux64/<version>/.
#   Output: <out dir>/{combatlog.tsv.gz,infolog.txt,stdout.txt,demo.txt}.
#   Honours ~/.config/zkgbai/arena.conf (ARENA_BONUS_WIDGETS, ARENA_ENGINE_MEMORY).
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
SPEED=30
TIMEOUT=1800
while getopts "s:t:" opt; do
    case $opt in
        s) SPEED=$OPTARG ;;
        t) TIMEOUT=$OPTARG ;;
        *) sed -n '2,9p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 2 ] || { sed -n '2,9p' "$0"; exit 2; }
DEMO=$(readlink -f "$1")
OUT=$2
VER=$(basename "$DEMO" | sed -E 's/.*_([0-9]{4}\.[0-9]{2}\.[0-9]{2})\.sdfz$/\1/')
ENGINE="$HOME/.spring/engine/linux64/$VER/spring-headless"
[ -f "$ENGINE" ] || { echo "no engine $VER for $DEMO" >&2; exit 2; }
# Lobby-downloaded engines may lack +x (the lobby sets it on launch, too).
[ -x "$ENGINE" ] || chmod +x "$ENGINE"
mkdir -p "$OUT"
WD="$OUT/wd"
rm -rf "$WD"
mkdir -p "$WD/LuaUI/Widgets"
CACHE=${ARENA_CACHE:-$(cd "$here/../.." && pwd)/target/arena-cache}
[ -d "$CACHE" ] && ln -s "$CACHE" "$WD/cache"
ARENA_CONF=${ARENA_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/zkgbai/arena.conf}
# shellcheck source=/dev/null
[ -f "$ARENA_CONF" ] && . "$ARENA_CONF"
for w in ${ARENA_BONUS_WIDGETS:-$here/combatlog_widget.lua}; do cp "$w" "$WD/LuaUI/Widgets/"; done
sed "s/@SPEED@/$SPEED/" "$here/replay_speed_widget.lua" > "$WD/LuaUI/Widgets/replay_speed_widget.lua"
launch=()
if [ -n "${ARENA_ENGINE_MEMORY:-}" ] && systemd-run --user --scope -q true 2>/dev/null; then
    launch=(systemd-run --user --scope -q -p MemoryMax="$ARENA_ENGINE_MEMORY" -p MemorySwapMax=0 --)
fi
echo "$DEMO" > "$OUT/demo.txt"
start=$(date +%s)
# The engine is exec'd from a bash -c so that only the engine's own command
# line reads "spring-headless --write-dir": the arena counts engines with
# pgrep -f '[s]pring-headless --write-dir' (the process name is recoil-main),
# and the timeout / systemd-run wrappers must not be counted.
"${launch[@]}" timeout "$TIMEOUT" bash -c 'exec "$0" --write-dir "$1" "$2"' "$ENGINE" "$WD" "$DEMO" > "$OUT/stdout.txt" 2>&1
status=$?
cp "$WD/infolog.txt" "$OUT/infolog.txt" 2>/dev/null
for c in "$WD"/combatlog/*.tsv; do [ -f "$c" ] && gzip -c "$c" > "$OUT/combatlog.tsv.gz"; done
frames=$(grep -o "REPLAY_END f=[0-9]*\|REPLAY_GAMEOVER f=[0-9]*" "$OUT/infolog.txt" 2>/dev/null | tail -1 | grep -o "[0-9]*$")
rm -rf "$WD"
echo "REPLAY status=$status seconds=$(( $(date +%s) - start )) frames=${frames:-?} log=$([ -f "$OUT/combatlog.tsv.gz" ] && echo yes || echo no) $(grep -c "^shot" <(zcat "$OUT/combatlog.tsv.gz" 2>/dev/null) 2>/dev/null) shots"
