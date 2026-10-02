# Relay server setup (by hand, on the server)

Public name: `remotemac.mooo.com` (FreeDNS) -> the server; the relay listens on TCP 7470.

The relay pairs one Mac agent with one Windows viewer per session and forwards their bytes. On a
public address it only admits joins that present the admission key (`RM_RELAY_KEY`).

Run on the server (Ubuntu), as the `ubuntu` user:

```bash
# 1. tools + Rust (the repo pins its toolchain; rustup fetches it on the first build)
sudo apt-get update && sudo apt-get install -y build-essential curl git openssl
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"

# 2. build the relay
git clone -b claude/inspiring-einstein-er2d37 https://github.com/ducminh1110/RemoteMac.git
cd RemoteMac && cargo build --release -p rm-relay
sudo install -m 0755 target/release/rm-relay /usr/local/bin/rm-relay

# 3. admission key (only root can read it); the last line prints it once, for the GitHub secret
sudo mkdir -p /etc/remote-mac
KEY=$(openssl rand -hex 24)
echo "RM_RELAY_KEY=$KEY" | sudo tee /etc/remote-mac/relay.env >/dev/null
sudo chmod 600 /etc/remote-mac/relay.env
echo "RM_RELAY_KEY = $KEY"

# 4. service
sudo tee /etc/systemd/system/rm-relay.service >/dev/null <<'UNIT'
[Unit]
Description=Remote Mac relay
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

[Install]
WantedBy=multi-user.target
UNIT
sudo systemctl daemon-reload
sudo systemctl enable --now rm-relay

# 5. firewall: open TCP 7470 (if ufw is on); a cloud firewall / security group may also need it
sudo ufw status | grep -q "Status: active" && sudo ufw allow 7470/tcp

# 6. check: "admission key required", and listening on 7470
sudo systemctl status rm-relay --no-pager | head -n 8
ss -ltn | grep 7470
```

Then in GitHub: repository **Settings → Secrets and variables → Actions → New repository
secret**, name `RM_RELAY_KEY`, value: the key printed in step 3.

Updating the relay later: `cd ~/RemoteMac && git pull && cargo build --release -p rm-relay &&
sudo install -m 0755 target/release/rm-relay /usr/local/bin/rm-relay && sudo systemctl restart rm-relay`.

Security notes: the relay leg is still plaintext TCP (key and per-run tokens included); TLS is
the next step. Prefer SSH keys over password login on the server.
