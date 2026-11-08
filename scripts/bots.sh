#!/usr/bin/env bash
# Four bots on a running chu-server, as in the 48-hour gate: random,
# random_mate1 (chushogi-gen with MateNodes=1), az_softz and az_a0gb
# (chushogi-az with softz_iter_0202.pt / a0gb_iter_0202.pt, one eval server
# each). They queue in LEAGUE and play until stopped (Ctrl-C stops all).
#
# Usage: scripts/bots.sh [SERVER]      SERVER: host:port (127.0.0.1:4081)
# env: LEAGUE (floodgate-600-10F), PASSWORD (bots; a first login creates the
#      account with it), OUT (logs; a new temp dir), PYTHON (python3, with
#      torch), EVAL_PORT (51630, and +1), SIMS (1024), THREADS (12)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SERVER=${1:-127.0.0.1:4081}
LEAGUE=${LEAGUE:-floodgate-600-10F}
PASSWORD=${PASSWORD:-bots}
OUT=${OUT:-$(mktemp -d)}
PYTHON=${PYTHON:-python3}
EVAL_PORT=${EVAL_PORT:-51630}
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
echo "logs: $OUT"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null; wait 2>/dev/null' EXIT
trap 'exit 130' INT TERM

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

common=(--server "$SERVER" --password "$PASSWORD" --game "$LEAGUE" --games 1000000 --log-format json)
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
echo "4 bots queued for $LEAGUE on $SERVER; Ctrl-C stops them"
wait
