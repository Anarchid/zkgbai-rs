#!/usr/bin/env python3
"""Load combat logs (tools/arena/combatlog_widget.lua) into pandas frames.

usage: combatdata.py build <cache-dir> <log-dir-or-file>...   (4 workers)
       combatdata.py info  <cache-dir>

A log becomes {kind: DataFrame} with typed columns plus 'meta' (dict: game,
map, teams {team: (kind, name)}, weapons DataFrame indexed by name).
Cached as one pickle per log in <cache-dir>/<series>__<match>.pkl; the
series name is the log's path relative to ~/Programs/zkgbai-runs with '/'
replaced by '__'.
"""
import gzip
import io
import os
import pickle
import sys
from multiprocessing import Pool

import numpy as np
import pandas as pd

RUNS = os.path.expanduser("~/Programs/zkgbai-runs")

INT_COLS = {"tlos", "n", "f0", "f1", "lag", "pid", "own", "oteam", "tid", "tteam", "tstun", "tdis", "tfire",
            "hits", "nvict", "tdead", "f", "vid", "vteam", "aid", "ateam", "uid", "uteam", "on", "stun", "dis", "fire"}
STR_COLS = {"src", "wd", "odef", "tt", "tdef", "vdef", "adef", "kind", "udef"}


def _frame(kind, cols, lines):
    if not lines:
        return pd.DataFrame(columns=cols)
    df = pd.read_csv(io.StringIO("".join(lines)), sep="\t", names=["_k"] + cols, header=None,
                     usecols=cols, dtype=str, keep_default_na=False, quoting=3, engine="c")
    for c in cols:
        if c in STR_COLS:
            df[c] = df[c].astype("category")
        elif c in INT_COLS:
            df[c] = pd.to_numeric(df[c], errors="coerce").fillna(-1).astype(np.int32)
        else:
            df[c] = pd.to_numeric(df[c], errors="coerce").astype(np.float32)
    return df


def read_log(path):
    opener = gzip.open if path.endswith(".gz") else open
    headers, lines, meta_rows = {}, {}, []
    with opener(path, "rt") as fh:
        for line in fh:
            if line.startswith("#"):
                parts = line[1:].rstrip("\n").split("\t")
                if len(parts) > 1:
                    headers[parts[0]] = parts[1:]
                    lines[parts[0]] = []
                continue
            k = line[:line.find("\t")]
            if k == "meta":
                meta_rows.append(line.rstrip("\n").split("\t")[1:])
            elif k in lines:
                lines[k].append(line)
    out = {k: _frame(k, headers[k], lines[k]) for k in lines if k != "meta"}
    meta = {"path": path, "teams": {}, "weapons": None}
    wcols, wrows = None, []
    for m in meta_rows:
        key = m[0]
        if key == "weaponcols":
            wcols = m[1:]
        elif key == "weapon":
            wrows.append(m[1:])
        elif key == "team":
            # team id, ally team, kind, name
            meta["teams"][int(m[1])] = (int(m[2]) if len(m) > 2 else -1, m[3] if len(m) > 3 else "", m[4] if len(m) > 4 else "")
        else:
            meta[key] = m[1] if len(m) == 2 else m[1:]
    if wcols and wrows:
        w = pd.DataFrame(wrows, columns=wcols).set_index("name")
        for c in w.columns:
            if c != "type":
                w[c] = pd.to_numeric(w[c], errors="coerce")
        meta["weapons"] = w
    out["meta"] = meta
    return out


def series_name(path):
    rel = os.path.relpath(os.path.dirname(path), RUNS) if path.startswith(RUNS) else os.path.basename(path)
    return rel.replace("/", "__").replace(" ", "_")


def find(paths):
    for p in paths:
        if p.startswith("@"):  # a file listing one log per line
            yield from (q for q in open(os.path.expanduser(p[1:])).read().splitlines() if q)
            continue
        p = os.path.expanduser(p)
        if os.path.isdir(p):
            for root, _, files in os.walk(p):
                for f in files:
                    if f == "combatlog.tsv.gz" or (root.endswith("combatlog") and f.endswith(".tsv")):
                        yield os.path.join(root, f)
        else:
            yield p


def _build_one(args):
    path, cache = args
    out = os.path.join(cache, series_name(path) + ".pkl")
    if os.path.exists(out) and os.path.getmtime(out) >= os.path.getmtime(path):
        return out, 0
    try:
        d = read_log(path)
    except Exception as e:  # noqa: BLE001
        return out, f"ERR {e}"
    with open(out + ".tmp", "wb") as fh:
        pickle.dump(d, fh, protocol=5)
    os.replace(out + ".tmp", out)
    return out, len(d.get("shot", ()))


def build(cache, paths, workers=4):
    os.makedirs(cache, exist_ok=True)
    jobs = [(p, cache) for p in find(paths)]
    n = 0
    with Pool(workers) as pool:
        for out, res in pool.imap_unordered(_build_one, jobs, chunksize=2):
            n += 1
            if isinstance(res, str):
                print(out, res, flush=True)
            elif n % 50 == 0:
                print(f"{n}/{len(jobs)}", flush=True)
    print(f"built {n} logs into {cache}")


def cached(cache):
    for f in sorted(os.listdir(cache)):
        if f.endswith(".pkl"):
            yield os.path.join(cache, f)


def load_cached(path):
    with open(path, "rb") as fh:
        return pickle.load(fh)


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "build":
        build(sys.argv[2], sys.argv[3:], int(os.environ.get("PRED_WORKERS", "4")))
    elif cmd == "info":
        tot = {}
        for f in cached(sys.argv[2]):
            d = load_cached(f)
            for k, v in d.items():
                if k != "meta":
                    tot[k] = tot.get(k, 0) + len(v)
        print(tot)
