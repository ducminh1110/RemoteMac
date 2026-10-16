//! Windows viewer for remote Mac applications. `keymap` and `net` are portable and unit-tested
//! everywhere; `ui` (Win32 windows + GDI presentation) only exists on Windows.
pub mod audio;
#[cfg(windows)]
pub mod banner;
pub mod cards;
pub mod chrome;
pub mod connectui;
pub mod crash;
pub mod demo;
pub mod dock;
pub mod field;
pub mod ico;
pub mod keymap;
pub mod launchview;
pub mod launchui;
pub mod lifecycle;
pub mod loadview;
pub mod logo;
pub mod look;
pub mod motion;
pub mod paint;
pub mod palette;
pub mod menu;
pub mod glass;
pub mod glassmenu;
pub mod gsdesktop;
pub mod scenes;
pub mod settings;
pub mod settingsui;
pub mod text;
#[cfg(windows)]
pub mod tour;
pub mod titlebar;
pub mod update;
pub mod net;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod comp;
#[cfg(windows)]
pub mod connect;
#[cfg(windows)]
pub mod frame;
#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod mfdec;
#[cfg(windows)]
pub mod nv12;
#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
pub mod native;
#[cfg(windows)]
pub mod minimize;
#[cfg(windows)]
pub mod navball;
#[cfg(windows)]
pub mod shortcuts;
#[cfg(windows)]
pub mod stage;
#[cfg(windows)]
pub mod tiles;
#[cfg(windows)]
pub mod settings_ui;
#[cfg(windows)]
pub mod splash;
#[cfg(windows)]
pub mod surface;
#[cfg(windows)]
#[cfg(windows)]
pub mod ui;

/// Whether the viewer keeps a log: only when started with `--logs-enabled` (or RM_LOGS=1).
pub fn logs_enabled() -> bool {
    std::env::args().any(|a| a == "--logs-enabled") || std::env::var("RM_LOGS").is_ok_and(|v| v == "1")
}

/// Where the log is said to be in a message: the file, or how to turn it on.
pub fn log_hint() -> String {
    if logs_enabled() {
        format!("Log: {}", log_path().display())
    } else {
        format!("Crash records: {}. For a full log, start Taro.exe with --logs-enabled.", crash::log_path().display())
    }
}

/// The viewer's log: %APPDATA%\\RemoteMac\\viewer.log (release builds have no console).
pub fn log_path() -> std::path::PathBuf {
    std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("RemoteMac").join("viewer.log")
}
