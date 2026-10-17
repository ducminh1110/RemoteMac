//! Taro Enterprise package builder (Wizard 1, `TaroEnterpriseSetup.exe`).
//!
//! A Windows app (its pages served on 127.0.0.1 and shown in its own WebView2 window) that builds a provider's own branded Taro: it takes
//! the provider's identity, primary colour, logo, icon and relay endpoint, writes them as one
//! `provider.json` (the contract in `rm-brand`), retrieves the exact, version-pinned official
//! Taro artifacts listed in a signed Enterprise release (`taro-update`'s component manifest),
//! verifies them, and assembles the provider's packages (Windows zip, macOS tar.gz, Linux relay
//! tar.gz) with that provider folder added. Nothing is compiled and no binary is modified.
//!
//! - [`preflight`]: system checks
//! - [`releases`]: discovering Enterprise releases and verifying their component manifest
//! - [`download`]: the download policy (https, allowed hosts) and verified, streamed downloads
//! - [`package`]: assembling the archives (pure Rust: zip, tar, flate2)
//! - [`build`]: the build task (download, assemble, atomically publish the output folder)
//! - [`check`]: re-opening every produced file to validate it
//! - [`server`]: the loopback-only HTTP server and its JSON API
//! - `window` (Windows): the app window that shows the pages

pub mod branding;
pub mod build;
pub mod check;
pub mod config;
pub mod download;
pub mod package;
pub mod platform;
pub mod preflight;
pub mod redact;
pub mod releases;
pub mod secret;
pub mod server;
pub mod state;
pub mod summary;
pub mod util;
#[cfg(windows)]
pub mod window;
