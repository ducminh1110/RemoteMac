#!/usr/bin/env bash
# Builds enterprise-update.zip for the relay component from the release's relay packages.
#   scripts/build-enterprise-update.sh VERSION OUT-DIR PACKAGE.tar.gz... [--base enterprise-core-files.json]
# Needs TARO_ENTERPRISE_SIGNING_KEY. Writes OUT-DIR/enterprise-update.zip and
# OUT-DIR/enterprise-core-files.json (the next release's base). Run from enterprise/.
set -euo pipefail
V="$1" OUT="$2"; shift 2
BASE=() PKGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE=(--base "$2"); shift 2 ;;
    *) PKGS+=("$1"); shift ;;
  esac
done
mkdir -p "$OUT"
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
CORES=()
for p in "${PKGS[@]}"; do
  tar -xzf "$p" -C "$W"
  d="$W/$(basename "$p" .tar.gz)"
  a="$(cat "$d/ARCH")"
  [[ "$(cat "$d/VERSION")" == "$V" ]] || { echo "$p is version $(cat "$d/VERSION"), not $V" >&2; exit 1; }
  CORES+=(--core "$a=$d/core")
done
MIG=()
if ls apps/relay/migrations/*.json >/dev/null 2>&1; then MIG=(--migrations apps/relay/migrations); fi
TU="${TARO_UPDATE_BIN:-target/release/taro-update}"
"$TU" pack --component relay --version "$V" "${CORES[@]}" "${BASE[@]}" "${MIG[@]}" \
  --restart taro-relay --commit "${GITHUB_SHA:-$(git rev-parse HEAD)}" \
  --out "$OUT/enterprise-update.zip" --core-files-out "$OUT/enterprise-core-files.json"
# what is in it: only the manifest, its signature, the checksums and approved core files
unzip -Z1 "$OUT/enterprise-update.zip" | while read -r n; do
  case "$n" in
    manifest.json|manifest.json.sig|checksums.sha256) ;;
    payload/any/core/*|payload/x86_64/core/*|payload/aarch64/core/*|migrations/*.json) ;;
    *) echo "unexpected entry in enterprise-update.zip: $n" >&2; exit 1 ;;
  esac
done
echo "$OUT/enterprise-update.zip"
