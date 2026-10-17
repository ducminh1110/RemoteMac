//! Step 5: Enterprise releases and their signed component manifest.
//!
//! Releases are discovered with the GitHub releases API of the release repository (tags
//! `enterprise-v<version>`, drafts ignored, newest version first). The chosen release's
//! `taro-enterprise-components.json` is only used after its `.sig` verifies with the trusted
//! release keys ([`taro_update::official_keys`] in the shipped wizard).

use crate::config::{Endpoints, Timings, COMPONENTS_FILE, COMPONENTS_SIG, TAG_PREFIX};
use crate::download::{check_url, fetch_small, safe_file_name};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use taro_update::manifest::{Component, Components, COMPONENT_IDS};
use taro_update::sig::TrustedKeys;
use taro_update::version::Version;

/// The largest component file accepted (a client or relay package).
pub const MAX_COMPONENT: u64 = 2 << 30;

#[derive(Clone, Debug, Serialize)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: String,
    pub name: Option<String>,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub html_url: Option<String>,
    pub components_url: Option<String>,
    pub sig_url: Option<String>,
    /// both files are attached
    pub usable: bool,
    pub note: Option<String>,
}

/// The Enterprise releases in a GitHub releases API answer, newest version first.
pub fn parse_releases(v: &Value) -> Vec<ReleaseInfo> {
    let mut out: Vec<(Version, ReleaseInfo)> = Vec::new();
    for r in v.as_array().map(Vec::as_slice).unwrap_or_default() {
        let tag = r["tag_name"].as_str().unwrap_or_default();
        if r["draft"].as_bool().unwrap_or(false) || !tag.starts_with(TAG_PREFIX) {
            continue;
        }
        let Some(ver) = Version::parse(tag) else {
            continue;
        };
        let asset = |name: &str| {
            r["assets"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(name)))
                .and_then(|x| x["browser_download_url"].as_str())
                .map(str::to_string)
        };
        let components_url = asset(COMPONENTS_FILE);
        let sig_url = asset(COMPONENTS_SIG);
        let usable = components_url.is_some() && sig_url.is_some();
        out.push((
            ver,
            ReleaseInfo {
                tag: tag.to_string(),
                version: tag[TAG_PREFIX.len()..].to_string(),
                name: r["name"].as_str().map(str::to_string),
                prerelease: r["prerelease"].as_bool().unwrap_or(false),
                published_at: r["published_at"].as_str().map(str::to_string),
                html_url: r["html_url"].as_str().map(str::to_string),
                components_url,
                sig_url,
                usable,
                note: (!usable).then(|| {
                    format!("{COMPONENTS_FILE} or its .sig is not attached to this release")
                }),
            },
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, r)| r).collect()
}

pub async fn discover(
    http: &reqwest::Client,
    ep: &Endpoints,
    t: &Timings,
) -> Result<Vec<ReleaseInfo>, String> {
    let url = ep.releases_url();
    let resp = tokio::time::timeout(
        t.http_timeout,
        http.get(&url)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .send(),
    )
    .await
    .map_err(|_| "GitHub did not answer in time".to_string())?
    .map_err(|e| format!("GitHub releases: {}", e.without_url()))?;
    let st = resp.status();
    if !st.is_success() {
        return Err(match st.as_u16() {
            403 | 429 => "GitHub refused the request (HTTP 403/429): the unauthenticated rate limit (60 requests an hour per IP address) may be used up. Wait a while and try again.".into(),
            404 => format!("The release repository {} was not found on GitHub.", ep.repo),
            s => format!("GitHub releases: HTTP {s}"),
        });
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("GitHub releases: {}", e.without_url()))?;
    Ok(parse_releases(&v))
}

pub fn label(id: &str) -> &'static str {
    match id {
        "windows-client" => "Windows client",
        "macos-client" => "macOS client",
        "linux-relay-x86_64" => "Linux relay (x86_64)",
        "linux-relay-aarch64" => "Linux relay (aarch64)",
        _ => "Unknown component",
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ComponentView {
    pub id: String,
    pub label: String,
    pub version: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// the URL passes the download policy
    pub url_ok: bool,
    pub url_error: Option<String>,
}

/// A verified component manifest.
#[derive(Clone, Debug, Serialize)]
pub struct Selected {
    pub tag: String,
    pub enterprise_version: String,
    pub taro_version: String,
    pub relay_version: Option<String>,
    pub released_at: String,
    /// the release key that signed it (`ed25519:…`)
    pub signed_by: String,
    pub components: Vec<ComponentView>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub manifest: Components,
}

impl Selected {
    pub fn component(&self, id: &str) -> Option<&Component> {
        self.manifest.get(id)
    }
}

/// Check the signature and the contents of a component manifest for the release `version`.
pub fn verify(
    bytes: &[u8],
    sig: &str,
    keys: &TrustedKeys,
    tag: &str,
    version: &str,
) -> Result<Selected, String> {
    let signed_by = keys
        .verify(bytes, sig)
        .map_err(|e| format!("{COMPONENTS_FILE}: {e}. It is not used."))?;
    let c = Components::verify(bytes, sig, keys)?;
    if c.enterprise_version != version {
        return Err(format!(
            "{COMPONENTS_FILE} is for Enterprise {}, but the release is {tag}; refused",
            c.enterprise_version
        ));
    }
    let mut seen = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut views = Vec::new();
    for comp in &c.components {
        if !seen.insert(comp.id.clone()) {
            return Err(format!(
                "{COMPONENTS_FILE} lists {} twice; refused",
                comp.id
            ));
        }
        if !COMPONENT_IDS.contains(&comp.id.as_str()) {
            warnings.push(format!("unknown component {} ignored", comp.id));
            continue;
        }
        if !safe_file_name(&comp.file) {
            return Err(format!(
                "{}: unusual file name {:?}; refused",
                comp.id, comp.file
            ));
        }
        if comp.size == 0 || comp.size > MAX_COMPONENT {
            return Err(format!(
                "{}: implausible size {}; refused",
                comp.id, comp.size
            ));
        }
        if comp.id.ends_with("-client") && comp.version != c.taro_version {
            warnings.push(format!(
                "{} is Taro {}, the release pins Taro {}",
                comp.id, comp.version, c.taro_version
            ));
        }
        let pol = check_url(&comp.url);
        views.push(ComponentView {
            id: comp.id.clone(),
            label: label(&comp.id).into(),
            version: comp.version.clone(),
            file: comp.file.clone(),
            url: comp.url.clone(),
            sha256: comp.sha256.to_ascii_lowercase(),
            size: comp.size,
            url_ok: pol.is_ok(),
            url_error: pol.err(),
        });
    }
    views.sort_by_key(|v| COMPONENT_IDS.iter().position(|x| *x == v.id));
    let relay_version = c
        .components
        .iter()
        .find(|x| x.id.starts_with("linux-relay-"))
        .map(|x| x.version.clone());
    Ok(Selected {
        tag: tag.into(),
        enterprise_version: c.enterprise_version.clone(),
        taro_version: c.taro_version.clone(),
        relay_version,
        released_at: c.released_at.clone(),
        signed_by,
        components: views,
        warnings,
        manifest: c,
    })
}

/// Download and verify the component manifest of `rel`.
pub async fn fetch_verified(
    http: &reqwest::Client,
    ep: &Endpoints,
    t: &Timings,
    keys: &TrustedKeys,
    rel: &ReleaseInfo,
) -> Result<Selected, String> {
    let (Some(cu), Some(su)) = (&rel.components_url, &rel.sig_url) else {
        return Err(format!(
            "{} has no {COMPONENTS_FILE} with a signature attached",
            rel.tag
        ));
    };
    let bytes = fetch_small(http, ep, t, cu, 1 << 20).await?;
    let sig = fetch_small(http, ep, t, su, 4096).await?;
    let sig = String::from_utf8(sig).map_err(|_| format!("{COMPONENTS_SIG} is not text"))?;
    verify(&bytes, &sig, keys, &rel.tag, &rel.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enterprise_releases_newest_first() {
        let asset = |tag: &str, n: &str| json!({"name": n, "browser_download_url": format!("https://github.com/o/r/releases/download/{tag}/{n}")});
        let v = json!([
            {"tag_name": "1.4.1", "draft": false, "assets": []},
            {"tag_name": "enterprise-v0.2.0", "draft": false, "assets": [asset("enterprise-v0.2.0", COMPONENTS_FILE), asset("enterprise-v0.2.0", COMPONENTS_SIG)]},
            {"tag_name": "enterprise-v0.10.0", "draft": true, "assets": []},
            {"tag_name": "enterprise-v0.3.0", "draft": false, "prerelease": true, "assets": [asset("enterprise-v0.3.0", COMPONENTS_FILE)]},
            {"tag_name": "enterprise-v0.1.0", "draft": false, "assets": [asset("enterprise-v0.1.0", COMPONENTS_FILE), asset("enterprise-v0.1.0", COMPONENTS_SIG)]},
            {"tag_name": "enterprise-vX", "draft": false, "assets": []}
        ]);
        let r = parse_releases(&v);
        let tags: Vec<&str> = r.iter().map(|x| x.tag.as_str()).collect();
        assert_eq!(
            tags,
            [
                "enterprise-v0.3.0",
                "enterprise-v0.2.0",
                "enterprise-v0.1.0"
            ]
        );
        assert!(!r[0].usable && r[0].prerelease);
        assert!(r[1].usable);
        assert_eq!(r[1].version, "0.2.0");
    }
}
