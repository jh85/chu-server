#!/usr/bin/env bash
# Bots on a running chu-server: random, random_mate1 (chushogi-gen with
# MateNodes=1) and one chushogi-az bot per checkpoint, each with its own eval
# server. They queue in LEAGUE and play until stopped (Ctrl-C stops all); a
# bridge that exits is started again after 30 s.
#
# Usage: scripts/bots.sh [SERVER] [[NAME=]CHECKPOINT ...]
#   SERVER      host:port of the chu-server; also from $SERVER (127.0.0.1:4081)
#   CHECKPOINT  a .pt file; its bot is az_NAME, NAME defaulting to the file
#               name without .pt. Without any: softz=softz_iter_0202.pt and
#               a0gb=a0gb_iter_0202.pt from the repo root.
# env: LEAGUE (floodgate-600-10F), PASSWORD (bots; a first login creates the
#      account with it), OUT (logs; a new temp dir), PYTHON (python3, with
#      torch), EVAL_PORT (51630, then +1 per checkpoint), SIMS (1024),
#      THREADS (eval-server threads; 2 x cores / checkpoints, at most 12)
# e.g. scripts/bots.sh 192.168.50.116:4081 softz=softz_iter_0202.pt \
#        a0gb=a0gb_iter_0202.pt ~/Downloads/iter_0262.pt ~/Downloads/iter_0322.pt
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SERVER=${SERVER:-127.0.0.1:4081}
if [[ $# -gt 0 && $1 != *.pt ]]; then SERVER=$1; shift; fi
[[ $# -gt 0 ]] || set -- "softz=$ROOT/softz_iter_0202.pt" "a0gb=$ROOT/a0gb_iter_0202.pt"

names=() ckpts=()
for arg in "$@"; do
  if [[ $arg == *=* ]]; then name=${arg%%=*} path=${arg#*=}; else path=$arg name=$(basename "$arg" .pt); fi
  [[ "az_$name" =~ ^[A-Za-z0-9_-]{1,32}$ ]] || { echo "bad bot name: az_$name" >&2; exit 2; }
  [[ -f $path ]] || { echo "no such checkpoint: $path" >&2; exit 2; }
  for n in "${names[@]}"; do [[ $n != "$name" ]] || { echo "duplicate bot name: az_$name" >&2; exit 2; }; done
  names+=("$name") ckpts+=("$(realpath "$path")")
done

LEAGUE=${LEAGUE:-floodgate-600-10F}
PASSWORD=${PASSWORD:-bots}
OUT=${OUT:-$(mktemp -d)}
PYTHON=${PYTHON:-python3}
EVAL_PORT=${EVAL_PORT:-51630}
SIMS=${SIMS:-1024}
THREADS=${THREADS:-$(( t = 2 * $(nproc) / ${#names[@]}, t > 12 ? 12 : (t < 1 ? 1 : t) ))}
AZ=$ROOT/vendor/chushogi/chushogi-az
GEN=$ROOT/vendor/chushogi/chushogi-gen
set -e
make -s -C "$ROOT/vendor/chushogi" chushogi-gen chushogi-az
cargo build --release -q --manifest-path "$ROOT/Cargo.toml"
set +e
BIN=$ROOT/target/release
mkdir -p "$OUT"
echo "logs: $OUT"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null; wait 2>/dev/null' EXIT
trap 'exit 130' INT TERM

DEVICE=$("$PYTHON" -c 'import torch; print("" if torch.cuda.is_available() else "--cpu")')
for i in "${!names[@]}"; do
  net=${names[$i]} port=$((EVAL_PORT + i))
  (cd "$ROOT/vendor/chushogi" && OMP_NUM_THREADS=$THREADS exec "$PYTHON" az/eval_server.py \
    --checkpoint "${ckpts[$i]}" --port "$port" --max-batch 128 --batch-wait-ms 1 --tcp-nodelay $DEVICE) \
    > "$OUT/eval_$net.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 120); do grep -q "serving on" "$OUT/eval_$net.log" && break; sleep 1; done
  grep -q "serving on" "$OUT/eval_$net.log" || { cat "$OUT/eval_$net.log"; exit 1; }
  "$PYTHON" "$ROOT/scripts/az_time_model.py" --az "$AZ" --gen "$GEN" --port "$port" "$OUT/time_model_$net.txt" 2> /dev/null
  echo "az_$net: ${ckpts[$i]}, port $port, $(tail -1 "$OUT/time_model_$net.txt") (sims ms)"
done

common=(--server "$SERVER" --password "$PASSWORD" --game "$LEAGUE" --games 1000000 --log-format json)
# Runs "$@" again 30 s after it exits, until this subshell is stopped.
supervise() {
  local child
  trap 'kill $child 2>/dev/null; exit 0' INT TERM
  while :; do
    "$@" & child=$!
    wait $child
    echo "{\"message\":\"bots.sh: chu-bridge exited ($?); restarting in 30 s\"}" >&2
    sleep 30 & child=$!
    wait $child
  done
}
bridge() {
  local name=$1; shift
  supervise "$BIN/chu-bridge" "${common[@]}" --name "$name" "$@" 2>> "$OUT/bridge_$name.log" &
  pids+=($!)
}
bridge random --engine "$GEN" --usi-option Seed=1
bridge random_mate1 --engine "$GEN" --usi-option Seed=2 --usi-option MateNodes=1
for i in "${!names[@]}"; do
  bridge "az_${names[$i]}" --engine "$AZ" --engine-arg --time-model --engine-arg "$OUT/time_model_${names[$i]}.txt" \
    --usi-option "EvalPort=$((EVAL_PORT + i))" --usi-option "Sims=$SIMS"
done
echo "$((${#names[@]} + 2)) bots queued for $LEAGUE on $SERVER; Ctrl-C stops them"
wait
