//! External endpoints and timings. Release builds use the constants below; tests construct
//! their own [`Endpoints`] pointing at a local fake (see `tests/common`).

use std::time::Duration;

/// The GitHub REST API (release discovery only; no credentials are ever sent).
pub const GITHUB_API: &str = "https://api.github.com";
/// The repository whose releases carry the Enterprise component manifests.
pub const RELEASE_REPO: &str = "ducminh1110/RemoteMac";
/// Enterprise releases are tagged `enterprise-v<version>` (never "latest").
pub const TAG_PREFIX: &str = "enterprise-v";
pub const COMPONENTS_FILE: &str = "taro-enterprise-components.json";
pub const COMPONENTS_SIG: &str = "taro-enterprise-components.json.sig";
/// Step 5: the release workflow, its signing secret and the trusted keys file of the repository.
pub const RELEASE_WORKFLOW: &str = "enterprise-release.yml";
pub const WORKFLOW_PATH: &str = ".github/workflows/enterprise-release.yml";
/// A classic token with exactly the scopes Step 5 needs, made on GitHub's page.
pub const NEW_TOKEN_URL: &str =
    "https://github.com/settings/tokens/new?scopes=repo,workflow&description=TaroEnterpriseSetup";
pub const SIGNING_SECRET: &str = "TARO_ENTERPRISE_SIGNING_KEY";
pub const KEYS_FILE: &str = "enterprise/keys/release-signing.pub";
pub const VERSION_FILE: &str = "enterprise/Cargo.toml";
/// The only hosts anything is downloaded from (always over https). GitHub serves release
/// assets from github.com and redirects to one of the other two.
pub const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
pub const USER_AGENT: &str = concat!("TaroEnterpriseSetup/", env!("CARGO_PKG_VERSION"));
/// "TaroEnterpriseSetup 0.1.0": who wrote provider.json and the provider manifest.
pub const BUILT_BY: &str = concat!("TaroEnterpriseSetup ", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct Endpoints {
    /// GitHub REST API base.
    pub github_api: String,
    /// `owner/repo` of the release repository.
    pub repo: String,
    /// Preflight reachability checks: (id, label, url).
    pub reachability: Vec<(String, String, String)>,
    /// Prefix rewrites applied to a URL *after* it passed the download policy and before it
    /// is fetched. Empty by default; the tests map `https://github.com/` and the other
    /// download hosts to a local fake server, so the signed manifest can carry real https URLs.
    pub rewrite: Vec<(String, String)>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            github_api: GITHUB_API.into(),
            repo: RELEASE_REPO.into(),
            reachability: vec![
                (
                    "github".into(),
                    "github.com reachable (HTTPS)".into(),
                    "https://github.com/".into(),
                ),
                (
                    "objects".into(),
                    "objects.githubusercontent.com reachable (HTTPS)".into(),
                    "https://objects.githubusercontent.com/".into(),
                ),
            ],
            rewrite: vec![],
        }
    }
}

impl Endpoints {
    /// The URL actually fetched for a policy-checked URL.
    pub fn fetch_url(&self, url: &str) -> String {
        for (from, to) in &self.rewrite {
            if let Some(rest) = url.strip_prefix(from.as_str()) {
                return format!("{to}{rest}");
            }
        }
        url.to_string()
    }

    pub fn releases_url(&self) -> String {
        format!(
            "{}/repos/{}/releases?per_page=100",
            self.github_api.trim_end_matches('/'),
            self.repo
        )
    }
}

#[derive(Clone, Debug)]
pub struct Timings {
    /// API requests and small files.
    pub http_timeout: Duration,
    /// The longest wait for the next chunk of a download.
    pub chunk_timeout: Duration,
    /// How often the release workflow's run is looked at (Step 5).
    pub actions_poll: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            http_timeout: Duration::from_secs(20),
            chunk_timeout: Duration::from_secs(60),
            actions_poll: Duration::from_secs(10),
        }
    }
}

impl Timings {
    pub fn fast() -> Self {
        Timings {
            http_timeout: Duration::from_secs(5),
            chunk_timeout: Duration::from_secs(5),
            actions_poll: Duration::from_millis(50),
        }
    }
}

/// The HTTP client. Redirects are never followed automatically: [`crate::download`] follows
/// them itself and checks every hop against the download policy. No request carries
/// credentials. Per-request deadlines come from [`Timings`] (a whole download has none: each
/// chunk has [`Timings::chunk_timeout`]).
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_only_applies_to_matching_prefixes() {
        let mut e = Endpoints::default();
        assert_eq!(e.fetch_url("https://github.com/x"), "https://github.com/x");
        e.rewrite = vec![(
            "https://github.com/".into(),
            "http://127.0.0.1:9/gh-web/".into(),
        )];
        assert_eq!(
            e.fetch_url("https://github.com/a/b"),
            "http://127.0.0.1:9/gh-web/a/b"
        );
        assert_eq!(
            e.fetch_url("https://evil.example/github.com/"),
            "https://evil.example/github.com/"
        );
        assert_eq!(
            e.releases_url(),
            "https://api.github.com/repos/ducminh1110/RemoteMac/releases?per_page=100"
        );
    }
}
