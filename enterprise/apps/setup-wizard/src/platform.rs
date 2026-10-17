//! Operating-system specifics: OS version, free disk space, opening the browser.

use serde::Serialize;
use std::path::Path;

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
    fn free_disk_reports_something() {
        let b = super::free_disk_bytes(&std::env::temp_dir()).unwrap();
        assert!(b > 0);
    }
}
