#!/usr/bin/env bash
# Phase-1 gate (DESIGN.md): GAMES games of `chushogi-gen go random` against
# itself in the `test` league (60 s + 1 s Fischer), through the server and two `chu-tools play`
# clients; then every record must pass vendor/chushogi/tests/replay_test.py
# and agree with random_match.py's referee (scripts/check_results.py).
#
# Usage: scripts/gate1.sh [OUT_DIR]     env: GAMES (100), PORT (14082)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$(mktemp -d)}
GAMES=${GAMES:-100}
PORT=${PORT:-14082}
GAME=test

cargo build --release -q --manifest-path "$ROOT/Cargo.toml"
BIN=$ROOT/target/release
GEN=$(ls -t "$BIN"/build/chu-rules-*/out/chushogi-gen | head -1)
mkdir -p "$OUT"
echo "output: $OUT"

"$BIN/chu-server" --listen "127.0.0.1:$PORT" --db "sqlite://$OUT/chu.db" --records "$OUT/records" \
  --leagues "$ROOT/leagues.toml" --http 127.0.0.1:0 2> "$OUT/server.log" &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
for _ in $(seq 50); do (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break; sleep 0.1; done

common=(--server "127.0.0.1:$PORT" --game "$GAME" --games "$GAMES" --gen "$GEN")
"$BIN/chu-tools" play "${common[@]}" --name random_a --color + --seed 1 > "$OUT/client_a.log" &
A=$!
"$BIN/chu-tools" play "${common[@]}" --name random_b --color - --seed 2 > "$OUT/client_b.log" &
B=$!
wait $A
wait $B
kill $SERVER
wait $SERVER 2>/dev/null || true

status=0
for day in "$OUT"/records/"$GAME"/*/*/*; do
  python3 "$ROOT/vendor/chushogi/tests/replay_test.py" "$day" --binary "$GEN" | tail -1 || status=1
done
python3 "$ROOT/scripts/check_results.py" "$OUT/records/$GAME" --db "$OUT/chu.db" --binary "$GEN" --expect "$GAMES" || status=1
exit $status
