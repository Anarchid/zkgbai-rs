#!/usr/bin/env bash
# Install the combat log widget machine-wide: into every arena match (through
# ARENA_BONUS_WIDGETS in ~/.config/zkgbai/arena.conf, read by match.sh) and
# into the Spring client's LuaUI/Widgets for human games. Re-run to update.
set -euo pipefail
src=$(cd "$(dirname "$0")" && pwd)/combatlog_widget.lua
conf_dir=${XDG_CONFIG_HOME:-$HOME/.config}/zkgbai
mkdir -p "$conf_dir/widgets"
cp "$src" "$conf_dir/widgets/combatlog_widget.lua"
if ! grep -q ARENA_BONUS_WIDGETS "$conf_dir/arena.conf" 2>/dev/null; then
    cat >> "$conf_dir/arena.conf" <<'CONF'

# Widgets every match gets (match.sh copies that read this file). The combat
# log writes <match dir>/combatlog.tsv.gz. Export the variable empty to run
# a match without them.
ARENA_BONUS_WIDGETS=${ARENA_BONUS_WIDGETS-$HOME/.config/zkgbai/widgets/combatlog_widget.lua}
CONF
fi
echo "arena: $conf_dir/widgets/combatlog_widget.lua (ARENA_BONUS_WIDGETS in $conf_dir/arena.conf)"
client=${SPRING_WRITE_DIR:-$HOME/.spring}/LuaUI/Widgets
if [ -d "$client" ]; then
    cp "$src" "$client/combatlog_widget.lua"
    echo "client: $client/combatlog_widget.lua (logs to $(dirname "$(dirname "$client")")/combatlog/)"
fi
