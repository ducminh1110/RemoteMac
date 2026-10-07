#!/usr/bin/env bash
# Mac side of the live test: builds the agent and connects it out to the relay server, then serves
# the Windows viewer until it disconnects, taking Mac screenshots meanwhile.
# Env: RM_RELAY (host:port), RM_RELAY_KEY, and RM_LIVE_ID + RM_LIVE_PASSWORD (or RM_SESSION +
# RM_SESSION_TOKEN).
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p out/mac-screens
./scripts/build-agent-macos.sh || exit 3
if [[ -n "${RM_LIVE_ID:-}" ]]; then
  # the way users run it: ID + password
  RM_TESTAPP="$PWD/out/rm-testapp" ./out/remote-agent-mac --logs-enabled --relay "$RM_RELAY" --id "$RM_LIVE_ID" --password "$RM_LIVE_PASSWORD" >out/agent-banner.txt 2>out/agent.log &
else
  RM_TESTAPP="$PWD/out/rm-testapp" ./out/remote-agent-mac --logs-enabled --relay "$RM_RELAY" --session "$RM_SESSION" 2>out/agent.log &
fi
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
