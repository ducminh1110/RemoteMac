#!/usr/bin/env bash
# Builds the macOS agent (Swift + the small Objective-C virtual-display shim) and the test app
# into out/. Used by every Mac script and workflow.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p out
swiftc -O probe/macos/testapp.swift -o out/rm-testapp 2>out/compile-testapp.log || { cat out/compile-testapp.log; exit 3; }
# built in: the relay used when none is given (release builds set RM_BUILD_DEFAULT_RELAY; from
# source there is none, and the Mac is reached on its own network only until --relay is given)
# and that relay's admission key (from a secret; empty otherwise)
printf 'let builtinRelay = "%s"\nlet builtinRelayKey = "%s"\n' "${RM_BUILD_DEFAULT_RELAY:-}" "${RM_BUILD_RELAY_KEY:-}" > out/BuildConfig.swift
clang -fobjc-arc -O2 ${RM_SWIFT_TARGET:+-target $RM_SWIFT_TARGET} -c agent/macos/VirtualDisplay.m -o out/VirtualDisplay.o 2>out/compile-vdisplay.log || { cat out/compile-vdisplay.log; exit 3; }
# the GameStream library (Rust, crates/rm-gamestream) for the same architecture
case "${RM_SWIFT_TARGET:-$(uname -m)}" in
  arm64*|aarch64*) RUST_TARGET=aarch64-apple-darwin ;;
  *) RUST_TARGET=x86_64-apple-darwin ;;
esac
rustup target add $RUST_TARGET >/dev/null 2>&1 || true
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --release -q -p rm-gamestream --target $RUST_TARGET 2>out/compile-gamestream.log || { cat out/compile-gamestream.log; echo "rm-gamestream failed to build" >&2; exit 3; }
GS_LIB=target/$RUST_TARGET/release
swiftc -O ${RM_SWIFT_TARGET:+-target $RM_SWIFT_TARGET} -import-objc-header agent/macos/Bridge.h agent/macos/*.swift out/BuildConfig.swift out/VirtualDisplay.o -L $GS_LIB -lrm_gamestream -framework CoreGraphics -framework Security -o out/remote-agent-mac 2>out/compile-agent.log || { cat out/compile-agent.log; echo "agent failed to compile" >&2; exit 3; }
grep -c warning out/compile-agent.log | xargs echo "agent compile warnings:"
