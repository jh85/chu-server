#!/usr/bin/env python3
"""Measure chushogi-az's search time per simulation count on this machine and
write the `--time-model` file its USI mode reads: one "<sims> <ms>" line per
search. The engine fits ms = a + b * sims to it.

Positions are the start position and positions after random openings from
chushogi-gen. The eval server must be running (az/eval_server.py).

Usage: az_time_model.py --az chushogi-az --gen chushogi-gen --port 51589 \
           [--sims 64 256 1024] [--positions 4] OUT
"""

import argparse
import subprocess
import sys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--az", required=True)
    ap.add_argument("--gen", required=True)
    ap.add_argument("--port", type=int, default=51589)
    ap.add_argument("--sims", type=int, nargs="+", default=[64, 256, 1024])
    ap.add_argument("--positions", type=int, default=4)
    a = ap.parse_args()

    gen = subprocess.Popen([a.gen], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    histories = [[]]
    for p in range(1, a.positions):
        print(f"seed {p}", file=gen.stdin, flush=True)
        gen.stdout.readline()
        moves = []
        for _ in range(10 * p):
            pos = f"position sfen {START}" + (" moves " + " ".join(moves) if moves else "")
            print(pos, file=gen.stdin, flush=True)
            gen.stdout.readline()
            print("go random", file=gen.stdin, flush=True)
            moves.append(gen.stdout.readline().split()[1])
        histories.append(moves)
    gen.stdin.close()
    gen.wait()

    az = subprocess.Popen([a.az], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

    def send(line):
        print(line, file=az.stdin, flush=True)

    def until(prefix):
        while True:
            line = az.stdout.readline()
            if not line:
                sys.exit("chushogi-az exited")
            if line.startswith(prefix):
                return line.split()

    send("usi")
    until("usiok")
    send(f"setoption name EvalPort value {a.port}")
    send("isready")
    until("readyok")
    rows = []
    for sims in a.sims:
        for moves in histories:
            send("usinewgame")
            send("position startpos" + (" moves " + " ".join(moves) if moves else ""))
            send(f"go nodes {sims}")
            info = until("info depth")  # not an "info string" line
            until("bestmove")
            ms = int(info[info.index("time") + 1])
            rows.append((sims, ms))
            print(f"{sims} sims: {ms} ms", file=sys.stderr)
    send("quit")
    az.wait()
    with open(a.out, "w") as f:
        f.write("# chushogi-az time model: <sims> <ms> per search\n")
        for sims, ms in rows:
            f.write(f"{sims} {ms}\n")


START = ("lfcsgekgscfl/a1b1txot1b1a/mvrhdqndhrvm/pppppppppppp/3i4i3/12/12/"
         "3I4I3/PPPPPPPPPPPP/MVRHDNQDHRVM/A1B1TOXT1B1A/LFCSGKEGSCFL b - 1")

if __name__ == "__main__":
    main()
