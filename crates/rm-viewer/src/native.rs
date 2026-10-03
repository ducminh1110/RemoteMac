//! Thin, safe-ish wrappers over the Windows shell/clipboard/icon APIs the viewer needs to make a
//! remote window behave like a local application window.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

const CF_UNICODETEXT: u32 = 13;

/// Per-window AppUserModelID: the taskbar groups windows by it, so every remote application
/// ("Xcode", "Safari", ...) gets its own taskbar button instead of piling under the viewer.
pub fn set_app_user_model_id(hwnd: HWND, id: &str) -> bool {
    unsafe {
        let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd) else { return false };
        let s = HSTRING::from(id);
        // VT_LPWSTR borrowing our HSTRING buffer for the duration of SetValue (the store copies it).
        let pv = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_LPWSTR,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 { pwszVal: PWSTR(s.as_ptr() as *mut u16) },
                }),
            },
        };
        let ok = store.SetValue(&PKEY_AppUserModel_ID, &pv).is_ok() && store.Commit().is_ok();
        std::mem::forget(pv); // do not PropVariantClear memory we do not own
        ok
    }
}

pub fn get_app_user_model_id(hwnd: HWND) -> Option<String> {
    unsafe {
        let store = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd).ok()?;
        let mut pv = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let inner = &pv.Anonymous.Anonymous;
        let out = (inner.vt == VT_LPWSTR && !inner.Anonymous.pwszVal.is_null()).then(|| inner.Anonymous.pwszVal.to_string().unwrap_or_default());
        let _ = PropVariantClear(&mut pv);
        out
    }
}

/// Build an HICON from square straight-alpha RGBA pixels.
pub fn make_icon(size: u32, rgba: &[u8]) -> Option<HICON> {
    if rgba.len() != (size * size * 4) as usize {
        return None;
    }
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]]; // RGBA -> BGRA, alpha kept straight (what icons expect)
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

pub fn set_window_icon(hwnd: HWND, icon: HICON) {
    unsafe {
        for which in [ICON_BIG, ICON_SMALL] {
            SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(icon.0 as isize)));
        }
    }
}

pub fn window_has_icon(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, WM_GETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(0))).0 != 0 }
}

/// Windows clipboard as UTF-16 text.
pub fn clipboard_text(owner: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let out = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        out
    }
}

pub fn set_clipboard_text(owner: HWND, text: &str) -> bool {
    unsafe {
        let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let ok = (|| {
            EmptyClipboard().ok()?;
            let g = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * 2).ok()?;
            let p = GlobalLock(g) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), p, utf16.len());
            let _ = GlobalUnlock(g);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0))).ok()?; // the system owns `g` now
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

pub fn listen_clipboard(hwnd: HWND) -> bool {
    unsafe { AddClipboardFormatListener(hwnd).is_ok() }
}

/// DIPs per Mac point: 1.0 at 100 %, 1.5 at 150 %. One Mac point is shown as one Windows DIP.
pub fn dpi_scale(hwnd: HWND) -> f64 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
}

/// The standard Windows "Open" dialog (File Explorer picker), owned by `owner`.
/// Returns the chosen file's path, or `None` if the user cancelled.
pub fn pick_open_file(owner: HWND, title: &str) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        let _ = dlg.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        let _ = dlg.SetTitle(&HSTRING::from(title));
        dlg.Show(Some(owner)).ok()?; // Err on cancel
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = p.to_string().ok().map(std::path::PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const c_void));
        path
    }
}

/// Rounded window corners on Windows 11 (ignored elsewhere), so the Mac chrome looks the part.
pub fn round_corners(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
    let pref = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&pref) as u32);
    }
}

/// Square corners (fullscreen covers the monitor edge to edge).
pub fn round_corners_off(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND};
    let pref = DWMWCP_DONOTROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&pref) as u32);
    }
}

/// Windows 11: no 1px DWM border (the composition window draws its own rounded edge).
pub fn no_border(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
    let none: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &none as *const _ as *const std::ffi::c_void, 4);
    }
}

static FONTS_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bundled Inter (UI) and JetBrains Mono (technical details), private to this process.
pub fn load_fonts() {
    const FONTS: [&[u8]; 5] = [
        include_bytes!("../fonts/Inter-Regular.ttf"),
        include_bytes!("../fonts/Inter-Medium.ttf"),
        include_bytes!("../fonts/Inter-SemiBold.ttf"),
        include_bytes!("../fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../fonts/JetBrainsMono-Medium.ttf"),
    ];
    let mut all = true;
    for f in FONTS {
        let mut n = 0u32;
        let h = unsafe { AddFontMemResourceEx(f.as_ptr() as *const std::ffi::c_void, f.len() as u32, None, std::ptr::addr_of_mut!(n)) };
        all &= !h.is_invalid() && n > 0;
    }
    FONTS_LOADED.store(all, std::sync::atomic::Ordering::SeqCst);
    if !all {
        eprintln!("warning: bundled fonts not loaded; using Segoe UI");
    }
}

/// GDI face name of Inter at a weight (each static weight is its own family for GDI).
pub fn ui_face(weight: i32) -> &'static str {
    if !FONTS_LOADED.load(std::sync::atomic::Ordering::SeqCst) {
        return "Segoe UI";
    }
    match weight {
        w if w >= 600 => "Inter SemiBold",
        w if w >= 500 => "Inter Medium",
        _ => "Inter",
    }
}

pub fn mono_face() -> &'static str {
    if FONTS_LOADED.load(std::sync::atomic::Ordering::SeqCst) { "JetBrains Mono" } else { "Consolas" }
}

/// Ways around DWM's frame line in a composition window's transparent corners (smoke tries them).
pub fn corner_remedy(hwnd: HWND, remedy: &str, radius: i32) {
    use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMNCRP_DISABLED, DWMWA_NCRENDERING_POLICY};
    use windows::Win32::UI::Controls::MARGINS;
    unsafe {
        match remedy {
            "extend-frame" => {
                let m = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
                let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
            }
            "nc-rendering-off" => {
                let p = DWMNCRP_DISABLED;
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_NCRENDERING_POLICY, &p as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&p) as u32);
            }
            "window-region" => {
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                let mut o = POINT::default();
                let _ = ClientToScreen(hwnd, &mut o);
                let mut wr = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut wr);
                let (dx, dy) = (o.x - wr.left, o.y - wr.top);
                let rgn = CreateRoundRectRgn(dx, dy, dx + r.right + 1, dy + r.bottom + 1, 2 * radius, 2 * radius);
                let _ = SetWindowRgn(hwnd, Some(rgn), true);
            }
            _ => {}
        }
    }
}

/// A plain message box (errors the user must see: the release build has no console).
pub fn message_box(title: &str, text: &str) {
    unsafe {
        let _ = MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND);
    }
}

/// Send this process's stderr (all the viewer's logging) to `path` when it has no console
/// (the release build is a GUI app), so problems can be read afterwards.
pub fn log_to_file(path: &std::path::Path) {
    use std::os::windows::io::IntoRawHandle;
    use windows::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE};
    unsafe {
        let current = GetStdHandle(STD_ERROR_HANDLE).unwrap_or_default();
        if !current.is_invalid() && !current.0.is_null() {
            return; // a console or a redirect already takes it
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(f) = std::fs::File::create(path) {
            let h = windows::Win32::Foundation::HANDLE(f.into_raw_handle());
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}
