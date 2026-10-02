//! Make the Mac's apps first-class Windows apps *while the Mac is connected*: one Start-menu
//! shortcut per app (so Windows Search finds "Xcode"), with the app's icon and the same
//! AppUserModelID as its windows (so pinning and taskbar grouping line up). A shortcut hands its
//! app to the running viewer. When the Mac disconnects or the viewer exits, they are removed, and
//! leftovers from a crash are cleaned at the next start: no shortcut ever points at a Mac that is gone.

use std::path::{Path, PathBuf};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SHGetKnownFolderPath, ShellLink, FOLDERID_Programs, KNOWN_FOLDER_FLAG};

/// (application id, display name, square RGBA icon if known)
pub type AppEntry = (String, String, Option<(u32, Vec<u8>)>);

pub fn aumid(app: &str) -> String {
    format!("RemoteMac.{}", app.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Remote Mac` (overridable for tests).
pub fn folder() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RM_SHORTCUT_DIR") {
        return Some(PathBuf::from(d));
    }
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Programs, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(|s| PathBuf::from(s).join("Remote Mac"))
    }
}

fn file_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !"<>:\"/\\|?*".contains(*c) && !c.is_control()).collect();
    let t = cleaned.trim().trim_end_matches('.');
    if t.is_empty() { "Mac app".into() } else { t.to_string() }
}

/// Create or refresh the shortcut for one app.
pub fn write(dir: &Path, exe: &Path, app: &str, name: &str, icon: Option<(u32, &[u8])>) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let lnk = dir.join(format!("{}.lnk", file_name(name)));
    let ico = dir.join("icons").join(format!("{}.ico", file_name(app)));
    if let Some(bytes) = icon.and_then(|(s, px)| crate::ico::rgba_to_ico(s, px)) {
        std::fs::create_dir_all(ico.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&ico, bytes).map_err(|e| e.to_string())?;
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        link.SetPath(&HSTRING::from(exe.as_os_str())).map_err(|e| e.to_string())?;
        link.SetArguments(&HSTRING::from(format!("--app {app}"))).map_err(|e| e.to_string())?;
        link.SetDescription(&HSTRING::from(format!("{name} on your Mac"))).map_err(|e| e.to_string())?;
        if ico.exists() {
            link.SetIconLocation(&HSTRING::from(ico.as_os_str()), 0).map_err(|e| e.to_string())?;
        }
        let store: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        let id = HSTRING::from(aumid(app));
        let pv = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 { vt: VT_LPWSTR, wReserved1: 0, wReserved2: 0, wReserved3: 0, Anonymous: PROPVARIANT_0_0_0 { pwszVal: PWSTR(id.as_ptr() as *mut u16) } }),
            },
        };
        let r = store.SetValue(&PKEY_AppUserModel_ID, &pv).and_then(|_| store.Commit());
        std::mem::forget(pv); // the store copied the string; we do not own it
        r.map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.to_string())?;
    }
    Ok(lnk)
}

/// Read back a shortcut's arguments and AppUserModelID (for verification).
pub fn read(lnk: &Path) -> Option<(String, Option<String>)> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
        let mut buf = [0u16; 512];
        link.GetArguments(&mut buf).ok()?;
        let args = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]);
        let store: IPropertyStore = link.cast().ok()?;
        let mut pv = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let inner = &pv.Anonymous.Anonymous;
        let id = (inner.vt == VT_LPWSTR && !inner.Anonymous.pwszVal.is_null()).then(|| inner.Anonymous.pwszVal.to_string().unwrap_or_default());
        let _ = PropVariantClear(&mut pv);
        Some((args, id))
    }
}

/// Exactly one shortcut per available app; shortcuts for apps that went away are removed.
pub fn sync(dir: &Path, exe: &Path, apps: &[AppEntry]) -> Vec<Result<PathBuf, String>> {
    let wanted: Vec<String> = apps.iter().map(|(_, n, _)| format!("{}.lnk", file_name(n))).collect();
    for lnk in list(dir) {
        if !wanted.contains(&lnk.file_name().unwrap_or_default().to_string_lossy().into_owned()) {
            let _ = std::fs::remove_file(lnk);
        }
    }
    apps.iter().map(|(id, name, icon)| write(dir, exe, id, name, icon.as_ref().map(|(s, px)| (*s, px.as_slice())))).collect()
}

pub fn list(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lnk")).collect())
        .unwrap_or_default()
}

/// The Mac is gone (disconnect, exit, or a crash last time): remove every shortcut and icon we made.
pub fn remove_all(dir: &Path) {
    for lnk in list(dir) {
        let _ = std::fs::remove_file(lnk);
    }
    let _ = std::fs::remove_dir_all(dir.join("icons"));
    let _ = std::fs::remove_dir(dir); // only if empty
}
