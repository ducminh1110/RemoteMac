#!/usr/bin/env bash
# Build and run the macOS feasibility probe. Intended for a macOS runner.
set -uo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname)" == "Darwin" ]] || { echo "must run on macOS" >&2; exit 2; }
mkdir -p out
swiftc -O probe/macos/testapp.swift -o out/rm-testapp 2>out/compile-testapp.log || { cat out/compile-testapp.log; echo "testapp failed to compile" >&2; exit 3; }
swiftc -O probe/macos/main.swift -o out/rm-probe 2>out/compile.log || { cat out/compile.log; echo "probe failed to compile" >&2; exit 3; }
./out/rm-probe > out/probe.json
rc=$?
cat out/probe.json
# gate verdict: G1,G3,G4,G5 must pass; G6 software ok; G2b is informational
python3 - <<'PY'
import json,sys
d=json.load(open("out/probe.json"))
st={g["id"]:g["status"] for g in d["gates"]}
required=["G1","G3","G4","G5","G6","G7"]
bad=[k for k in required if st.get(k)!="pass"]
print("\nINFO (TextEdit, not required):", {k:v for k,v in st.items() if k.startswith("T")})
print("\nGATE VERDICT:", "GO" if not bad else "NO-GO (failed/inconclusive: %s)"%", ".join(bad))
m1=["M1a","M1b","M1c","M1d"]
bad1=[k for k in m1 if st.get(k)!="pass"]
print("M1 VERDICT:", "GO" if not bad1 else "NO-GO (failed/inconclusive: %s)"%", ".join(bad1))
for g in d["gates"]:
    if g["id"] in m1: print("  ", g["id"], g["status"], "-", g["detail"])
sys.exit(0 if not bad and not bad1 else 1)
PY
