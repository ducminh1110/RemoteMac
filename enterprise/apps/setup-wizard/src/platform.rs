//! Operating-system specifics: OS version, free disk space, the default output folder, opening
//! the browser and a folder.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct OsInfo {
    pub family: String,
    pub name: String,
    pub version: String,
    pub build: String,
    pub arch: String,
}

#[cfg(windows)]
pub fn os_info() -> Result<OsInfo, String> {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut v = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: `v` is a properly sized, initialised OSVERSIONINFOW.
    let status = unsafe { RtlGetVersion(&mut v) };
    if status.is_err() {
        return Err(format!("RtlGetVersion failed: {:?}", status));
    }
    let name = if v.dwMajorVersion == 10 && v.dwBuildNumber >= 22000 {
        "Windows 11"
    } else if v.dwMajorVersion == 10 {
        "Windows 10"
    } else {
        "Windows"
    };
    Ok(OsInfo {
        family: "windows".into(),
        name: name.into(),
        version: format!("{}.{}", v.dwMajorVersion, v.dwMinorVersion),
        build: v.dwBuildNumber.to_string(),
        arch: std::env::consts::ARCH.into(),
    })
}

#[cfg(unix)]
pub fn os_info() -> Result<OsInfo, String> {
    // SAFETY: uname fills a zeroed utsname; the fields are NUL-terminated C strings.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return Err("uname() failed".into());
        }
        let s = |f: &[libc::c_char]| {
            std::ffi::CStr::from_ptr(f.as_ptr())
                .to_string_lossy()
                .into_owned()
        };
        Ok(OsInfo {
            family: std::env::consts::OS.into(),
            name: s(&u.sysname),
            version: s(&u.release),
            build: s(&u.version),
            arch: s(&u.machine),
        })
    }
}

#[cfg(not(any(unix, windows)))]
pub fn os_info() -> Result<OsInfo, String> {
    Err("OS information is not available on this platform".into())
}

/// Free bytes available to the current user on the volume holding `path`.
#[cfg(windows)]
pub fn free_disk_bytes(path: &Path) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut avail = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut avail), None, None) }
        .map_err(|e| format!("GetDiskFreeSpaceExW failed: {e}"))?;
    Ok(avail)
}

#[cfg(unix)]
pub fn free_disk_bytes(path: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: `c` is a valid C string, `st` is zeroed and written by statvfs.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        #[allow(clippy::unnecessary_cast)]
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }
}

#[cfg(not(any(unix, windows)))]
pub fn free_disk_bytes(_path: &Path) -> Result<u64, String> {
    Err("not available on this platform".into())
}

/// The nearest existing folder at or above `path` (an output folder may not exist yet).
pub fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|p| p.is_dir()).map(Path::to_path_buf)
}

/// The user's Documents folder (the known folder, so a folder redirected to OneDrive is found).
#[cfg(windows)]
pub fn documents_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: SHGetKnownFolderPath returns a CoTaskMem-allocated NUL-terminated string that is
    // read once and freed.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s.map(PathBuf::from)
    }
    .or_else(|| std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Documents")))
}

/// Where packages go unless the provider picks another folder: `Documents\Taro Enterprise`
/// on Windows, `./dist` elsewhere.
pub fn default_output_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(d) = documents_dir() {
            return d.join("Taro Enterprise");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dist")
}

/// Where backups of release signing keys are kept: `%LOCALAPPDATA%\Taro Enterprise\signing`
/// on Windows (each file encrypted for this Windows account with DPAPI), `~/.config/
/// taro-enterprise/signing` elsewhere (readable by the owner only).
pub fn default_key_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("Taro Enterprise").join("signing");
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/taro-enterprise/signing")
}

/// Encrypt for this Windows account (DPAPI). Elsewhere the bytes stay as they are and the
/// file's permissions protect them.
#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `data` for the call; `out` is allocated by DPAPI and freed below.
    unsafe {
        CryptProtectData(&input, None, None, None, None, 0, &mut out).map_err(|e| e.to_string())?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        Ok(v)
    }
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut out)
            .map_err(|e| e.to_string())?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        Ok(v)
    }
}

#[cfg(not(windows))]
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    Ok(data.to_vec())
}

#[cfg(not(windows))]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    Ok(data.to_vec())
}

/// Write a file only its owner can read (Windows: the folder's ACL already is the user's).
pub fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.write_all(data)
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Show a folder in the file manager (Explorer on Windows).
#[cfg(windows)]
pub fn open_folder(path: &Path) -> Result<(), String> {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let p = HSTRING::from(path.as_os_str());
    // SAFETY: all strings are valid NUL-terminated wide strings for the duration of the call.
    let r = unsafe { ShellExecuteW(None, w!("explore"), &p, None, None, SW_SHOWNORMAL) };
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW returned {}", r.0 as isize))
    }
}

#[cfg(not(windows))]
pub fn open_folder(path: &Path) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{opener}: {e}"))
}

/// Print to the console of the program that started this one (a GUI app has none of its own),
/// so `--help` and `--browser` still print in a terminal.
#[cfg(windows)]
pub fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: no arguments besides the constant; failing (no parent console) is harmless.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// A plain message box (errors before or without the window).
#[cfg(windows)]
pub fn message_box(title: &str, text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    // SAFETY: both strings live for the duration of the call; no owner window.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// Open `url` in the default browser. Returns an error message if it could not be started
/// (the URL is printed to the console in any case).
#[cfg(windows)]
pub fn open_browser(url: &str) -> Result<(), String> {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let u = HSTRING::from(url);
    // SAFETY: all strings are valid NUL-terminated wide strings for the duration of the call.
    let r = unsafe { ShellExecuteW(None, w!("open"), &u, None, None, SW_SHOWNORMAL) };
    // ShellExecuteW returns a value > 32 on success.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW returned {}", r.0 as isize))
    }
}

#[cfg(target_os = "macos")]
pub fn open_browser(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn open_browser(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(any(unix, windows)))]
pub fn open_browser(_url: &str) -> Result<(), String> {
    Err("no browser launcher on this platform".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn os_info_reports_something() {
        let i = super::os_info().unwrap();
        assert!(!i.name.is_empty());
        assert!(!i.arch.is_empty());
    }

    #[test]
    fn ancestors_and_default_dir() {
        let t = std::env::temp_dir();
        assert_eq!(
            super::existing_ancestor(&t.join("does/not/exist/yet")).unwrap(),
            t
        );
        assert!(super::default_output_dir().is_absolute());
    }

    #[test]
    fn free_disk_reports_something() {
        let b = super::free_disk_bytes(&std::env::temp_dir()).unwrap();
        assert!(b > 0);
    }
}
