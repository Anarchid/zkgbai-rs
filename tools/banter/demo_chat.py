#!/usr/bin/env python3
"""Pull in-game chat out of Spring/Recoil demos (.sdfz).

Per demo: the startscript gives player names, spectator flags, teams and
AIs; the packet stream gives NETMSG_CHAT (id 7) with the sender's player
number, the destination and the text. Truncated demos (a game still
running, a crash) are read as far as they go.

Usage: demo_chat.py OUT_DIR DEMO... | --dir DEMO_DIR [--jobs N]
Writes OUT_DIR/chat.jsonl (one message per line) and OUT_DIR/games.jsonl
(one demo per line: players, AIs, map, date, message count).
"""

import argparse
import json
import os
import re
import struct
import sys
import zlib
from multiprocessing import Pool

NETMSG_CHAT = 7
HEADER = struct.Struct("<16sii256s16sQiiiiiiiiiiii")


def read_gz(path):
    """The whole decompressed demo, or as much as a truncated file holds."""
    with open(path, "rb") as f:
        raw = f.read()
    d = zlib.decompressobj(16 + zlib.MAX_WBITS)
    try:
        return d.decompress(raw)
    except zlib.error:
        return b""


def parse_script(script):
    """Players (by player number), AIs and a few game keys."""
    players, ais, teams, game = {}, [], {}, {}
    for kind, idx, body in re.findall(r"\[(player|ai|team)(\d+)\]\s*\{([^{}]*)\}", script, re.I):
        kv = {}
        for line in body.split(";"):
            if "=" in line:
                k, v = line.split("=", 1)
                kv[k.strip().lower()] = v.strip()
        kind = kind.lower()
        if kind == "player":
            players[int(idx)] = kv
        elif kind == "ai":
            ais.append(kv)
        else:
            teams[int(idx)] = kv
    for key in ("mapname", "gametype", "hostip", "ishost"):
        m = re.search(r"\b%s\s*=\s*([^;]*);" % key, script, re.I)
        if m:
            game[key] = m.group(1).strip()
    # Player numbers are the [playerN] indices in order (the engine remaps
    # them to be contiguous).
    ordered = [players[k] for k in sorted(players)]
    return ordered, ais, teams, game


def chat_of(packet):
    """(from, dest, text) of a chat packet, sized by a u8 or a u16."""
    n = len(packet)
    if n >= 5 and packet[1] == n:
        off = 2
    elif n >= 6 and struct.unpack_from("<H", packet, 1)[0] == n:
        off = 3
    else:
        return None
    frm, dest = packet[off], packet[off + 1]
    text = packet[off + 2 :].split(b"\0", 1)[0].decode("utf-8", "replace")
    return frm, dest, text


def process(path):
    data = read_gz(path)
    if len(data) < HEADER.size:
        return None, []
    h = HEADER.unpack_from(data, 0)
    magic, version, header_size = h[0], h[1], h[2]
    if not magic.startswith(b"spring demofile"):
        return None, []
    engine = h[3].split(b"\0", 1)[0].decode("ascii", "replace")
    unix_time, script_size = h[5], h[6]
    game_time = h[8]
    script = data[header_size : header_size + script_size].decode("utf-8", "replace")
    players, ais, teams, game = parse_script(script)

    def who(num):
        if 0 <= num < len(players):
            p = players[num]
            return p.get("name", "?"), p.get("spectator", "0") == "1", p.get("team")
        return ("<server>" if num >= 250 else "#%d" % num), None, None

    msgs = []
    pos = header_size + script_size
    end = len(data)
    unpack = struct.unpack_from
    while pos + 8 <= end:
        t, length = unpack("<fI", data, pos)
        pos += 8
        if length == 0 or pos + length > end:
            break
        if data[pos] == NETMSG_CHAT:
            c = chat_of(data[pos : pos + length])
            # 252 allies, 253 spectators, 254 everyone, 0-251 a whisper;
            # 255 is autohost traffic (SPRINGIE:READY and the like).
            if c and c[1] != 255 and not c[2].startswith("SPRINGIE:"):
                frm, dest, text = c
                name, spec, team = who(frm)
                msgs.append(
                    {
                        "demo": os.path.basename(path),
                        "t": round(t, 1),
                        "from": name,
                        "spec": spec,
                        "team": team,
                        "dest": dest,
                        "text": text,
                    }
                )
        pos += length

    humans = [p for p in players if p.get("spectator", "0") != "1"]
    meta = {
        "demo": os.path.basename(path),
        "unix": unix_time,
        "engine": engine,
        "map": game.get("mapname"),
        "game_time": game_time,
        "players": [p.get("name") for p in humans],
        "specs": sum(1 for p in players if p.get("spectator", "0") == "1"),
        "ais": [a.get("shortname") or a.get("name") for a in ais],
        "teams": len(teams),
        "msgs": len(msgs),
        "bytes": len(data),
    }
    return meta, msgs


def safe(path):
    try:
        return process(path)
    except Exception as e:  # one bad demo must not end the run
        return {"demo": os.path.basename(path), "error": repr(e)}, []


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("demos", nargs="*")
    ap.add_argument("--dir")
    ap.add_argument("--jobs", type=int, default=6)
    a = ap.parse_args()
    paths = list(a.demos)
    if a.dir:
        paths += sorted(os.path.join(a.dir, f) for f in os.listdir(a.dir) if f.endswith(".sdfz"))
    os.makedirs(a.out, exist_ok=True)
    n_msgs = n_games = 0
    with open(os.path.join(a.out, "chat.jsonl"), "w") as cf, open(os.path.join(a.out, "games.jsonl"), "w") as gf:
        with Pool(a.jobs) as pool:
            for i, (meta, msgs) in enumerate(pool.imap_unordered(safe, paths, chunksize=4)):
                if meta:
                    gf.write(json.dumps(meta) + "\n")
                    n_games += 1
                for m in msgs:
                    cf.write(json.dumps(m, ensure_ascii=False) + "\n")
                n_msgs += len(msgs)
                if (i + 1) % 200 == 0:
                    print(f"{i + 1}/{len(paths)} demos, {n_msgs} messages", file=sys.stderr, flush=True)
    print(f"done: {n_games} demos, {n_msgs} messages", file=sys.stderr)


if __name__ == "__main__":
    main()
