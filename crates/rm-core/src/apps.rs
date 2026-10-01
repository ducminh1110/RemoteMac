//! Server-side application allowlist. The client may only name a registered
//! application id; arguments, cwd and env are validated here, and the result
//! is an argv vector — never a shell string.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppDescriptor {
    pub id: String,
    pub name: String,
    /// Absolute path to the executable (not a shell command line).
    pub executable: PathBuf,
    pub fixed_args: Vec<String>,
    /// Max number of extra client-supplied args.
    pub max_extra_args: usize,
    /// Env var names the client may set.
    pub allowed_env: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaunchError {
    #[error("unknown application '{0}'")]
    UnknownApp(String),
    #[error("too many arguments (max {0})")]
    TooManyArgs(usize),
    #[error("argument contains NUL or control characters")]
    BadArgument,
    #[error("working directory must be a relative path inside the session root")]
    BadWorkingDirectory,
    #[error("environment variable '{0}' not allowed")]
    EnvNotAllowed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedLaunch {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

pub struct AppRegistry {
    apps: Vec<AppDescriptor>,
    session_root: PathBuf,
}

impl AppRegistry {
    pub fn new(session_root: impl Into<PathBuf>, apps: Vec<AppDescriptor>) -> Self {
        Self { apps, session_root: session_root.into() }
    }

    /// Catalog for the first feasibility milestone: plain executables inside
    /// the system app bundles. Launching `Contents/MacOS/<bin>` directly keeps
    /// the control primitive a terminal-started process (see docs/SPEC.md §3).
    pub fn default_macos(session_root: impl Into<PathBuf>) -> Self {
        let app = |id: &str, name: &str, exe: &str, max_extra_args| AppDescriptor {
            id: id.into(),
            name: name.into(),
            executable: PathBuf::from(exe),
            fixed_args: vec![],
            max_extra_args,
            allowed_env: vec![],
        };
        Self::new(
            session_root,
            vec![
                app("textedit", "TextEdit", "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit", 4),
                app("xcode", "Xcode", "/Applications/Xcode.app/Contents/MacOS/Xcode", 4),
                app("simulator", "Simulator", "/Applications/Xcode.app/Contents/Developer/Applications/Simulator.app/Contents/MacOS/Simulator", 0),
            ],
        )
    }

    pub fn descriptors(&self) -> &[AppDescriptor] {
        &self.apps
    }

    pub fn validate(
        &self,
        id: &str,
        args: &[String],
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
    ) -> Result<ValidatedLaunch, LaunchError> {
        let app = self.apps.iter().find(|a| a.id == id).ok_or_else(|| LaunchError::UnknownApp(id.into()))?;
        if args.len() > app.max_extra_args {
            return Err(LaunchError::TooManyArgs(app.max_extra_args));
        }
        if args.iter().any(|a| a.chars().any(|c| c.is_control()) || a.len() > 4096) {
            return Err(LaunchError::BadArgument);
        }
        let cwd = match cwd {
            None => self.session_root.clone(),
            Some(c) => {
                let p = Path::new(c);
                let ok = !c.is_empty()
                    && p.is_relative()
                    && !c.contains('\0')
                    && p.components().all(|x| matches!(x, Component::Normal(_)));
                if !ok {
                    return Err(LaunchError::BadWorkingDirectory);
                }
                self.session_root.join(p)
            }
        };
        for k in env.keys() {
            if !app.allowed_env.contains(k) {
                return Err(LaunchError::EnvNotAllowed(k.clone()));
            }
        }
        let mut full = app.fixed_args.clone();
        full.extend(args.iter().cloned());
        Ok(ValidatedLaunch { executable: app.executable.clone(), args: full, cwd, env: env.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> AppRegistry {
        AppRegistry::default_macos("/Users/runner/session")
    }
    fn none() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn known_app_ok() {
        let v = reg().validate("textedit", &["a.txt".into()], None, &none()).unwrap();
        assert!(v.executable.ends_with("TextEdit"));
        assert_eq!(v.args, vec!["a.txt"]);
        assert_eq!(v.cwd, PathBuf::from("/Users/runner/session"));
    }

    #[test]
    fn unknown_app_and_shell_strings_rejected() {
        assert_eq!(reg().validate("rm -rf /", &[], None, &none()), Err(LaunchError::UnknownApp("rm -rf /".into())));
        assert!(matches!(reg().validate("/bin/sh", &[], None, &none()), Err(LaunchError::UnknownApp(_))));
    }

    #[test]
    fn args_are_data_not_shell() {
        // metacharacters are fine as argv entries (no shell is involved) ...
        assert!(reg().validate("textedit", &["; rm -rf ~".into()], None, &none()).is_ok());
        // ... but control chars are refused
        assert_eq!(reg().validate("textedit", &["a\nb".into()], None, &none()), Err(LaunchError::BadArgument));
        assert_eq!(reg().validate("textedit", &["a\0b".into()], None, &none()), Err(LaunchError::BadArgument));
        let many: Vec<String> = (0..5).map(|i| i.to_string()).collect();
        assert_eq!(reg().validate("textedit", &many, None, &none()), Err(LaunchError::TooManyArgs(4)));
    }

    #[test]
    fn cwd_traversal_rejected() {
        for bad in ["/etc", "../x", "a/../../x", "", "~/x"] {
            let r = reg().validate("textedit", &[], Some(bad), &none());
            // "~/x" is a normal relative component sequence ("~", "x"): contained, so allowed.
            if bad == "~/x" {
                assert!(r.is_ok());
            } else {
                assert_eq!(r, Err(LaunchError::BadWorkingDirectory), "{bad}");
            }
        }
        let ok = reg().validate("textedit", &[], Some("proj/sub"), &none()).unwrap();
        assert_eq!(ok.cwd, PathBuf::from("/Users/runner/session/proj/sub"));
    }

    #[test]
    fn env_must_be_allowlisted() {
        let mut e = none();
        e.insert("DYLD_INSERT_LIBRARIES".into(), "/tmp/evil.dylib".into());
        assert!(matches!(reg().validate("textedit", &[], None, &e), Err(LaunchError::EnvNotAllowed(_))));
    }
}
