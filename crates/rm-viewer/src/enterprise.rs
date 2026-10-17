//! Links from a Taro Enterprise portal: `taro://connect?server=<portal>&grant=<grant>`. The
//! grant is redeemed once at the portal (`POST <server>/api/v1/connect/redeem`) for the rented
//! Mac's Taro ID, relay and password; Taro then connects as it would with an ID and password
//! typed in. Portable (the HTTPS request itself is Windows-only, in update.rs).

/// What a link asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// the portal's origin (https, or http on this computer for development)
    pub server: String,
    pub grant: String,
}

/// How to reach the Mac, from the portal.
#[derive(Clone, PartialEq, Eq)]
pub struct Mac {
    pub taro_id: String,
    pub relay: Option<String>,
    pub password: String,
    pub name: String,
}

impl std::fmt::Debug for Mac {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Mac").field("taro_id", &self.taro_id).field("relay", &self.relay).field("password", &"<redacted>").field("name", &self.name).finish()
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// A portal origin Taro may talk to: https anywhere, http only on this computer.
fn valid_server(s: &str) -> bool {
    let (scheme, rest) = match s.split_once("://") {
        Some(x) => x,
        None => return false,
    };
    let host = rest.split('/').next().unwrap_or("");
    let name = host.rsplit_once(':').map_or(host, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    let plain_ok = matches!(name, "127.0.0.1" | "localhost" | "[::1]");
    !name.is_empty() && !name.contains('@') && rest.split('/').skip(1).all(|p| p.is_empty()) && (scheme == "https" || (scheme == "http" && plain_ok))
}

/// Read a `taro://connect?...` link.
pub fn parse_link(url: &str) -> Result<Link, String> {
    let rest = url.trim().strip_prefix("taro://").ok_or("not a Taro link")?;
    let (what, query) = rest.split_once('?').ok_or("the link has no details")?;
    if what.trim_end_matches('/') != "connect" {
        return Err(format!("Taro does not know what to do with {what:?} links"));
    }
    let (mut server, mut grant) = (None, None);
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        match k {
            "server" => server = percent_decode(v),
            "grant" => grant = percent_decode(v),
            _ => {}
        }
    }
    let server = server.ok_or("the link names no portal")?.trim_end_matches('/').to_string();
    let grant = grant.ok_or("the link has no grant")?;
    if !valid_server(&server) {
        return Err("the link's portal address is not a secure (https) address".into());
    }
    if grant.len() > 200 || !grant.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        return Err("the link's grant is not valid".into());
    }
    Ok(Link { server, grant })
}

/// The portal's answer to a redeem (or why it refused).
pub fn parse_reply(status: u16, body: &[u8]) -> Result<Mac, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| format!("the portal answered {status} with something that is not JSON"))?;
    if status != 200 {
        let m = v.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("the portal refused the link");
        return Err(m.to_string());
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let taro_id = s("taro_id").and_then(|i| rm_protocol::session::normalize_id(&i)).ok_or("the portal gave no valid Taro ID")?;
    let password = s("password").filter(|p| !p.is_empty()).ok_or("the portal gave no password")?;
    Ok(Mac { taro_id, relay: s("relay").filter(|r| !r.is_empty()), password, name: s("mac_name").unwrap_or_else(|| "Rented Mac".into()) })
}

/// Redeem a link's grant (once).
#[cfg(windows)]
pub fn redeem(link: &Link) -> Result<Mac, String> {
    let body = serde_json::json!({ "grant": link.grant, "client": format!("Taro {}", env!("CARGO_PKG_VERSION")) }).to_string();
    let (status, reply) = crate::update::http::post_json(&format!("{}/api/v1/connect/redeem", link.server), &body)?;
    parse_reply(status, &reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let l = parse_link("taro://connect?server=https%3A%2F%2Fportal.example.com&grant=0f8fad5b-d9cb-469f-a165-70867728950e.AbC_d-9").unwrap();
        assert_eq!(l.server, "https://portal.example.com");
        assert_eq!(l.grant, "0f8fad5b-d9cb-469f-a165-70867728950e.AbC_d-9");
        assert_eq!(parse_link("taro://connect?server=http%3A%2F%2F127.0.0.1%3A18080&grant=a.b").unwrap().server, "http://127.0.0.1:18080");
        for bad in [
            "https://portal.example.com",
            "taro://connect",
            "taro://delete?server=https%3A%2F%2Fa.b&grant=a.b",
            "taro://connect?server=http%3A%2F%2Fportal.example.com&grant=a.b",
            "taro://connect?server=https%3A%2F%2Fuser%40evil.example&grant=a.b",
            "taro://connect?server=https%3A%2F%2Fa.b%2Fpath&grant=a.b",
            "taro://connect?server=https%3A%2F%2Fa.b&grant=a%22b",
            "taro://connect?grant=a.b",
            "taro://connect?server=https%3A%2F%2Fa.b&grant=a%2",
        ] {
            assert!(parse_link(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn replies() {
        let ok = br#"{"taro_id":"912 345 678","relay":null,"password":"pw","mac_name":"Mac mini 001","rental_ends_at":null}"#;
        let m = parse_reply(200, ok).unwrap();
        assert_eq!((m.taro_id.as_str(), m.relay.as_deref(), m.name.as_str()), ("912345678", None, "Mac mini 001"));
        assert!(!format!("{m:?}").contains("\"pw\""));
        let refused = br#"{"error":{"code":"invalid","message":"This connection link is not valid any more.","request_id":"x"}}"#;
        assert_eq!(parse_reply(422, refused).unwrap_err(), "This connection link is not valid any more.");
        assert!(parse_reply(200, br#"{"taro_id":"12","password":"x"}"#).is_err());
        assert!(parse_reply(502, b"<html>").is_err());
    }
}
