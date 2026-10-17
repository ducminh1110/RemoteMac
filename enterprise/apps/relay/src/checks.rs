//! Wizard 2's checks: the machine, DNS and the network, the firewall. Each says what it found
//! and, when something is wrong, what to do about it.

use crate::system::{free_bytes, System};
use serde::Serialize;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use taro_update::policy::Layout;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub detail: String,
    /// what to do when it is not ok
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
}

fn c(
    id: &str,
    label: &str,
    status: Status,
    detail: impl Into<String>,
    fix: impl Into<String>,
) -> Check {
    // advice only for what needs it
    let fix = if status == Status::Ok {
        String::new()
    } else {
        fix.into()
    };
    Check {
        id: id.into(),
        label: label.into(),
        status,
        detail: detail.into(),
        fix,
    }
}

pub fn worst(checks: &[Check]) -> Status {
    if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Ok
    }
}

fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// Can `port` be listened on (TCP and UDP)? Err(why) otherwise.
pub fn port_free(port: u16, udp: bool) -> Result<(), String> {
    std::net::TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("TCP {port}: {e}"))?;
    if udp {
        std::net::UdpSocket::bind(("0.0.0.0", port)).map_err(|e| format!("UDP {port}: {e}"))?;
    }
    Ok(())
}

/// The machine: OS, architecture, privileges, systemd, disk, ports.
pub fn system(
    sys: &dyn System,
    layout: &Layout,
    relay_port: u16,
    need_web_ports: bool,
    ours_running: bool,
) -> Vec<Check> {
    let mut v = Vec::new();
    let os = os_name();
    v.push(c(
        "os",
        "Operating system",
        if std::env::consts::OS == "linux" {
            Status::Ok
        } else {
            Status::Fail
        },
        os,
        "The relay runs on Linux (Ubuntu 22.04+, Debian 12+, RHEL 9+ and similar).",
    ));
    let arch = std::env::consts::ARCH;
    let arch_ok = matches!(arch, "x86_64" | "aarch64");
    v.push(c(
        "arch",
        "CPU architecture",
        if arch_ok { Status::Ok } else { Status::Fail },
        arch,
        "Use the relay package built for this CPU (x86_64 or aarch64).",
    ));
    v.push(c(
        "root",
        "Administrator rights",
        if sys.is_root() {
            Status::Ok
        } else {
            Status::Fail
        },
        if sys.is_root() {
            "running as root"
        } else {
            "not root"
        },
        "Run setup with sudo: sudo taro-relay setup",
    ));
    let systemd = sys.systemd() && sys.has_command("systemctl");
    v.push(c(
        "systemd",
        "systemd",
        if systemd { Status::Ok } else { Status::Fail },
        if systemd { "available" } else { "not found" },
        "The relay is run as a systemd service; this system does not use systemd.",
    ));
    match free_bytes(&layout.state) {
        Some(b) if b < 50 << 20 => v.push(c(
            "disk",
            "Free disk space",
            Status::Fail,
            format!("{} MB free for {}", b >> 20, layout.state.display()),
            "Free at least 200 MB (logs, update backups).",
        )),
        Some(b) if b < 500 << 20 => v.push(c(
            "disk",
            "Free disk space",
            Status::Warn,
            format!("{} MB free", b >> 20),
            "500 MB or more is recommended for update backups.",
        )),
        Some(b) => v.push(c(
            "disk",
            "Free disk space",
            Status::Ok,
            format!("{} GB free", b >> 30),
            "",
        )),
        None => v.push(c(
            "disk",
            "Free disk space",
            Status::Warn,
            "could not be measured",
            "",
        )),
    }
    let mut ports = vec![(relay_port, true, "the relay (clients connect here)")];
    if need_web_ports {
        ports.push((80, false, "HTTP (ACME certificate checks, redirect)"));
        ports.push((443, false, "HTTPS (status page)"));
    }
    for (p, udp, what) in ports {
        let label = format!("Port {p}{}", if udp { " TCP+UDP" } else { "" });
        match port_free(p, udp) {
            Ok(()) => v.push(c(&format!("port-{p}"), &label, Status::Ok, format!("free for {what}"), "")),
            Err(_) if ours_running => v.push(c(&format!("port-{p}"), &label, Status::Ok, "in use by the running Taro relay (restarted when the settings are applied)", "")),
            Err(e) => v.push(c(&format!("port-{p}"), &label, Status::Fail, format!("in use: {e}"), format!("Find what uses it with `sudo ss -ltnup | grep :{p}` and stop it, or choose another port."))),
        }
    }
    v
}

pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// DNS and the network (real, or a fake in tests).
pub trait Net: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str) -> Fut<'a, Result<Vec<IpAddr>, String>>;
    /// This server's public addresses as the internet sees them.
    fn public_ips(&self) -> Fut<'_, Vec<IpAddr>>;
    /// An https GET answers (any status): the status.
    fn reach<'a>(&'a self, url: &'a str) -> Fut<'a, Result<u16, String>>;
}

pub struct RealNet {
    http: reqwest::Client,
}

impl RealNet {
    pub fn new() -> RealNet {
        RealNet {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .user_agent(concat!("taro-relay/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client"),
        }
    }
}

impl Default for RealNet {
    fn default() -> Self {
        Self::new()
    }
}

impl Net for RealNet {
    fn resolve<'a>(&'a self, host: &'a str) -> Fut<'a, Result<Vec<IpAddr>, String>> {
        Box::pin(async move {
            if let Ok(ip) = host.parse::<IpAddr>() {
                return Ok(vec![ip]);
            }
            let r = hickory_resolver::TokioResolver::builder_tokio()
                .and_then(|b| b.build())
                .map_err(|e| e.to_string())?;
            let fq = format!("{}.", host.trim_end_matches('.'));
            match r.lookup_ip(fq).await {
                Ok(l) => Ok(l.iter().collect()),
                Err(e) if e.is_no_records_found() || e.is_nx_domain() => Ok(vec![]),
                Err(e) => Err(e.to_string()),
            }
        })
    }

    fn public_ips(&self) -> Fut<'_, Vec<IpAddr>> {
        Box::pin(async move {
            let mut v = Vec::new();
            for u in ["https://api.ipify.org", "https://api6.ipify.org"] {
                if let Ok(r) = self.http.get(u).send().await {
                    if let Ok(t) = r.text().await {
                        if let Ok(ip) = t.trim().parse() {
                            v.push(ip);
                        }
                    }
                }
            }
            v
        })
    }

    fn reach<'a>(&'a self, url: &'a str) -> Fut<'a, Result<u16, String>> {
        Box::pin(async move {
            self.http
                .get(url)
                .send()
                .await
                .map(|r| r.status().as_u16())
                .map_err(|e| e.to_string())
        })
    }
}

fn private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])
        }
        IpAddr::V6(v) => {
            v.is_loopback()
                || (v.segments()[0] & 0xfe00) == 0xfc00
                || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// The relay host points at this server; the CA and the update source are reachable.
pub async fn network(
    net: &dyn Net,
    host: &str,
    acme_directory: Option<&str>,
    update_source: &str,
) -> Vec<Check> {
    let mut v = Vec::new();
    let public = net.public_ips().await;
    let public_txt = if public.is_empty() {
        "unknown".to_string()
    } else {
        public
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match net.resolve(host).await {
        Err(e) => v.push(c("dns", &format!("DNS for {host}"), Status::Fail, format!("lookup failed: {e}"), "Check the server's DNS resolver (/etc/resolv.conf).")),
        Ok(ips) if ips.is_empty() => v.push(c("dns", &format!("DNS for {host}"), Status::Fail, "no A or AAAA record", format!("Create an A record (and AAAA for IPv6) for {host} pointing at {public_txt}, wait for it to spread, then check again."))),
        Ok(ips) => {
            let list = ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
            if public.is_empty() {
                v.push(c("dns", &format!("DNS for {host}"), Status::Warn, format!("{host} -> {list}; this server's public address could not be found to compare"), "Make sure that address is this server's."));
            } else if ips.iter().any(|i| public.contains(i)) {
                v.push(c("dns", &format!("DNS for {host}"), Status::Ok, format!("{host} -> {list} (this server)"), ""));
            } else if ips.iter().all(private) {
                v.push(c("dns", &format!("DNS for {host}"), Status::Warn, format!("{host} -> {list}, a private address; this server is {public_txt} from outside"), "Fine for clients on this private network only; others need a public address."));
            } else {
                v.push(c("dns", &format!("DNS for {host}"), Status::Fail, format!("{host} -> {list}, but this server is {public_txt}"), format!("Change the record of {host} to {public_txt} (or run setup on the server it points to).")));
            }
        }
    }
    if let Some(dir) = acme_directory {
        match net.reach(dir).await {
            Ok(s) if s < 500 => v.push(c(
                "acme",
                "Certificate authority",
                Status::Ok,
                format!("{dir} answers"),
                "",
            )),
            Ok(s) => v.push(c(
                "acme",
                "Certificate authority",
                Status::Warn,
                format!("{dir} answered {s}"),
                "Try again later.",
            )),
            Err(e) => v.push(c(
                "acme",
                "Certificate authority",
                Status::Fail,
                format!("{dir}: {e}"),
                "Outbound HTTPS must be allowed for certificates.",
            )),
        }
    }
    match net.reach(update_source).await {
        Ok(_) => v.push(c(
            "updates",
            "Update source",
            Status::Ok,
            "GitHub releases answer",
            "",
        )),
        Err(e) => v.push(c(
            "updates",
            "Update source",
            Status::Warn,
            e,
            "Updates can still be applied from a downloaded enterprise-update.zip.",
        )),
    }
    v
}

/// The local firewall and the rules the relay needs.
#[derive(Clone, Debug, Serialize)]
pub struct Firewall {
    /// ufw | firewalld | iptables | none
    pub kind: String,
    pub active: bool,
    pub needed: Vec<String>,
    pub detail: String,
}

pub fn firewall(sys: &dyn System, relay_port: u16, web: bool) -> Firewall {
    let mut needed = vec![format!("{relay_port}/tcp"), format!("{relay_port}/udp")];
    if web {
        needed.push("80/tcp".into());
        needed.push("443/tcp".into());
    }
    if sys.has_command("ufw") {
        if let Ok(s) = sys.run("ufw", &["status"]) {
            let active = s.contains("Status: active");
            let missing: Vec<String> = needed
                .iter()
                .filter(|r| !s.contains(r.as_str()))
                .cloned()
                .collect();
            return Firewall {
                kind: "ufw".into(),
                active,
                detail: if !active {
                    "ufw is installed but inactive".into()
                } else if missing.is_empty() {
                    "all needed ports are allowed".into()
                } else {
                    format!("not allowed yet: {}", missing.join(", "))
                },
                needed,
            };
        }
    }
    if sys.has_command("firewall-cmd") && sys.run("firewall-cmd", &["--state"]).is_ok() {
        let s = sys
            .run("firewall-cmd", &["--list-ports"])
            .unwrap_or_default();
        let missing: Vec<String> = needed
            .iter()
            .filter(|r| !s.contains(r.as_str()))
            .cloned()
            .collect();
        return Firewall {
            kind: "firewalld".into(),
            active: true,
            detail: if missing.is_empty() {
                "all needed ports are open".into()
            } else {
                format!("not open yet: {}", missing.join(", "))
            },
            needed,
        };
    }
    if sys.has_command("iptables") {
        if let Ok(s) = sys.run("iptables", &["-S", "INPUT"]) {
            let closed =
                s.contains("-P INPUT DROP") || s.contains("-j REJECT") || s.contains("-j DROP");
            return Firewall {
                kind: "iptables".into(),
                active: closed,
                detail: if closed {
                    "the INPUT chain drops unknown traffic".into()
                } else {
                    "no blocking rules".into()
                },
                needed,
            };
        }
    }
    Firewall {
        kind: "none".into(),
        active: false,
        detail: "no local firewall found".into(),
        needed,
    }
}

/// Open the needed ports in the local firewall; what was done.
pub fn open_firewall(sys: &dyn System, fw: &Firewall) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    match fw.kind.as_str() {
        "ufw" if fw.active => {
            for r in &fw.needed {
                sys.run("ufw", &["allow", r])?;
                done.push(format!("ufw allow {r}"));
            }
        }
        "firewalld" => {
            for r in &fw.needed {
                sys.run("firewall-cmd", &["--permanent", &format!("--add-port={r}")])?;
                done.push(format!("firewall-cmd --permanent --add-port={r}"));
            }
            sys.run("firewall-cmd", &["--reload"])?;
        }
        "iptables" if fw.active => {
            for r in &fw.needed {
                let (port, proto) = r.split_once('/').unwrap_or((r, "tcp"));
                if sys
                    .run(
                        "iptables",
                        &["-C", "INPUT", "-p", proto, "--dport", port, "-j", "ACCEPT"],
                    )
                    .is_err()
                {
                    sys.run(
                        "iptables",
                        &["-I", "INPUT", "-p", proto, "--dport", port, "-j", "ACCEPT"],
                    )?;
                    done.push(format!(
                        "iptables -I INPUT -p {proto} --dport {port} -j ACCEPT"
                    ));
                }
            }
            if sys.has_command("netfilter-persistent") {
                let _ = sys.run("netfilter-persistent", &["save"]);
            }
        }
        _ => {}
    }
    Ok(done)
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::HashMap;

    pub struct FakeNet {
        pub records: HashMap<String, Vec<IpAddr>>,
        pub public: Vec<IpAddr>,
        pub down: Vec<String>,
    }

    impl Net for FakeNet {
        fn resolve<'a>(&'a self, host: &'a str) -> Fut<'a, Result<Vec<IpAddr>, String>> {
            Box::pin(async move { Ok(self.records.get(host).cloned().unwrap_or_default()) })
        }
        fn public_ips(&self) -> Fut<'_, Vec<IpAddr>> {
            Box::pin(async move { self.public.clone() })
        }
        fn reach<'a>(&'a self, url: &'a str) -> Fut<'a, Result<u16, String>> {
            Box::pin(async move {
                if self.down.iter().any(|d| url.contains(d.as_str())) {
                    Err("unreachable".into())
                } else {
                    Ok(200)
                }
            })
        }
    }

    #[tokio::test]
    async fn dns_is_compared_with_the_public_address() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let mut n = FakeNet {
            records: HashMap::from([("relay.acme.example".into(), vec![ip])]),
            public: vec![ip],
            down: vec![],
        };
        let v = network(
            &n,
            "relay.acme.example",
            Some("https://acme"),
            "https://api.github.com",
        )
        .await;
        assert_eq!(v[0].status, Status::Ok, "{:?}", v[0]);
        n.public = vec!["198.51.100.1".parse().unwrap()];
        let v = network(&n, "relay.acme.example", None, "https://api.github.com").await;
        assert_eq!(v[0].status, Status::Fail);
        assert!(v[0].fix.contains("198.51.100.1"));
        let v = network(&n, "missing.acme.example", None, "https://api.github.com").await;
        assert_eq!(v[0].status, Status::Fail);
        n.down = vec!["github".into()];
        let v = network(&n, "relay.acme.example", None, "https://api.github.com").await;
        assert_eq!(v.last().unwrap().status, Status::Warn);
    }
}
