#!/usr/bin/env bash
# A piloted match: ZKGBAI with the operator API (and so MCPL) on, against an
# unsteered copy of itself (or another AI), in real time, both journaled into
# the output dir, and the pilot's endpoint printed as soon as the AI is up.
#
# usage: pilot.sh [options] [<baseline AI spec>]
#   -m MAP        map (default: "Comet Catcher Redux v3.1")
#   -l LIB        ZKGBAI library for both sides (default: target/release/libzkgbai.so)
#   -f FACTORY    the pilot's factory (AI option `factory`, e.g. factorytank)
#   -M MODULES    modules muted from the start (AI option `mute`, comma list)
#   -D FEATURES   features off for the pilot (AI option `disable`, e.g. mexporc)
#   -p PORT       the pilot's operator port (default 7460)
#   -s SPEED      sim speed (default 1: real time, what a pilot needs)
#   -F FRAMES     stop after this many sim frames (default: game over)
#   -r PROFILE    the pilot's operator profile (AI option `profile`, e.g. posture)
#   -G GREED      the pilot side's greed dial from the start (AI option `greed`)
#   -T            also run an unpiloted twin of the match (same options minus the
#                 operator, same sides) at 20x into <dir>/twin, for the pilot's
#                 marginal value
#   -o DIR        output dir (default: a fresh dir under $ZKGBAI_RUNS or /tmp)
#   -B            baseline side first (swap sides; the first slot wins more)
#   -w            print the endpoint and wait for the match to end (default:
#                 print it and leave the match running in the background)
#   The baseline spec defaults to the same library unsteered ("ZKGBAI:base@LIB").
#
# Prints: PILOT url=http://127.0.0.1:7460 mcpl=ws://127.0.0.1:7460/mcpl out=<dir>
# then (with -w) the match's RESULT line. The replay stays in <dir>/wd/demos/.
set -uo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
MAP="Comet Catcher Redux v3.1"
LIB="$here/target/release/libzkgbai.so"
FACTORY=""
MUTE=""
DISABLE=""
PORT=7460
SPEED=1
FRAMES=0
OUT=""
SWAP=0
WAIT=0
PROFILE=""
GREED=""
TWIN=0
while getopts "m:l:f:M:D:p:s:F:r:G:o:BwT" opt; do
    case $opt in
        F) FRAMES=$OPTARG ;;
        r) PROFILE=$OPTARG ;;
        G) GREED=$OPTARG ;;
        T) TWIN=1 ;;
        m) MAP=$OPTARG ;;
        l) LIB=$OPTARG ;;
        f) FACTORY=$OPTARG ;;
        M) MUTE=$OPTARG ;;
        D) DISABLE=$OPTARG ;;
        p) PORT=$OPTARG ;;
        s) SPEED=$OPTARG ;;
        o) OUT=$OPTARG ;;
        B) SWAP=1 ;;
        w) WAIT=1 ;;
        *) sed -n '2,30p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ -f "$LIB" ] || { echo "no library at $LIB (cargo build --release -p zkgbai)" >&2; exit 2; }
BASE=${1:-"ZKGBAI:base@$LIB"}
OUT=${OUT:-$(mktemp -d "${ZKGBAI_RUNS:-${TMPDIR:-/tmp}}/pilot.$(date +%Y%m%d-%H%M%S).XXXX")}
mkdir -p "$OUT"

# The twin gets the same options without the operator.
twin_opts=""
[ -n "$FACTORY" ] && twin_opts="$twin_opts&factory=$FACTORY"
[ -n "$MUTE" ] && twin_opts="$twin_opts&mute=$MUTE"
[ -n "$DISABLE" ] && twin_opts="$twin_opts&disable=$DISABLE"
[ -n "$GREED" ] && twin_opts="$twin_opts&greed=$GREED"
opts="operator=$PORT$twin_opts"
[ -n "$PROFILE" ] && opts="$opts&profile=$PROFILE"
PILOT="ZKGBAI:pilot@$LIB?$opts"
TWINSPEC="ZKGBAI:twin@$LIB?${twin_opts#&}"
if [ "$SWAP" -eq 1 ]; then specs=("$BASE" "$PILOT"); twins=("$BASE" "$TWINSPEC"); else specs=("$PILOT" "$BASE"); twins=("$TWINSPEC" "$BASE"); fi
if [ "$TWIN" -eq 1 ]; then
    mkdir -p "$OUT/twin"
    ZKGBAI_JOURNAL=out "$here/tools/arena/match.sh" -m "$MAP" -s 20 -f "$FRAMES" -t 1800 -p $((PORT + 1000)) -o "$OUT/twin" "${twins[@]}" > "$OUT/twin/match.log" 2>&1 &
    disown $! 2>/dev/null
fi

# The AI announces its server in <tmp>/zkgbai-operator/<pid>-t<team>.json.
DISC="${TMPDIR:-/tmp}/zkgbai-operator"
before=$(ls "$DISC" 2>/dev/null | sort | tr '\n' ' ')
ZKGBAI_JOURNAL=out "$here/tools/arena/match.sh" -m "$MAP" -s "$SPEED" -f "$FRAMES" -t 7200 -k -o "$OUT" "${specs[@]}" > "$OUT/match.log" 2>&1 &
MATCH=$!
echo "match $MATCH starting on \"$MAP\": ${specs[*]}" >&2
url=""
for _ in $(seq 1 180); do
    sleep 1
    kill -0 "$MATCH" 2>/dev/null || break
    for f in "$DISC"/*.json; do
        [ -f "$f" ] || continue
        case " $before " in *" $(basename "$f") "*) continue ;; esac
        u=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["url"])' "$f" 2>/dev/null) || continue
        case $u in *:$PORT) url=$u ;; esac
    done
    [ -n "$url" ] && break
done
if [ -z "$url" ]; then
    echo "the pilot's operator did not come up; see $OUT/match.log and $OUT/wd/infolog.txt" >&2
    [ "$WAIT" -eq 1 ] && wait "$MATCH"
    exit 1
fi
echo "PILOT url=$url mcpl=${url/http:/ws:}/mcpl out=$OUT"
if [ "$WAIT" -eq 1 ]; then
    wait "$MATCH"
    tail -2 "$OUT/match.log"
else
    disown "$MATCH" 2>/dev/null
fi
