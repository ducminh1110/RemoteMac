#!/usr/bin/env bash
# Mac side of the live test: builds the agent and connects it out to the relay server, then serves
# the Windows viewer until it disconnects, taking Mac screenshots meanwhile.
# Env: RM_RELAY (host:port), RM_SESSION, RM_SESSION_TOKEN, RM_RELAY_KEY.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p out/mac-screens
swiftc -O probe/macos/testapp.swift -o out/rm-testapp 2>out/compile-testapp.log || { cat out/compile-testapp.log; exit 3; }
swiftc -O agent/macos/*.swift -o out/remote-agent-mac 2>out/compile-agent.log || { cat out/compile-agent.log; exit 3; }
RM_TESTAPP="$PWD/out/rm-testapp" ./out/remote-agent-mac --relay "$RM_RELAY" --session "$RM_SESSION" 2>out/agent.log &
AGENT=$!
limit=$(( $(date +%s) + ${RM_WAIT_SECS:-1500} ))
n=0
while (( $(date +%s) < limit )) && kill -0 $AGENT 2>/dev/null; do
  grep -q "client disconnected" out/agent.log && { echo "viewer disconnected"; break; }
  if grep -q "handshake complete" out/agent.log && (( n < 40 )); then
    n=$((n + 1)); screencapture -x "out/mac-screens/mac-$(printf %02d $n).png" 2>/dev/null
  fi
  sleep 10
done
kill $AGENT 2>/dev/null
echo "=== agent log"; cat out/agent.log
grep -q "handshake complete" out/agent.log
