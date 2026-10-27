#!/usr/bin/env bash
# Builds the generic Taro relay package (no provider folder yet; Wizard 1 adds it):
#   scripts/package-relay.sh VERSION ARCH TARO-RELAY-BINARY OUT-DIR
#   PG_DIR=DIR: the PostgreSQL for the portal (scripts/fetch-postgres.sh ARCH DIR) goes into
#   core/share/pg; the portal and the console go into core/web/enterprise
# -> OUT-DIR/taro-relay-VERSION-linux-ARCH.tar.gz (and the unpacked folder, whose core/ is what
#    enterprise-update.zip is packed from). Run from enterprise/.
set -euo pipefail
V="$1" A="$2" B="$3" OUT="$4"
d="$OUT/taro-relay-$V-linux-$A"
rm -rf "$d" && mkdir -p "$d/core/bin" "$d/core/web/relay" "$d/core/share" "$d/provider"
install -m 0755 "$B" "$d/core/bin/taro-relay"
cp -r apps/relay/web/setup apps/relay/web/status "$d/core/web/relay/"
mkdir -p "$d/core/web/relay/common"
cp packages/console/console.css packages/console/console.js "$d/core/web/relay/common/"
# the portal, the console and their shared look (the relay serves them: one web site)
D="$(cd "$d" && pwd)"
for a in customer-portal provider-console; do
  mkdir -p "$D/core/web/enterprise/apps/$a"
  (cd "apps/$a" && find . -type f ! -path './tests/*' ! -name '*.md' -exec cp --parents {} "$D/core/web/enterprise/apps/$a/" \;)
done
mkdir -p "$d/core/web/enterprise/packages"
cp -r packages/console packages/design-tokens "$d/core/web/enterprise/packages/"
if [[ -n "${PG_DIR:-}" ]]; then
  cp -a "$PG_DIR" "$d/core/share/pg"
fi
cp ../LICENSE "$d/core/share/LICENSE"
cp ../NOTICE.md "$d/core/share/NOTICE.md"
install -m 0755 apps/relay/package/install.sh "$d/install.sh"
cp apps/relay/package/README.md "$d/README.md"
echo "$V" > "$d/VERSION"
echo "$A" > "$d/ARCH"
echo "The Taro Enterprise package builder (Wizard 1) puts provider.json, logo.png and icon.png here." > "$d/provider/README.txt"
find "$d" -type f ! -path '*/bin/*' ! -name install.sh -exec chmod 0644 {} +
tar -C "$OUT" --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner -czf "$d.tar.gz" "$(basename "$d")"
echo "$d.tar.gz"
