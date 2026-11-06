#!/usr/bin/env bash
set -euo pipefail
base=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$base/../../.." && pwd)
mode=${1:-pit}
label=${2:-$mode}
root=${CARGO_TARGET_DIR:-/home/annarhiid/disk2/cargo-target/zkgbai-rs/fortified-reclaim}
library=${RECLAIM_POST_LIB:-$root/debug/libzkgbai.so}
out=$root/engine-$label
[ ! -e "$out" ] || { echo "Output already exists: $out" >&2; exit 1; }
findmnt -T "$root" -n -o OPTIONS | rg -q '(^|,)rw(,|$)'
available=$(awk '/MemAvailable:/{print $2}' /proc/meminfo)
engines=$(ps -eo args | awk '$1 ~ /\/spring(-headless)?$/ {n++} END {print n+0}')
[ "$available" -ge 18000000 ] && [ "$engines" -lt 6 ] || { echo "Host resource gate: available=$available engines=$engines";exit 1; }
game=$out/wd/games/zk-reclaim-fixture.sdd
mkdir -p "$game/LuaRules/Configs"
cp -r "$base/fixture/." "$game/"
case "$mode" in
 pit) echo 'return {height=200}' > "$game/LuaRules/Configs/reclaim_fixture.lua";;
 wall) echo 'return {height=60}' > "$game/LuaRules/Configs/reclaim_fixture.lua";;
 threat) echo 'return {height=200,threat=true}' > "$game/LuaRules/Configs/reclaim_fixture.lua";;
 *) exit 2;;
esac
cp "$repo/crates/zkgbai/data/AIOptions.lua" "$(dirname "$library")/AIOptions.lua"
export ARENA_MAX_ENGINES=5 ARENA_MIN_FREE_MB=18000 ARENA_BONUS_WIDGETS=''
export ARENA_CACHE=$root/arena-cache ZKGBAI_JOURNAL=out
exec "$repo/tools/arena/match.sh" -m 'Agalta v1.0' -g 'Zero-K Reclaim Post Fixture $VERSION' -e /home/annarhiid/.spring/engine/linux64/2026.07.04 -f 9000 -t 600 -s 40 -k -o "$out" \
 "ZKGBAI:reclaimtest@$library?test=reclaimpost&operator=off" \
 "ZKGBAI:reclaimtest@$library?test=reclaimpost&disable=reclaimpost&operator=off"
