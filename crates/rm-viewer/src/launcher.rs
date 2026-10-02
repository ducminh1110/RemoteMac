//! The "Remote Mac" launcher: a native window with a large-icon ListView of the Mac's applications.
//! Double-click (or Enter) launches one; several can run at once, each in its own windows and
//! taskbar group. Only one viewer process runs: a second invocation (e.g. a Start-menu shortcut)
//! forwards its `--app` to this window with WM_COPYDATA and exits.

use std::ffi::c_void;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmLauncher");
/// COPYDATASTRUCT.dwData tag for "launch this application id".
pub const COPYDATA_LAUNCH: usize = 0x524D_4C31; // "RML1"
const ICON_PX: i32 = 48;

pub struct Launcher {
    pub hwnd: HWND,
    pub list: HWND,
    images: HIMAGELIST,
    /// Application ids in list order.
    pub ids: Vec<String>,
}

impl Launcher {
    pub fn create(hinst: HINSTANCE, show: bool) -> Option<Self> {
        unsafe {
            let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES };
            let _ = InitCommonControlsEx(&icc);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("Remote Mac"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 620, 420, None, None, Some(hinst), None).ok()?;
            let list = CreateWindowExW(WINDOW_EX_STYLE(0), WC_LISTVIEWW, w!(""), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | LVS_ICON | LVS_AUTOARRANGE | LVS_SINGLESEL),
                0, 0, 600, 380, Some(hwnd), None, Some(hinst), None).ok()?;
            let images = ImageList_Create(ICON_PX, ICON_PX, ILC_COLOR32, 8, 8);
            SendMessageW(list, LVM_SETIMAGELIST, Some(WPARAM(LVSIL_NORMAL as usize)), Some(LPARAM(images.0)));
            let l = Self { hwnd, list, images, ids: vec![] };
            l.status("connecting…");
            if show {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            Some(l)
        }
    }

    pub fn status(&self, s: &str) {
        unsafe { let _ = SetWindowTextW(self.hwnd, &HSTRING::from(format!("Remote Mac — {s}"))); }
    }

    pub fn fit(&self) {
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let _ = MoveWindow(self.list, 0, 0, rc.right, rc.bottom, true);
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
            self.status(&format!("{} applications", apps.len()));
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
