#!/usr/bin/env bash
# Taro relay (Taro Enterprise): FRESH installation from this package, then its first-run setup.
#
#   sudo ./install.sh                  install, then start the setup (Wizard 2)
#   sudo ./install.sh --no-setup       install only; run `sudo taro-relay setup` later
#   sudo ./install.sh --recover        recovery: put this package's core back over a damaged one,
#                                      keeping /etc/taro and /var/lib/taro (the old core is saved)
#   sudo ./install.sh --replace-provider   also replace /etc/taro/provider with this package's
#                                      (the current one is saved first)
#
# Routine upgrades are not done with this script. They use enterprise-update.zip, which only
# replaces the shared core:   sudo taro-relay update apply --latest --yes
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"
SETUP=1 RECOVER=0 REPLACE_PROVIDER=0
for a in "$@"; do
  case "$a" in
    --no-setup) SETUP=0 ;;
    --recover) RECOVER=1 ;;
    --replace-provider) REPLACE_PROVIDER=1 ;;
    -h|--help) sed -n 2,14p "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

# TARO_ROOT: install under that folder instead of / (tests)
ROOT="${TARO_ROOT:-}"
OPT="$ROOT/opt/taro" ETC="$ROOT/etc/taro" VAR="$ROOT/var/lib/taro" BIN="$ROOT/usr/local/bin"
VERSION="$(cat VERSION)" PKG_ARCH="$(cat ARCH)"
STAMP="$(date -u +%Y%m%d%H%M%S)"
ok()   { printf '  \033[32m✔\033[0m %s\n' "$*"; }
note() { printf '  \033[33m!\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m✕\033[0m %s\n' "$*" >&2; exit 1; }

echo "Taro relay $VERSION ($PKG_ARCH)"
if [[ -z "$ROOT" ]]; then
  [[ $EUID -eq 0 ]] || fail "run it with sudo:  sudo $0 $*"
  command -v systemctl >/dev/null || fail "systemd is required"
fi
case "$(uname -m)" in x86_64|amd64) ARCH=x86_64 ;; aarch64|arm64) ARCH=aarch64 ;; *) ARCH="$(uname -m)" ;; esac
[[ "$ARCH" == "$PKG_ARCH" ]] || fail "this package is for $PKG_ARCH, this server is $ARCH: use the $ARCH package"
[[ -f provider/provider.json || -f "$ETC/provider/provider.json" ]] || fail "this package has no provider folder: build your provider's package with the Taro Enterprise package builder (Wizard 1)"

if [[ -f "$OPT/INSTALLED.json" && $RECOVER -eq 0 ]]; then
  have="$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$OPT/INSTALLED.json" | head -1)"
  echo
  echo "  The Taro relay $have is already installed here. This script only installs."
  echo "    upgrade:   sudo taro-relay update apply --latest --yes   (or a downloaded enterprise-update.zip)"
  echo "    settings:  sudo taro-relay setup"
  echo "    recovery:  sudo $0 --recover   (reinstalls the core, keeps settings and data)"
  exit 3
fi

if [[ $RECOVER -eq 1 && -d "$OPT/core" ]]; then
  mkdir -p "$VAR/recovery"
  cp -a "$OPT/core" "$VAR/recovery/core-$STAMP"
  [[ -f "$OPT/INSTALLED.json" ]] && cp -a "$OPT/INSTALLED.json" "$VAR/recovery/core-$STAMP/"
  rm -rf "$OPT/core"
  ok "the previous core is saved in $VAR/recovery/core-$STAMP"
fi

# the shared core
mkdir -p "$OPT" "$BIN"
cp -a core "$OPT/core.new"
[[ -z "$ROOT" ]] && chown -R root:root "$OPT/core.new"
chmod -R u=rwX,go=rX "$OPT/core.new"
chmod 0755 "$OPT/core.new/bin/taro-relay"
rm -rf "$OPT/core" && mv "$OPT/core.new" "$OPT/core"
ln -sfn "$OPT/core/bin/taro-relay" "$BIN/taro-relay"
ok "core installed in $OPT/core"

# the provider's customization: installed once, never overwritten without being asked
if [[ -f provider/provider.json ]]; then
  if [[ ! -f "$ETC/provider/provider.json" ]]; then
    mkdir -p "$ETC/provider"
    cp -a provider/. "$ETC/provider/"
    chmod 0644 "$ETC/provider/"* 2>/dev/null || true
    chmod 0640 "$ETC/provider/provider.json"
    ok "provider folder installed in $ETC/provider"
  elif cmp -s provider/provider.json "$ETC/provider/provider.json"; then
    ok "provider folder: unchanged"
  elif [[ $REPLACE_PROVIDER -eq 1 ]]; then
    mkdir -p "$VAR/recovery"
    cp -a "$ETC/provider" "$VAR/recovery/provider-$STAMP"
    cp -a provider/. "$ETC/provider/"
    chmod 0640 "$ETC/provider/provider.json"
    ok "provider folder replaced (the previous one is in $VAR/recovery/provider-$STAMP)"
  else
    note "the installed provider folder differs from this package's: kept (--replace-provider replaces it)"
  fi
fi

# TARO_ROOT only when installing under a folder (an empty one would make the paths relative)
RUN=(env); [[ -n "$ROOT" ]] && RUN=(env TARO_ROOT="$ROOT")
"${RUN[@]}" "$OPT/core/bin/taro-relay" install-core --version "$VERSION" --arch "$ARCH" >/dev/null
ok "recorded as version $VERSION"

if [[ $RECOVER -eq 1 && -z "$ROOT" ]] && systemctl list-unit-files taro-relay.service >/dev/null 2>&1; then
  systemctl restart taro-relay && ok "relay restarted"
fi

if [[ $SETUP -eq 1 && $RECOVER -eq 0 ]]; then
  echo
  exec "${RUN[@]}" "$OPT/core/bin/taro-relay" setup
fi
echo
echo "  Next: sudo taro-relay setup"
