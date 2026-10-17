//! Taro Enterprise, the provider's portal (provider.json's `account.portal`):
//!
//! - links from it, `taro://connect?server=<portal>&grant=<grant>`: the grant is redeemed once at
//!   the portal (`POST <server>/api/v1/connect/redeem`) for the rented Mac's Taro ID, relay and
//!   password; Taro then connects as it would with an ID and password typed in;
//! - signing in to it from Taro (a device code: Taro shows a code and opens the portal, the user
//!   approves it there, Taro gets a token), then the Macs the user rents ("My Macs"), each one a
//!   click away (`POST /api/v1/device/macs/{id}/connect` gives its ID, relay and password).
//!
//! Portable (the HTTPS requests themselves are Windows-only, in update.rs).

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

/// Signing in: what Taro shows (the code) and where the user approves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    /// the portal's page for this code (opened in the browser)
    pub url: String,
    pub interval: u64,
}

/// Where a sign-in is.
#[derive(Clone, PartialEq, Eq)]
pub enum Poll {
    Pending,
    SlowDown,
    Approved { token: String, email: String },
}

impl std::fmt::Debug for Poll {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Poll::Pending => f.write_str("Pending"),
            Poll::SlowDown => f.write_str("SlowDown"),
            Poll::Approved { email, .. } => write!(f, "Approved({email}, <token>)"),
        }
    }
}

/// A Mac the signed-in user rents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rented {
    pub rental: String,
    pub name: String,
    /// "M4 · 1 day · Acme Studio"
    pub detail: String,
    /// it can be connected to now
    pub ready: bool,
    /// why not, when it is not ("Being prepared")
    pub state: String,
}

/// The token stopped working (signed out in the portal, revoked, expired).
pub const SIGNED_OUT: &str = "You were signed out. Sign in again.";

fn api_error(status: u16, v: &serde_json::Value, what: &str) -> String {
    if status == 401 {
        return SIGNED_OUT.into();
    }
    v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from).unwrap_or_else(|| format!("the portal answered {status} to {what}"))
}

fn json_of(status: u16, body: &[u8]) -> Result<serde_json::Value, String> {
    serde_json::from_slice(body).map_err(|_| format!("the portal answered {status} with something that is not JSON"))
}

pub fn parse_start(status: u16, body: &[u8], portal: &str) -> Result<DeviceStart, String> {
    let v = json_of(status, body)?;
    if status != 201 && status != 200 {
        return Err(api_error(status, &v, "the sign-in"));
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let device_code = s("device_code").filter(|c| !c.is_empty()).ok_or("the portal gave no sign-in code")?;
    let user_code = s("user_code").filter(|c| c.len() <= 16 && c.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).ok_or("the portal gave no valid code")?;
    // the page to open must be the portal's own
    let url = s("verification_uri_complete").filter(|u| u.starts_with(&format!("{portal}/"))).unwrap_or_else(|| format!("{portal}/device?code={user_code}"));
    let interval = v.get("interval").and_then(|i| i.as_u64()).unwrap_or(5).clamp(2, 60);
    Ok(DeviceStart { device_code, user_code, url, interval })
}

pub fn parse_poll(status: u16, body: &[u8]) -> Result<Poll, String> {
    let v = json_of(status, body)?;
    match (status, v.get("status").and_then(|s| s.as_str())) {
        (202, Some("slow_down")) => Ok(Poll::SlowDown),
        (202, _) => Ok(Poll::Pending),
        (200, Some("approved")) => {
            let token = v.get("access_token").and_then(|t| t.as_str()).filter(|t| t.starts_with("tdv_")).ok_or("the portal gave no token")?;
            Ok(Poll::Approved { token: token.into(), email: v.get("email").and_then(|e| e.as_str()).unwrap_or("").into() })
        }
        _ => Err(api_error(status, &v, "the sign-in")),
    }
}

fn state_words(status: &str) -> &'static str {
    match status {
        "ready" => "Ready",
        "pending_payment" | "draft" => "Waiting for payment",
        "paid" | "provisioning" => "Being prepared",
        "suspended" => "Suspended",
        "cancel_pending" => "Ending",
        _ => "Not available",
    }
}

pub fn parse_macs(status: u16, body: &[u8]) -> Result<Vec<Rented>, String> {
    let v = json_of(status, body)?;
    if status != 200 {
        return Err(api_error(status, &v, "My Macs"));
    }
    let list = v.get("macs").and_then(|m| m.as_array()).ok_or("the portal gave no list of Macs")?;
    Ok(list
        .iter()
        .filter_map(|m| {
            let s = |k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let rental = s("rental_id");
            if rental.is_empty() || !rental.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return None;
            }
            let name = Some(s("mac_name")).filter(|n| !n.is_empty()).unwrap_or_else(|| s("plan"));
            let detail = [s("model"), s("plan"), s("organization")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
            let st = s("status");
            let ready = m.get("connectable").and_then(|c| c.as_bool()).unwrap_or(false);
            let state = if ready { "Ready".into() } else if st == "ready" { "Starting up".into() } else { state_words(&st).into() };
            Some(Rented { rental, name, detail, ready, state })
        })
        .collect())
}

/// The token Taro keeps for the portal (protected for this Windows user, connect.rs).
#[cfg(windows)]
pub mod device {
    use super::*;
    use crate::update::http::call;

    fn client() -> String {
        format!("{} {} on Windows", crate::brand::name(), env!("CARGO_PKG_VERSION"))
    }

    pub fn start(portal: &str) -> Result<DeviceStart, String> {
        let body = serde_json::json!({ "client": client() }).to_string();
        let (st, b) = call("POST", &format!("{portal}/api/v1/devices/authorize"), None, Some(&body))?;
        parse_start(st, &b, portal)
    }

    pub fn poll(portal: &str, device_code: &str) -> Result<Poll, String> {
        let body = serde_json::json!({ "device_code": device_code }).to_string();
        let (st, b) = call("POST", &format!("{portal}/api/v1/devices/token"), None, Some(&body))?;
        parse_poll(st, &b)
    }

    pub fn macs(portal: &str, token: &str) -> Result<Vec<Rented>, String> {
        let (st, b) = call("GET", &format!("{portal}/api/v1/device/macs"), Some(token), None)?;
        parse_macs(st, &b)
    }

    pub fn connect(portal: &str, token: &str, rental: &str) -> Result<Mac, String> {
        let (st, b) = call("POST", &format!("{portal}/api/v1/device/macs/{rental}/connect"), Some(token), Some("{}"))?;
        if st == 401 {
            return Err(SIGNED_OUT.into());
        }
        parse_reply(st, &b)
    }

    pub fn sign_out(portal: &str, token: &str) {
        let _ = call("POST", &format!("{portal}/api/v1/device/sign-out"), Some(token), Some("{}"));
    }
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
    fn signing_in() {
        let portal = "https://portal.example.com";
        let ok = br#"{"device_code":"dc","user_code":"BCDF-GHJK","verification_uri":"https://portal.example.com/device","verification_uri_complete":"https://portal.example.com/device?code=BCDF-GHJK","expires_in":600,"interval":5}"#;
        let s = parse_start(201, ok, portal).unwrap();
        assert_eq!((s.user_code.as_str(), s.url.as_str(), s.interval), ("BCDF-GHJK", "https://portal.example.com/device?code=BCDF-GHJK", 5));
        // a page elsewhere is never opened
        let elsewhere = br#"{"device_code":"dc","user_code":"BCDF-GHJK","verification_uri_complete":"https://evil.example/device","interval":0}"#;
        let s = parse_start(201, elsewhere, portal).unwrap();
        assert_eq!((s.url.as_str(), s.interval), ("https://portal.example.com/device?code=BCDF-GHJK", 2));
        assert!(parse_start(201, br#"{"device_code":"dc","user_code":"<b>"}"#, portal).is_err());
        let paused = br#"{"error":{"code":"unavailable","message":"Signing in to the app is paused for a moment."}}"#;
        assert_eq!(parse_start(503, paused, portal).unwrap_err(), "Signing in to the app is paused for a moment.");

        assert_eq!(parse_poll(202, br#"{"status":"pending","interval":5}"#).unwrap(), Poll::Pending);
        assert_eq!(parse_poll(202, br#"{"status":"slow_down"}"#).unwrap(), Poll::SlowDown);
        let p = parse_poll(200, br#"{"status":"approved","access_token":"tdv_abc.def","email":"a@b.c"}"#).unwrap();
        assert_eq!(p, Poll::Approved { token: "tdv_abc.def".into(), email: "a@b.c".into() });
        assert!(!format!("{p:?}").contains("tdv_"), "the token is never printed");
        let denied = br#"{"error":{"code":"invalid","message":"The sign-in was denied in the portal."}}"#;
        assert_eq!(parse_poll(422, denied).unwrap_err(), "The sign-in was denied in the portal.");
        assert!(parse_poll(200, br#"{"status":"approved","access_token":"nope"}"#).is_err());
    }

    #[test]
    fn my_macs() {
        let body = br#"{"macs":[
            {"rental_id":"0f8fad5b-d9cb-469f-a165-70867728950e","organization":"Acme Studio","plan":"M4 1 day","status":"ready","mac_name":"Mac mini 001","model":"Mac mini M4","region":"eu-west","connectable":true},
            {"rental_id":"1f8fad5b-d9cb-469f-a165-70867728950e","organization":"Acme Studio","plan":"M4 Pro 1 week","status":"provisioning","mac_name":null,"connectable":false},
            {"rental_id":"bad id","plan":"x","status":"ready","connectable":true}]}"#;
        let m = parse_macs(200, body).unwrap();
        assert_eq!(m.len(), 2, "an entry without a valid id is dropped");
        assert_eq!((m[0].name.as_str(), m[0].detail.as_str(), m[0].ready), ("Mac mini 001", "Mac mini M4 · M4 1 day · Acme Studio", true));
        assert_eq!((m[1].name.as_str(), m[1].state.as_str(), m[1].ready), ("M4 Pro 1 week", "Being prepared", false));
        assert_eq!(parse_macs(401, br#"{"error":{"code":"unauthenticated","message":"x"}}"#).unwrap_err(), SIGNED_OUT);
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
