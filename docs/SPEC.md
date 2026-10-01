# Remote Mac App — Spec (phương án 2: GitHub Actions + terminal-launched agent)

Stream **từng ứng dụng macOS** (Xcode là mục tiêu cuối) sang cửa sổ riêng trên Windows.
Không phải Remote Desktop, không stream cả desktop. Mac chạy trên runner macOS của GitHub Actions (ephemeral).

## 1. Quyết định kiến trúc

| # | Quyết định | Lý do |
|---|-----------|-------|
| A1 | `remote-agent` là **executable chạy từ shell/Terminal**, không phải `.app` | Trên runner, chỉ tiến trình khởi chạy từ shell/bash/zsh có (hoặc có thể có) quyền Screen Recording/Accessibility. Agent không được phụ thuộc Finder/`open`. |
| A2 | Ứng dụng đích cũng được chạy bằng **đường dẫn executable** (`.../Contents/MacOS/TextEdit`), argv, không qua shell | Giữ chuỗi quyền: shell → agent → app. Không có `execute(shell_string)`. |
| A3 | Windows client **không bao giờ** nhận GitHub token của runner; runner **không bao giờ** nhận GitHub token của user | Cô lập thông tin xác thực. Chỉ có `session_id` + token phiên ngắn hạn. |
| A4 | Agent kết nối **outbound** tới relay; client cũng outbound tới relay | Runner không có cổng inbound. Relay chỉ ghép cặp + chuyển byte. |
| A5 | Remote Mac = **phiên tạm** (tối đa vài giờ), UI phải hiển thị thời gian còn lại | Runner bị huỷ khi job kết thúc. |
| A6 | Có `ComputeProvider` trait; GitHub chỉ là một provider | Sau này thêm Mac cá nhân/cloud mà không đổi client. |
| A7 | Production chỉ chạy app trong allowlist phía agent; arg/cwd/env được validate | Runner là máy thật, dù tạm thời. |

Luồng: `Windows client → relay ← remote-agent (trong runner) → TextEdit/Xcode`.
Client dispatch workflow bằng GitHub API chính thức (`workflow_dispatch`), theo dõi run, rồi kết nối relay.

## 2. CỔNG KHẢ THI KỸ THUẬT (Feasibility Gate) — làm TRƯỚC mọi thứ khác

Toàn bộ dự án giả định runner cho phép: có phiên GUI, chụp **một cửa sổ**, bơm input, encode video.
Những điều này **chưa được xác minh**. Không viết compositor GPU/Direct3D hay streaming trước khi cổng này pass.

Chạy: Actions → **Feasibility Gate (macOS runner)** → *Run workflow* (thử `macos-15`, `macos-15-intel`, `macos-26`).
Mã: `probe/macos/main.swift`, `scripts/run-feasibility-probe.sh`. Kết quả JSON nằm trong artifact `feasibility-<runner>`.

| Gate | Câu hỏi | Pass khi | Nếu FAIL |
|------|---------|----------|----------|
| G1 | Tiến trình từ shell có truy cập phiên GUI/WindowServer? | `CGSessionCopyCurrentDictionary` ≠ nil và display > 0 | **Dừng.** Không có cách stream cửa sổ. |
| G2a/b | Screen Recording / Accessibility đã được cấp? (thông tin) | preflight/AXIsProcessTrusted | Ghi nhận; quyết định G4/G5 mới là thật. Không sửa TCC.db (vi phạm bảo mật macOS). |
| G3 | Chạy GUI app bằng executable path + liệt kê được cửa sổ **đang hiển thị** của nó? | có cửa sổ layer 0, `onscreen`, ≥ 64×64 (app còn sở hữu các cửa sổ nền ẩn 500×500 và thanh 1024×24 — phải bỏ qua) | Thử `open -a` như phương án phụ; nếu vẫn fail → dừng. |
| G4 | Chụp **riêng một cửa sổ** (ScreenCaptureKit), ảnh không đen/trống? | ≥ 8 màu khác nhau | Fallback: chụp cả màn hình + crop (mức 2 trong spec gốc) — làm giảm giá trị sản phẩm, cần quyết định lại. |
| G5 | Bơm phím vào app và **thấy hiệu ứng**? | số pixel đổi > max(50, 3×nhiễu không-input) **hoặc** giá trị ô nhập đọc qua Accessibility đổi | Không điều khiển được → chỉ còn chế độ xem; dừng. |
| G6 | Có encoder H.264 (phần cứng hoặc phần mềm)? | VTCompressionSession tạo được | Dùng codec phần mềm trong Rust (openh264). |
| G7 | Runner gọi ra ngoài HTTPS được? (UDP/QUIC kiểm tra riêng ở M2) | HTTP 2xx | Cần relay qua cổng 443/WebSocket. |

Script in `GATE VERDICT: GO` hoặc `NO-GO (…)`. `inconclusive` được tính là chưa pass.
Các gate bắt buộc (G1, G3–G7) chạy trên app Cocoa nhỏ trong repo (`probe/macos/testapp.swift`, có ô nhập là first responder,
khởi chạy bằng đường dẫn executable). TextEdit chạy song song làm gate thông tin `T*` (ứng dụng thật, không bắt buộc).

### Kết quả đo (run [36830721590](https://github.com/ducminh1110/RemoteMac/actions/runs/36830721590), commit `3f360bc`) — **GO trên cả 3 runner**

| | `macos-15` | `macos-15-intel` | `macos-26` |
|---|---|---|---|
| macOS / kiến trúc | 15.7.9 / arm64 | 15.7.9 / x86_64 | 26.6.2 / arm64 |
| Màn hình ảo | 1024×768 | 1920×1080 | 1024×768 |
| G1 phiên GUI từ shell | pass (`onConsole=1`) | pass | pass |
| G2 Screen Recording / Accessibility | đã cấp sẵn (`true`/`true`) | đã cấp sẵn | đã cấp sẵn |
| G4 chụp 1 cửa sổ (app test) | 480×348, 80 màu | 480×348, 81 màu | 480×352, 80 màu |
| G5 gõ phím vào app test | pass | pass (cả 2 đường) | pass |
| T* TextEdit: cửa sổ / chụp | 601×491, 197 màu | 601×491, 143 màu | 603×505 |
| T5a `postToPid` (pixel đổi / nhiễu) | 1116 / 28; AX = "Hik hi" | 1095 / 0; AX = "Hik hi" | pass (AX đổi) |
| T5b activate + HID tap (pixel đổi) | 304; AX = "Hik hihik hi" | 289 | pass |
| G6 H.264 | **phần cứng** | **chỉ phần mềm** (`-12908`) | **phần cứng** |
| G7 HTTPS ra ngoài | 200 | 200 | 200 |

Kết luận đã có bằng chứng:
- Tiến trình khởi chạy từ shell **có** phiên GUI và **đã được cấp sẵn** Screen Recording + Accessibility; không cần sửa TCC.
- Chạy app GUI bằng đường dẫn executable, chụp riêng một cửa sổ qua ScreenCaptureKit, gõ phím vào đó đều hoạt động.
- Gửi phím bằng `CGEvent.postToPid` hoạt động **không cần activate** app (quan trọng: điều khiển nền không cần giành focus). Đường HID tap sau `activate` cũng hoạt động.
- Runner Intel không có encoder phần cứng → phải có đường codec phần mềm; ưu tiên runner arm64.

**Chưa được chứng minh bởi gate này (đừng suy diễn từ "GO"):**
- Mới chụp **ảnh tĩnh** (`SCScreenshotManager`), chưa chạy luồng liên tục `SCStream`: FPS, độ trễ, mức dùng CPU chưa đo.
- Chưa test chuột, cuộn, resize, đóng cửa sổ, nhiều cửa sổ/dialog con, menu popup.
- Chưa test encode khung đã chụp → decode (mới chỉ tạo được `VTCompressionSession`).
- Chưa test kết nối ra relay thật (chỉ HTTPS 200 tới github.com; UDP/QUIC chưa kiểm tra), thời gian sống job, hay Xcode/Simulator.
- Cổng chạy bằng step `run:` trong workflow; agent thật sẽ chạy trong cùng chuỗi tiến trình đó nên nhiều khả năng kế thừa quyền, nhưng cần xác nhận ở M1.

Bài học khi làm gate (đã sửa trong probe, giữ lại để không lặp lại):
1. Lượt đầu `macos-15` báo GO **oan**: chọn nhầm cửa sổ 106×108 và so hash ảnh nên con trỏ nhấp nháy cũng làm "pass". Nay đo thêm nhiễu không-input và đọc giá trị ô nhập bằng Accessibility.
2. Lỗi "TextEdit không nhận phím" lúc đầu **không phải** do runner: do tôi truyền file tạm qua argv nên TextEdit hiện hộp thoại "document could not be opened". Bỏ tham số thì TextEdit mở tài liệu bình thường.
3. `api.github.com/zen` trả 403 do rate limit; đổi sang kiểm tra "có phản hồi HTTP".

Ghi chú vận hành: `workflow_dispatch` trả 404 cho tới khi file workflow nằm trên nhánh mặc định; trong lúc phát triển workflow này
chạy bằng `push` lên nhánh làm việc (xem `.github/workflows/feasibility-gate.yml`).

**Gate bổ sung (không đo bằng code): Điều khoản GitHub.** Điều khoản sử dụng Actions hạn chế dùng runner cho mục đích
ngoài việc build/test/deploy dự án của repo (ví dụ dùng như máy tính từ xa đa dụng). Cần đọc lại điều khoản hiện hành và
tự đánh giá rủi ro tài khoản bị khoá trước khi dùng nghiêm túc; đây là rủi ro sản phẩm, không phải rủi ro kỹ thuật.
Vì vậy provider abstraction (A6) và backend Mac cá nhân/cloud là đường lui, không phải tính năng "sau này".

Thứ tự phụ thuộc: **Gate (ĐÃ GO) → M1 (capture liên tục + input đầy đủ) → M2 (video + client Windows native) → Xcode.**

## 3. Trạng thái triển khai (trung thực)

| Thành phần | Trạng thái |
|-----------|-----------|
| `rm-protocol`: khung gói tin, kênh ưu tiên, negotiate phiên bản, giới hạn kích thước, từ chối gói hỏng | Xong, có test (11) |
| `rm-core`: state machine provisioning (không thể tới `Ready` nếu chưa handshake), `ComputeProvider`, allowlist app | Xong, có test (9) |
| `rm-relay`: ghép cặp phiên, so sánh token constant-time, timeout, chặn id/hello bậy | Xong, có test (6). **Truyền tải là TCP thuần — chỉ để phát triển** |
| `rm-agent` (`remote-agent`): handshake, báo capability, list/launch/terminate qua allowlist | Xong, có test (6). Chưa chạy trên Mac thật |
| `rm-client` (`remote-mac`): handshake → `Ready`, list/launch; test end-to-end client↔relay↔agent | Xong, có test (1). Bản CLI, **chưa có cửa sổ** |
| Probe macOS (Swift) + workflow Gate | **Đã chạy trên 3 runner thật: GO** (số liệu ở §2). Sau 4 vòng sửa lỗi của chính probe |
| Capture, input injection, encode/decode, compositor Windows | **Chưa làm** — chờ Gate |
| Mã hoá đầu-cuối (Noise/QUIC) + TLS cho relay | **Chưa làm — bắt buộc trước khi dùng thật** |
| GitHub provider (device-flow OAuth, `workflow_dispatch`, theo dõi run, huỷ run) | **Chưa làm** — M3 (Gate hiện chạy bằng push lên nhánh) |
| Clipboard, file transfer, audio, DPI, đa màn hình, Simulator, Xcode | Sau M2 |

## 4. Lộ trình sau Gate

1. **M1 (Mac):** (a) `SCStream` liên tục cho 1 cửa sổ, đo FPS/độ trễ/CPU trên runner; (b) bơm chuột/cuộn/phím theo `Message::{Mouse*,Key,TextInput}` vào TextEdit; (c) resize/đóng/nhiều cửa sổ; (d) VideoToolbox encode khung thật; (e) agent kết nối ra relay thật từ runner (HTTPS/WebSocket 443, thử UDP). Mỗi mục là một gate mới trong cùng workflow.
2. **M2 (video + Windows):** H.264 qua VideoToolbox → khung video trên kênh `Video`; client Windows Rust + Win32 + Direct3D11 + Media Foundation tạo cửa sổ native cho mỗi `WindowCreated`. Cần máy Windows/CI `windows-latest` để build và test.
3. **M3:** E2E mã hoá, GitHub provider, UI chọn repo/Request Mac/Stop Mac, hiển thị thời gian còn lại.
4. **M4:** clipboard, file grant, reconnect, DPI, đa màn hình → Xcode + Simulator.

## 5. Bảo mật (đã áp dụng / còn thiếu)

- Áp dụng: allowlist app; argv không qua shell; cwd chỉ trong thư mục phiên; env chỉ theo allowlist; token không đi qua argv (dùng `RM_SESSION_TOKEN`) và không được log; giới hạn kích thước khung; relay giới hạn số phiên chờ + timeout.
- Còn thiếu: mã hoá truyền tải, xoay khoá, thu hồi thiết bị, fuzzing parser, chế độ dev có đánh dấu rõ. Workflow input `session_id`/token sẽ hiển thị trong UI Actions → cần `::add-mask::` và token ngắn hạn khi làm provider.

## 6. Chạy thử trên máy dev

```
cargo test --workspace
cargo run -p rm-relay                       # relay trên 127.0.0.1:47900
RM_SESSION_TOKEN=0123456789abcdef-demo cargo run -p rm-agent -- --relay 127.0.0.1:47900 --session demo-1 --capabilities out/probe.json
RM_SESSION_TOKEN=0123456789abcdef-demo cargo run -p rm-client -- --relay 127.0.0.1:47900 --session demo-1
```
