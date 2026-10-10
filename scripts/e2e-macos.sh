#!/usr/bin/env bash
# End-to-end on one macOS machine: relay + remote-agent-mac + Rust client over loopback
# (found on the local network by its ID; the far-link run goes through the relay).
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
./out/remote-agent-mac --logs-enabled --relay 127.0.0.1:$PORT --id $ID --password "$PASS" >out/agent-banner.txt 2>out/agent.log &
AGENT=$!
sleep 2
echo "=== agent banner"; cat out/agent-banner.txt
grep -q "ID session to connect: 123 456 789" out/agent-banner.txt || { echo "agent banner missing the ID"; RC_BANNER=1; }
# a wrong password is refused at once by the Mac (end-to-end handshake), through the relay and
# on this network alike; the relay never sees anything it could check a password against
if RM_NO_LAN=1 ./target/release/remote-mac --relay 127.0.0.1:$PORT --id $ID --password wrong-password 2>out/client-wrong.log >/dev/null; then
  echo "wrong password was accepted (relay)"; RC_BANNER=1
fi
grep -q "wrong password" out/client-wrong.log || { echo "wrong password (relay): unexpected reply: $(cat out/client-wrong.log)"; RC_BANNER=1; }
sleep 3 # the Mac starts over for the next viewer
if ./target/release/remote-mac --id $ID --password wrong-password 2>out/client-wrong-lan.log >/dev/null; then
  echo "wrong password was accepted (LAN)"; RC_BANNER=1
fi
grep -q "wrong password" out/client-wrong-lan.log || { echo "wrong password (LAN): unexpected reply: $(cat out/client-wrong-lan.log)"; RC_BANNER=1; }
sleep 3
# the viewer finds the Mac on this network by its ID and connects straight to it
./target/release/remote-mac --relay 127.0.0.1:$PORT --id $ID --password "$PASS" --e2e testapp 2>out/client.log | tee out/e2e.txt
RC=${PIPESTATUS[0]}
[[ -n "${RC_BANNER:-}" && $RC == 0 ]] && RC=1
# the Swift FEC must produce the same bytes as the Rust one, and video must have used UDP
grep -q "fec self-test ok" out/agent.log || { echo "Swift FEC self-test failed (parity differs from Rust)"; [[ $RC == 0 ]] && RC=1; }
grep -q "secure self-test ok" out/agent.log || { echo "Swift secure self-test failed (differs from Rust)"; [[ $RC == 0 ]] && RC=1; }
grep -q "end-to-end encrypted" out/client.log || { echo "the session was not end-to-end encrypted"; [[ $RC == 0 ]] && RC=1; }
UDP_FRAMES=$(sed -n 's/^UDP frames=\([0-9]*\).*/\1/p' out/e2e.txt)
echo "video frames received over UDP: ${UDP_FRAMES:-0}"
[[ "${UDP_FRAMES:-0}" -gt 30 ]] || { echo "video did not move to UDP"; [[ $RC == 0 ]] && RC=1; }
# loopback loses nothing: the measured loss must say so (it drives the bitrate)
LOSS=$(sed -n 's/^UDP .* loss=\([0-9.]*\)%.*/\1/p' out/e2e.txt)
echo "measured loss on loopback: ${LOSS:-?}%"
awk -v l="${LOSS:-100}" 'BEGIN { exit !(l < 3) }' || { echo "loss misreported on a clean link"; [[ $RC == 0 ]] && RC=1; }
# client and agent swap addresses and punch through: video ends up on the direct path
grep -q "path=direct:" out/e2e.txt || { echo "no direct path between client and agent"; [[ $RC == 0 ]] && RC=1; }
grep -q "direct path to the client" out/agent.log || { echo "agent never saw a direct path"; [[ $RC == 0 ]] && RC=1; }
grep -q "client connected (on this network)" out/agent.log || { echo "the client did not come straight over the local network"; [[ $RC == 0 ]] && RC=1; }

# a viewer whose network vanishes without a word: the Mac notices (heartbeat) and takes the next
# connection; the apps of the lost session stay open
sleep 3
./target/release/remote-mac --id $ID --password "$PASS" --vanish 2>out/client-vanish.log &
VANISH=$!
for _ in $(seq 1 30); do grep -q "nothing from the viewer" out/agent.log && break; sleep 1; done
grep -q "nothing from the viewer" out/agent.log || { echo "the Mac did not notice a viewer gone silent"; [[ $RC == 0 ]] && RC=1; }
grep -q "the apps stay open" out/agent.log || { echo "the Mac did not keep the apps for a lost connection"; [[ $RC == 0 ]] && RC=1; }
sleep 3
if ./target/release/remote-mac --id $ID --password "$PASS" >out/client-after.log 2>&1; then
  echo "a new connection after the lost one: ok"
else
  echo "no new connection after a lost one: $(tail -3 out/client-after.log)"; [[ $RC == 0 ]] && RC=1
fi
kill $VANISH 2>/dev/null

# a viewer that typed the Mac's address (IPv4 or IPv6, any network): straight to the Mac, as
# Moonlight to Sunshine: only the address and the password (no ID, no discovery, no relay), the
# same end-to-end handshake; a wrong password is refused the same way
for ADDR in 127.0.0.1:7471 "[::1]:7471"; do
  sleep 3
  if ./target/release/remote-mac --direct "$ADDR" --password "$PASS" >out/client-direct.log 2>&1 \
     && grep -q "straight to" out/client-direct.log && grep -q "end-to-end encrypted" out/client-direct.log; then
    echo "connected by the typed address $ADDR: ok"
  else
    echo "no connection by the typed address $ADDR: $(tail -3 out/client-direct.log)"; [[ $RC == 0 ]] && RC=1
  fi
done
sleep 3
grep -q "straight to this Mac's address" out/agent.log || { echo "the agent did not take the direct session"; [[ $RC == 0 ]] && RC=1; }
if ./target/release/remote-mac --direct 127.0.0.1:7471 --password wrong-password >out/client-direct-wrong.log 2>&1; then
  echo "wrong password was accepted (typed address)"; [[ $RC == 0 ]] && RC=1
fi
grep -q "wrong password" out/client-direct-wrong.log || { echo "wrong password (typed address): unexpected reply: $(cat out/client-direct-wrong.log)"; [[ $RC == 0 ]] && RC=1; }
grep -q "Or type this Mac's address" out/agent-banner.txt || { echo "the banner does not show the Mac's address"; [[ $RC == 0 ]] && RC=1; }

# the launcher users start (./macbridge.sh): it passes options on, says when the program is
# missing, and reports the privacy permissions (granted or not depends on this runner)
L=out/launcher; rm -rf $L; mkdir -p $L
cp agent/macos/macbridge.sh $L/
"$L/macbridge.sh" --version >/dev/null 2>out/launcher-missing.txt; LRC=$?
[[ $LRC == 2 ]] && grep -q "not next to this script" out/launcher-missing.txt \
  || { echo "launcher without the program: exit $LRC, $(cat out/launcher-missing.txt)"; [[ $RC == 0 ]] && RC=1; }
cp out/remote-agent-mac $L/macbridge
[[ "$("$L/macbridge.sh" --version)" == "macbridge "* ]] || { echo "the launcher does not pass --version on"; [[ $RC == 0 ]] && RC=1; }
"$L/macbridge.sh" --check >out/launcher-check.txt 2>&1; LRC=$?
echo "=== launcher --check (exit $LRC)"; cat out/launcher-check.txt
{ [[ $LRC == 0 || $LRC == 3 ]] && grep -q "MacBridge permissions:" out/launcher-check.txt; } \
  || { echo "the launcher's permission check failed"; [[ $RC == 0 ]] && RC=1; }

# in the background, as users start it from a terminal (a pseudo-terminal here): the copy in
# the background watches over the one doing the work; that one dying (a crash) is started again
# and the Mac is reachable again; --stop ends both
B=out/bg; rm -rf $B; mkdir -p $B; cp out/remote-agent-mac $B/macbridge
BG_ID=456789123
bgconnect() { RM_NO_LAN=1 ./target/release/remote-mac --relay 127.0.0.1:$PORT --id $BG_ID --password "$PASS" >out/client-bg-$1.log 2>&1; }
RM_NO_LAN=1 script -q /dev/null $B/macbridge --logs-enabled --relay 127.0.0.1:$PORT --id $BG_ID --password "$PASS" >out/bg-banner.txt 2>&1 </dev/null
sleep 3
SUP=$(tr -d '[:space:]' <"$HOME/Library/Application Support/RemoteMac/macbridge.pid" 2>/dev/null)
W1=$(pgrep -P "${SUP:-0}" | head -1)
if [[ -n "$SUP" && -n "$W1" ]] && bgconnect 1; then
  echo "in the background (watcher $SUP, worker $W1): reachable"
else
  echo "in the background: not reachable (watcher '$SUP', worker '$W1'): $(tail -3 out/client-bg-1.log)"; tail -8 out/bg-banner.txt; [[ $RC == 0 ]] && RC=1
fi
[[ -n "$W1" ]] && kill -9 "$W1"
sleep 4
W2=$(pgrep -P "${SUP:-0}" | head -1)
if [[ -n "$W2" && "$W2" != "$W1" ]] && bgconnect 2; then
  echo "a worker that died was started again ($W2): reachable"
else
  echo "after the worker died: not reachable (worker '$W2'): $(tail -3 out/client-bg-2.log)"; [[ $RC == 0 ]] && RC=1
fi
$B/macbridge --stop >out/bg-stop.txt 2>&1 || { echo "--stop failed: $(cat out/bg-stop.txt)"; [[ $RC == 0 ]] && RC=1; }
sleep 2
if pgrep -f "$B/macbridge" >/dev/null; then echo "--stop left MacBridge running: $(pgrep -fl "$B/macbridge")"; [[ $RC == 0 ]] && RC=1; fi
echo "=== background log"; tail -25 "$HOME/Library/Logs/MacBridge/macbridge.log" 2>/dev/null

# far, lossy link: a relay limited to 6 Mbit/s that drops 5% of UDP packets; FEC rebuilds them
# and the agent adapts its bitrate instead of queueing video (informational, not gating)
# (RM_NO_P2P, RM_NO_LAN: this one must go through the throttled relay)
export RM_NO_P2P=1 RM_NO_LAN=1
RM_RELAY_THROTTLE_KBPS=6000 RM_RELAY_UDP_LOSS_PCT=5 ./target/release/rm-relay 127.0.0.1:$((PORT+1)) 2>out/relay-slow.log &
SLOW=$!
sleep 1
./out/remote-agent-mac --logs-enabled --relay 127.0.0.1:$((PORT+1)) --id 987654321 --password "$PASS" >/dev/null 2>out/agent-slow.log &
AGENT_SLOW=$!
sleep 2
./target/release/remote-mac --relay 127.0.0.1:$((PORT+1)) --id 987654321 --password "$PASS" --e2e testapp 2>out/client-slow.log | tee out/e2e-slow.txt || true
echo "=== far-link agent (bitrate adaptation, FEC)"; grep -E "bitrate|dropped|UDP" out/agent-slow.log | tail -14
kill $AGENT $AGENT_SLOW $RELAY $SLOW 2>/dev/null
echo "=== client log"; cat out/client.log
echo "=== agent log"; tail -40 out/agent.log
exit $RC
