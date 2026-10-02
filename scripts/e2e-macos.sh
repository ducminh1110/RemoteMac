#!/usr/bin/env bash
# End-to-end on one macOS machine: relay + remote-agent-mac + Rust client over loopback.
set -uo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname)" == "Darwin" ]] || { echo "must run on macOS" >&2; exit 2; }
mkdir -p out
./scripts/build-agent-macos.sh || exit 3
cargo build --release -p rm-relay -p rm-client 2>&1 | tail -2

export RM_SESSION_TOKEN="e2e-$(uuidgen | tr -d -)"
export RM_TESTAPP="$PWD/out/rm-testapp"
PORT=47900
./target/release/rm-relay 127.0.0.1:$PORT 2>out/relay.log &
RELAY=$!
sleep 1
./out/remote-agent-mac --relay 127.0.0.1:$PORT --session e2e-1 2>out/agent.log &
AGENT=$!
sleep 2
./target/release/remote-mac --relay 127.0.0.1:$PORT --session e2e-1 --e2e testapp 2>out/client.log | tee out/e2e.txt
RC=${PIPESTATUS[0]}
kill $AGENT $RELAY 2>/dev/null
echo "=== client log"; cat out/client.log
echo "=== agent log"; tail -40 out/agent.log
exit $RC
