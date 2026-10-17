//! Taro's updates, from the repository's releases on GitHub. The newest release (not a
//! pre-release) is compared with this build; on Windows its installer is downloaded, checked
//! against the SHA-256 GitHub keeps for it, and run: it closes Taro, installs over it (for this
//! user, no administrator) and starts it again. The Mac updates with `taro-update.sh` in its
//! folder.
//!
//! A provider's Taro (brand.rs) is the official Taro.exe renamed, its `provider/` folder next to
//! it: it takes the same releases, but in place ([`Plan::ReplaceExe`]): the release's plain
//! `Taro.exe` is downloaded and checked, written next to this program as `<exe>.new`, this
//! program renamed to `<exe>.old` (Windows lets a running program be renamed), `.new` renamed to
//! this program's own name, and the new one started; `.old` is removed at its next start. The
//! provider folder is never touched, and a failed step puts the old program back. A provider
//! may also turn updates off (`updates.clients: off`): then nothing is ever asked for.
//!
//! The state lives here and is shown by Settings > About this Taro; the automatic check (at
//! start and every few hours, Settings) brings a notice when there is something new.

use std::path::{Path, PathBuf};
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
/// The plain program among a release's files (what a provider's Taro takes in place).
pub const PROGRAM: &str = "Taro.exe";

/// How this copy of Taro is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// the standard Taro: the release's installer
    Installer,
    /// a provider's Taro: the release's Taro.exe in place of this program
    ReplaceExe,
    /// a provider's Taro with updates off: never checked
    Nothing,
}

pub fn plan_for(u: crate::brand::Updates) -> Plan {
    match u {
        crate::brand::Updates::Installer => Plan::Installer,
        crate::brand::Updates::InPlace => Plan::ReplaceExe,
        crate::brand::Updates::Off => Plan::Nothing,
    }
}

/// This copy's plan.
pub fn plan() -> Plan {
    plan_for(crate::brand::get().updates())
}

/// What installing a release comes to.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    RunInstaller(Installer),
    SwapExe(Installer),
    /// not installed, and why (in words)
    Refuse(String),
}

/// What installing `r` comes to with `plan` (`name`, `vendor`: this copy's, for the words).
pub fn decide(plan: Plan, r: &Release, name: &str, vendor: Option<&str>) -> Step {
    let vendor = vendor.unwrap_or("your provider");
    let ours = |i: &Installer| i.url.starts_with(DOWNLOADS);
    match plan {
        Plan::Nothing => Step::Refuse(format!("Updates are provided by {vendor}.")),
        Plan::Installer => match &r.installer {
            None => Step::Refuse(format!("Taro {} has no Windows installer: download it from the releases page.", r.version)),
            Some(i) if !ours(i) => Step::Refuse("The update is not on Taro's releases page: not installed.".into()),
            Some(i) => Step::RunInstaller(i.clone()),
        },
        Plan::ReplaceExe => match &r.program {
            None => Step::Refuse(format!("{name} {} is out: update by downloading the new package from {vendor}.", r.version)),
            Some(i) if !ours(i) => Step::Refuse("The update is not on Taro's releases page: not installed.".into()),
            // in place, without an installer of its own: only a file the release vouches for
            Some(i) if i.sha256.is_none() => Step::Refuse(format!("The release lists no SHA-256 for {PROGRAM}: not installed. Download the new package from {vendor}.")),
            Some(i) => Step::SwapExe(i.clone()),
        },
    }
}

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
    /// the plain Taro.exe (releases from 1.4.2 on)
    pub program: Option<Installer>,
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
        self.words_for(crate::brand::get())
    }

    /// In words, with `b`'s name (a provider's product is named as such).
    pub fn words_for(&self, b: &crate::brand::Brand) -> String {
        let name = b.name();
        match self {
            State::Unknown if b.updates() == crate::brand::Updates::Off => format!("Updates are provided by {}.", b.vendor().unwrap_or("your provider")),
            State::Unknown => "Updates come from Taro's releases on GitHub.".into(),
            State::Checking => "Checking for updates…".into(),
            State::UpToDate => format!("{name} is up to date."),
            State::Available(r) => format!("{name} {} is available.", r.version),
            State::Downloading { version, done, total } if *total > 0 => format!("Downloading {name} {version}… {} %", (done * 100).checked_div(*total).unwrap_or(0).min(100)),
            State::Downloading { version, .. } => format!("Downloading {name} {version}…"),
            State::Installing(v) => format!("Installing {name} {v}: {name} closes and opens again."),
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
        crate::brand::named(&self.taro_words()).into_owned()
    }

    fn taro_words(&self) -> String {
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
    let asset = |name: &str| {
        v["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"].as_str() == Some(name))).and_then(|a| {
            Some(Installer {
                url: a["browser_download_url"].as_str()?.to_string(),
                size: a["size"].as_u64().unwrap_or(0),
                sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
            })
        })
    };
    Some(Release { version, installer: asset(INSTALLER), program: asset(PROGRAM) })
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
    // a provider that sends its own packages: nothing is ever asked for
    if plan() == Plan::Nothing {
        return;
    }
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

/// Install the available release, as this copy's plan says: the standard Taro downloads its
/// installer, checks it and runs it; a provider's Taro puts the release's Taro.exe in place of
/// this program and starts it. Taro then ends.
pub fn install() {
    let State::Available(r) = state() else { return };
    let b = crate::brand::get();
    let step = decide(plan(), &r, b.name(), b.vendor());
    let i = match &step {
        Step::Refuse(why) => {
            set(State::Failed(why.clone()));
            return;
        }
        Step::RunInstaller(i) | Step::SwapExe(i) => i.clone(),
    };
    set(State::Downloading { version: r.version.clone(), done: 0, total: i.size });
    std::thread::spawn(move || {
        let done = match step {
            Step::SwapExe(_) => download_and_swap(&r, &i),
            _ => download_and_run(&r, &i),
        };
        match done {
            Ok(()) => {
                if let Some(f) = ON_EXIT.lock().unwrap().as_ref() {
                    f();
                }
            }
            Err(e) => set(State::Failed(format!("{} {} could not be installed: {e}", b.name(), r.version))),
        }
    });
}

/// `<exe>.new`, `<exe>.old`: beside the program, the same name with that ending.
pub fn beside(exe: &Path, ending: &str) -> PathBuf {
    let mut name = exe.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(ending);
    exe.with_file_name(name)
}

/// Put `new` (the new program's bytes) in place of the program at `exe`, keeping its name:
/// `<exe>.new` is written, `exe` renamed to `<exe>.old`, `.new` renamed to `exe`. When a step
/// fails, what was there is put back (the old program under its own name, no `.new` left).
/// Nothing else in the folder is touched (a provider's `provider/` stays as it is).
pub fn swap_exe(exe: &Path, new: &[u8]) -> Result<(), String> {
    swap_exe_with(exe, new, |a, b| std::fs::rename(a, b))
}

/// [`swap_exe`] with the renaming given (tests make it fail).
pub fn swap_exe_with(exe: &Path, new: &[u8], rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> Result<(), String> {
    let (next, old) = (beside(exe, ".new"), beside(exe, ".old"));
    if !exe.is_file() {
        return Err(format!("{} is not there", exe.display()));
    }
    let _ = std::fs::remove_file(&next);
    if let Err(e) = std::fs::write(&next, new) {
        let _ = std::fs::remove_file(&next);
        return Err(format!("could not write {} ({e}): is the folder writable?", next.display()));
    }
    // a program left from an update before (it should be gone since the last start)
    if old.exists() && std::fs::remove_file(&old).is_err() {
        let _ = std::fs::remove_file(&next);
        return Err(format!("{} is still there and in use: start this program again, then update", old.display()));
    }
    if let Err(e) = rename(exe, &old) {
        let _ = std::fs::remove_file(&next);
        return Err(format!("could not move {} aside ({e})", exe.display()));
    }
    if let Err(e) = rename(&next, exe) {
        let _ = std::fs::remove_file(&next);
        return Err(match rename(&old, exe) {
            Ok(()) => format!("could not put the new program in place ({e}): the old one is back"),
            Err(back) => format!("could not put the new program in place ({e}), nor the old one back ({back}): it is {}", old.display()),
        });
    }
    Ok(())
}

/// At start: what an update in place left behind (`<exe>.old`, a half-written `<exe>.new`).
pub fn clean_after_update(exe: &Path) {
    for ending in [".old", ".new"] {
        let p = beside(exe, ending);
        if p.is_file() {
            match std::fs::remove_file(&p) {
                Ok(()) => eprintln!("removed {} (left by the last update)", p.display()),
                Err(e) => eprintln!("{}: {e} (removed at a later start)", p.display()),
            }
        }
    }
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

/// The file `i` of release `r`, checked against what the release lists.
#[cfg(windows)]
fn download(r: &Release, i: &Installer) -> Result<Vec<u8>, String> {
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
    Ok(bytes)
}

/// A provider's Taro: the release's Taro.exe in place of this program (its own name kept, the
/// provider folder left alone), then started; this one ends.
#[cfg(windows)]
fn download_and_swap(r: &Release, i: &Installer) -> Result<(), String> {
    let bytes = download(r, i)?;
    if !bytes.starts_with(b"MZ") {
        return Err("the download is not a Windows program".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("where this program is: {e}"))?;
    set(State::Installing(r.version.clone()));
    swap_exe(&exe, &bytes)?;
    // the new program, once this one is gone (it then removes <exe>.old); a log if this has one
    let mut cmd = std::process::Command::new(&exe);
    if crate::logs_enabled() {
        cmd.arg("--logs-enabled");
    }
    cmd.arg("--wait-pid").arg(std::process::id().to_string());
    if let Err(e) = cmd.spawn() {
        return Err(format!("it is in place, but did not start ({e}): start {} again", crate::brand::name()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn download_and_swap(_: &Release, _: &Installer) -> Result<(), String> {
    Err("updates install on Windows".into())
}

#[cfg(windows)]
fn download_and_run(r: &Release, i: &Installer) -> Result<(), String> {
    let bytes = download(r, i)?;
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

/// HTTPS with WinHTTP (the system's proxy and certificates; redirects followed).
#[cfg(windows)]
pub(crate) mod http {
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
    pub fn get(url: &str, accept: &str, progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
        let (status, body) = request("GET", url, &format!("Accept: {accept}\r\n"), None, progress)?;
        if status != 200 {
            let host = url.split('/').nth(2).unwrap_or(url);
            return Err(format!("{host} answered {status}"));
        }
        Ok(body)
    }

    /// POST a JSON body; the status and the body, whatever the status (the caller reads errors).
    pub fn post_json(url: &str, json: &str) -> Result<(u16, Vec<u8>), String> {
        let (status, body) = request("POST", url, "Accept: application/json\r\nContent-Type: application/json\r\n", Some(json.as_bytes()), |_, _| {})?;
        Ok((status as u16, body))
    }

    /// A JSON API call with an optional bearer token; the status and the body, whatever the
    /// status (the caller reads errors).
    pub fn call(method: &str, url: &str, bearer: Option<&str>, json: Option<&str>) -> Result<(u16, Vec<u8>), String> {
        let mut headers = String::from("Accept: application/json\r\n");
        if json.is_some() {
            headers.push_str("Content-Type: application/json\r\n");
        }
        if let Some(t) = bearer {
            headers.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        let (status, body) = request(method, url, &headers, json.map(str::as_bytes), |_, _| {})?;
        Ok((status as u16, body))
    }

    /// `https://host[:port]/path`, or plain http to this computer only (development).
    fn split(url: &str) -> Result<(bool, String, u16, String), String> {
        let (secure, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
            (Some(r), _) => (true, r),
            (None, Some(r)) => (false, r),
            _ => return Err("not an https address".into()),
        };
        let (hostport, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h.to_string(), p.parse::<u16>().map_err(|_| "bad port")?),
            _ => (hostport.to_string(), if secure { 443 } else { 80 }),
        };
        if !secure && !matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
            return Err("only https addresses are allowed".into());
        }
        Ok((secure, host, port, path))
    }

    fn request(method: &str, url: &str, headers: &str, body: Option<&[u8]>, mut progress: impl FnMut(u64, u64)) -> Result<(u64, Vec<u8>), String> {
        let (secure, host, port, path) = split(url)?;
        let host = host.as_str();
        unsafe {
            let agent = HSTRING::from(format!("Taro/{} (Windows)", super::VERSION));
            let session = open(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "WinHttpOpen")?;
            let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 20_000, 30_000);
            let connect = open(WinHttpConnect(session.0, &HSTRING::from(host), port, 0), "connect")?;
            let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
            let request = open(WinHttpOpenRequest(connect.0, &HSTRING::from(method), &HSTRING::from(path.as_str()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags), "request")?;
            let headers: Vec<u16> = headers.encode_utf16().collect();
            WinHttpAddRequestHeaders(request.0, &headers, WINHTTP_ADDREQ_FLAG_ADD).map_err(|e| format!("headers: {e}"))?;
            let (ptr, len) = body.map_or((None, 0), |b| (Some(b.as_ptr() as *const core::ffi::c_void), b.len() as u32));
            WinHttpSendRequest(request.0, None, ptr, len, len, 0).map_err(|e| format!("could not reach {host} ({})", e.message()))?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| format!("no answer from {host} ({})", e.message()))?;
            let number = |info: u32| -> Option<u64> {
                let (mut v, mut len) = (0u32, 4u32);
                WinHttpQueryHeaders(request.0, info | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut v as *mut u32 as *mut _), &mut len, std::ptr::null_mut()).ok().map(|_| v as u64)
            };
            let status = number(WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
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
            Ok((status, body))
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
        // a provider's Taro says its own name; with updates off, who provides them
        let acme = crate::brand::tests::acme("#0F766E", "notify");
        assert_eq!(State::UpToDate.words_for(&acme), "Acme Connect is up to date.");
        assert_eq!(State::Unknown.words_for(&acme), "Updates come from Taro's releases on GitHub.");
        assert_eq!(State::Unknown.words_for(&crate::brand::tests::acme("#0F766E", "off")), "Updates are provided by Acme Cloud Ltd.");
    }

    fn asset(name: &str, digest: bool) -> Installer {
        Installer { url: format!("{DOWNLOADS}1.5.0/{name}"), size: 3, sha256: digest.then(|| sha256_hex(b"abc")) }
    }

    fn release(installer: bool, program: bool) -> Release {
        Release { version: "1.5.0".into(), installer: installer.then(|| asset(INSTALLER, true)), program: program.then(|| asset(PROGRAM, true)) }
    }

    #[test]
    fn the_plain_program_is_found_among_a_releases_files() {
        let json = br#"{"tag_name":"1.5.0","assets":[
            {"name":"Taro-Setup.exe","browser_download_url":"https://github.com/ducminh1110/RemoteMac/releases/download/1.5.0/Taro-Setup.exe","size":3},
            {"name":"Taro.exe","browser_download_url":"https://github.com/ducminh1110/RemoteMac/releases/download/1.5.0/Taro.exe","size":3,
             "digest":"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}]}"#;
        let r = parse_release(json).unwrap();
        let p = r.program.unwrap();
        assert!(p.url.ends_with("/1.5.0/Taro.exe"));
        assert_eq!(verify(b"abc", &p), Ok(()));
        assert!(verify(b"abd", &p).is_err());
        assert!(r.installer.unwrap().url.ends_with("/Taro-Setup.exe"));
        assert_eq!(parse_release(br#"{"tag_name":"1.4.1","assets":[]}"#).unwrap().program, None);
    }

    #[test]
    fn each_copy_updates_its_own_way() {
        use crate::brand::{tests::acme, Brand};
        assert_eq!(plan_for(Brand::standard().updates()), Plan::Installer);
        assert_eq!(plan_for(acme("#0F766E", "notify").updates()), Plan::ReplaceExe);
        assert_eq!(plan_for(acme("#0F766E", "off").updates()), Plan::Nothing);
        // the standard Taro: the installer, as always (the plain program is not its business)
        let r = release(true, true);
        assert_eq!(decide(Plan::Installer, &r, "Taro", None), Step::RunInstaller(asset(INSTALLER, true)));
        assert!(matches!(decide(Plan::Installer, &release(false, true), "Taro", None), Step::Refuse(w) if w.contains("no Windows installer")));
        // a provider's Taro: the plain program, in place (never the installer, which would put
        // the standard Taro over it)
        assert_eq!(decide(Plan::ReplaceExe, &r, "Acme Connect", Some("Acme Cloud Ltd")), Step::SwapExe(asset(PROGRAM, true)));
        assert!(matches!(decide(Plan::ReplaceExe, &release(true, false), "Acme Connect", Some("Acme Cloud Ltd")),
            Step::Refuse(w) if w == "Acme Connect 1.5.0 is out: update by downloading the new package from Acme Cloud Ltd."));
        // in place only with the release's SHA-256, and only from Taro's releases
        let mut no_digest = release(false, true);
        no_digest.program = Some(asset(PROGRAM, false));
        assert!(matches!(decide(Plan::ReplaceExe, &no_digest, "Acme Connect", Some("Acme Cloud Ltd")), Step::Refuse(w) if w.contains("SHA-256")));
        let mut elsewhere = release(true, true);
        elsewhere.program.as_mut().unwrap().url = "https://example.com/Taro.exe".into();
        elsewhere.installer.as_mut().unwrap().url = "https://example.com/Taro-Setup.exe".into();
        assert!(matches!(decide(Plan::ReplaceExe, &elsewhere, "Acme Connect", None), Step::Refuse(w) if w.contains("not on Taro's releases page")));
        assert!(matches!(decide(Plan::Installer, &elsewhere, "Taro", None), Step::Refuse(w) if w.contains("not on Taro's releases page")));
        // updates off: nothing at all
        assert_eq!(decide(Plan::Nothing, &r, "Acme Connect", Some("Acme Cloud Ltd")), Step::Refuse("Updates are provided by Acme Cloud Ltd.".into()));
    }

    /// A provider's folder: Acme.exe and provider/provider.json.
    fn package(name: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("rm-viewer-swap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("provider")).unwrap();
        std::fs::write(d.join("provider").join("provider.json"), "{\"provider\":\"kept\"}").unwrap();
        let exe = d.join("Acme.exe");
        std::fs::write(&exe, b"MZ old").unwrap();
        (d, exe)
    }

    fn listing(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn the_program_is_swapped_in_place_and_the_old_one_removed_at_the_next_start() {
        let (d, exe) = package("ok");
        swap_exe(&exe, b"MZ new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ new", "the new program under the provider's name");
        assert_eq!(std::fs::read(beside(&exe, ".old")).unwrap(), b"MZ old");
        assert!(!beside(&exe, ".new").exists());
        assert_eq!(std::fs::read_to_string(d.join("provider").join("provider.json")).unwrap(), "{\"provider\":\"kept\"}", "the provider folder is not touched");
        assert_eq!(listing(&d), ["Acme.exe", "Acme.exe.old", "provider"]);
        // the next start cleans up
        clean_after_update(&exe);
        assert_eq!(listing(&d), ["Acme.exe", "provider"]);
        // a second update while an old .old is still there (removable): fine
        std::fs::write(beside(&exe, ".old"), b"stale").unwrap();
        swap_exe(&exe, b"MZ newer").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ newer");
        assert_eq!(std::fs::read(beside(&exe, ".old")).unwrap(), b"MZ new");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_failed_rename_puts_the_old_program_back() {
        // the new program cannot be moved in (the second rename): the old one is put back
        let (d, exe) = package("rollback");
        let calls = std::cell::Cell::new(0);
        let flaky = |a: &Path, b: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(std::io::Error::other("refused"))
            } else {
                std::fs::rename(a, b)
            }
        };
        let e = swap_exe_with(&exe, b"MZ new", flaky).unwrap_err();
        assert!(e.contains("the old one is back"), "{e}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ old");
        assert_eq!(listing(&d), ["Acme.exe", "provider"], "no .new, no .old left");
        // the program cannot be moved aside (the first rename): nothing changes
        let e = swap_exe_with(&exe, b"MZ new", |_, _| Err(std::io::Error::other("in use"))).unwrap_err();
        assert!(e.contains("could not move"), "{e}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ old");
        assert_eq!(listing(&d), ["Acme.exe", "provider"]);
        // no program there: refused before anything is written
        assert!(swap_exe(&d.join("Missing.exe"), b"MZ").is_err());
        assert_eq!(listing(&d), ["Acme.exe", "provider"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
