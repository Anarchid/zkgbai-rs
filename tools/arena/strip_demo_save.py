#!/usr/bin/env python3
"""Make a save taken during demo playback loadable as a live game.

A /save during a replay stores the playback's setup script, which carries
`isfromdemo=1;` and `demofile=...;`; loading it makes the server replay the
demo again and reject the loading player ("User name duplicated in the
demo"). The header is gzip of NUL-terminated strings (engine version, setup
script, game, map) followed by the creg data, whose packages hold absolute
stream offsets, so the two keys are renamed in place (same length) and the
file re-gzipped: `isfromdemo` -> `wasfromdmo`, `demofile` -> `demofilx`.

usage: strip_demo_save.py <in.ssf> <out.ssf>
"""
import gzip
import re
import sys

src, dst = sys.argv[1], sys.argv[2]
data = gzip.open(src, "rb").read()
first = data.index(b"\0") + 1
second = data.index(b"\0", first)
setup = data[first:second]
cleaned = re.sub(rb"(?im)^([ \t]*)isfromdemo=", rb"\1wasfromdmo=", setup)
cleaned = re.sub(rb"(?im)^([ \t]*)demofile=", rb"\1demofilx=", cleaned)
assert len(cleaned) == len(setup)
print(f"setup script {len(setup)} -> {len(cleaned)} bytes")
with gzip.open(dst, "wb", compresslevel=5) as f:
    f.write(data[:first] + cleaned + data[second:])
