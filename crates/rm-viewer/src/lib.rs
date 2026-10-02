//! Windows viewer for remote Mac applications. `keymap` and `net` are portable and unit-tested
//! everywhere; `ui` (Win32 windows + GDI presentation) only exists on Windows.
pub mod keymap;
pub mod net;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod native;
#[cfg(windows)]
pub mod ui;
