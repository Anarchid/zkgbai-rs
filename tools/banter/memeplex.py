#!/usr/bin/env python3
"""Rank what players say in game chat, from demo_chat.py's corpus.

A meme is a phrase many different people say in many different games, so
phrases are ranked by distinct senders, not by message count (one player's
catchphrase or a widget's spam ranks low). For each phrase: senders, games,
where in the game it is said (fraction of the game's length), and to whom
(allies / all / spectators).

Also cut out: lines by one named player (--me, the tone reference), what
AIs said in chat (lines like "<ZKGBAI> gg!"), and what humans said in the
30 s after an AI line (reactions).

Usage: memeplex.py CORPUS_DIR [--me Anarchid] [--min-senders 5]
Writes CORPUS_DIR/{phrases.tsv,ngrams.tsv,me.txt,ai_lines.tsv,reactions.txt,SUMMARY.md}.
"""

import argparse
import collections
import json
import os
import re
import statistics

DEST = {252: "allies", 253: "specs", 254: "all"}
NOISE = re.compile(
    r"^(!|/|i choose:|poll:|team .*,|game ?over|.*\bis id[il]e\b|<\w+>!|\[[^\]]*\]$)",
    re.I,
)
AI_LINE = re.compile(r"^<([A-Za-z0-9_\[\]-]+)>\s*(.+)$")


def norm(text):
    t = text.lower().strip()
    t = re.sub(r"(.)\1{2,}", r"\1\1", t)  # "gggg" -> "gg", "lolll" -> "loll"
    t = re.sub(r"[^\w\s'?!:()<>-]", "", t)
    t = re.sub(r"([?!.])[?!.]+", r"\1", t)
    return re.sub(r"\s+", " ", t).strip()


def phase(frac):
    if frac is None:
        return "?"
    return "open" if frac < 0.1 else ("end" if frac > 0.9 else "mid")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus")
    ap.add_argument("--me", default="Anarchid")
    ap.add_argument("--min-senders", type=int, default=5)
    a = ap.parse_args()
    games = {}
    for line in open(os.path.join(a.corpus, "games.jsonl")):
        g = json.loads(line)
        games[g["demo"]] = g

    msgs, seen = [], set()
    ai_lines, by_demo = [], collections.defaultdict(list)
    for line in open(os.path.join(a.corpus, "chat.jsonl")):
        m = json.loads(line)
        text = m["text"].strip()
        if m["from"] == "<server>" or not text or len(text) > 200:
            continue
        m_ai = AI_LINE.match(text)
        if m_ai:
            ai_lines.append((m["demo"], m["t"], m_ai.group(1), m_ai.group(2)))
            by_demo[m["demo"]].append(("ai", m["t"], m_ai.group(1), m_ai.group(2)))
            continue
        if NOISE.match(text):
            continue
        key = (m["demo"], m["from"], text)
        if key in seen:  # widgets and spammers repeat themselves
            continue
        seen.add(key)
        gt = games.get(m["demo"], {}).get("game_time") or 0
        m["frac"] = min(1.0, m["t"] / gt) if gt > 60 else None
        m["n"] = norm(text)
        msgs.append(m)
        by_demo[m["demo"]].append(("human", m["t"], m["from"], text))

    # Phrases by distinct senders.
    ph = collections.defaultdict(lambda: {"senders": set(), "games": set(), "fracs": [], "dest": collections.Counter(), "spec": 0, "n": 0, "forms": collections.Counter()})
    for m in msgs:
        if not m["n"]:
            continue
        p = ph[m["n"]]
        p["senders"].add(m["from"])
        p["games"].add(m["demo"])
        p["n"] += 1
        p["forms"][m["text"]] += 1
        p["dest"][DEST.get(m["dest"], "whisper")] += 1
        p["spec"] += bool(m["spec"])
        if m["frac"] is not None:
            p["fracs"].append(m["frac"])
    rows = []
    for k, p in ph.items():
        if len(p["senders"]) < a.min_senders:
            continue
        med = statistics.median(p["fracs"]) if p["fracs"] else None
        rows.append((len(p["senders"]), len(p["games"]), p["n"], med, k, p))
    rows.sort(key=lambda r: (-r[0], -r[1]))
    with open(os.path.join(a.corpus, "phrases.tsv"), "w") as f:
        f.write("senders\tgames\tmsgs\tmedian_frac\tphase\tspec_share\tdest\tphrase\tcommon_form\n")
        for s, g, n, med, k, p in rows:
            dest = ",".join(f"{d}:{c}" for d, c in p["dest"].most_common())
            form = p["forms"].most_common(1)[0][0]
            f.write(f"{s}\t{g}\t{n}\t{'' if med is None else f'{med:.2f}'}\t{phase(med)}\t{p['spec'] / n:.2f}\t{dest}\t{k}\t{form}\n")

    # Word n-grams (2-4) by distinct senders, inside longer lines.
    ng = collections.defaultdict(set)
    for m in msgs:
        w = m["n"].split()
        if len(w) < 3:
            continue
        for n in (2, 3, 4):
            for i in range(len(w) - n + 1):
                ng[" ".join(w[i : i + n])].add(m["from"])
    ngr = sorted(((len(s), k) for k, s in ng.items() if len(s) >= a.min_senders * 4), reverse=True)
    with open(os.path.join(a.corpus, "ngrams.tsv"), "w") as f:
        f.write("senders\tngram\n")
        for s, k in ngr:
            f.write(f"{s}\t{k}\n")

    # Tone reference: the named player's own in-game lines.
    mine = [m for m in msgs if m["from"] == a.me]
    with open(os.path.join(a.corpus, "me.txt"), "w") as f:
        for m in mine:
            f.write(f"{m['demo'][:10]}\t{m['t']:.0f}\t{DEST.get(m['dest'], 'whisper')}\t{m['text']}\n")

    # What AIs said, and what humans said within 30 s after.
    with open(os.path.join(a.corpus, "ai_lines.tsv"), "w") as f:
        f.write("demo\tt\tai\ttext\n")
        for d, t, who, text in ai_lines:
            f.write(f"{d}\t{t}\t{who}\t{text}\n")
    n_react = 0
    with open(os.path.join(a.corpus, "reactions.txt"), "w") as f:
        for d, evs in by_demo.items():
            evs.sort(key=lambda e: e[1])
            for i, (kind, t, who, text) in enumerate(evs):
                if kind != "ai":
                    continue
                after = [e for e in evs[i + 1 :] if e[1] - t <= 30 and e[0] == "human"]
                if not after:
                    continue
                n_react += 1
                f.write(f"== {d} @{t:.0f}s <{who}> {text}\n")
                for _, t2, who2, text2 in after:
                    f.write(f"   +{t2 - t:.0f}s {who2}: {text2}\n")

    senders = {m["from"] for m in msgs}
    ai_tally = collections.Counter((w, t) for _, _, w, t in ai_lines)
    with open(os.path.join(a.corpus, "SUMMARY.md"), "w") as f:
        f.write(f"# Game-chat corpus\n\n{len(games)} demos, {len(msgs)} human messages after noise/dedup, "
                f"{len(senders)} senders; {len(mine)} by {a.me}; {len(ai_lines)} AI lines, {n_react} followed by human chat within 30 s.\n\n")
        f.write(f"## Top phrases (>= {a.min_senders} senders)\n\n| senders | games | phase | phrase |\n|---|---|---|---|\n")
        for s, g, n, med, k, p in rows[:80]:
            f.write(f"| {s} | {g} | {phase(med)} | {k.replace('|', '/')} |\n")
        f.write("\n## AI lines\n\n| count | ai | text |\n|---|---|---|\n")
        for (w, t), c in ai_tally.most_common(60):
            f.write(f"| {c} | {w} | {t.replace('|', '/')} |\n")
    print(f"{len(msgs)} msgs, {len(senders)} senders, {len(rows)} phrases, {len(mine)} mine, {len(ai_lines)} ai lines, {n_react} reacted")


if __name__ == "__main__":
    main()
