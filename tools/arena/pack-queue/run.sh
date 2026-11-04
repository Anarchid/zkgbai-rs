#!/usr/bin/env bash
# Engine regression for the synchronous idle-attack gap in Pack move queues.
# The old sequence is a positive control; the new one must move both tanks.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
out=${1:-$root/target/pack-queue-regression}
ARENA_WIDGETS="$here/probe.lua" "$here/../match.sh" -m 'Fairyland v1.0' -s 10 -f 420 -t 90 -k -o "$out" NullAI:0.1 NullAI:0.1
python3 - "$out/infolog.txt" <<'PY'
import re
import sys
rows = {}
names = {}
for line in open(sys.argv[1]):
    match = re.search(r'QUEUEPROBE order 180 (\d+) (\w+)', line)
    if match:
        names[match[1]] = match[2]
    match = re.search(r'QUEUEPROBE (\d+) (\d+) ([\d.-]+) ([\d.-]+) (.*)', line)
    if match:
        frame, unit, x, z, queue = match.groups()
        rows[int(frame), unit] = (float(x), float(z), queue.strip())
assert set(names.values()) == {'tankassault', 'vehassault'}, names
for unit, name in names.items():
    old = rows[185, unit]
    stuck = rows[295, unit]
    start = rows[300, unit]
    moving = rows[360, unit]
    assert old[2].startswith('20:') and ';31109:' in old[2], (name, old)
    assert abs(old[0] - stuck[0]) + abs(old[1] - stuck[1]) < 1, (name, stuck)
    assert moving[2].startswith('31109:') and '20:' not in moving[2], (name, moving)
    assert moving[1] - start[1] > 100, (name, start, moving)
    print(f'{name}: old queue stuck; corrected queue advanced {moving[1] - start[1]:.0f} elmos')
PY
