#!/usr/bin/env bash
# Replay a queue of lobby demos (replay-harvest.sh) one after another,
# keeping the machine's engine count under a ceiling.
#
# usage: replay-queue.sh [-j JOBS] [-c CEILING] <queue.tsv> <out root>
#   queue.tsv: tab-separated with a header; first column = demo file name
#   under ~/.spring/demos. Each demo goes to <out root>/<demo name without
#   .sdfz>/; a demo with a REPLAY line in its run.log is skipped. JOBS
#   replays run at once (default 1); none starts while pgrep counts
#   CEILING or more spring-headless (default 7). Stop with: touch <out root>/STOP
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
JOBS=1
CEILING=7
while getopts "j:c:" opt; do
    case $opt in
        j) JOBS=$OPTARG ;;
        c) CEILING=$OPTARG ;;
        *) sed -n '2,10p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 2 ] || { sed -n '2,10p' "$0"; exit 2; }
QUEUE=$1
ROOT=$2
mkdir -p "$ROOT"
tail -n +2 "$QUEUE" | cut -f1 | while IFS= read -r demo; do
    [ -f "$ROOT/STOP" ] && { echo "STOP file present, exiting"; break; }
    name=${demo%.sdfz}
    out="$ROOT/$name"
    if grep -q "^REPLAY " "$out/run.log" 2>/dev/null; then continue; fi
    [ -f "$HOME/.spring/demos/$demo" ] || { echo "missing $demo"; continue; }
    # same count as match.sh (the engine's process name is recoil-main, so match the command line)
    while [ "$(pgrep -c -f '[s]pring-headless --write-dir')" -gt "$CEILING" ] || [ "$(jobs -rp | wc -l)" -ge "$JOBS" ]; do
        sleep 15
        [ -f "$ROOT/STOP" ] && break
    done
    [ -f "$ROOT/STOP" ] && { echo "STOP file present, exiting"; break; }
    mkdir -p "$out"
    echo "$(date +%H:%M:%S) start $demo"
    "$here/replay-harvest.sh" -s 30 -t 2400 "$HOME/.spring/demos/$demo" "$out" > "$out/run.log" 2>&1 &
done
wait
echo "queue done"
