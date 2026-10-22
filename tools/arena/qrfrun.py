#!/usr/bin/env python3
"""Staged raid-defense series (zkgbai-rs `test=qrf` vs `test=raidgen`).

usage: qrfrun.py -o OUT --lib LIB [-j 4] [-f 18000] [--maps "Agalta v1.0,Otago 1.4"]
                 [--seeds 1-8] [--atk "raid_comp=cloakraid:3"] --arm NAME=DEFOPTS [--arm ...]
       qrfrun.py --summary OUT [OUT ...]

Every (map, seed) is played once per arm, with the same raid_seed, so arms
face the same raid sequence (places, compositions); the defender's side is
seed % 2. An arm may name its own library: NAME@/path/lib.so=DEFOPTS.
Results: OUT/results.jsonl (one line per game), summary printed.
"""
import argparse, json, math, os, re, subprocess, sys, threading, queue, time

MATCH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "match.sh")
SCORE = re.compile(r"qrfscore: (.*)")
RAID = re.compile(r"raidscore: (.*)")


def kv(line):
    out = {}
    for tok in line.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if "/" in v:
                a, b = v.split("/", 1)
                out[k] = float(a)
                out[k + "_of"] = float(b)
            else:
                try:
                    out[k] = float(v)
                except ValueError:
                    out[k] = v
    return out


def parse(dirname):
    path = os.path.join(dirname, "infolog.txt")
    if not os.path.exists(path):
        return None
    score = raid = None
    qrf_lines = 0
    for line in open(path, errors="replace"):
        m = SCORE.search(line)
        if m:
            score = kv(m.group(1))
        m = RAID.search(line)
        if m:
            raid = kv(m.group(1))
        if "qrf:" in line:
            qrf_lines += 1
    if score is None:
        return None
    r = dict(score)
    if raid:
        for k, v in raid.items():
            r["atk_" + k] = v
    return r


def seeds_of(s):
    out = []
    for part in s.split(","):
        if "-" in part:
            a, b = part.split("-")
            out.extend(range(int(a), int(b) + 1))
        else:
            out.append(int(part))
    return out


def run(args):
    args.out = os.path.abspath(args.out)
    os.makedirs(args.out, exist_ok=True)
    arms = []
    for a in args.arm:
        name, opts = a.split("=", 1)
        lib = args.lib
        if "@" in name:
            name, lib = name.split("@", 1)
        arms.append((name, lib, opts))
    jobs = queue.Queue()
    maps = [m.strip() for m in args.maps.split(",") if m.strip()]
    for seed in seeds_of(args.seeds):
        for m in maps:
            for name, lib, opts in arms:
                jobs.put((m, seed, name, lib, opts))
    res_path = os.path.join(args.out, "results.jsonl")
    lock = threading.Lock()
    port = [args.port]

    def worker():
        while True:
            try:
                m, seed, name, lib, opts = jobs.get_nowait()
            except queue.Empty:
                return
            tag = f"{re.sub('[^A-Za-z0-9]+', '_', m)}-s{seed}-{name}"
            d = os.path.join(args.out, tag)
            done = parse(d)
            if done is None or done.get("t", 0) * 30 < args.frames - 30:
                with lock:
                    port[0] += 2
                    p = port[0]
                dspec = f"ZKGBAI:def@{lib}?test=qrf" + (f"&{opts}" if opts else "")
                aspec = f"ZKGBAI:atk@{args.atk_lib or args.lib}?test=raidgen&raid_seed={seed}" + (f"&{args.atk}" if args.atk else "")
                specs = [dspec, aspec] if seed % 2 == 0 else [aspec, dspec]
                cmd = [MATCH, "-m", m, "-f", str(args.frames), "-t", str(args.timeout), "-p", str(p), "-o", d] + specs
                t0 = time.time()
                out = subprocess.run(cmd, capture_output=True, text=True)
                result = [l for l in out.stdout.splitlines() if l.startswith("RESULT")]
                done = parse(d)
                wall = time.time() - t0
            else:
                result, wall = ["(cached)"], 0
            row = {"map": m, "seed": seed, "arm": name, "dir": d, "wall": round(wall), "result": result[-1] if result else ""}
            if done:
                row.update(done)
            with lock:
                with open(res_path, "a") as f:
                    f.write(json.dumps(row) + "\n")
                print(f"{tag}: mexes={row.get('mexes','?')}/{row.get('mexes_of','?')} lost_metal={row.get('lost_metal','?')} "
                      f"force={row.get('force','?')} kills={row.get('kills','?')} income_sum={row.get('income_sum','?')} "
                      f"({row['wall']} s) {row['result'][:40]}", flush=True)

    threads = [threading.Thread(target=worker) for _ in range(args.j)]
    for t in threads:
        t.start()
        time.sleep(3)
    for t in threads:
        t.join()
    summary([args.out])


def mean_se(xs):
    if not xs:
        return float("nan"), float("nan")
    m = sum(xs) / len(xs)
    if len(xs) < 2:
        return m, float("nan")
    v = sum((x - m) ** 2 for x in xs) / (len(xs) - 1)
    return m, math.sqrt(v / len(xs))


def summary(outs):
    rows = {}
    for out in outs:
        p = os.path.join(out, "results.jsonl")
        if not os.path.exists(p):
            continue
        for line in open(p):
            r = json.loads(line)
            if "mexes" not in r:
                continue
            rows[(r["map"], r["seed"], r["arm"])] = r  # last wins
    arms = sorted({k[2] for k in rows})
    keys = ["mexes", "lost_mexes", "lost_metal", "income_sum", "force", "force_lost_metal", "kills", "atk_spawned", "atk_expired"]
    print(f"{'arm':24s} {'n':>3s} " + " ".join(f"{k:>16s}" for k in keys))
    for a in arms:
        rs = [r for k, r in rows.items() if k[2] == a]
        cells = []
        for k in keys:
            m, se = mean_se([r.get(k, 0.0) for r in rs])
            cells.append(f"{m:9.1f}±{se:<6.1f}")
        print(f"{a:24s} {len(rs):3d} " + " ".join(cells))
    # Paired differences against the first arm, by (map, seed).
    if len(arms) > 1:
        base = arms[0]
        for a in arms[1:]:
            d = [rows[(m, s, a)]["lost_metal"] - rows[(m, s, base)]["lost_metal"]
                 for (m, s, x) in rows if x == base and (m, s, a) in rows]
            mm, se = mean_se(d)
            print(f"paired lost_metal {a} - {base}: {mm:.0f} ± {se:.0f} (n={len(d)})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out")
    ap.add_argument("--lib")
    ap.add_argument("--atk-lib")
    ap.add_argument("-j", type=int, default=4)
    ap.add_argument("-f", "--frames", type=int, default=18000)
    ap.add_argument("-t", "--timeout", type=int, default=600)
    ap.add_argument("--maps", default="Agalta v1.0")
    ap.add_argument("--seeds", default="1-4")
    ap.add_argument("--atk", default="")
    ap.add_argument("--arm", action="append", default=[])
    ap.add_argument("--port", type=int, default=24000)
    ap.add_argument("--summary", nargs="*")
    a = ap.parse_args()
    if a.summary is not None:
        summary(a.summary)
        return
    if not (a.out and a.lib and a.arm):
        ap.error("need -o, --lib and --arm")
    run(a)


if __name__ == "__main__":
    main()
