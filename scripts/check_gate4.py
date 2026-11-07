#!/usr/bin/env python3
"""Phase-4 gate check: follow a live game in a phone-sized headless Firefox
(a 390 x 844 viewport, through geckodriver), then download its record from the page and
replay it.

1. Wait for a game in progress on the front page and open /game/<id>.
2. Sample the page every SAMPLE seconds for WATCH seconds without reloading
   (a marker set on `window` must survive): the move list must grow, the
   board must redraw and the clock text must change. A burst of samples
   every 200 ms then catches individual moves: the running clock must
   switch sides with each one.
3. Keep the page open until the game ends: the status must turn into the
   result, still without a reload, and the page's move list must equal the
   record's moves.usi.
4. Open the finished page, download every file behind its record links,
   check record.json against the API, and run replay_test.py on the copy.

Screenshots go to OUT: live_start.png, live_later.png, finished.png.

Usage: check_gate4.py OUT --http http://127.0.0.1:18081 --binary chushogi-gen
"""

import argparse
import json
import re
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from webdriver import WD  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]

SAMPLE_JS = """
const t = (id) => (document.getElementById(id) || {}).textContent || '';
const running = document.querySelector('.clock.running');
return {
  marker: window.__gate4 === 1,
  moves: document.querySelectorAll('#moves li').length,
  last: [...document.querySelectorAll('#moves li')].slice(-1).map(li => li.lastChild.textContent)[0] || null,
  ply: t('ply'), b: t('clock-b'), w: t('clock-w'),
  running: running ? running.id : null,
  status: t('status'),
  hot: document.querySelectorAll('#board .sq.hot').length,
  squares: document.querySelectorAll('#board .sq').length,
  width: document.documentElement.scrollWidth, view: window.innerWidth,
};
"""


def get(url):
    with urllib.request.urlopen(url, timeout=30) as r:
        return r.read()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=Path)
    ap.add_argument("--http", default="http://127.0.0.1:18081")
    ap.add_argument("--binary", default=str(ROOT / "vendor/chushogi/chushogi-gen"))
    ap.add_argument("--watch", type=int, default=60)
    ap.add_argument("--sample", type=int, default=5)
    ap.add_argument("--end-within", type=int, default=3600)
    a = ap.parse_args()
    fails = []

    def check(ok, what):
        print(("PASS " if ok else "FAIL ") + what)
        if not ok:
            fails.append(what)
        return ok

    # 1. A live game.
    gid = None
    for _ in range(600):
        m = re.search(r'href="/game/([^"]+)"', get(a.http + "/live").decode())
        if m:
            gid = m.group(1)
            break
        time.sleep(1)
    if not check(gid is not None, "a game in progress appears on the front page"):
        return 1
    print("game", gid)

    w = WD(port=4450, width=390, height=844)
    try:
        w.open_phone(f"{a.http}/game/{gid}")
        time.sleep(1)
        w.js("window.__gate4 = 1;")
        w.shot_phone(str(a.out / "live_start.png"))
        samples = [w.js(SAMPLE_JS)]
        check(samples[0]["squares"] == 144, "the board shows 144 squares")
        check(samples[0]["view"] == 390, f"a phone-sized viewport ({samples[0]['view']} CSS px)")
        check(samples[0]["width"] <= samples[0]["view"], f"no horizontal scroll at {samples[0]['view']} px")
        for _ in range(a.watch // a.sample):
            time.sleep(a.sample)
            samples.append(w.js(SAMPLE_JS))
        w.shot_phone(str(a.out / "live_later.png"))
        first, last = samples[0], samples[-1]
        for s in samples:
            print(f"  moves {s['moves']:4}  last {s['last'] or '-':10}  sente {s['b']:>16}  gote {s['w']:>16}  "
                  f"running {s['running']}  {s['status']}")
        check(all(s["marker"] for s in samples), "no reload while watching")
        check(last["moves"] > first["moves"], f"moves arrive without reload ({first['moves']} -> {last['moves']})")
        check(len({(s["b"], s["w"]) for s in samples}) > len(samples) // 2, "the clocks change between samples")
        # Moves come every few seconds: sample finely to see each one.
        burst = []
        for _ in range(75):
            burst.append(w.js(SAMPLE_JS))
            time.sleep(0.2)
        turns = [(s["moves"], s["running"]) for s in burst if s["status"] == "live"]
        changes = [(a, b) for a, b in zip(turns, turns[1:]) if a != b]
        flips = [(a, b) for a, b in changes if a[0] != b[0]]
        check(len(flips) >= 2 and all((b[0] - a[0]) % 2 == (a[1] != b[1]) for a, b in changes)
              and all(r == ("clock-b" if n % 2 == 0 else "clock-w") for n, r in turns),
              f"the running clock follows the side to move ({len(flips)} moves seen in the burst)")
        check(all(s["hot"] >= 2 for s in samples if s["moves"]), "the last move is highlighted")

        # 3. To the end, still without a reload.
        deadline = time.time() + a.end_within
        s = last
        while s["status"] == "live" and time.time() < deadline:
            time.sleep(5)
            s = w.js(SAMPLE_JS)
        check(s["status"] != "live", f"the game ends on the page: {s['status']!r}")
        check(s["marker"], "still no reload at the end")
        page_moves = w.js("return [...document.querySelectorAll('#moves li')].map(li => li.lastChild.textContent)")

        # 4. The finished page and its record links.
        w.open_phone(f"{a.http}/game/{gid}")
        w.shot_phone(str(a.out / "finished.png"))
        links = w.js("return [...document.querySelectorAll('.downloads a')].map(a => [a.href, a.getAttribute('download')])")
    finally:
        w.quit()

    check(len(links) == 5, f"{len(links)} record links on the finished page")
    dest = a.out / "download" / gid
    dest.mkdir(parents=True, exist_ok=True)
    for href, _ in links:
        (dest / href.rsplit("/", 1)[1]).write_bytes(get(href))
    rec_moves = (dest / "moves.usi").read_text().split()
    check(page_moves == rec_moves, f"the page's {len(page_moves)} moves equal the record's {len(rec_moves)}")
    rec = json.loads((dest / "record.json").read_text())
    api = json.loads(get(f"{a.http}/api/games?league={rec['league']}"))
    row = next((g for g in api if g["id"] == gid), None)
    check(row is not None and row["plies"] == rec["plies"] and row["reason"] == rec["reason"],
          f"record.json agrees with /api/games ({rec['plies']} plies, {rec['reason']})")
    r = subprocess.run([sys.executable, str(ROOT / "vendor/chushogi/tests/replay_test.py"), str(dest.parent),
                        "--binary", a.binary], capture_output=True, text=True)
    print(r.stdout.strip().splitlines()[-1] if r.stdout.strip() else r.stderr)
    check(r.returncode == 0, "the downloaded record replays with tests/replay_test.py")
    print(f"{len(fails)} failed")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
