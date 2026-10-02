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
