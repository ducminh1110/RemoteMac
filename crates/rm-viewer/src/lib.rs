//! Windows viewer for remote Mac applications. `keymap` and `net` are portable and unit-tested
//! everywhere; `ui` (Win32 windows + GDI presentation) only exists on Windows.
pub mod chrome;
pub mod ico;
pub mod keymap;
pub mod menu;
pub mod gsdesktop;
pub mod settings;
pub mod net;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod comp;
#[cfg(windows)]
pub mod connect;
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
pub mod shortcuts;
#[cfg(windows)]
pub mod settings_ui;
#[cfg(windows)]
pub mod splash;
#[cfg(windows)]
pub mod ui;

/// The viewer's log: %APPDATA%\\RemoteMac\\viewer.log (release builds have no console).
pub fn log_path() -> std::path::PathBuf {
    std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("RemoteMac").join("viewer.log")
}
