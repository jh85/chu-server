#!/usr/bin/env python3
"""Phase-3 gate check over a scripts/gate3.sh output directory.

- Rounds: every round time of the league in (start, end] has exactly one
  `rounds` row, fired within 2 s of its time, and no other rows exist; every
  game a round paired is in the games table.
- Games: no player lost on #TIME_UP, #ILLEGAL_MOVE or #ILLEGAL_ACTION, and no
  game ended in #CHUDAN.
- Ratings: every bot is published (15 games) and the final ratings follow
  --order: groups from weakest to strongest, comma-separated names within a
  group may come in any order (their order is reported).
- server.log has no WARN or ERROR line.

Usage: check_gate3.py OUT --league L --start EPOCH --end EPOCH --order G1 G2 ...
"""

import argparse
import json
import re
import sqlite3
import statistics
import sys
import tomllib
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

FAULTS = {"#TIME_UP", "#ILLEGAL_MOVE", "#ILLEGAL_ACTION"}
PUBLISH_AFTER = 15


def epoch(iso):
    return datetime.strptime(iso, "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc).timestamp()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=Path)
    ap.add_argument("--league", required=True)
    ap.add_argument("--start", type=int, required=True)
    ap.add_argument("--end", type=int, required=True)
    ap.add_argument("--order", nargs="+", required=True)
    a = ap.parse_args()

    cfg = next(l for l in tomllib.loads((a.out / "leagues.toml").read_text())["league"] if l["name"] == a.league)
    every, offset = cfg["every_min"] * 60, cfg.get("offset_min", 0) * 60 % (cfg["every_min"] * 60)
    db = sqlite3.connect(a.out / "chu.db")
    fails = []

    # Rounds.
    expected = []
    t = (a.start - offset) // every * every + offset
    while t <= a.end - 2:
        if t > a.start:
            expected.append(t)
        t += every
    rows = db.execute("SELECT scheduled_at, fired_at, pool_json, paired_json, sat_out FROM rounds WHERE league = ?",
                      (a.league,)).fetchall()
    got = {round(epoch(r[0])): r for r in rows}
    if len(got) != len(rows):
        fails.append("two rounds rows for one time")
    missing = [t for t in expected if t not in got]
    extra = sorted(set(got) - set(expected))
    for t in missing:
        fails.append(f"round {datetime.fromtimestamp(t, timezone.utc):%Y-%m-%d %H:%M} missing")
    for t in extra:
        fails.append(f"round {datetime.fromtimestamp(t, timezone.utc):%Y-%m-%d %H:%M} not on the schedule")
    lags = [epoch(r[1]) - epoch(r[0]) for r in rows]
    if lags and not all(0 <= x < 2 for x in lags):
        fails.append(f"rounds fired late or early: {min(lags):.3f}..{max(lags):.3f} s")
    game_ids = {r[0] for r in db.execute("SELECT id FROM games")}
    pairs_per_round = Counter()
    pool_sizes = Counter()
    sat_out = Counter()
    pairings = Counter()
    for r in rows:
        pool, paired = json.loads(r[2]), json.loads(r[3])
        pool_sizes[len(pool)] += 1
        pairs_per_round[len(paired)] += 1
        if r[4]:
            sat_out[r[4]] += 1
        for s, g, gid in paired:
            pairings[tuple(sorted((s, g)))] += 1
            if gid not in game_ids:
                fails.append(f"round {r[0]}: game {gid} not in the games table")

    # Games.
    games = db.execute("SELECT id, sente, gote, result, reason, ply_count, started_at, ended_at FROM games "
                       "WHERE league = ?", (a.league,)).fetchall()
    finished = [g for g in games if g[3] is not None]
    reasons = Counter(g[4] for g in finished)
    score = defaultdict(Counter)
    for gid, sente, gote, result, reason, plies, started, ended in finished:
        if reason == "#CHUDAN":
            fails.append(f"{gid}: #CHUDAN")
            continue
        winner = sente if result == "+" else gote if result == "-" else None
        loser = gote if result == "+" else sente if result == "-" else None
        if winner:
            score[winner]["W"] += 1
            score[loser]["L"] += 1
            if reason in FAULTS:
                fails.append(f"{gid}: {loser} lost on {reason}")
        else:
            score[sente]["D"] += 1
            score[gote]["D"] += 1
    durations = sorted((epoch(g[7]) - epoch(g[6])) / 60 for g in finished)

    # Ratings.
    ratings = {p: (r, n) for p, r, n in db.execute("SELECT player, rating, games FROM ratings WHERE league = ?",
                                                    (a.league,))}
    groups = [g.split(",") for g in a.order]
    for name in sum(groups, []):
        if name not in ratings:
            fails.append(f"{name}: no rating")
        elif ratings[name][1] < PUBLISH_AFTER:
            fails.append(f"{name}: {ratings[name][1]} games, not published")
    for lower, upper in zip(groups, groups[1:]):
        for lo in lower:
            for up in upper:
                if lo in ratings and up in ratings and not ratings[lo][0] < ratings[up][0]:
                    fails.append(f"{lo} ({ratings[lo][0]:.0f}) is not below {up} ({ratings[up][0]:.0f})")

    log = re.sub(r"\x1b\[[0-9;]*m", "", (a.out / "server.log").read_text())
    noisy = [l for l in log.splitlines() if " WARN " in l or " ERROR " in l]
    for l in noisy[:20]:
        fails.append(f"server.log: {l}")

    for f in fails:
        print("FAIL", f)
    hours = (a.end - a.start) / 3600
    print(f"{hours:.1f} h, {len(expected)} rounds due, {len(rows)} fired, {len(missing)} missing; "
          f"fired {min(lags, default=0) * 1000:.0f}..{max(lags, default=0) * 1000:.0f} ms after their time")
    print("pool sizes: " + ", ".join(f"{k} players x{v}" for k, v in sorted(pool_sizes.items()))
          + "; sat out: " + ", ".join(f"{k} {v}" for k, v in sorted(sat_out.items())))
    print("pairings: " + ", ".join(f"{s}-{g} {n}" for (s, g), n in sorted(pairings.items())))
    print(f"games: {len(games)} started, {len(finished)} finished; "
          + ", ".join(f"{k} {v}" for k, v in sorted(reasons.items())))
    if durations:
        print(f"game minutes: median {statistics.median(durations):.1f}, max {durations[-1]:.1f}")
    for name, (r, n) in sorted(ratings.items(), key=lambda x: x[1][0]):
        s = score[name]
        print(f"  {name:<14} {r:8.1f}  {n:3d} games  +{s['W']} -{s['L']} ={s['D']}")
    top = [n for n in groups[-1] if n in ratings]
    if len(top) > 1:
        print("top group order: " + " < ".join(sorted(top, key=lambda n: ratings[n][0])))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
