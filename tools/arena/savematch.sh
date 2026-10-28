#!/usr/bin/env bash
# Run match.sh and save the game at a frame: installs save_widget.lua via
# ARENA_WIDGETS, keeps the write-dir (-k) so loadmatch.sh can resume it.
#
# usage: ARENA_SAVE_FRAME=18000 [ARENA_SAVE_NAME=name] [ARENA_SAVE_EVERY=frames]
#            savematch.sh <match.sh args...>
#   The save lands in <out>/wd/Saves/<name>.ssf (name defaults to
#   arena_<frame>). Pass -o <out> to match.sh to know where that is. With
#   ARENA_SAVE_EVERY, it saves again every that many frames after the first,
#   as arena_<frame>.ssf (the packaged Zero-K only: /save crashes on zk.sdd).
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
FRAME=${ARENA_SAVE_FRAME:-0}
NAME=${ARENA_SAVE_NAME:-arena_$FRAME}
gen=$(mktemp -d "${TMPDIR:-/tmp}/zk-savewidget.XXXXXX")
EVERY=${ARENA_SAVE_EVERY:-0}
sed -e "s/@SAVE_FRAME@/$FRAME/" -e "s/@SAVE_NAME@/$NAME/" -e "s/@SAVE_EVERY@/$EVERY/" \
    "$here/save_widget.lua" > "$gen/save_widget.lua"
export ARENA_WIDGETS="${ARENA_WIDGETS:-} $gen/save_widget.lua"
"$here/match.sh" -k "$@"
rm -rf "$gen"
