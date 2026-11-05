#!/usr/bin/env python3
"""Phase-2 gate check over a scripts/gate2.sh output directory.

For every game record whose players include an `az_*` bridge:
- az did not lose on #TIME_UP, #ILLEGAL_MOVE or #ILLEGAL_ACTION;
- every az move in moves.usi is the `move` event of az's bridge log for that
  game and ply (the engine's bestmove, forwarded unchanged), and the bridge
  forwarded no move that is missing from the record.

Prints az's results, think times against the clock, and how many searches the
bridge had to stop.

Usage: check_gate2.py OUT_DIR [--expect N]
"""

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

BRIDGE_FAULTS = {"#TIME_UP", "#ILLEGAL_MOVE", "#ILLEGAL_ACTION"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=Path)
    ap.add_argument("--expect", type=int)
    a = ap.parse_args()

    events = {}  # (game, ply) -> move event of an az bridge
    stopped = resign = 0
    errors = []
    for log in sorted(a.out.glob("bridge_az_*.log")):
        for line in log.read_text().splitlines():
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                errors.append(f"{log.name}: not JSON: {line[:120]}")
                continue
            if e["level"] in ("WARN", "ERROR") and e["message"] != "no bestmove near the time limit; stop":
                errors.append(f"{log.name}: {e['level']} {e['message']}")
            if e["message"] == "move":
                events[(e["game"], e["ply"])] = e
                stopped += e["stopped"]
            elif e["message"] == "resign":
                resign += 1

    failures = []
    results = Counter()
    reasons = Counter()
    think = []
    covered = set()
    records = sorted(a.out.rglob("record.json"))
    for path in records:
        rec = json.loads(path.read_text())
        sente, gote = rec["players"]["sente"]["name"], rec["players"]["gote"]["name"]
        az_side = "sente" if sente.startswith("az_") else "gote" if gote.startswith("az_") else None
        if az_side is None:
            continue
        reasons[rec["reason"]] += 1
        outcome = "draw" if rec["winner"] is None else "win" if rec["winner"] == az_side else "loss"
        results[outcome] += 1
        if outcome == "loss" and rec["reason"] in BRIDGE_FAULTS:
            failures.append(f"{rec['id']}: az lost on {rec['reason']}")
        first = 0 if az_side == "sente" else 1
        for i in range(first, len(rec["moves_usi"]), 2):
            ply = i + 1
            e = events.get((rec["id"], ply))
            if e is None or e["usi"] != rec["moves_usi"][i]:
                failures.append(f"{rec['id']} ply {ply}: record {rec['moves_usi'][i]}, bridge {e and e['usi']}")
            else:
                covered.add((rec["id"], ply))
                think.append(e["think_ms"])
    extra = set(events) - covered
    for game, ply in sorted(extra):
        failures.append(f"{game} ply {ply}: bridge sent {events[(game, ply)]['usi']}, not in the record")
    if a.expect is not None and sum(results.values()) != a.expect:
        failures.append(f"{sum(results.values())} az games, expected {a.expect}")

    for f in errors + failures:
        print("FAIL", f)
    think.sort()
    print(f"az: {results['win']} wins, {results['loss']} losses, {results['draw']} draws; "
          + ", ".join(f"{k} {v}" for k, v in sorted(reasons.items())))
    if think:
        print(f"az moves matching the bridge's bestmove log: {len(think)}; think ms median "
              f"{think[len(think) // 2]}, max {think[-1]}; stopped by the bridge {stopped}; resigned {resign}")
    return 1 if failures or errors else 0


if __name__ == "__main__":
    sys.exit(main())
