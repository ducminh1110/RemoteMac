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
| G3 | Chạy GUI app bằng executable path + liệt kê được cửa sổ của nó? | có cửa sổ layer 0 của pid | Thử `open -a` như phương án phụ; nếu vẫn fail → dừng. |
| G4 | Chụp **riêng một cửa sổ** (ScreenCaptureKit), ảnh không đen/trống? | ≥ 8 màu khác nhau | Fallback: chụp cả màn hình + crop (mức 2 trong spec gốc) — làm giảm giá trị sản phẩm, cần quyết định lại. |
| G5 | Bơm phím vào app và **thấy hiệu ứng** trong ảnh chụp? | hash ảnh đổi | Không điều khiển được → chỉ còn chế độ xem; dừng. |
| G6 | Có encoder H.264 (phần cứng hoặc phần mềm)? | VTCompressionSession tạo được | Dùng codec phần mềm trong Rust (openh264). |
| G7 | Runner gọi ra ngoài HTTPS được? (UDP/QUIC kiểm tra riêng ở M2) | HTTP 2xx | Cần relay qua cổng 443/WebSocket. |

Script in `GATE VERDICT: GO` hoặc `NO-GO (…)`. `inconclusive` được tính là chưa pass.

**Gate bổ sung (không đo bằng code): Điều khoản GitHub.** Điều khoản sử dụng Actions hạn chế dùng runner cho mục đích
ngoài việc build/test/deploy dự án của repo (ví dụ dùng như máy tính từ xa đa dụng). Cần đọc lại điều khoản hiện hành và
tự đánh giá rủi ro tài khoản bị khoá trước khi dùng nghiêm túc; đây là rủi ro sản phẩm, không phải rủi ro kỹ thuật.
Vì vậy provider abstraction (A6) và backend Mac cá nhân/cloud là đường lui, không phải tính năng "sau này".

Thứ tự phụ thuộc: **Gate → M1 (capture+input 1 cửa sổ) → M2 (video + client Windows native) → Xcode.**

## 3. Trạng thái triển khai (trung thực)

| Thành phần | Trạng thái |
|-----------|-----------|
| `rm-protocol`: khung gói tin, kênh ưu tiên, negotiate phiên bản, giới hạn kích thước, từ chối gói hỏng | Xong, có test (11) |
| `rm-core`: state machine provisioning (không thể tới `Ready` nếu chưa handshake), `ComputeProvider`, allowlist app | Xong, có test (9) |
| `rm-relay`: ghép cặp phiên, so sánh token constant-time, timeout, chặn id/hello bậy | Xong, có test (6). **Truyền tải là TCP thuần — chỉ để phát triển** |
| `rm-agent` (`remote-agent`): handshake, báo capability, list/launch/terminate qua allowlist | Xong, có test (6). Chưa chạy trên Mac thật |
| `rm-client` (`remote-mac`): handshake → `Ready`, list/launch; test end-to-end client↔relay↔agent | Xong, có test (1). Bản CLI, **chưa có cửa sổ** |
| Probe macOS (Swift) + workflow Gate | Viết xong, **chưa biên dịch/chạy trên macOS**. Lần chạy đầu trên runner chính là bài test của nó |
| Capture, input injection, encode/decode, compositor Windows | **Chưa làm** — chờ Gate |
| Mã hoá đầu-cuối (Noise/QUIC) + TLS cho relay | **Chưa làm — bắt buộc trước khi dùng thật** |
| GitHub provider (device-flow OAuth, `workflow_dispatch`, theo dõi run, huỷ run) | **Chưa làm** — M1; Gate có thể chạy tay qua UI |
| Clipboard, file transfer, audio, DPI, đa màn hình, Simulator, Xcode | Sau M2 |

## 4. Lộ trình sau Gate

1. **M1 (Mac):** agent chụp cửa sổ TextEdit (ScreenCaptureKit) + bơm input theo `Message::{Mouse*,Key,TextInput}`; test trên runner bằng workflow.
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
