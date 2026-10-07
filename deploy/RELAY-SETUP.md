# Running a MacBridge relay

The relay is the meeting point between a Mac (`macbridge`) and a Windows PC (`MacBridge.exe`)
that are **not** on the same network. Both connect *out* to it, so neither opens a port, and a
Mac behind home NAT or Wi-Fi is reachable. The relay pairs the two sides by session ID and
forwards bytes; it does not need to understand the stream.

```
  Windows (MacBridge.exe) ──TCP/UDP──►  relay.example.com:7470  ◄──TCP/UDP── Mac (macbridge)
                                        (rm-relay, systemd)
```

On the same network no relay is used at all: the viewer finds the Mac by its ID with a UDP
broadcast (port 7471) and connects straight to it.

| Secret | Held by | Purpose |
|---|---|---|
| `RM_RELAY_KEY` (admission key) | the server and every machine allowed to use it | the relay refuses anyone without it |
| ID + password | the Mac and you | pairs the right PC with the right Mac; only a matching password proof is let through |

> Sessions are end-to-end encrypted: the relay only carries ciphertext and never learns the
> password. It does see who connects to which ID and how much traffic flows. Keep the admission
> key private and never commit it; on GitHub keep it in Actions secrets only.

---

## 1. One command (recommended)

From the latest release, on an Ubuntu / Debian server (x86_64 or ARM64):

```bash
tar -xzf macbridge-relay.tar.gz && cd remotemac-relay
sudo ./remotemac-relay-setup.sh --name relay.example.com      # --port 7470 --key KEY optional
```

It installs `rm-relay` as a systemd service (a prebuilt static binary, or built from the bundled
source on other CPUs), stores the admission key in `/etc/remote-mac/relay.env`, opens the port
in ufw/iptables, checks that strangers are refused, and prints the ports to open in your
provider's firewall and the address to give the Mac and the PC. Running it again updates the
relay and keeps the key.

Then:

```bash
# Mac
RM_RELAY_KEY=<key> ./macbridge --relay relay.example.com:7470 --password PASS
```

On Windows type `relay.example.com:7470` under **Relay server** in the connect window, with
`RM_RELAY_KEY` set for the user (`setx RM_RELAY_KEY "<key>"`), or use a build with the key built
in (see *Building from source* in the README).

## 2. By hand

### DNS

Point an `A` record (e.g. `relay.example.com`) at the server's public IP. A free dynamic DNS
name works. The plain IP address works as well.

### Build

```bash
sudo apt-get update && sudo apt-get install -y build-essential curl git openssl
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal && . "$HOME/.cargo/env"
git clone https://github.com/ducminh1110/RemoteMac.git && cd RemoteMac
cargo build --release -p rm-relay
sudo install -m 0755 target/release/rm-relay /usr/local/bin/rm-relay
```

### Admission key

```bash
sudo mkdir -p /etc/remote-mac
KEY=$(openssl rand -hex 24)
echo "RM_RELAY_KEY=$KEY" | sudo tee /etc/remote-mac/relay.env >/dev/null
sudo chmod 600 /etc/remote-mac/relay.env
echo "$KEY"    # give it to your Mac and PC; keep it out of chats and repositories
```

### Service

```bash
sudo tee /etc/systemd/system/rm-relay.service >/dev/null <<'UNIT'
[Unit]
Description=MacBridge relay
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile=/etc/remote-mac/relay.env
ExecStart=/usr/local/bin/rm-relay 0.0.0.0:7470
Restart=always
RestartSec=2
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
# the Mac IDs it hands out are kept here ($STATE_DIRECTORY/ids.json)
StateDirectory=rm-relay

[Install]
WantedBy=multi-user.target
UNIT
sudo systemctl daemon-reload && sudo systemctl enable --now rm-relay
```

### Firewall

```bash
sudo ufw status | grep -q "Status: active" && sudo ufw allow 7470/tcp && sudo ufw allow 7470/udp
```

Also allow **TCP 7470 and UDP 7470 inbound** in your provider's firewall / security group.

- TCP 7470: session, control, input, menus, clipboard.
- UDP 7470: video with FEC, the smooth path. Without it, video falls back to TCP: it still
  works, but stutters more on long or lossy links.

## 3. Checks

```bash
sudo systemctl status rm-relay --no-pager | head -n 8      # active (running)
journalctl -u rm-relay -n 5 --no-pager                      # "... admission key required"
ss -ltnu | grep 7470                                        # TCP and UDP listening
printf '{"session_id":"probe","role":"agent","token":"0123456789abcdef0"}\n' | nc relay.example.com 7470
# -> ERR not admitted   (strangers are refused)
```

From Windows: `Test-NetConnection relay.example.com -Port 7470` → `TcpTestSucceeded : True`.

## 4. Troubleshooting

| Symptom | Likely cause |
|---|---|
| viewer: "relay ... not reachable" | TCP 7470 closed in the provider's firewall, or the service is down |
| "ERR not admitted" | the Mac or PC has no or a different `RM_RELAY_KEY` |
| connects, but video stutters; stats show TCP | UDP 7470 blocked somewhere |
| "This Mac was not found on this network" | the Mac is elsewhere and no relay is entered in the viewer |
| "Wrong password" | the password differs from the one the Mac shows |

Logs: `journalctl -u rm-relay -f`. Restart: `sudo systemctl restart rm-relay`.
