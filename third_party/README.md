# Vendored upstream code

Copied unmodified (except where noted) so the build needs no submodules or network.

| Directory | Upstream | Commit |
|---|---|---|
| `moonlight-common-c` | https://github.com/moonlight-stream/moonlight-common-c | f900dd4767759c7b9d0e93bcea666b55c69ea62f |
| `moonlight-common-c/enet` | https://github.com/cgutman/enet | aca87840b57f045a1f7f9299e4b1b9b8e2a5e2f1 |
| `moonlight-common-c/nanors` | https://github.com/sleepybishop/nanors | b1e3c22ca0cdc0bb83e3cd6ed1a2fc77869ed99a |

`src/PlatformCrypto.c` is not compiled: `crates/moonlight-sys` provides the same five functions
in Rust (AES-GCM / AES-CBC from RustCrypto), so no OpenSSL is needed on Windows.

Ported (not copied as files) from Sunshine a2d3713a65bda7dc0c4cdecd859b950ac7cf71c8 and
moonlight-qt 6e699e4dc9514a0bb23e79ec5bf6c1cd53cf6b83: see the module docs in
`crates/rm-gamestream` and `crates/rm-viewer/src/nv12.rs`.
