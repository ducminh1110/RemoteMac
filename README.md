# RemoteMac

Stream từng ứng dụng macOS (mục tiêu: Xcode) sang cửa sổ riêng trên Windows, dùng runner macOS của GitHub Actions làm máy Mac tạm.

**Đọc [docs/SPEC.md](docs/SPEC.md).** Việc đầu tiên: chạy workflow *Feasibility Gate (macOS runner)* để biết runner có cho phép chụp cửa sổ + bơm input hay không. Kết quả quyết định có tiếp tục hay không.

`cargo test --workspace` chạy được trên mọi OS (protocol, relay, agent, client điều khiển). Phần capture/encode/render chưa có — chờ Gate.
