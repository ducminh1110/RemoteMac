#!/usr/bin/env bash
# The relay package from end to end, under a temporary root (no systemd needed):
# a fresh install, a second install refused, a recovery that keeps settings, data and branding.
#   scripts/test-relay-package.sh TARO-RELAY-BINARY        (run from enterprise/)
set -euo pipefail
B="$(realpath "$1")"
V="$("$B" version | awk '{print $2}')"
A="$(uname -m)"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
pkg="$(scripts/package-relay.sh "$V" "$A" "$B" "$W/out" | tail -1)"
tar -tzf "$pkg" | sort > "$W/list"
for f in install.sh VERSION ARCH README.md core/bin/taro-relay core/web/relay/setup/index.html core/web/relay/status/app.js core/web/relay/common/liquidglass.js core/web/relay/common/taro-glass.css provider/README.txt; do
  grep -q "/$f\$" "$W/list" || { echo "missing $f in the package"; exit 1; }
done
tar -xzf "$pkg" -C "$W"
P="$W/taro-relay-$V-linux-$A"
# what Wizard 1 adds
cat > "$P/provider/provider.json" <<JSON
{"schema":"taro.provider/v1","provider_id":"acme-cloud","product":{"name":"Acme Connect","short_name":"Acme","vendor":"Acme Cloud Ltd"},
 "branding":{"primary_color":"#0F766E","theme":"system"},"relay":{"host":"relay.acme.example","port":7470,"key":"acme-admission-key-0123456789"}}
JSON
R="$W/root"
TARO_ROOT="$R" "$P/install.sh" --no-setup
test -x "$R/opt/taro/core/bin/taro-relay" && test -L "$R/usr/local/bin/taro-relay"
grep -q "\"version\": \"$V\"" "$R/opt/taro/INSTALLED.json"
test "$(stat -c %a "$R/etc/taro/provider/provider.json")" = 640
cmp "$P/provider/provider.json" "$R/etc/taro/provider/provider.json"
echo "fresh install: ok"

# a second install is refused: upgrades are enterprise-update.zip's job
set +e; TARO_ROOT="$R" "$P/install.sh" --no-setup >"$W/again" 2>&1; rc=$?; set -e
test $rc -eq 3 && grep -q "update apply" "$W/again"
echo "second install refused: ok"

# the server has run: settings, data, a certificate, a branding change by the provider
mkdir -p "$R/etc/taro/runtime" "$R/var/lib/taro/relay" "$R/var/lib/taro/tls"
echo 'schema = "x"' > "$R/etc/taro/runtime/relay.toml"
echo '{"123456789":"o"}' > "$R/var/lib/taro/relay/ids.json"
echo KEY > "$R/var/lib/taro/tls/privkey.pem"
sed -i 's/#0F766E/#2563EB/' "$R/etc/taro/provider/provider.json"
cp "$R/etc/taro/provider/provider.json" "$W/provider.before"
# the core gets damaged
rm "$R/opt/taro/core/web/relay/setup/app.js"
echo broken > "$R/opt/taro/core/bin/taro-relay"; chmod +x "$R/opt/taro/core/bin/taro-relay"
TARO_ROOT="$R" "$P/install.sh" --recover
cmp "$P/core/bin/taro-relay" "$R/opt/taro/core/bin/taro-relay"
test -f "$R/opt/taro/core/web/relay/setup/app.js"
ls "$R/var/lib/taro/recovery/" | grep -q '^core-'
cmp "$W/provider.before" "$R/etc/taro/provider/provider.json"
grep -q 'schema = "x"' "$R/etc/taro/runtime/relay.toml"
grep -q 123456789 "$R/var/lib/taro/relay/ids.json"
grep -q KEY "$R/var/lib/taro/tls/privkey.pem"
echo "recovery keeps settings, data and branding: ok"

# the wrong architecture is refused
echo other > "$P/ARCH"
set +e; TARO_ROOT="$W/root2" "$P/install.sh" --no-setup >/dev/null 2>&1; rc=$?; set -e
test $rc -ne 0
echo "wrong architecture refused: ok"
