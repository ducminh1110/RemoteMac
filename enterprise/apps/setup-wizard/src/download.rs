//! The download policy and verified, streamed downloads.
//!
//! Everything the wizard downloads (the component manifest, its signature and the components)
//! must be an `https://` URL on one of [`DOWNLOAD_HOSTS`] (default port, no user info). Redirects
//! are followed by hand, at most five, and every hop is checked the same way, so a manifest or
//! a redirect can never make the wizard fetch from anywhere else. Components are streamed to a
//! temporary file next to their destination, with the size and sha256 from the signed manifest
//! enforced while streaming and checked at the end; a mismatch deletes the file.

use crate::config::{Endpoints, Timings, DOWNLOAD_HOSTS};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use url::Url;

const MAX_REDIRECTS: usize = 5;

/// The URL is allowed by the download policy.
pub fn check_url(raw: &str) -> Result<Url, String> {
    let u = Url::parse(raw).map_err(|e| format!("\"{raw}\" is not a valid URL ({e})"))?;
    if u.scheme() != "https" {
        return Err(format!("{raw}: only https:// downloads are allowed"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(format!(
            "{raw}: URLs with user names or passwords are refused"
        ));
    }
    if u.port().is_some() {
        return Err(format!("{raw}: only the default https port is allowed"));
    }
    let host = u.host_str().unwrap_or("");
    if !DOWNLOAD_HOSTS.contains(&host) {
        return Err(format!(
            "{host} is not an allowed download host (allowed: {})",
            DOWNLOAD_HOSTS.join(", ")
        ));
    }
    Ok(u)
}

/// A plain, safe file name (used for temporary files): letters, digits, `.`, `-`, `_`.
pub fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// GET a policy-checked URL, following redirects that stay within the policy.
async fn get(
    http: &reqwest::Client,
    ep: &Endpoints,
    url: &str,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, String> {
    let mut cur = check_url(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let fetch = ep.fetch_url(cur.as_str());
        let resp = tokio::time::timeout(timeout, http.get(&fetch).send())
            .await
            .map_err(|_| format!("{cur}: no answer within {} s", timeout.as_secs()))?
            .map_err(|e| format!("{cur}: {}", e.without_url()))?;
        let st = resp.status();
        if st.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{cur}: redirect without a location"))?;
            let next = cur
                .join(loc)
                .map_err(|e| format!("{cur}: bad redirect ({e})"))?;
            cur = check_url(next.as_str()).map_err(|e| format!("redirect refused: {e}"))?;
            continue;
        }
        if !st.is_success() {
            return Err(format!("{cur}: HTTP {}", st.as_u16()));
        }
        return Ok(resp);
    }
    Err(format!("{url}: too many redirects"))
}

/// A small file (a manifest, a signature), at most `max` bytes.
pub async fn fetch_small(
    http: &reqwest::Client,
    ep: &Endpoints,
    t: &Timings,
    url: &str,
    max: usize,
) -> Result<Vec<u8>, String> {
    let mut resp = get(http, ep, url, t.http_timeout).await?;
    let mut out = Vec::new();
    loop {
        let chunk = tokio::time::timeout(t.chunk_timeout, resp.chunk())
            .await
            .map_err(|_| format!("{url}: the download stalled"))?
            .map_err(|e| format!("{url}: {}", e.without_url()))?;
        let Some(c) = chunk else { break };
        if out.len() + c.len() > max {
            return Err(format!("{url}: larger than {max} bytes"));
        }
        out.extend_from_slice(&c);
    }
    Ok(out)
}

/// Download `url` to `dest`, enforcing `size` and `sha256` (lower-case hex) from the signed
/// manifest. `progress` gets the bytes received so far. On any failure nothing is left at
/// `dest` (or at its temporary name).
#[allow(clippy::too_many_arguments)]
pub async fn download_verified(
    http: &reqwest::Client,
    ep: &Endpoints,
    t: &Timings,
    url: &str,
    size: u64,
    sha256: &str,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), String> {
    let part = dest.with_extension("part");
    let r = stream_to(http, ep, t, url, size, sha256, &part, cancel, progress).await;
    match r {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display())),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_to(
    http: &reqwest::Client,
    ep: &Endpoints,
    t: &Timings,
    url: &str,
    size: u64,
    sha256: &str,
    part: &Path,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), String> {
    let mut resp = get(http, ep, url, t.http_timeout).await?;
    if let Some(len) = resp.content_length() {
        if len != size {
            return Err(format!(
                "{url}: the server announces {len} bytes, the signed manifest says {size}; refused"
            ));
        }
    }
    let mut f = std::fs::File::create(part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut h = Sha256::new();
    let mut got: u64 = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let chunk = tokio::time::timeout(t.chunk_timeout, resp.chunk())
            .await
            .map_err(|_| format!("{url}: the download stalled"))?
            .map_err(|e| format!("{url}: {}", e.without_url()))?;
        let Some(c) = chunk else { break };
        got += c.len() as u64;
        if got > size {
            return Err(format!(
                "{url}: more than the {size} bytes the signed manifest says; refused"
            ));
        }
        h.update(&c);
        f.write_all(&c)
            .map_err(|e| format!("{}: {e}", part.display()))?;
        progress(got);
    }
    f.sync_all()
        .map_err(|e| format!("{}: {e}", part.display()))?;
    drop(f);
    if got != size {
        return Err(format!(
            "{url}: {got} bytes received, the signed manifest says {size}; refused"
        ));
    }
    let actual = hex::encode(h.finalize());
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(format!(
            "{url}: sha256 {actual} does not match the signed manifest ({sha256}); refused"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_on_the_allowed_hosts() {
        for ok in [
            "https://github.com/ducminh1110/RemoteMac/releases/download/1.4.1/Taro-windows.zip",
            "https://objects.githubusercontent.com/github-production-release-asset/1",
            "https://release-assets.githubusercontent.com/x?sig=1",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://github.com/x",
            "https://evil.example.com/Taro-windows.zip",
            "https://github.com.evil.example/x",
            "https://user:pw@github.com/x",
            "https://github.com:8443/x",
            "ftp://github.com/x",
            "file:///etc/passwd",
            "https://api.github.com/x",
            "not a url",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn temp_names_are_plain() {
        assert!(safe_file_name("Taro-windows.zip"));
        assert!(safe_file_name("taro-relay-0.2.0-linux-x86_64.tar.gz"));
        for bad in ["", "../x", "a/b", ".hidden", "a b", "a\\b", "C:x"] {
            assert!(!safe_file_name(bad), "{bad}");
        }
    }
}
