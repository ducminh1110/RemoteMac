# Hướng dẫn đầy đủ: relay server `remotemac.mooo.com`

Relay là "điểm hẹn" giữa máy Mac (agent) và máy Windows (viewer). Cả hai đều **kết nối ra**
relay, không máy nào phải mở cổng vào, nên Mac sau NAT, sau wifi nhà hay trên runner CI đều nối
được. Relay chỉ ghép cặp và chuyển tiếp byte, không đọc nội dung.

```
  Windows (remote-mac-viewer) ──TCP──►  remotemac.mooo.com:7470  ◄──TCP── Mac (remote-agent-mac)
                                        (rm-relay, systemd)
```

Ba lớp khoá:

| Gì | Ai giữ | Dùng để |
|---|---|---|
| `RM_RELAY_KEY` (admission key) | server + mọi máy được phép dùng relay | relay từ chối mọi kết nối không có key (người lạ không dùng ké được) |
| session id (vd. `my-mac`) | Mac + Windows của bạn | relay ghép đúng Mac với đúng Windows |
| `RM_SESSION_TOKEN` (≥16 ký tự) | Mac + Windows của bạn | chỉ người biết token mới vào được session đó |

> Lưu ý bảo mật: đường truyền tới relay hiện là TCP **chưa mã hoá** (TLS là bước tiếp theo của
> dự án). Key và token không bao giờ được commit vào repo; trên GitHub chỉ để trong Secrets.

---

## 0. Cách nhanh nhất (bản phát hành)

Workflow **Release builds** (tab Actions) tạo sẵn 3 gói, đã cài sẵn `remotemac.mooo.com` và key:

| Gói | Chạy ở đâu | Dùng thế nào |
|---|---|---|
| `remotemac-relay.tar.gz` | server | `tar -xzf remotemac-relay.tar.gz && cd remotemac-relay && sudo ./remotemac-relay-setup.sh` — tự cài từ A–Z, rồi in các cổng cần mở (**TCP 7470**, TCP 22) |
| `remotemac-macos.tar.gz` | Mac | `tar -xzf remotemac-macos.tar.gz && cd remotemac && xattr -d com.apple.quarantine remotemac; ./remotemac --password MatKhau` → in ra `ID session to connect` + `Password` |
| `RemoteMac-windows.zip` | Windows | mở `RemoteMac.exe`, gõ ID + mật khẩu, bấm **Connect** |

Chạy lại script trên server là cập nhật relay (key giữ nguyên). Các mục dưới đây là cách làm tay
từng bước (để hiểu hoặc khi gỡ lỗi).

---

## 1. DNS (FreeDNS)

Bản ghi `A`: `remotemac.mooo.com` → IP server (hiện `140.211.166.242`).

Kiểm tra từ bất kỳ máy nào:

```bash
nslookup remotemac.mooo.com      # phải ra 140.211.166.242
```

Nếu IP server thay đổi: vào FreeDNS → *Dynamic DNS* sửa lại bản ghi (hoặc dùng URL cập nhật
động FreeDNS cấp, chạy bằng `curl` trong cron trên server).

---

## 2. Chuẩn bị server (Ubuntu, user `ubuntu`)

```bash
ssh ubuntu@remotemac.mooo.com

# đổi mật khẩu tạm và (khuyên dùng) chuyển sang đăng nhập bằng SSH key
passwd
# trên máy bạn:  ssh-copy-id ubuntu@remotemac.mooo.com
# rồi trên server tắt đăng nhập bằng mật khẩu:
#   sudo sed -i 's/^#\?PasswordAuthentication .*/PasswordAuthentication no/' /etc/ssh/sshd_config && sudo systemctl restart ssh
```

## 3. Cài Rust và build relay

```bash
sudo apt-get update && sudo apt-get install -y build-essential curl git openssl
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"

git clone -b claude/inspiring-einstein-er2d37 https://github.com/ducminh1110/RemoteMac.git
cd RemoteMac && cargo build --release -p rm-relay
sudo install -m 0755 target/release/rm-relay /usr/local/bin/rm-relay
```

(Build lần đầu tải toolchain Rust repo ghim sẵn, mất vài phút. Chạy được trên x86_64 lẫn ARM.)

## 4. Admission key

```bash
sudo mkdir -p /etc/remote-mac
KEY=$(openssl rand -hex 24)
echo "RM_RELAY_KEY=$KEY" | sudo tee /etc/remote-mac/relay.env >/dev/null
sudo chmod 600 /etc/remote-mac/relay.env
echo "RM_RELAY_KEY = $KEY"     # chép lại key này (dùng ở bước 7 và 8), không gửi lên chat/repo
```

## 5. Chạy relay như một service (tự chạy lại khi lỗi / khi server khởi động lại)

```bash
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
```

## 6. Mở cổng 7470/TCP

```bash
sudo ufw status | grep -q "Status: active" && sudo ufw allow 7470/tcp
```

Nếu nhà cung cấp server có **firewall riêng** (Security Group / Cloud Firewall trên trang quản
lý): thêm luật cho phép **TCP 7470 inbound**. Chỉ cần cổng này (và 22 cho SSH).

### Kiểm tra

Trên server:

```bash
sudo systemctl status rm-relay --no-pager | head -n 8   # active (running)
journalctl -u rm-relay -n 5 --no-pager                   # "... admission key required"
ss -ltn | grep 7470                                      # LISTEN 0.0.0.0:7470
```

Từ máy Windows (PowerShell):

```powershell
Test-NetConnection remotemac.mooo.com -Port 7470          # TcpTestSucceeded : True
```

Từ một máy Linux/Mac bất kỳ (relay phải **từ chối** khi không có key):

```bash
printf '{"session_id":"probe","role":"agent","token":"0123456789abcdef0"}\n' | nc remotemac.mooo.com 7470
# -> ERR not admitted
```

---

## 7. Nối máy Mac (agent)

Trên Mac (macOS 14 trở lên; cần Xcode Command Line Tools: `xcode-select --install`):

```bash
git clone -b claude/inspiring-einstein-er2d37 https://github.com/ducminh1110/RemoteMac.git
cd RemoteMac && ./scripts/build-agent-macos.sh          # -> out/remote-agent-mac

export RM_RELAY_KEY="<key ở bước 4>"
export RM_SESSION_TOKEN="$(openssl rand -hex 16)"       # token của bạn; chép lại cho máy Windows
echo "token: $RM_SESSION_TOKEN"
./out/remote-agent-mac --relay remotemac.mooo.com:7470 --session my-mac
```

Lần đầu macOS sẽ hỏi quyền cho app chạy agent (Terminal / iTerm):
**System Settings → Privacy & Security → Screen Recording** và **Accessibility** → bật, rồi chạy
lại lệnh trên. Agent in `relay joined, session=my-mac` là đang chờ máy Windows (relay giữ chỗ
5 phút; agent tự thoát nếu không ai vào, chạy lại là được).

## 8. Nối máy Windows (viewer)

Tải `remote-mac-viewer.exe` từ artifact **remote-mac-viewer-windows-x64** của workflow
*Windows viewer* (tab Actions của repo), rồi trong PowerShell:

```powershell
$env:RM_RELAY_KEY = "<key ở bước 4>"
$env:RM_SESSION_TOKEN = "<token in ra ở bước 7>"
.\remote-mac-viewer.exe --relay remotemac.mooo.com:7470 --session my-mac
```

Launcher hiện ra với các app của Mac (**Mac Desktop** đứng đầu: điều khiển cả máy ở fullscreen).
Double-click để mở; các app cũng có trong Start menu / Windows Search khi đang kết nối.

Mẹo: đặt sẵn biến môi trường cho user (`setx RM_RELAY_KEY "..."`, `setx RM_SESSION_TOKEN "..."`)
rồi tạo shortcut tới
`remote-mac-viewer.exe --relay remotemac.mooo.com:7470 --session my-mac`.

---

## 9. Chạy thử Mac ↔ Windows trên GitHub Actions (CI)

1. Repo → **Settings → Secrets and variables → Actions → New repository secret**:
   tên `RM_RELAY_KEY`, giá trị: key ở bước 4.
2. Kích hoạt workflow *Live Mac <-> Windows*: tạo hoặc sửa file `live/trigger` rồi push lên
   nhánh (hoặc nhờ Claude làm). Hai runner cùng nối ra relay, viewer mở Xcode/TextEdit/testapp,
   gõ thử chữ, chụp màn hình rồi cả hai job tự kết thúc. Token mỗi lần chạy được sinh riêng từ
   `RM_RELAY_KEY` + số hiệu lần chạy.

---

## 10. Cập nhật relay

```bash
cd ~/RemoteMac && git pull && . "$HOME/.cargo/env" && cargo build --release -p rm-relay \
  && sudo install -m 0755 target/release/rm-relay /usr/local/bin/rm-relay && sudo systemctl restart rm-relay
```

Đổi key: sửa `/etc/remote-mac/relay.env`, `sudo systemctl restart rm-relay`, cập nhật secret
GitHub và biến môi trường trên các máy.

## 11. Gỡ lỗi nhanh

| Triệu chứng | Nguyên nhân thường gặp |
|---|---|
| `connect failed` / `TcpTestSucceeded: False` | firewall (ufw hoặc firewall của nhà cung cấp) chưa mở 7470; service chưa chạy |
| `ERR not admitted` | `RM_RELAY_KEY` trên máy không khớp với server |
| `ERR session mismatch` | token hai bên khác nhau, hoặc hai máy cùng vai trò (2 agent) vào cùng session |
| `ERR pair timeout` | bên kia không vào trong 5 phút; chạy lại |
| Viewer báo *This Mac is not online* | trên Mac chưa chạy `./remotemac`, hoặc gõ sai ID |
| Viewer báo *Wrong password* | sai mật khẩu; sai 5 lần thì session bị khoá 1 phút (*Too many wrong passwords*) |
| `ERR bad session or token` | session chỉ được chữ/số/`-` (≤64 ký tự); token 16–128 ký tự |
| Mac vào được nhưng không có hình | chưa cấp quyền Screen Recording / Accessibility cho Terminal |
| Xem log relay | `journalctl -u rm-relay -f` |
