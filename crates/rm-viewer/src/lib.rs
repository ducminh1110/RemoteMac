//! Windows viewer for remote Mac applications. `keymap` and `net` are portable and unit-tested
//! everywhere; `ui` (Win32 windows + GDI presentation) only exists on Windows.
pub mod chrome;
pub mod ico;
pub mod keymap;
pub mod menu;
pub mod net;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod comp;
#[cfg(windows)]
pub mod connect;
#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
pub mod native;
#[cfg(windows)]
pub mod shortcuts;
#[cfg(windows)]
pub mod ui;
