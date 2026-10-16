//! External endpoints and timings. Production values are constants; tests construct their own
//! [`Endpoints`] pointing at a local fake server.

use std::time::Duration;

pub const GITHUB_API: &str = "https://api.github.com";
pub const GITHUB_WEB: &str = "https://github.com";
pub const STRIPE_API: &str = "https://api.stripe.com";
pub const DEFAULT_TEMPLATE: &str = "ducminh1110/taro-enterprise-template";
pub const RELEASE_WORKFLOW: &str = "enterprise-release.yml";
pub const USER_AGENT: &str = concat!("TaroEnterpriseSetup/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct Endpoints {
    /// GitHub REST API. The GitHub token is only ever sent here.
    pub github_api: String,
    /// github.com, used only for the OAuth device flow (the token is received from it,
    /// never sent to it).
    pub github_web: String,
    /// Stripe API. The Stripe key is only ever sent here.
    pub stripe_api: String,
    /// URL template for the domain health check; `{domain}` is replaced.
    pub domain_health: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            github_api: GITHUB_API.into(),
            github_web: GITHUB_WEB.into(),
            stripe_api: STRIPE_API.into(),
            domain_health: "https://{domain}/healthz".into(),
        }
    }
}

impl Endpoints {
    pub fn domain_health_url(&self, domain: &str) -> String {
        self.domain_health.replace("{domain}", domain)
    }
}

#[derive(Clone, Debug)]
pub struct Timings {
    /// One "second" of the device-flow polling interval.
    pub device_unit: Duration,
    /// Release workflow polling period.
    pub release_poll: Duration,
    /// How long to keep looking for the dispatched run / the release.
    pub release_find_timeout: Duration,
    pub release_wait_after_success: Duration,
    /// Delay between retries while a freshly generated repository becomes ready.
    pub repo_ready_retry: Duration,
    pub repo_ready_attempts: u32,
    pub http_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            device_unit: Duration::from_secs(1),
            release_poll: Duration::from_secs(5),
            release_find_timeout: Duration::from_secs(120),
            release_wait_after_success: Duration::from_secs(180),
            repo_ready_retry: Duration::from_secs(2),
            repo_ready_attempts: 10,
            http_timeout: Duration::from_secs(20),
        }
    }
}

impl Timings {
    /// Fast timings for tests.
    pub fn fast() -> Self {
        Timings {
            device_unit: Duration::from_millis(5),
            release_poll: Duration::from_millis(20),
            release_find_timeout: Duration::from_secs(5),
            release_wait_after_success: Duration::from_secs(5),
            repo_ready_retry: Duration::from_millis(10),
            repo_ready_attempts: 5,
            http_timeout: Duration::from_secs(5),
        }
    }
}

/// HTTP clients.
///
/// * `api`: for GitHub/Stripe API calls that carry credentials. Redirects are **not**
///   followed, so a credential can never be forwarded to another host by a redirect.
/// * `plain`: for unauthenticated requests (reachability, health checks, log downloads
///   from the pre-signed URL GitHub redirects to).
#[derive(Clone)]
pub struct Http {
    pub api: reqwest::Client,
    pub plain: reqwest::Client,
}

impl Http {
    pub fn new(timeout: Duration) -> Self {
        let api = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client");
        let plain = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("HTTP client");
        Http { api, plain }
    }
}
