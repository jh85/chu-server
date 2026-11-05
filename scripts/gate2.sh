#!/usr/bin/env bash
# Phase-2 gate (DESIGN.md): chushogi-az with CKPT against `chushogi-gen` (a
# random player), GAMES games at 600+10F, both engines behind chu-bridge.
# Pairs of bridges run in parallel, each pair in its own challenge league so
# az never meets az; az has sente in the first half of the pairs and gote in
# the second. Then scripts/check_gate2.py: no az loss on #TIME_UP,
# #ILLEGAL_MOVE or #ILLEGAL_ACTION, every az move in the records is the
# engine's bestmove from the bridge log, and every record replays.
#
# Usage: scripts/gate2.sh [OUT_DIR]
# env: CKPT (a0gb_iter_0202.pt), GAMES (20), PAIRS (4), PYTHON (python3, with
#      torch), PORT (14083), EVAL_PORT (51600), SIMS (1024)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$(mktemp -d)}
CKPT=${CKPT:-$ROOT/a0gb_iter_0202.pt}
GAMES=${GAMES:-20}
PAIRS=${PAIRS:-4}
PYTHON=${PYTHON:-python3}
PORT=${PORT:-14083}
EVAL_PORT=${EVAL_PORT:-51600}
SIMS=${SIMS:-1024}
TC=600-10F
AZ=$ROOT/vendor/chushogi/chushogi-az
(( GAMES % PAIRS == 0 )) || { echo "GAMES must be a multiple of PAIRS" >&2; exit 2; }

cargo build --release -q --manifest-path "$ROOT/Cargo.toml"
BIN=$ROOT/target/release
GEN=$(ls -t "$BIN"/build/chu-rules-*/out/chushogi-gen | head -1)
mkdir -p "$OUT"
echo "output: $OUT"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT

# The eval server, as in the chushogi README; on CPU without CUDA.
DEVICE=$("$PYTHON" -c 'import torch; print("" if torch.cuda.is_available() else "--cpu")')
(cd "$ROOT/vendor/chushogi" && exec "$PYTHON" az/eval_server.py --checkpoint "$CKPT" --port "$EVAL_PORT" \
  --max-batch 128 --batch-wait-ms 1 --tcp-nodelay $DEVICE) > "$OUT/eval_server.log" 2>&1 &
pids+=($!)
for _ in $(seq 120); do grep -q "serving on" "$OUT/eval_server.log" && break; sleep 1; done
grep -q "serving on" "$OUT/eval_server.log" || { cat "$OUT/eval_server.log"; exit 1; }

"$PYTHON" "$ROOT/scripts/az_time_model.py" --az "$AZ" --gen "$GEN" --port "$EVAL_PORT" "$OUT/time_model.txt" 2> /dev/null

"$BIN/chu-server" --listen "127.0.0.1:$PORT" --db "sqlite://$OUT/chu.db" --records "$OUT/records" \
  2> "$OUT/server.log" &
pids+=($!)
for _ in $(seq 50); do (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break; sleep 0.1; done

bridges=()
for p in $(seq "$PAIRS"); do
  game=chu-gate2_$p-$TC
  if (( p <= PAIRS / 2 )); then az_color=+; gen_color=-; else az_color=-; gen_color=+; fi
  common=(--server "127.0.0.1:$PORT" --password gate --game "$game" --games $((GAMES / PAIRS)) --log-format json)
  "$BIN/chu-bridge" "${common[@]}" --name "az_$p" --color "$az_color" \
    --engine "$AZ" --engine-arg --time-model --engine-arg "$OUT/time_model.txt" \
    --usi-option "EvalPort=$EVAL_PORT" --usi-option "Sims=$SIMS" 2> "$OUT/bridge_az_$p.log" &
  bridges+=($!)
  "$BIN/chu-bridge" "${common[@]}" --name "random_$p" --color "$gen_color" \
    --engine "$GEN" --usi-option "Seed=$p" 2> "$OUT/bridge_random_$p.log" &
  bridges+=($!)
done
status=0
for b in "${bridges[@]}"; do wait "$b" || status=1; done

for day in "$OUT"/records/*/*/*/*; do
  python3 "$ROOT/vendor/chushogi/tests/replay_test.py" "$day" --binary "$GEN" | tail -1 || status=1
done
python3 "$ROOT/scripts/check_results.py" "$OUT/records" --db "$OUT/chu.db" --binary "$GEN" --expect "$GAMES" || status=1
python3 "$ROOT/scripts/check_gate2.py" "$OUT" --expect "$GAMES" || status=1
exit $status
