//! The "Remote Mac" launcher: a native window with a large-icon ListView of the Mac's applications.
//! Double-click (or Enter) launches one; several can run at once, each in its own windows and
//! taskbar group. Only one viewer process runs: a second invocation (e.g. a Start-menu shortcut)
//! forwards its `--app` to this window with WM_COPYDATA and exits.

use std::ffi::c_void;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmLauncher");
/// COPYDATASTRUCT.dwData tag for "launch this application id".
pub const COPYDATA_LAUNCH: usize = 0x524D_4C31; // "RML1"
/// WM_COMMAND id of the Settings button
pub const ID_SETTINGS: usize = 300;
const ICON_PX: i32 = 64;
/// Layout (DIPs at 96 dpi): heading band, footer band, side margin.
const HEAD: i32 = 76;
const FOOT: i32 = 16;
const SIDE: i32 = 18;
const BG: (u8, u8, u8) = (247, 247, 248);

pub struct Launcher {
    pub hwnd: HWND,
    pub list: HWND,
    settings: HWND,
    images: HIMAGELIST,
    /// Application ids in list order.
    pub ids: Vec<String>,
    /// Footer line (JetBrains Mono): connection details.
    footer: String,
    /// The grid's font: kept alive as long as the list uses it.
    _font: HFONT,
}

fn rgb((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn font(face: &str, px: i32, weight: i32) -> HFONT {
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, &HSTRING::from(face)) }
}

impl Launcher {
    pub fn create(hinst: HINSTANCE, show: bool) -> Option<Self> {
        unsafe {
            let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES };
            let _ = InitCommonControlsEx(&icc);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MacBridge"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 680, 460, None, None, Some(hinst), None).ok()?;
            let list = CreateWindowExW(WINDOW_EX_STYLE(0), WC_LISTVIEWW, w!(""), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | LVS_ICON | LVS_AUTOARRANGE | LVS_SINGLESEL),
                0, 0, 600, 380, Some(hwnd), None, Some(hinst), None).ok()?;
            let images = ImageList_Create(ICON_PX, ICON_PX, ILC_COLOR32, 8, 8);
            SendMessageW(list, LVM_SETIMAGELIST, Some(WPARAM(LVSIL_NORMAL as usize)), Some(LPARAM(images.0)));
            // a calm, Mac-like grid: Inter labels on the window's own background, roomy cells
            let ui = font(crate::native::ui_face(500), 13, 500);
            SendMessageW(list, WM_SETFONT, Some(WPARAM(ui.0 as usize)), Some(LPARAM(1)));
            SendMessageW(list, LVM_SETBKCOLOR, None, Some(LPARAM(rgb(BG).0 as isize)));
            SendMessageW(list, LVM_SETTEXTBKCOLOR, None, Some(LPARAM(rgb(BG).0 as isize)));
            SendMessageW(list, LVM_SETTEXTCOLOR, None, Some(LPARAM(rgb((30, 30, 32)).0 as isize)));
            SendMessageW(list, LVM_SETICONSPACING, None, Some(LPARAM(((112 << 16) | 120) as isize)));
            let ex = (LVS_EX_DOUBLEBUFFER | LVS_EX_BORDERSELECT) as isize;
            SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(ex as usize)), Some(LPARAM(ex)));
            let _ = windows::Win32::UI::Controls::SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
            let settings = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), w!("⚙  Settings"), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0), 0, 0, 110, 30, Some(hwnd), Some(HMENU(ID_SETTINGS as *mut c_void)), Some(hinst), None).ok()?;
            SendMessageW(settings, WM_SETFONT, Some(WPARAM(ui.0 as usize)), Some(LPARAM(1)));
            let l = Self { hwnd, list, settings, images, ids: vec![], footer: String::new(), _font: ui };
            let mut l = l;
            l.fit(); // WM_SIZE during creation came before the viewer's state existed
            l.status("connecting…");
            if show {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            Some(l)
        }
    }

    pub fn status(&mut self, s: &str) {
        self.footer = s.to_string();
        unsafe {
            // the title stays "MacBridge": what is connected where is no concern of the app list
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn scale(&self) -> f64 {
        (unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(self.hwnd) } as f64 / 96.0).max(1.0)
    }

    pub fn fit(&self) {
        let sc = self.scale();
        let px = |v: i32| (v as f64 * sc).round() as i32;
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let _ = MoveWindow(self.list, px(SIDE), px(HEAD), (rc.right - 2 * px(SIDE)).max(1), (rc.bottom - px(HEAD) - px(FOOT)).max(1), true);
            // Settings, top right in the heading band
            let _ = MoveWindow(self.settings, rc.right - px(SIDE) - px(116), px(22), px(116), px(32), true);
        }
    }

    /// Heading ("MacBridge" + how many apps) above the app grid.
    pub fn paint(&self, hdc: HDC) {
        let sc = self.scale();
        let px = |v: i32| (v as f64 * sc).round() as i32;
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let b = CreateSolidBrush(rgb(BG));
            FillRect(hdc, &rc, b);
            let _ = DeleteObject(b.into());
            SetBkMode(hdc, TRANSPARENT);
            let title = font(crate::native::ui_face(600), px(22), 600);
            let sub = font(crate::native::ui_face(400), px(13), 400);
            let mono = font(crate::native::mono_face(), px(11), 400);
            let old = SelectObject(hdc, title.into());
            SetTextColor(hdc, rgb((22, 22, 24)));
            let mut t: Vec<u16> = "MacBridge".encode_utf16().collect();
            let mut r = RECT { left: px(SIDE + 6), top: px(16), right: rc.right - px(SIDE), bottom: px(46) };
            DrawTextW(hdc, &mut t, &mut r, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
            SelectObject(hdc, sub.into());
            SetTextColor(hdc, rgb((120, 120, 128)));
            let mut t: Vec<u16> = format!("{} apps · double-click to open", self.ids.len()).encode_utf16().collect();
            let mut r = RECT { left: px(SIDE + 6), top: px(46), right: rc.right - px(SIDE), bottom: px(66) };
            DrawTextW(hdc, &mut t, &mut r, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
            SelectObject(hdc, old);
            for f in [title, sub, mono] {
                let _ = DeleteObject(f.into());
            }
        }
    }

    pub fn set_apps(&mut self, apps: &[(String, String, bool)]) {
        unsafe {
            SendMessageW(self.list, LVM_DELETEALLITEMS, None, None);
            self.ids.clear();
            for (i, (id, name, available)) in apps.iter().enumerate() {
                let label = if *available { name.clone() } else { format!("{name} (not installed)") };
                let mut text: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                let item = LVITEMW { mask: LVIF_TEXT | LVIF_IMAGE, iItem: i as i32, pszText: PWSTR(text.as_mut_ptr()), iImage: -1, ..Default::default() };
                SendMessageW(self.list, LVM_INSERTITEMW, None, Some(LPARAM(&item as *const _ as isize)));
                self.ids.push(id.clone());
            }
            let n = apps.len();
            let prev = self.footer.split(" · ").filter(|p| !p.ends_with(" apps") && *p != "connecting…").collect::<Vec<_>>().join(" · ");
            self.status(&if prev.is_empty() { format!("{n} apps") } else { format!("{prev} · {n} apps") });
        }
    }

    pub fn set_icon(&self, app: &str, icon: HICON) {
        let Some(row) = self.ids.iter().position(|x| x == app) else { return };
        unsafe {
            let img = ImageList_ReplaceIcon(self.images, -1, icon);
            let item = LVITEMW { mask: LVIF_IMAGE, iItem: row as i32, iImage: img, ..Default::default() };
            SendMessageW(self.list, LVM_SETITEMW, None, Some(LPARAM(&item as *const _ as isize)));
        }
    }

    pub fn count(&self) -> usize {
        unsafe { SendMessageW(self.list, LVM_GETITEMCOUNT, None, None).0 as usize }
    }

    /// Application id for an LVN_ITEMACTIVATE notification, if it is ours.
    pub fn activated(&self, lp: LPARAM) -> Option<String> {
        unsafe {
            let hdr = &*(lp.0 as *const NMHDR);
            if hdr.hwndFrom != self.list || hdr.code != LVN_ITEMACTIVATE {
                return None;
            }
            let act = &*(lp.0 as *const NMITEMACTIVATE);
            self.ids.get(act.iItem.max(0) as usize).cloned()
        }
    }
}

/// If another viewer is already running, hand it `app` and return true (this process should exit).
pub fn forward_to_running_instance(app: Option<&str>) -> bool {
    unsafe {
        let Ok(existing) = FindWindowW(CLASS, PCWSTR::null()) else { return false };
        if existing.0.is_null() {
            return false;
        }
        if let Some(app) = app {
            let bytes = app.as_bytes();
            let cds = COPYDATASTRUCT { dwData: COPYDATA_LAUNCH, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut c_void };
            SendMessageW(existing, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&cds as *const _ as isize)));
        } else {
            let _ = ShowWindow(existing, SW_SHOW);
        }
        let _ = SetForegroundWindow(existing);
        true
    }
}

/// Decode a WM_COPYDATA launch request.
pub fn copydata_app(lp: LPARAM) -> Option<String> {
    unsafe {
        let cds = &*(lp.0 as *const COPYDATASTRUCT);
        if cds.dwData != COPYDATA_LAUNCH || cds.lpData.is_null() || cds.cbData > 256 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
        std::str::from_utf8(bytes).ok().filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')).map(String::from)
    }
}
