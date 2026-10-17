//! What setup does to the machine (users, ownership, systemd, the firewall), behind one trait
//! so the whole setup runs against a fake in tests.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The service account the relay runs as.
pub const USER: &str = "taro-relay";

pub trait System: Send + Sync {
    fn is_root(&self) -> bool;
    /// Run a program; its stdout, or an error with its stderr.
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
    fn has_command(&self, name: &str) -> bool;
    /// `chown user:group path`.
    fn chown(&self, path: &Path, user: &str, group: &str) -> Result<(), String>;
    /// Where systemd units go.
    fn units_dir(&self) -> PathBuf;
    fn systemd(&self) -> bool;
}

pub struct Real;

impl System for Real {
    fn is_root(&self) -> bool {
        #[cfg(unix)]
        unsafe {
            libc::geteuid() == 0
        }
        #[cfg(not(unix))]
        false
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(format!(
                "{program} {}: {}",
                args.join(" "),
                if err.is_empty() {
                    out.status.to_string()
                } else {
                    err
                }
            ))
        }
    }

    fn has_command(&self, name: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
            || ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
                .iter()
                .any(|d| Path::new(d).join(name).is_file())
    }

    fn chown(&self, path: &Path, user: &str, group: &str) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let (u, g) = (CString::new(user).unwrap(), CString::new(group).unwrap());
            let uid = unsafe { libc::getpwnam(u.as_ptr()) };
            let gid = unsafe { libc::getgrnam(g.as_ptr()) };
            if uid.is_null() || gid.is_null() {
                return Err(format!("no user {user} or group {group}"));
            }
            let p = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
            let r = unsafe { libc::chown(p.as_ptr(), (*uid).pw_uid, (*gid).gr_gid) };
            if r != 0 {
                return Err(format!(
                    "chown {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = (path, user, group);
            Err("not supported".into())
        }
    }

    fn units_dir(&self) -> PathBuf {
        "/etc/systemd/system".into()
    }

    fn systemd(&self) -> bool {
        Path::new("/run/systemd/system").is_dir()
    }
}

/// A machine that records what it was asked to do (tests).
pub struct Fake {
    pub root: bool,
    pub units: PathBuf,
    pub commands: Mutex<Vec<String>>,
    /// programs that are "installed"
    pub programs: Vec<String>,
    /// commands (prefix match) that fail
    pub failing: Mutex<Vec<String>>,
    /// the service account exists (set by `useradd`)
    pub user_exists: Mutex<bool>,
}

impl Fake {
    pub fn new(units: PathBuf) -> Fake {
        Fake {
            root: true,
            units,
            commands: Mutex::new(vec![]),
            programs: vec!["systemctl".into(), "useradd".into()],
            failing: Mutex::new(vec![]),
            user_exists: Mutex::new(false),
        }
    }

    pub fn log(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}

impl System for Fake {
    fn is_root(&self) -> bool {
        self.root
    }
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let line = format!("{program} {}", args.join(" "));
        self.commands.lock().unwrap().push(line.clone());
        if self
            .failing
            .lock()
            .unwrap()
            .iter()
            .any(|f| line.starts_with(f.as_str()))
        {
            return Err(format!("{line}: failed"));
        }
        // `id -u taro-relay` answers "no such user" until useradd ran
        if program == "useradd" {
            *self.user_exists.lock().unwrap() = true;
        }
        if program == "id" {
            return if *self.user_exists.lock().unwrap() {
                Ok("999\n".into())
            } else {
                Err("id: no such user".into())
            };
        }
        // nothing runs as a service here
        if line.starts_with("systemctl is-active") {
            return Err("inactive".into());
        }
        Ok(String::new())
    }
    fn has_command(&self, name: &str) -> bool {
        self.programs.iter().any(|p| p == name)
    }
    fn chown(&self, path: &Path, user: &str, group: &str) -> Result<(), String> {
        self.commands
            .lock()
            .unwrap()
            .push(format!("chown {user}:{group} {}", path.display()));
        Ok(())
    }
    fn units_dir(&self) -> PathBuf {
        self.units.clone()
    }
    fn systemd(&self) -> bool {
        true
    }
}

/// Free bytes on the file system holding `path` (its nearest existing parent).
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(p.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        Some(s.f_bavail as u64 * s.f_frsize as u64)
    }
    #[cfg(not(unix))]
    None
}
