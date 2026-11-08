#!/usr/bin/env bash
# Phase-4 gate (DESIGN.md): follow a live game on a phone, moves and clocks
# updating without reload; download its record and replay it.
#
# A fresh server (web front end on 0.0.0.0:HTTP, so a phone on the LAN can
# open it) and two chushogi-az bridges, softz_iter_0202 vs a0gb_iter_0202,
# in a challenge league `demo` at 600+10F (OUT/leagues.toml): an az-vs-az game lasts several minutes.
# scripts/check_gate4.py follows the game in a phone-sized headless Firefox
# and does the download and replay; open http://<this host>:HTTP/ on a real
# phone at the same time to watch along.
#
# Usage: scripts/gate4.sh [OUT_DIR]
# env: PYTHON (python3, with torch), PORT (14085), HTTP (18081),
#      EVAL_PORT (51620, and +1), THREADS (12)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$(mktemp -d)}
PYTHON=${PYTHON:-python3}
PORT=${PORT:-14085}
HTTP=${HTTP:-18081}
EVAL_PORT=${EVAL_PORT:-51620}
THREADS=${THREADS:-12}
LEAGUE=demo
AZ=$ROOT/vendor/chushogi/chushogi-az
GEN=$ROOT/vendor/chushogi/chushogi-gen
set -e
make -s -C "$ROOT/vendor/chushogi" chushogi-gen
cargo build --release -q --manifest-path "$ROOT/Cargo.toml"
set +e
BIN=$ROOT/target/release
mkdir -p "$OUT"
cat > "$OUT/leagues.toml" <<TOML
[[league]]
name = "$LEAGUE"
kind = "challenge"
total_s = 600
byoyomi_s = 0
increment_s = 10
rules = "lishogi"
rated = false
TOML
echo "output: $OUT"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null' EXIT

DEVICE=$("$PYTHON" -c 'import torch; print("" if torch.cuda.is_available() else "--cpu")')
i=0
for net in softz a0gb; do
  port=$((EVAL_PORT + i)); i=$((i + 1))
  (cd "$ROOT/vendor/chushogi" && OMP_NUM_THREADS=$THREADS exec "$PYTHON" az/eval_server.py \
    --checkpoint "$ROOT/${net}_iter_0202.pt" --port "$port" --max-batch 128 --batch-wait-ms 1 --tcp-nodelay $DEVICE) \
    > "$OUT/eval_$net.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 120); do grep -q "serving on" "$OUT/eval_$net.log" && break; sleep 1; done
  grep -q "serving on" "$OUT/eval_$net.log" || { cat "$OUT/eval_$net.log"; exit 1; }
done

"$BIN/chu-server" --listen "127.0.0.1:$PORT" --http "0.0.0.0:$HTTP" --db "sqlite://$OUT/chu.db" \
  --records "$OUT/records" --leagues "$OUT/leagues.toml" 2> "$OUT/server.log" &
pids+=($!)
for _ in $(seq 50); do (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break; sleep 0.1; done
echo "web front end: http://$(hostname -I 2>/dev/null | cut -d' ' -f1):$HTTP/"

i=0
for net in softz a0gb; do
  color=$([ $i = 0 ] && echo + || echo -)
  "$BIN/chu-bridge" --server "127.0.0.1:$PORT" --password gate --game "$LEAGUE" --color "$color" --games 1 \
    --log-format json --name "az_$net" --engine "$AZ" --usi-option "EvalPort=$((EVAL_PORT + i))" \
    2> "$OUT/bridge_az_$net.log" &
  pids+=($!)
  i=$((i + 1))
done

python3 "$ROOT/scripts/check_gate4.py" "$OUT" --http "http://127.0.0.1:$HTTP" --binary "$GEN"
status=$?
echo "$status" > "$OUT/DONE"
exit $status
