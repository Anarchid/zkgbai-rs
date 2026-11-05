#!/usr/bin/env python3
"""Check an artificial reclaim-post run; this is not a profitability test."""
import json
import pathlib
import re
import sys

run = pathlib.Path(sys.argv[1])
mode = sys.argv[2]
log = (run / 'wd/infolog.txt').read_text()
assert not re.search(r'panicked|Skirmish AI <[^>]*>: error:|reclaim_fixture.lua.*error', log, re.I), 'engine/AI error in log'
paths = list((run / 'journal').glob('*t0.jsonl'))
assert len(paths) == 1, paths
rows = [json.loads(line) for line in paths[0].read_text().splitlines()]
posts = [row for row in rows if row.get('k') == 'reclaim_post']
assert posts, 'no policy observations'
starts = [row for row in posts if row.get('event') == 'start']
if mode == 'threat':
    assert not starts, starts
    assert any(row.get('reasons', {}).get('enemy_reach', 0) for row in posts), 'no known threat veto'
    print('PASS: known artillery vetoed construction')
else:
    assert len(starts) == 1, starts
    assert any(row.get('event') == 'ready' for row in posts), 'Caretaker never completed'
    assert not any(row.get('event') in ('abort', 'lost') for row in posts), 'aborted/lost post'
    initial = next(line for line in log.splitlines() if 'RECLAIM_FIXTURE f=6000 ' in line)
    assert re.search(r'support=\d+:20/80:1\.000;\d+:50/600:0\.100', initial), initial
    final = next(line for line in log.splitlines() if 'RECLAIM_FIXTURE f=9000 ' in line)
    assert re.search(r'caretakers=\d+:[^:]+:1\.000:500 ', final), final
    assert re.search(r'support=\d+:80/80:1\.000;\d+:600/600:1\.000', final), final
    yields = [row for row in posts if row.get('event') == 'yield']
    reclaimed = max(row['reclaimed_estimate'] for row in yields)
    assert reclaimed > 100, reclaimed
    print(f'PASS: {mode} completed, reclaimed ~{reclaimed:.0f} metal, repaired radar and finished Solar')
