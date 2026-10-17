//! Domain & TLS step: which DNS records to create, and real verification of them.
//!
//! TLS certificates are issued on the server by Caddy (on-demand TLS); the wizard only
//! verifies the outcome: DNS records resolve, the `_taro-verify` TXT value matches, and
//! `GET https://<domain>/healthz` answers 200 over a certificate the system trusts.

use serde::Serialize;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// DNS lookups used by the verification, behind a trait so tests can substitute answers.
pub trait DnsResolver: Send + Sync {
    fn txt<'a>(&'a self, name: &'a str) -> BoxFut<'a, Result<Vec<String>, String>>;
    fn ips<'a>(&'a self, name: &'a str) -> BoxFut<'a, Result<Vec<IpAddr>, String>>;
    fn cname<'a>(&'a self, name: &'a str) -> BoxFut<'a, Result<Vec<String>, String>>;
}

/// Real resolver (hickory), using the system DNS configuration and falling back to
/// Cloudflare's public resolvers if the system configuration cannot be read. A fresh
/// resolver is built per verification, so records added a minute ago are not hidden by a cache.
pub struct HickoryDns;

fn resolver() -> Result<hickory_resolver::TokioResolver, String> {
    use hickory_resolver::config::{ResolverConfig, CLOUDFLARE};
    use hickory_resolver::net::runtime::TokioRuntimeProvider;
    hickory_resolver::TokioResolver::builder_tokio()
        .and_then(|b| b.build())
        .or_else(|_| {
            hickory_resolver::Resolver::builder_with_config(
                ResolverConfig::udp_and_tcp(&CLOUDFLARE),
                TokioRuntimeProvider::default(),
            )
            .build()
        })
        .map_err(|e| e.to_string())
}

fn dns_err(e: hickory_resolver::net::NetError) -> String {
    if e.is_no_records_found() || e.is_nx_domain() {
        "no records found".into()
    } else {
        e.to_string()
    }
}

fn fq(name: &str) -> String {
    // Absolute name so the system search domains are not appended.
    format!("{}.", name.trim_end_matches('.'))
}

impl DnsResolver for HickoryDns {
    fn txt<'a>(&'a self, name: &'a str) -> BoxFut<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            use hickory_resolver::proto::rr::RData;
            let r = resolver()?.txt_lookup(fq(name)).await.map_err(dns_err)?;
            Ok(r.answers()
                .iter()
                .filter_map(|rec| match &rec.data {
                    RData::TXT(t) => Some(
                        t.txt_data
                            .iter()
                            .map(|p| String::from_utf8_lossy(p).into_owned())
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect())
        })
    }

    fn ips<'a>(&'a self, name: &'a str) -> BoxFut<'a, Result<Vec<IpAddr>, String>> {
        Box::pin(async move {
            let r = resolver()?.lookup_ip(fq(name)).await.map_err(dns_err)?;
            Ok(r.iter().collect())
        })
    }

    fn cname<'a>(&'a self, name: &'a str) -> BoxFut<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            use hickory_resolver::proto::rr::{RData, RecordType};
            let r = resolver()?
                .lookup(fq(name), RecordType::CNAME)
                .await
                .map_err(dns_err)?;
            Ok(r.answers()
                .iter()
                .filter_map(|rec| match &rec.data {
                    RData::CNAME(c) => Some(c.0.to_ascii().trim_end_matches('.').to_lowercase()),
                    _ => None,
                })
                .collect())
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// A subdomain of a Taro-managed zone (needs `TARO_SETUP_MANAGED_ZONE`).
    Managed,
    /// The customer's own domain.
    Custom,
}

#[derive(Clone, Debug, Serialize)]
pub struct DnsRecord {
    pub kind: &'static str,
    pub name: String,
    pub value: String,
    pub purpose: &'static str,
}

/// Server target: an IP address (A/AAAA record) or a host name (CNAME record).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Ip(IpAddr),
    Host(String),
}

pub fn parse_target(raw: &str) -> Result<Target, String> {
    let t = raw.trim();
    if let Ok(ip) = t.parse::<IpAddr>() {
        if ip.is_loopback() || ip.is_unspecified() {
            return Err("The server target must be the server's public IP address or host name, not a loopback address.".into());
        }
        return Ok(Target::Ip(ip));
    }
    crate::validate::fqdn(t)
        .map(Target::Host)
        .map_err(|e| format!("Server target: {e} Use the server's public IP or host name."))
}

pub const VERIFY_LABEL: &str = "_taro-verify";

pub fn verify_value(token: &str) -> String {
    format!("taro-verify={token}")
}

pub fn records(domain: &str, target: &Target, token: &str) -> Vec<DnsRecord> {
    let main = match target {
        Target::Ip(IpAddr::V4(ip)) => DnsRecord {
            kind: "A",
            name: domain.to_string(),
            value: ip.to_string(),
            purpose: "Points the portal domain at your server.",
        },
        Target::Ip(IpAddr::V6(ip)) => DnsRecord {
            kind: "AAAA",
            name: domain.to_string(),
            value: ip.to_string(),
            purpose: "Points the portal domain at your server.",
        },
        Target::Host(h) => DnsRecord {
            kind: "CNAME",
            name: domain.to_string(),
            value: h.clone(),
            purpose: "Points the portal domain at your server's host name.",
        },
    };
    vec![
        main,
        DnsRecord {
            kind: "TXT",
            name: format!("{VERIFY_LABEL}.{domain}"),
            value: verify_value(token),
            purpose: "Proves you control the domain. Can be removed after setup.",
        },
    ]
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct DomainVerification {
    pub dns_ok: bool,
    pub https_ok: bool,
    pub checks: Vec<Check>,
    pub at: u64,
}

pub async fn verify_dns(
    dns: &Arc<dyn DnsResolver>,
    domain: &str,
    target: &Target,
    token: &str,
) -> Vec<Check> {
    let mut out = Vec::new();
    let txt_name = format!("{VERIFY_LABEL}.{domain}");
    let want = verify_value(token);
    match dns.txt(&txt_name).await {
        Ok(vals) if vals.iter().any(|v| v.trim() == want) => out.push(Check {
            name: format!("TXT {txt_name}"),
            ok: true,
            detail: "verification value found".into(),
            hint: None,
        }),
        Ok(vals) => out.push(Check {
            name: format!("TXT {txt_name}"),
            ok: false,
            detail: if vals.is_empty() {
                "no TXT value".into()
            } else {
                format!("found {} value(s), none match", vals.len())
            },
            hint: Some(format!(
                "Create a TXT record named {txt_name} with the value {want}. DNS changes can take a few minutes to propagate."
            )),
        }),
        Err(e) => out.push(Check {
            name: format!("TXT {txt_name}"),
            ok: false,
            detail: e,
            hint: Some(format!(
                "Create a TXT record named {txt_name} with the value {want}, then verify again."
            )),
        }),
    }

    match target {
        Target::Ip(ip) => {
            let kind = if ip.is_ipv4() { "A" } else { "AAAA" };
            match dns.ips(domain).await {
                Ok(ips) if ips.contains(ip) => out.push(Check {
                    name: format!("{kind} {domain}"),
                    ok: true,
                    detail: format!("resolves to {ip}"),
                    hint: None,
                }),
                Ok(ips) => out.push(Check {
                    name: format!("{kind} {domain}"),
                    ok: false,
                    detail: if ips.is_empty() {
                        "does not resolve".into()
                    } else {
                        format!(
                            "resolves to {}",
                            ips.iter()
                                .map(|i| i.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    },
                    hint: Some(format!("Set the {kind} record of {domain} to {ip}.")),
                }),
                Err(e) => out.push(Check {
                    name: format!("{kind} {domain}"),
                    ok: false,
                    detail: e,
                    hint: Some(format!(
                        "Create an {kind} record for {domain} pointing to {ip}."
                    )),
                }),
            }
        }
        Target::Host(h) => {
            let cname = dns.cname(domain).await;
            let ok_cname = matches!(&cname, Ok(v) if v.iter().any(|c| c == h));
            if ok_cname {
                out.push(Check {
                    name: format!("CNAME {domain}"),
                    ok: true,
                    detail: format!("points to {h}"),
                    hint: None,
                });
            } else {
                // Flattened CNAMEs (ALIAS/ANAME) show up as A records: accept if the address
                // sets match.
                let a = dns.ips(domain).await.unwrap_or_default();
                let b = dns.ips(h).await.unwrap_or_default();
                let flat = !a.is_empty() && a.iter().all(|ip| b.contains(ip));
                out.push(Check {
                    name: format!("CNAME {domain}"),
                    ok: flat,
                    detail: if flat {
                        format!("no CNAME, but resolves to the same addresses as {h} (flattened)")
                    } else {
                        match cname {
                            Ok(v) if !v.is_empty() => format!("points to {}", v.join(", ")),
                            Ok(_) => "no CNAME record".into(),
                            Err(e) => e,
                        }
                    },
                    hint: (!flat)
                        .then(|| format!("Create a CNAME record for {domain} pointing to {h}.")),
                });
            }
        }
    }
    out
}

/// HTTPS health check: `GET <url>` must answer 200 with a certificate the system trusts.
pub async fn check_health(http: &reqwest::Client, url: &str, name: &str) -> Check {
    match http.get(url).send().await {
        Ok(r) if r.status().as_u16() == 200 => Check {
            name: name.into(),
            ok: true,
            detail: format!("GET {url} -> 200"),
            hint: None,
        },
        Ok(r) => Check {
            name: name.into(),
            ok: false,
            detail: format!("GET {url} -> {}", r.status()),
            hint: Some("The server answered but not with 200. Check that the Taro stack is running (docker compose ps) and that Caddy proxies to the portal.".into()),
        },
        Err(e) => {
            let s = format!("{:?}", e);
            let tls = s.contains("certificate") || s.contains("Certificate") || s.contains("tls");
            let timeout = e.is_timeout();
            Check {
                name: name.into(),
                ok: false,
                detail: format!("GET {url} failed: {}", e.without_url()),
                hint: Some(if tls {
                    "TLS failed. Caddy issues the certificate on the first request once DNS points at the server; ports 80 and 443 must be reachable from the internet. Wait a minute and verify again.".into()
                } else if timeout {
                    "Timed out. Check the server firewall allows ports 80/443 and that DNS points at the right server.".into()
                } else {
                    "Could not connect. Check that DNS points at the server and the Taro stack (Caddy) is running.".into()
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    pub struct FakeDns {
        pub txt: HashMap<String, Vec<String>>,
        pub ips: HashMap<String, Vec<IpAddr>>,
        pub cname: HashMap<String, Vec<String>>,
    }

    impl DnsResolver for FakeDns {
        fn txt<'a>(&'a self, n: &'a str) -> BoxFut<'a, Result<Vec<String>, String>> {
            Box::pin(async move { self.txt.get(n).cloned().ok_or("no records found".into()) })
        }
        fn ips<'a>(&'a self, n: &'a str) -> BoxFut<'a, Result<Vec<IpAddr>, String>> {
            Box::pin(async move { self.ips.get(n).cloned().ok_or("no records found".into()) })
        }
        fn cname<'a>(&'a self, n: &'a str) -> BoxFut<'a, Result<Vec<String>, String>> {
            Box::pin(async move { self.cname.get(n).cloned().ok_or("no records found".into()) })
        }
    }

    #[tokio::test]
    async fn verify_ok_and_mismatch() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let mut f = FakeDns {
            txt: HashMap::new(),
            ips: HashMap::new(),
            cname: HashMap::new(),
        };
        f.txt.insert(
            "_taro-verify.portal.example.com".into(),
            vec!["taro-verify=abc123".into()],
        );
        f.ips.insert("portal.example.com".into(), vec![ip]);
        let dns: Arc<dyn DnsResolver> = Arc::new(f);
        let ok = verify_dns(&dns, "portal.example.com", &Target::Ip(ip), "abc123").await;
        assert!(ok.iter().all(|c| c.ok), "{ok:?}");
        let bad = verify_dns(&dns, "portal.example.com", &Target::Ip(ip), "zzz").await;
        assert!(!bad[0].ok);
        let other: IpAddr = "198.51.100.1".parse().unwrap();
        let bad = verify_dns(&dns, "portal.example.com", &Target::Ip(other), "abc123").await;
        assert!(!bad[1].ok);
    }

    #[tokio::test]
    async fn cname_and_flattened() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let mut f = FakeDns {
            txt: HashMap::new(),
            ips: HashMap::new(),
            cname: HashMap::new(),
        };
        f.cname
            .insert("a.example.com".into(), vec!["srv.host.net".into()]);
        f.ips.insert("b.example.com".into(), vec![ip]);
        f.ips.insert("srv.host.net".into(), vec![ip]);
        let dns: Arc<dyn DnsResolver> = Arc::new(f);
        let t = Target::Host("srv.host.net".into());
        assert!(verify_dns(&dns, "a.example.com", &t, "x").await[1].ok);
        assert!(verify_dns(&dns, "b.example.com", &t, "x").await[1].ok);
        assert!(!verify_dns(&dns, "c.example.com", &t, "x").await[1].ok);
    }

    #[test]
    fn record_list() {
        let r = records(
            "portal.example.com",
            &parse_target("2001:db8::1").unwrap(),
            "tok",
        );
        assert_eq!(r[0].kind, "AAAA");
        assert_eq!(r[1].name, "_taro-verify.portal.example.com");
        assert_eq!(r[1].value, "taro-verify=tok");
        assert!(parse_target("127.0.0.1").is_err());
        assert_eq!(
            parse_target("Srv.Host.net").unwrap(),
            Target::Host("srv.host.net".into())
        );
    }
}
