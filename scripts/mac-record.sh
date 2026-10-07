#!/usr/bin/env bash
# Records a real Mac session for replay on Windows: relay + remote-agent-mac on this Mac (loopback
# only) and the recorder client, which opens each app (Xcode, TextEdit, ...), keeps what its windows
# show (video, titles, menu bar, icon) and closes it again. Output: out/recording/session.rmrec plus
# Mac screenshots of each app for comparison.
set -uo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname)" == "Darwin" ]] || { echo "must run on macOS" >&2; exit 2; }
APPS="${1:-xcode,textedit,testapp}"
mkdir -p out/recording
./scripts/build-agent-macos.sh || exit 3
cargo build --release -p rm-relay -p rm-client 2>&1 | tail -1

export RM_SESSION_TOKEN="rec-$(uuidgen | tr -d -)"
export RM_TESTAPP="$PWD/out/rm-testapp"
PORT=47960
./target/release/rm-relay 127.0.0.1:$PORT 2>out/relay.log &
RELAY=$!
sleep 1
./out/remote-agent-mac --logs-enabled --relay 127.0.0.1:$PORT --session rec-1 2>out/agent.log &
AGENT=$!
sleep 2
./target/release/remote-mac --relay 127.0.0.1:$PORT --session rec-1 --record out/recording/session.rmrec --apps "$APPS" --settle 15 --shots out/recording 2>out/recorder.log
RC=$?
kill $AGENT $RELAY 2>/dev/null
echo "=== recorder log"; cat out/recorder.log
echo "=== agent log"; tail -40 out/agent.log
ls -l out/recording
exit $RC
