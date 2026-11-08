#!/usr/bin/env bash
# Phase-3 gate (DESIGN.md): four bots of known strength order queue for a
# floodgate league and play whatever the scheduler pairs, unattended, for
# HOURS. Then the server is stopped (games still running stay unfinished and
# unrated) and scripts/check_gate3.py checks the rounds, the games and the
# ratings; `chu-tools rate` must reproduce the server's ratings, and every
# finished record must replay.
#
#   random        chushogi-gen, a random legal move
#   random_mate1  chushogi-gen MateNodes=1: an immediate royal capture, else random
#   az_softz      chushogi-az, softz_iter_0202.pt
#   az_a0gb       chushogi-az, a0gb_iter_0202.pt
#
# Each az engine has its own eval server (CPU without CUDA), THREADS torch
# threads each, and its own time model.
#
# Usage: scripts/gate3.sh [OUT_DIR]
# env: HOURS (48), LEAGUES (leagues.toml), LEAGUE (floodgate-600-10F),
#      PYTHON (python3, with torch), PORT (14084), EVAL_PORT (51610, and +1),
#      SIMS (1024), THREADS (12)
# Writes OUT/DONE with the exit status when finished.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$(mktemp -d)}
HOURS=${HOURS:-48}
LEAGUES=${LEAGUES:-$ROOT/leagues.toml}
LEAGUE=${LEAGUE:-floodgate-600-10F}
PYTHON=${PYTHON:-python3}
PORT=${PORT:-14084}
EVAL_PORT=${EVAL_PORT:-51610}
SIMS=${SIMS:-1024}
THREADS=${THREADS:-12}
AZ=$ROOT/vendor/chushogi/chushogi-az
GEN=$ROOT/vendor/chushogi/chushogi-gen
set -e
make -s -C "$ROOT/vendor/chushogi" chushogi-gen
cargo build --release -q --manifest-path "$ROOT/Cargo.toml"
set +e
BIN=$ROOT/target/release
mkdir -p "$OUT"
cp "$LEAGUES" "$OUT/leagues.toml"
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
  "$PYTHON" "$ROOT/scripts/az_time_model.py" --az "$AZ" --gen "$GEN" --port "$port" "$OUT/time_model_$net.txt" 2> /dev/null
  echo "$net: $(tail -1 "$OUT/time_model_$net.txt") (sims ms)"
done

START=$(date +%s)
"$BIN/chu-server" --listen "127.0.0.1:$PORT" --db "sqlite://$OUT/chu.db" --records "$OUT/records" \
  --leagues "$OUT/leagues.toml" --http 127.0.0.1:0 2> "$OUT/server.log" &
server=$!
pids+=($server)
for _ in $(seq 50); do (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break; sleep 0.1; done

common=(--server "127.0.0.1:$PORT" --password gate --game "$LEAGUE" --games 1000000 --log-format json)
bridge() {
  local name=$1; shift
  "$BIN/chu-bridge" "${common[@]}" --name "$name" "$@" 2> "$OUT/bridge_$name.log" &
  pids+=($!)
}
bridge random --engine "$GEN" --usi-option Seed=1
bridge random_mate1 --engine "$GEN" --usi-option Seed=2 --usi-option MateNodes=1
i=0
for net in softz a0gb; do
  bridge "az_$net" --engine "$AZ" --engine-arg --time-model --engine-arg "$OUT/time_model_$net.txt" \
    --usi-option "EvalPort=$((EVAL_PORT + i))" --usi-option "Sims=$SIMS"
  i=$((i + 1))
done

sleep "$(python3 -c "print(int($HOURS * 3600))")"
END=$(date +%s)
kill "$server"; wait "$server" 2>/dev/null
kill "${pids[@]}" 2>/dev/null; wait 2>/dev/null
echo "ran $START..$END"

status=0
for day in "$OUT"/records/*/*/*/*; do
  [ -d "$day" ] || continue
  python3 "$ROOT/vendor/chushogi/tests/replay_test.py" "$day" --binary "$GEN" | tail -1 || status=1
done
python3 "$ROOT/scripts/check_results.py" "$OUT/records" --db "$OUT/chu.db" --binary "$GEN" || status=1
"$BIN/chu-tools" rate --db "sqlite://$OUT/chu.db" --league "$LEAGUE" || status=1
python3 "$ROOT/scripts/check_gate3.py" "$OUT" --league "$LEAGUE" --start "$START" --end "$END" \
  --order random random_mate1 az_softz,az_a0gb || status=1
echo "$status" > "$OUT/DONE"
exit $status
