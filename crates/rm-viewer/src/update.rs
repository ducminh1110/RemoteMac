//! Taro's updates, from the repository's releases on GitHub. The newest release (not a
//! pre-release) is compared with this build; on Windows its installer is downloaded, checked
//! against the SHA-256 GitHub keeps for it, and run: it closes Taro, installs over it (for this
//! user, no administrator) and starts it again. The Mac updates with `taro-update.sh` in its
//! folder.
//!
//! The state lives here and is shown by Settings > About this Taro; the automatic check (at
//! start and every few hours, Settings) brings a notice when there is something new.

use std::sync::Mutex;

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The newest release (GitHub's REST API; pre-releases are never "latest").
#[cfg_attr(not(windows), allow(dead_code))]
const LATEST: &str = "https://api.github.com/repos/ducminh1110/RemoteMac/releases/latest";
/// Where the repository's release files are: an installer from anywhere else is not run.
const DOWNLOADS: &str = "https://github.com/ducminh1110/RemoteMac/releases/download/";
/// The Windows installer among a release's files.
pub const INSTALLER: &str = "Taro-Setup.exe";

/// A release's Windows installer: where it is, how large, its SHA-256 (hex) when GitHub has it.
#[derive(Debug, Clone, PartialEq)]
pub struct Installer {
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
}

/// A release, as much of it as an update needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub installer: Option<Installer>,
}

/// Where updating is at.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// not asked yet
    Unknown,
    Checking,
    UpToDate,
    Available(Release),
    /// the installer coming down: bytes so far, and in all (0: not known)
    Downloading { version: String, done: u64, total: u64 },
    /// the installer runs: Taro closes, and opens again once it is done
    Installing(String),
    Failed(String),
}

impl State {
    /// Checking, downloading or installing: nothing else is started meanwhile.
    pub fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading { .. } | State::Installing(_))
    }

    /// In words, for Settings > About this Taro.
    pub fn words(&self) -> String {
        match self {
            State::Unknown => "Updates come from Taro's releases on GitHub.".into(),
            State::Checking => "Checking for updates…".into(),
            State::UpToDate => "Taro is up to date.".into(),
            State::Available(r) => format!("Taro {} is available.", r.version),
            State::Downloading { version, done, total } if *total > 0 => format!("Downloading Taro {version}… {} %", (done * 100).checked_div(*total).unwrap_or(0).min(100)),
            State::Downloading { version, .. } => format!("Downloading Taro {version}…"),
            State::Installing(v) => format!("Installing Taro {v}: Taro closes and opens again."),
            State::Failed(why) => why.clone(),
        }
    }
}

/// The Taro on the Mac (feature "update"): where checking and updating it is at.
#[derive(Debug, Clone, PartialEq)]
pub enum MacState {
    Unknown,
    Checking,
    UpToDate,
    Available(String),
    /// the Mac fetches and puts the new Taro in place: how far (0-100)
    Installing(u32),
    /// in place; the Mac starts it when this connection ends (nothing is cut off)
    Done(String),
    Failed(String),
}

impl MacState {
    pub fn busy(&self) -> bool {
        matches!(self, MacState::Checking | MacState::Installing(_))
    }

    /// 0-100 while it is being installed.
    pub fn progress(&self) -> Option<u32> {
        match self {
            MacState::Installing(p) => Some(*p),
            _ => None,
        }
    }

    pub fn words(&self) -> String {
        match self {
            MacState::Unknown => "Asks the Mac to look for a newer Taro.".into(),
            MacState::Checking => "Asking the Mac…".into(),
            MacState::UpToDate => "Taro on the Mac is up to date.".into(),
            MacState::Available(v) => format!("Taro {v} is available for the Mac."),
            MacState::Installing(p) => format!("The Mac is updating… {p} %"),
            MacState::Done(v) => format!("Done: the Mac has Taro {v}. It starts when this connection ends."),
            MacState::Failed(why) => why.clone(),
        }
    }
}

/// A version as numbers and a pre-release part: "1.2.3", "v1.2.0-beta.3".
fn parse(v: &str) -> Option<([u64; 3], Option<String>)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let mut n = [0u64; 3];
    for (i, part) in core.split('.').enumerate() {
        if i >= 3 {
            return None;
        }
        n[i] = part.parse().ok()?;
    }
    Some((n, pre))
}

/// Pre-release parts in order: numbers as numbers (beta.10 after beta.9), else as text.
fn pre_order(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.split('.'), b.split('.'));
    loop {
        match (x.next(), y.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(p), Some(q)) => {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => p.cmp(q),
                };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

/// Whether version `a` is newer than `b` (a pre-release comes before its release; a version
/// that cannot be read is never newer).
pub fn newer(a: &str, b: &str) -> bool {
    let (Some((x, xp)), Some((y, yp))) = (parse(a), parse(b)) else { return false };
    if x != y {
        return x > y;
    }
    match (xp, yp) {
        (None, Some(_)) => true,
        (Some(p), Some(q)) => pre_order(&p, &q) == std::cmp::Ordering::Greater,
        _ => false,
    }
}

/// The version in a Mac's hello ("taro 1.2.3 Version 15.1 …"; older ones "macbridge …").
pub fn version_in(agent: &str) -> Option<String> {
    agent.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && parse(w).is_some()).map(str::to_string)
}

/// The release GitHub describes (its "latest release" answer); None for a draft, a
/// pre-release, or something else.
pub fn parse_release(json: &[u8]) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let version = v["tag_name"].as_str()?.trim_start_matches(['v', 'V']).to_string();
    parse(&version)?;
    let installer = v["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"].as_str() == Some(INSTALLER))).and_then(|a| {
        Some(Installer {
            url: a["browser_download_url"].as_str()?.to_string(),
            size: a["size"].as_u64().unwrap_or(0),
            sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
        })
    });
    Some(Release { version, installer })
}

/// The SHA-256 of `bytes`, in hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether a downloaded installer is the one the release lists (its size, its digest).
pub fn verify(bytes: &[u8], i: &Installer) -> Result<(), String> {
    if i.size > 0 && bytes.len() as u64 != i.size {
        return Err(format!("the download is {} bytes, the release lists {}", bytes.len(), i.size));
    }
    match &i.sha256 {
        Some(want) if sha256_hex(bytes) != *want => Err("the download does not match the release (SHA-256)".into()),
        _ => Ok(()),
    }
}

static STATE: Mutex<State> = Mutex::new(State::Unknown);
type Hook = Box<dyn Fn() + Send>;
/// Told each time the state changes (from any thread).
static ON_CHANGE: Mutex<Option<Hook>> = Mutex::new(None);
/// Ends Taro for the installer (without closing the Mac's apps).
static ON_EXIT: Mutex<Option<Hook>> = Mutex::new(None);

static MAC_STATE: Mutex<MacState> = Mutex::new(MacState::Unknown);

pub fn mac_state() -> MacState {
    MAC_STATE.lock().unwrap().clone()
}

pub fn set_mac(s: MacState) {
    *MAC_STATE.lock().unwrap() = s;
    if let Some(f) = ON_CHANGE.lock().unwrap().as_ref() {
        f();
    }
}

/// What the Mac said about its update (`MacUpdateStatus`).
pub fn mac_status(state: &str, latest: &str, progress: u32, detail: &str) {
    set_mac(match state {
        "up_to_date" => MacState::UpToDate,
        "available" => MacState::Available(latest.into()),
        "installing" => MacState::Installing(progress.min(100)),
        "done" => MacState::Done(latest.into()),
        _ => MacState::Failed(if detail.is_empty() { "The Mac could not update.".into() } else { detail.chars().take(160).collect() }),
    });
}

pub fn state() -> State {
    STATE.lock().unwrap().clone()
}

fn set(s: State) {
    *STATE.lock().unwrap() = s;
    if let Some(f) = ON_CHANGE.lock().unwrap().as_ref() {
        f();
    }
}

/// `f` runs each time the state changes, on whatever thread changed it.
pub fn on_change(f: impl Fn() + Send + 'static) {
    *ON_CHANGE.lock().unwrap() = Some(Box::new(f));
}

/// How Taro ends once the installer runs.
pub fn on_exit(f: impl Fn() + Send + 'static) {
    *ON_EXIT.lock().unwrap() = Some(Box::new(f));
}

/// Ask for the newest release, in the background. `quiet` (the automatic check): a failure
/// is not shown, the state goes back to what it was.
pub fn check(quiet: bool) {
    let before = {
        let mut s = STATE.lock().unwrap();
        if s.busy() {
            return;
        }
        std::mem::replace(&mut *s, State::Checking)
    };
    set(State::Checking);
    std::thread::spawn(move || {
        let found = fetch_latest();
        set(match found {
            Ok(r) if newer(&r.version, VERSION) => State::Available(r),
            Ok(_) => State::UpToDate,
            Err(_) if quiet => before,
            Err(e) => State::Failed(format!("Could not check for updates: {e}")),
        });
    });
}

/// Download the available release's installer, check it and run it; Taro then ends.
pub fn install() {
    let State::Available(r) = state() else { return };
    let Some(i) = r.installer.clone() else {
        set(State::Failed(format!("Taro {} has no Windows installer: download it from the releases page.", r.version)));
        return;
    };
    if !i.url.starts_with(DOWNLOADS) {
        set(State::Failed("The update is not on Taro's releases page: not installed.".into()));
        return;
    }
    set(State::Downloading { version: r.version.clone(), done: 0, total: i.size });
    std::thread::spawn(move || match download_and_run(&r, &i) {
        Ok(()) => {
            if let Some(f) = ON_EXIT.lock().unwrap().as_ref() {
                f();
            }
        }
        Err(e) => set(State::Failed(format!("Taro {} could not be installed: {e}", r.version))),
    });
}

#[cfg(windows)]
fn fetch_latest() -> Result<Release, String> {
    let body = http::get(LATEST, "application/vnd.github+json", |_, _| {})?;
    parse_release(&body).ok_or_else(|| "no release found".to_string())
}

#[cfg(not(windows))]
fn fetch_latest() -> Result<Release, String> {
    Err("updates are checked by the Windows app".into())
}

#[cfg(windows)]
fn download_and_run(r: &Release, i: &Installer) -> Result<(), String> {
    let mut shown = u64::MAX;
    let version = r.version.clone();
    let bytes = http::get(&i.url, "application/octet-stream", |done, total| {
        // the progress once per percent, not once per chunk
        let pct = (done * 100).checked_div(total).unwrap_or(done >> 20);
        if pct != shown {
            shown = pct;
            set(State::Downloading { version: version.clone(), done, total });
        }
    })?;
    verify(&bytes, i)?;
    let path = std::env::temp_dir().join(format!("Taro-Setup-{}.exe", r.version));
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    set(State::Installing(r.version.clone()));
    // silent, for this user; it closes what is left of Taro, then starts it again (the
    // installer's RELAUNCH)
    std::process::Command::new(&path)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/RELAUNCH=1"])
        .spawn()
        .map_err(|e| format!("the installer did not start ({e})"))?;
    Ok(())
}

#[cfg(not(windows))]
fn download_and_run(_: &Release, _: &Installer) -> Result<(), String> {
    Err("updates install on Windows".into())
}

/// HTTPS GET with WinHTTP (the system's proxy and certificates; redirects followed).
#[cfg(windows)]
mod http {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;

    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    fn open(h: *mut core::ffi::c_void, what: &str) -> Result<Handle, String> {
        if h.is_null() {
            Err(format!("{what}: {}", std::io::Error::last_os_error()))
        } else {
            Ok(Handle(h))
        }
    }

    /// The body of `url` (https only); `progress` gets bytes so far and in all (0: not known).
    pub fn get(url: &str, accept: &str, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
        let rest = url.strip_prefix("https://").ok_or("not an https address")?;
        let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
        unsafe {
            let agent = HSTRING::from(format!("Taro/{} (Windows)", super::VERSION));
            let session = open(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "WinHttpOpen")?;
            let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 20_000, 30_000);
            let connect = open(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0), "connect")?;
            let request = open(WinHttpOpenRequest(connect.0, &HSTRING::from("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE), "request")?;
            let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
            WinHttpAddRequestHeaders(request.0, &headers, WINHTTP_ADDREQ_FLAG_ADD).map_err(|e| format!("headers: {e}"))?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|e| format!("could not reach {host} ({})", e.message()))?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| format!("no answer from {host} ({})", e.message()))?;
            let number = |info: u32| -> Option<u64> {
                let (mut v, mut len) = (0u32, 4u32);
                WinHttpQueryHeaders(request.0, info | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut v as *mut u32 as *mut _), &mut len, std::ptr::null_mut()).ok().map(|_| v as u64)
            };
            let status = number(WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
            if status != 200 {
                return Err(format!("{host} answered {status}"));
            }
            let total = number(WINHTTP_QUERY_CONTENT_LENGTH).unwrap_or(0);
            let mut body = Vec::with_capacity(total.min(64 << 20) as usize);
            loop {
                let mut avail = 0u32;
                WinHttpQueryDataAvailable(request.0, &mut avail).map_err(|e| format!("reading: {}", e.message()))?;
                if avail == 0 {
                    break;
                }
                if body.len() + avail as usize > 256 << 20 {
                    return Err("the download is too large".into());
                }
                let start = body.len();
                body.resize(start + avail as usize, 0);
                let mut read = 0u32;
                WinHttpReadData(request.0, body[start..].as_mut_ptr() as *mut _, avail, &mut read).map_err(|e| format!("reading: {}", e.message()))?;
                body.truncate(start + read as usize);
                progress(body.len() as u64, total);
            }
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_in_order() {
        assert!(newer("1.2.3", "1.2.2"));
        assert!(newer("v1.3.0", "1.2.9"));
        assert!(newer("2.0", "1.9.9"));
        assert!(!newer("1.2.2", "1.2.2"));
        assert!(!newer("1.2.1", "1.2.2"));
        // a pre-release comes before its release, and after the one before
        assert!(newer("1.2.0", "1.2.0-beta.3"));
        assert!(!newer("1.2.0-beta.3", "1.2.0"));
        assert!(newer("1.2.0-beta.10", "1.2.0-beta.9"));
        assert!(newer("1.2.0-beta.1", "1.1.9"));
        // something else is never newer
        assert!(!newer("latest", "1.0.0"));
        assert!(!newer("1.2.3.4", "1.0.0"));
    }

    #[test]
    fn a_macs_version_from_its_hello() {
        assert_eq!(version_in("taro 1.2.3 Version 15.1 (Build 24B83)").as_deref(), Some("1.2.3"));
        assert_eq!(version_in("macbridge 1.2.0-beta.3 Version 14.6").as_deref(), Some("1.2.0-beta.3"));
        assert_eq!(version_in("rm-fakeagent"), None);
    }

    #[test]
    fn the_latest_release_and_its_installer() {
        let json = br#"{"tag_name":"1.2.4","draft":false,"prerelease":false,"assets":[
            {"name":"Taro-macos.tar.gz","browser_download_url":"https://github.com/ducminh1110/RemoteMac/releases/download/1.2.4/Taro-macos.tar.gz","size":10},
            {"name":"Taro-Setup.exe","browser_download_url":"https://github.com/ducminh1110/RemoteMac/releases/download/1.2.4/Taro-Setup.exe","size":3,
             "digest":"sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "1.2.4");
        let i = r.installer.clone().unwrap();
        assert!(i.url.starts_with(DOWNLOADS) && i.url.ends_with("/1.2.4/Taro-Setup.exe"));
        assert_eq!(i.size, 3);
        // the download is checked: "abc" is the digest above, anything else is not
        assert_eq!(verify(b"abc", &i), Ok(()));
        assert!(verify(b"abd", &i).is_err());
        assert!(verify(b"abcd", &i).is_err(), "another size");
        // a pre-release, a draft or something else is no update
        assert_eq!(parse_release(br#"{"tag_name":"1.3.0-beta.1","prerelease":true,"assets":[]}"#), None);
        assert_eq!(parse_release(br#"{"tag_name":"1.3.0","draft":true}"#), None);
        assert_eq!(parse_release(br#"{"message":"Not Found"}"#), None);
        // a release without an installer still says its version
        assert_eq!(parse_release(br#"{"tag_name":"v1.2.5","assets":[]}"#).map(|r| (r.version, r.installer)), Some(("1.2.5".into(), None)));
    }

    #[test]
    fn the_mac_update_in_words() {
        mac_status("installing", "1.2.4", 40, "");
        assert_eq!(mac_state(), MacState::Installing(40));
        assert!(mac_state().busy() && mac_state().words().contains("40 %"));
        mac_status("done", "1.2.4", 100, "");
        assert!(mac_state().words().starts_with("Done"));
        mac_status("failed", "", 0, "");
        assert_eq!(mac_state().words(), "The Mac could not update.");
        mac_status("available", "1.2.4", 0, "");
        assert!(!mac_state().busy());
        set_mac(MacState::Unknown);
    }

    #[test]
    fn the_state_in_words() {
        assert_eq!(State::Downloading { version: "1.2.4".into(), done: 512, total: 1024 }.words(), "Downloading Taro 1.2.4… 50 %");
        assert!(State::Checking.busy() && !State::UpToDate.busy());
    }
}
