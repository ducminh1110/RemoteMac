#!/usr/bin/env bash
# Builds the macOS agent (Swift + the small Objective-C virtual-display shim) and the test app
# into out/. Used by every Mac script and workflow.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p out
swiftc -O probe/macos/testapp.swift -o out/rm-testapp 2>out/compile-testapp.log || { cat out/compile-testapp.log; exit 3; }
# relay admission key built in (release builds pass RM_RELAY_KEY from a secret; empty otherwise)
printf 'let builtinRelayKey = "%s"\n' "${RM_BUILD_RELAY_KEY:-}" > out/BuildConfig.swift
clang -fobjc-arc -O2 ${RM_SWIFT_TARGET:+-target $RM_SWIFT_TARGET} -c agent/macos/VirtualDisplay.m -o out/VirtualDisplay.o 2>out/compile-vdisplay.log || { cat out/compile-vdisplay.log; exit 3; }
swiftc -O ${RM_SWIFT_TARGET:+-target $RM_SWIFT_TARGET} -import-objc-header agent/macos/VirtualDisplay.h agent/macos/*.swift out/BuildConfig.swift out/VirtualDisplay.o -framework CoreGraphics -o out/remote-agent-mac 2>out/compile-agent.log || { cat out/compile-agent.log; echo "agent failed to compile" >&2; exit 3; }
grep -c warning out/compile-agent.log | xargs echo "agent compile warnings:"
