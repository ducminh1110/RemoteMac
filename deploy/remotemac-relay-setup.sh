#!/usr/bin/env bash
# RemoteMac relay server: one-shot setup from A to Z (Ubuntu / Debian).
#
#   sudo ./remotemac-relay-setup.sh                # install or update, keep the existing key
#   sudo ./remotemac-relay-setup.sh --port 7470 --key KEY
#
# It installs the rm-relay binary (bundled next to this script, or built from the bundled source
# when this machine's CPU has no prebuilt binary), stores the admission key, runs the relay as a
# systemd service, opens the port in the local firewall and checks that it answers. At the end it
# prints which ports to open in your hosting provider's firewall.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PORT=7470
KEY=""
# key the RemoteMac.exe / remotemac builds of this release carry (filled in by the release build)
BUILTIN_KEY="__RM_BUILTIN_KEY__"
DNS_NAME="remotemac.mooo.com"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --key) KEY="$2"; shift 2 ;;
    -h|--help) sed -n 2,10p "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
ok()   { printf '    \033[32m✔\033[0m %s\n' "$*"; }
warn() { printf '    \033[33m!\033[0m %s\n' "$*"; }

[[ $EUID -eq 0 ]] || { echo "run with sudo:  sudo $0 $*" >&2; exit 1; }
command -v systemctl >/dev/null || { echo "systemd is required" >&2; exit 1; }

step "1/6  Packages"
export DEBIAN_FRONTEND=noninteractive
if command -v apt-get >/dev/null; then
  apt-get update -qq
  apt-get install -y -qq ca-certificates curl openssl iproute2 netcat-openbsd >/dev/null
  ok "curl, openssl, iproute2, netcat"
else
  warn "not a Debian/Ubuntu system: make sure curl, openssl and ss are installed"
fi

step "2/6  rm-relay binary"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) BIN="$HERE/rm-relay-x86_64" ;;
  aarch64|arm64) BIN="$HERE/rm-relay-aarch64" ;;
  *) BIN="" ;;
esac
if [[ -n "$BIN" && -x "$BIN" ]]; then
  install -m 0755 "$BIN" /usr/local/bin/rm-relay
  ok "installed the prebuilt binary for $ARCH"
else
  warn "no prebuilt binary for $ARCH: building from the bundled source (a few minutes)"
  [[ -f "$HERE/remotemac-src.tar.gz" ]] || { echo "remotemac-src.tar.gz is missing next to this script" >&2; exit 1; }
  command -v apt-get >/dev/null && apt-get install -y -qq build-essential >/dev/null
  BUILD="$(mktemp -d)"
  tar -xzf "$HERE/remotemac-src.tar.gz" -C "$BUILD"
  export RUSTUP_HOME="$BUILD/rustup" CARGO_HOME="$BUILD/cargo"
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none >/dev/null
  (cd "$BUILD" && "$CARGO_HOME/bin/rustup" toolchain install --profile minimal >/dev/null && "$CARGO_HOME/bin/cargo" build --release -q -p rm-relay)
  install -m 0755 "$BUILD/target/release/rm-relay" /usr/local/bin/rm-relay
  rm -rf "$BUILD"
  ok "built and installed rm-relay"
fi

step "3/6  Admission key"
mkdir -p /etc/remote-mac
ENV_FILE=/etc/remote-mac/relay.env
OLD_KEY="$(sed -n 's/^RM_RELAY_KEY=//p' "$ENV_FILE" 2>/dev/null || true)"
if [[ -z "$KEY" ]]; then
  if [[ "$BUILTIN_KEY" != "__RM_BUILTIN_KEY__" ]]; then
    KEY="$BUILTIN_KEY"   # what this release's Mac and Windows apps send: they work out of the box
  elif [[ -n "$OLD_KEY" ]]; then
    KEY="$OLD_KEY"
  else
    KEY="$(openssl rand -hex 24)"
    warn "generated a new key: the Mac and Windows apps need it (RM_RELAY_KEY) or a rebuild with it"
  fi
fi
umask 077
printf 'RM_RELAY_KEY=%s\n' "$KEY" > "$ENV_FILE"
chmod 600 "$ENV_FILE"
if [[ -n "$OLD_KEY" && "$OLD_KEY" != "$KEY" ]]; then warn "the key changed (the previous one no longer works)"; fi
if [[ -n "$KEY" ]]; then ok "key stored in $ENV_FILE"; else warn "no key: anyone may use this relay"; fi

step "4/6  systemd service"
cat > /etc/systemd/system/rm-relay.service <<UNIT
[Unit]
Description=RemoteMac relay
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile=$ENV_FILE
ExecStart=/usr/local/bin/rm-relay 0.0.0.0:$PORT
Restart=always
RestartSec=2
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable rm-relay >/dev/null 2>&1
systemctl restart rm-relay
sleep 1
systemctl is-active --quiet rm-relay && ok "rm-relay is running (starts on boot, restarts on failure)" || { journalctl -u rm-relay -n 20 --no-pager; exit 1; }

step "5/6  Local firewall"
if command -v ufw >/dev/null && ufw status | grep -q "Status: active"; then
  ufw allow 22/tcp >/dev/null
  ufw allow "$PORT"/tcp >/dev/null
  ok "ufw: allowed $PORT/tcp (and 22/tcp for SSH)"
elif command -v iptables >/dev/null && iptables -S INPUT 2>/dev/null | grep -qE -- "-j (REJECT|DROP)|-P INPUT DROP"; then
  # images that ship a closed iptables INPUT chain (e.g. Oracle Cloud)
  iptables -C INPUT -p tcp --dport "$PORT" -j ACCEPT 2>/dev/null || iptables -I INPUT -p tcp --dport "$PORT" -j ACCEPT
  if command -v netfilter-persistent >/dev/null; then netfilter-persistent save >/dev/null 2>&1 || true; fi
  ok "iptables: accepted $PORT/tcp"
else
  ok "no active local firewall"
fi

step "6/6  Checks"
ss -ltn | grep -q ":$PORT " && ok "listening on 0.0.0.0:$PORT" || warn "not listening on $PORT"
REPLY="$(printf '{"session_id":"probe","role":"agent","token":"0123456789abcdef0"}\n' | timeout 3 nc -q 1 127.0.0.1 "$PORT" 2>/dev/null || true)"
if [[ -n "$KEY" ]]; then
  [[ "$REPLY" == "ERR not admitted"* ]] && ok "strangers are refused (admission key enforced)" || warn "unexpected probe reply: '$REPLY'"
fi
PUBLIC_IP="$(curl -s --max-time 4 https://api.ipify.org || true)"
DNS_IP="$(getent ahostsv4 "$DNS_NAME" 2>/dev/null | awk 'NR==1{print $1}' || true)"
if [[ -n "$PUBLIC_IP" && -n "$DNS_IP" ]]; then
  [[ "$PUBLIC_IP" == "$DNS_IP" ]] && ok "$DNS_NAME -> $DNS_IP (this server)" || warn "$DNS_NAME points to $DNS_IP but this server is $PUBLIC_IP: fix the A record at FreeDNS"
fi

echo
bold "RemoteMac relay is installed."
echo
bold "Open these ports in your provider's firewall / security group (inbound):"
echo "    TCP $PORT   RemoteMac relay (Mac and Windows both connect here)"
echo "    TCP 22     SSH (to administer the server)"
echo "    (no UDP ports and nothing else are needed; the Mac and the PC open no ports at all)"
echo
echo "Check from Windows (PowerShell):  Test-NetConnection $DNS_NAME -Port $PORT"
echo "Logs:     journalctl -u rm-relay -f"
echo "Restart:  sudo systemctl restart rm-relay"
if [[ -n "$KEY" && "$KEY" != "$BUILTIN_KEY" ]]; then
  echo
  bold "Admission key (give it only to your own Mac / Windows apps as RM_RELAY_KEY):"
  echo "    $KEY"
fi
