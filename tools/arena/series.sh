#!/usr/bin/env bash
# Play a series of headless matches between two AIs across maps, in parallel,
# alternating sides, and summarise the results.
#
# usage: series.sh [-n games-per-map] [-j parallel] [-f max-frames] [-t timeout]
#                  [-M maps-file] [-o out-dir] [-P base-port] <AI_A> <AI_B>
# AI specs as for match.sh. Maps file: one map name per line.
# Writes <out-dir>/results.csv and prints a summary.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
N=2
J=8
FRAMES=0
TIMEOUT=1500
MAPS="$here/maps.txt"
OUT=""
PORT_BASE=40000
while getopts "n:j:f:t:M:o:P:" opt; do
    case $opt in
        n) N=$OPTARG ;;
        j) J=$OPTARG ;;
        f) FRAMES=$OPTARG ;;
        t) TIMEOUT=$OPTARG ;;
        M) MAPS=$OPTARG ;;
        o) OUT=$OPTARG ;;
        P) PORT_BASE=$OPTARG ;;
        *) sed -n '2,9p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 2 ] || { sed -n '2,9p' "$0"; exit 2; }
A=$1
B=$2
OUT=${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/zk-series.XXXXXX")}
mkdir -p "$OUT"

jobs_file="$OUT/jobs.txt"
: > "$jobs_file"
i=0
while IFS= read -r map; do
    [ -z "$map" ] && continue
    case $map in \#*) continue ;; esac
    for g in $(seq 1 "$N"); do
        i=$((i + 1))
        # Alternate sides so start-position bias cancels out.
        if [ $((g % 2)) -eq 1 ]; then echo "$i|$map|A|$A|$B"; else echo "$i|$map|B|$B|$A"; fi >> "$jobs_file"
    done
done < "$MAPS"

run_one() {
    IFS='|' read -r idx map first ai0 ai1 <<< "$1"
    res=$("$here/match.sh" -m "$map" -f "$FRAMES" -t "$TIMEOUT" -o "$OUT/m$idx" -p $((PORT_BASE + idx)) "$ai0" "$ai1" 2>/dev/null | grep '^RESULT')
    w=$(sed -n 's/.*winner=\([A-Za-z]*\).*/\1/p' <<< "$res")
    # Translate slot winner (A = team 0) back to the series' A/B.
    if [ "$first" = B ]; then
        case $w in A) w=B ;; B) w=A ;; esac
    fi
    frame=$(sed -n 's/.*frame=\([0-9]*\).*/\1/p' <<< "$res")
    panics=$(sed -n 's/.*panics=\([0-9]*\).*/\1/p' <<< "$res")
    echo "$idx,\"$map\",$first,$w,$frame,$panics"
}
export -f run_one
export OUT FRAMES TIMEOUT PORT_BASE here

echo "idx,map,a_slot,winner,frame,panics" > "$OUT/results.csv"
xargs -P "$J" -I{} bash -c 'run_one "$@"' _ {} < "$jobs_file" >> "$OUT/results.csv"

total=$(($(wc -l < "$OUT/results.csv") - 1))
wa=$(awk -F, 'NR>1 && $4=="A"' "$OUT/results.csv" | wc -l)
wb=$(awk -F, 'NR>1 && $4=="B"' "$OUT/results.csv" | wc -l)
other=$((total - wa - wb))
echo "SERIES games=$total A=$wa B=$wb other=$other  A: $A  B: $B  dir=$OUT"
awk -F, 'NR>1 {print "  " $2 ": " $4 " (f=" $5 ", panics=" $6 ")"}' "$OUT/results.csv" | sort
