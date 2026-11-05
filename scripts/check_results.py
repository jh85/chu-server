#!/usr/bin/env python3
"""Check server game records against random_match.py's referee.

For every record directory (record.json + moves.usi) under ROOT, the move list
is replayed through chushogi-gen with the referee loop of
vendor/chushogi/tools/random_match.py (status, then fourfold repetition, then
the 1000-ply cap, after every move). The game must end exactly at the last
recorded move with the same status, winner and end_reason. random_match.py has
no perpetual-check rule, so a server `perpetualCheck` is accepted where the
referee finds a repetition, and reported separately.

With --db, the games table must hold one finished row per record with the same
ply count and reason.

Usage: check_results.py ROOT [--binary chushogi-gen] [--db chu.db] [--expect N]
"""

import argparse
import json
import sqlite3
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "vendor/chushogi/tools"))
from random_match import PLY_CAP, Engine  # noqa: E402

STATUS_NAMES = {"bareking": "bareKing", "royalslost": "royalsLost",
                "stalemate": "stalemate", "draw": "draw"}


def referee_result(binary, initial, moves):
    """random_match.play_game's referee half, on a fixed move list.
    Returns (ply, result) of the first ending, or (None, None)."""
    ref = Engine(binary)
    try:
        ref.cmd(f"position sfen {initial}", "ok")
        sfen = ref.cmd("sfen", "sfen")
        seen = {" ".join(sfen.split()[:3]): 1}
        for ply, mv in enumerate(moves, 1):
            ref.cmd(f"move {mv}", "ok")
            sfen = ref.cmd("sfen", "sfen")
            key = " ".join(sfen.split()[:3])
            seen[key] = seen.get(key, 0) + 1
            status = ref.cmd("status", "status").split()
            if status[0] != "playing":
                return ply, {"status": STATUS_NAMES[status[0]],
                             "winner": status[1] if len(status) > 1 else None,
                             "end_reason": status[0]}
            if seen[key] >= 4:
                return ply, {"status": "draw", "winner": None, "end_reason": "repetition"}
            if ply >= PLY_CAP:
                return ply, {"status": "draw", "winner": None, "end_reason": "moveLimit"}
        return None, None
    finally:
        ref.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("root", type=Path)
    ap.add_argument("--binary", default="./chushogi-gen")
    ap.add_argument("--db", type=Path)
    ap.add_argument("--expect", type=int)
    a = ap.parse_args()

    dirs = sorted(p.parent for p in a.root.rglob("record.json"))
    failures, perpetual, reasons = [], 0, {}
    for d in dirs:
        rec = json.loads((d / "record.json").read_text())
        moves = (d / "moves.usi").read_text().split()
        got = {k: rec[k] for k in ("status", "winner", "end_reason")}
        reasons[rec["reason"]] = reasons.get(rec["reason"], 0) + 1
        ply, want = referee_result(a.binary, rec["initial_sfen"], moves)
        if rec["end_reason"] == "perpetualCheck" and want and want["end_reason"] == "repetition":
            perpetual += 1
            want = got
        if ply != len(moves) or want != got:
            failures.append(f"{d.name}: server {got} after {len(moves)} plies; "
                            f"random_match referee {want} at ply {ply}")

    if a.db:
        db = sqlite3.connect(a.db)
        rows = {r[0]: r[1:] for r in db.execute(
            "SELECT id, ply_count, reason, ended_at FROM games")}
        for d in dirs:
            rec = json.loads((d / "record.json").read_text())
            row = rows.get(rec["id"])
            if row is None or row[2] is None or row[:2] != (rec["plies"], rec["reason"]):
                failures.append(f"{rec['id']}: db row {row} vs record "
                                f"{rec['plies']} plies {rec['reason']}")
            n = db.execute("SELECT COUNT(*) FROM moves WHERE game_id = ?", (rec["id"],)).fetchone()[0]
            if n != rec["plies"]:
                failures.append(f"{rec['id']}: {n} move rows, {rec['plies']} plies")
        if len(rows) != len(dirs):
            failures.append(f"{len(rows)} game rows, {len(dirs)} records")

    if a.expect is not None and len(dirs) != a.expect:
        failures.append(f"{len(dirs)} records, expected {a.expect}")
    for f in failures:
        print("FAIL", f)
    print(f"{len(dirs) - len(failures)}/{len(dirs)} records agree with random_match.py's referee"
          f"{' and the database' if a.db else ''}; reasons: "
          + ", ".join(f"{k} {v}" for k, v in sorted(reasons.items()))
          + (f"; {perpetual} perpetual-check repetitions" if perpetual else ""))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
