//! What every handler and worker shares.

use std::sync::Arc;

use crate::billing::stripe::Billing;
use crate::config::Config;
use crate::mail::Mailer;
use crate::metrics::Metrics;
use crate::provider::Provider;
use crate::ratelimit::Limiter;

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

pub struct Inner {
    pub config: Config,
    pub db: sqlx::PgPool,
    /// the provider adapter, and the provider_integrations row it is
    pub provider: Arc<dyn Provider>,
    pub provider_id: uuid::Uuid,
    /// Stripe, when configured
    pub billing: Option<Arc<dyn Billing>>,
    pub mailer: Arc<dyn Mailer>,
    pub metrics: Metrics,
    pub limiter: Limiter,
    pub http: reqwest::Client,
    /// Google's signing keys (cached)
    pub jwks: tokio::sync::RwLock<Option<(std::time::Instant, jsonwebtoken::jwk::JwkSet)>>,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// Which app a request came to: the Customer Portal or the Provider Console. Each has its own
/// listener, cookie and session audience; an ops session is never accepted by the portal, nor a
/// customer session by the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    Customer,
    Ops,
}

impl Audience {
    pub fn key(&self) -> &'static str {
        match self {
            Audience::Customer => "customer",
            Audience::Ops => "ops",
        }
    }

    pub fn parse(s: &str) -> Option<Audience> {
        match s {
            "customer" => Some(Audience::Customer),
            "ops" => Some(Audience::Ops),
            _ => None,
        }
    }

    pub fn origin(&self, c: &Config) -> String {
        let u = match self {
            Audience::Customer => &c.public_url,
            Audience::Ops => &c.ops_url,
        };
        u.origin().ascii_serialization()
    }
}
