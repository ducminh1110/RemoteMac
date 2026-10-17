//! The relay's machine settings, `/etc/taro/runtime/relay.toml`, written by Wizard 2 (`taro-relay
//! setup`). They are this server's, not the provider's: the provider's settings (product, the
//! intended relay host, the admission key) stay in `/etc/taro/provider/provider.json`, untouched.
//!
//! Rewriting keeps what the operator added (comments, settings this version does not know).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use taro_update::policy::Layout;

pub const FILE: &str = "relay.toml";
pub const SCHEMA: &str = "taro.relay-runtime/v1";
/// The loopback-only health endpoint (the updater and `taro-relay health` ask it).
pub const HEALTH_LISTEN: &str = "127.0.0.1:7479";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// a certificate from an ACME CA (Let's Encrypt), renewed by the relay itself
    #[default]
    Acme,
    /// certificate files managed elsewhere
    Files,
    /// no HTTPS status page (the relay protocol is end-to-end encrypted by the clients anyway)
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// only when the operator runs `taro-relay update`
    #[default]
    Manual,
    /// look daily; tell (status page, `update status`), never install
    Notify,
    /// look daily; install inside the maintenance window
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relay {
    /// where clients connect (TCP and UDP)
    pub listen: String,
    /// the DNS name clients use (checked to point at this server)
    pub public_host: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tls {
    pub mode: TlsMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub directory: String,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub https_listen: String,
    pub http_listen: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Updates {
    pub mode: UpdateMode,
    /// `Sun 03:00-05:00` or `daily 02:00-04:00`, UTC; only for `auto`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// GitHub's releases API of the repository Enterprise releases come from
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runtime {
    pub schema: String,
    pub configured_at: String,
    pub relay: Relay,
    pub tls: Tls,
    pub updates: Updates,
}

pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
pub const RELEASES: &str = "https://api.github.com/repos/ducminh1110/RemoteMac/releases";

impl Runtime {
    /// The defaults for a first setup.
    pub fn defaults(layout: &Layout, public_host: &str, port: u16) -> Runtime {
        Runtime {
            schema: SCHEMA.into(),
            configured_at: String::new(),
            relay: Relay {
                listen: format!("0.0.0.0:{port}"),
                public_host: public_host.into(),
            },
            tls: Tls {
                mode: TlsMode::Acme,
                email: None,
                directory: LETS_ENCRYPT.into(),
                cert: layout.state.join("tls/fullchain.pem"),
                key: layout.state.join("tls/privkey.pem"),
                https_listen: "0.0.0.0:443".into(),
                http_listen: "0.0.0.0:80".into(),
            },
            updates: Updates {
                mode: UpdateMode::Manual,
                window: None,
                source: RELEASES.into(),
            },
        }
    }

    pub fn path(layout: &Layout) -> PathBuf {
        layout.runtime.join(FILE)
    }

    /// The settings, if setup has been completed.
    pub fn load(layout: &Layout) -> Result<Option<Runtime>, String> {
        let p = Self::path(layout);
        match std::fs::read_to_string(&p) {
            Ok(s) => {
                let r: Runtime = toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
                if r.schema != SCHEMA {
                    return Err(format!(
                        "{}: schema {} is not {SCHEMA}",
                        p.display(),
                        r.schema
                    ));
                }
                r.check().map_err(|e| format!("{}: {e}", p.display()))?;
                Ok(Some(r))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    pub fn check(&self) -> Result<(), String> {
        for (what, a) in [
            ("relay.listen", &self.relay.listen),
            ("tls.https_listen", &self.tls.https_listen),
            ("tls.http_listen", &self.tls.http_listen),
        ] {
            a.parse::<std::net::SocketAddr>()
                .map_err(|_| format!("{what}: {a} is not an address:port"))?;
        }
        if !rm_brand::valid_host(&self.relay.public_host) {
            return Err(format!(
                "relay.public_host: {} is not a host name",
                self.relay.public_host
            ));
        }
        if self.tls.mode == TlsMode::Acme
            && self.tls.email.as_deref().is_none_or(|e| !e.contains('@'))
        {
            return Err("tls.email: an email address is needed for ACME".into());
        }
        if let Some(w) = &self.updates.window {
            Window::parse(w)?;
        }
        if self.updates.mode == UpdateMode::Auto && self.updates.window.is_none() {
            return Err("updates.window: automatic updates need a maintenance window".into());
        }
        if !self.updates.source.starts_with("https://") {
            return Err("updates.source: must be https".into());
        }
        Ok(())
    }

    pub fn relay_port(&self) -> u16 {
        self.relay
            .listen
            .parse::<std::net::SocketAddr>()
            .map(|a| a.port())
            .unwrap_or(7470)
    }

    /// Write, keeping the operator's comments and unknown settings; the previous file is kept
    /// as `relay.toml.<stamp>.bak`. Returns the backup's path.
    pub fn save(&self, layout: &Layout, stamp: &str) -> Result<Option<PathBuf>, String> {
        self.check()?;
        let p = Self::path(layout);
        std::fs::create_dir_all(&layout.runtime)
            .map_err(|e| format!("{}: {e}", layout.runtime.display()))?;
        let old = std::fs::read_to_string(&p).ok();
        let mut doc: toml_edit::DocumentMut = match &old {
            Some(s) => s.parse().map_err(|e| format!("{}: {e}", p.display()))?,
            None => HEADER.parse().expect("header parses"),
        };
        let fresh: toml_edit::DocumentMut = toml::to_string(self)
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|e: toml_edit::TomlError| e.to_string())?;
        merge(doc.as_table_mut(), fresh.as_table());
        if self.tls.email.is_none() {
            if let Some(t) = doc.get_mut("tls").and_then(|t| t.as_table_like_mut()) {
                t.remove("email");
            }
        }
        if self.updates.window.is_none() {
            if let Some(t) = doc.get_mut("updates").and_then(|t| t.as_table_like_mut()) {
                t.remove("window");
            }
        }
        let backup = match &old {
            Some(s) if s != &doc.to_string() => {
                let b = layout.runtime.join(format!("{FILE}.{stamp}.bak"));
                std::fs::write(&b, s).map_err(|e| format!("{}: {e}", b.display()))?;
                Some(b)
            }
            _ => None,
        };
        taro_update::migrate::write_atomic(&p, doc.to_string().as_bytes())?;
        Ok(backup)
    }
}

const HEADER: &str = "# Taro relay: this server's settings, written by `sudo taro-relay setup` (Wizard 2).\n# Run it again to change them; your own comments and extra settings are kept.\n# The provider's settings (product, relay host, admission key) are in /etc/taro/provider/.\n";

fn merge(into: &mut toml_edit::Table, from: &toml_edit::Table) {
    for (k, v) in from.iter() {
        match (into.get_mut(k), v) {
            (Some(toml_edit::Item::Table(dst)), toml_edit::Item::Table(src)) => merge(dst, src),
            (Some(dst @ toml_edit::Item::Value(_)), toml_edit::Item::Value(src)) => {
                // keep the comment decoration of the operator's line
                let decor = dst.as_value().map(|d| d.decor().clone());
                let mut nv = src.clone();
                if let Some(d) = decor {
                    *nv.decor_mut() = d;
                }
                *dst = toml_edit::Item::Value(nv);
            }
            _ => {
                into.insert(k, v.clone());
            }
        }
    }
}

/// A weekly or daily maintenance window, UTC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// 0 = Monday … 6 = Sunday; none: every day
    pub day: Option<u8>,
    pub start_min: u32,
    pub end_min: u32,
}

impl Window {
    pub fn parse(s: &str) -> Result<Window, String> {
        let bad = || format!("\"{s}\": write it as `Sun 03:00-05:00` or `daily 02:00-04:00` (UTC)");
        let s = s.trim().trim_end_matches("UTC").trim();
        let (d, times) = s.split_once(' ').ok_or_else(bad)?;
        let day = match d.to_ascii_lowercase().as_str() {
            "daily" => None,
            "mon" => Some(0),
            "tue" => Some(1),
            "wed" => Some(2),
            "thu" => Some(3),
            "fri" => Some(4),
            "sat" => Some(5),
            "sun" => Some(6),
            _ => return Err(bad()),
        };
        let (a, b) = times.trim().split_once('-').ok_or_else(bad)?;
        let hm = |t: &str| -> Option<u32> {
            let (h, m) = t.trim().split_once(':')?;
            let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
            (h < 24 && m < 60).then_some(h * 60 + m)
        };
        let (start_min, end_min) = (hm(a).ok_or_else(bad)?, hm(b).ok_or_else(bad)?);
        if start_min == end_min {
            return Err(bad());
        }
        Ok(Window {
            day,
            start_min,
            end_min,
        })
    }

    /// `unix` seconds (UTC) fall inside the window (a window may cross midnight).
    pub fn contains(&self, unix: u64) -> bool {
        let days = unix / 86400;
        let minute = ((unix % 86400) / 60) as u32;
        // 1970-01-01 was a Thursday (3)
        let weekday = ((days + 3) % 7) as u8;
        let in_range = |m: u32| {
            if self.start_min < self.end_min {
                m >= self.start_min && m < self.end_min
            } else {
                m >= self.start_min || m < self.end_min
            }
        };
        match self.day {
            None => in_range(minute),
            Some(d) if self.start_min < self.end_min => weekday == d && in_range(minute),
            // crossing midnight: the evening on the day, the morning after
            Some(d) => {
                (weekday == d && minute >= self.start_min)
                    || (weekday == (d + 1) % 7 && minute < self.end_min)
            }
        }
    }
}

/// Owner-only or group-readable modes on Unix.
pub fn set_mode(p: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let _ = (p, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows() {
        let w = Window::parse("Sun 03:00-05:00 UTC").unwrap();
        // 2026-10-18 is a Sunday
        let midnight = 1_792_281_600u64; // 2026-10-18T00:00Z
        let sunday_4am = midnight + 4 * 3600;
        assert_eq!((midnight / 86400 + 3) % 7, 6, "fixture is a Sunday");
        assert!(w.contains(sunday_4am));
        assert!(!w.contains(midnight + 6 * 3600));
        assert!(!w.contains(sunday_4am + 86400));
        let n = Window::parse("daily 23:00-01:00").unwrap();
        assert!(
            n.contains(midnight + 30 * 60)
                && n.contains(midnight + 23 * 3600 + 5 * 60)
                && !n.contains(midnight + 12 * 3600)
        );
        for bad in [
            "sometime",
            "Sun 3-5",
            "Sun 25:00-26:00",
            "Fun 01:00-02:00",
            "daily 02:00-02:00",
        ] {
            assert!(Window::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn saving_keeps_the_operators_lines() {
        let d = std::env::temp_dir().join(format!("taro-relay-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let layout = Layout::under(&d);
        let mut r = Runtime::defaults(&layout, "relay.acme.example", 7470);
        r.tls.email = Some("ops@acme.example".into());
        assert!(r.save(&layout, "1").unwrap().is_none());
        let p = Runtime::path(&layout);
        let edited = std::fs::read_to_string(&p)
            .unwrap()
            .replace("[updates]", "# my note\n[updates]\nextra = \"kept\"");
        std::fs::write(&p, edited).unwrap();
        r.updates.mode = UpdateMode::Notify;
        let backup = r.save(&layout, "2").unwrap().expect("backed up");
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(
            t.contains("# my note")
                && t.contains("extra = \"kept\"")
                && t.contains("mode = \"notify\""),
            "{t}"
        );
        assert!(std::fs::read_to_string(backup)
            .unwrap()
            .contains("mode = \"manual\""));
        assert_eq!(
            Runtime::load(&layout).unwrap().unwrap().updates.mode,
            UpdateMode::Notify
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn checks() {
        let layout = Layout::under(Path::new("/x"));
        let mut r = Runtime::defaults(&layout, "relay.acme.example", 7470);
        assert!(r.check().is_err(), "ACME needs an email");
        r.tls.email = Some("a@b.c".into());
        assert!(r.check().is_ok());
        r.updates.mode = UpdateMode::Auto;
        assert!(r.check().is_err(), "auto needs a window");
        r.updates.window = Some("daily 02:00-04:00".into());
        assert!(r.check().is_ok());
        r.relay.listen = "7470".into();
        assert!(r.check().is_err());
    }
}
