#!/usr/bin/env bash
# End-to-end on one macOS machine: relay + remote-agent-mac + Rust client over loopback.
set -uo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname)" == "Darwin" ]] || { echo "must run on macOS" >&2; exit 2; }
mkdir -p out
./scripts/build-agent-macos.sh || exit 3
cargo build --release -p rm-relay -p rm-client 2>&1 | tail -2

# the ID + password mode users run (./remotemac --password ...): the Swift agent and the Rust
# client must derive the same relay session and token from them
ID=123456789
PASS="e2e-$(uuidgen | tr -d - | cut -c1-8)"
export RM_TESTAPP="$PWD/out/rm-testapp"
PORT=47900
./target/release/rm-relay 127.0.0.1:$PORT 2>out/relay.log &
RELAY=$!
sleep 1
./out/remote-agent-mac --relay 127.0.0.1:$PORT --id $ID --password "$PASS" >out/agent-banner.txt 2>out/agent.log &
AGENT=$!
sleep 2
echo "=== agent banner"; cat out/agent-banner.txt
grep -q "ID session to connect: 123 456 789" out/agent-banner.txt || { echo "agent banner missing the ID"; RC_BANNER=1; }
# a wrong password is refused at once
if ./target/release/remote-mac --relay 127.0.0.1:$PORT --id $ID --password wrong-password 2>out/client-wrong.log >/dev/null; then
  echo "wrong password was accepted"; RC_BANNER=1
fi
grep -q "session mismatch" out/client-wrong.log || { echo "wrong password: unexpected reply: $(cat out/client-wrong.log)"; RC_BANNER=1; }
./target/release/remote-mac --relay 127.0.0.1:$PORT --id $ID --password "$PASS" --e2e testapp 2>out/client.log | tee out/e2e.txt
RC=${PIPESTATUS[0]}
[[ -n "${RC_BANNER:-}" && $RC == 0 ]] && RC=1
# the Swift FEC must produce the same bytes as the Rust one, and video must have used UDP
grep -q "fec self-test ok" out/agent.log || { echo "Swift FEC self-test failed (parity differs from Rust)"; [[ $RC == 0 ]] && RC=1; }
UDP_FRAMES=$(sed -n 's/^UDP frames=\([0-9]*\).*/\1/p' out/e2e.txt)
echo "video frames received over UDP: ${UDP_FRAMES:-0}"
[[ "${UDP_FRAMES:-0}" -gt 30 ]] || { echo "video did not move to UDP"; [[ $RC == 0 ]] && RC=1; }
# loopback loses nothing: the measured loss must say so (it drives the bitrate)
LOSS=$(sed -n 's/^UDP .* loss=\([0-9.]*\)%.*/\1/p' out/e2e.txt)
echo "measured loss on loopback: ${LOSS:-?}%"
awk -v l="${LOSS:-100}" 'BEGIN { exit !(l < 3) }' || { echo "loss misreported on a clean link"; [[ $RC == 0 ]] && RC=1; }

# far, lossy link: a relay limited to 6 Mbit/s that drops 5% of UDP packets; FEC rebuilds them
# and the agent adapts its bitrate instead of queueing video (informational, not gating)
RM_RELAY_THROTTLE_KBPS=6000 RM_RELAY_UDP_LOSS_PCT=5 ./target/release/rm-relay 127.0.0.1:$((PORT+1)) 2>out/relay-slow.log &
SLOW=$!
sleep 1
./out/remote-agent-mac --relay 127.0.0.1:$((PORT+1)) --id 987654321 --password "$PASS" >/dev/null 2>out/agent-slow.log &
AGENT_SLOW=$!
sleep 2
./target/release/remote-mac --relay 127.0.0.1:$((PORT+1)) --id 987654321 --password "$PASS" --e2e testapp 2>out/client-slow.log | tee out/e2e-slow.txt || true
echo "=== far-link agent (bitrate adaptation, FEC)"; grep -E "bitrate|dropped|UDP" out/agent-slow.log | tail -14
kill $AGENT $AGENT_SLOW $RELAY $SLOW 2>/dev/null
echo "=== client log"; cat out/client.log
echo "=== agent log"; tail -40 out/agent.log
exit $RC
