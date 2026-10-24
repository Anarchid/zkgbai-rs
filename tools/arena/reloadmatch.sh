#!/usr/bin/env bash
# Run match.sh and reload the Skirmish AIs of some teams at a frame
# (reload_widget.lua via ARENA_WIDGETS): the AI is re-created mid-game and
# gets Init + UnitCreated/UnitFinished for every standing unit, the engine's
# PostLoad path for a loaded save. Keeps the write dir (-k).
#
# usage: ARENA_RELOAD_FRAME=18000 [ARENA_RELOAD_TEAMS="0 1"] reloadmatch.sh <match.sh args...>
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
FRAME=${ARENA_RELOAD_FRAME:-0}
TEAMS=${ARENA_RELOAD_TEAMS:-0 1}
gen=$(mktemp -d "${TMPDIR:-/tmp}/zk-reloadwidget.XXXXXX")
sed -e "s/@RELOAD_FRAME@/$FRAME/" -e "s/@RELOAD_TEAMS@/$(echo $TEAMS | tr ' ' ',')/" "$here/reload_widget.lua" > "$gen/reload_widget.lua"
export ARENA_WIDGETS="${ARENA_WIDGETS:-} $gen/reload_widget.lua"
"$here/match.sh" -k "$@"
rm -rf "$gen"
