//! Configuration from the environment (12-factor). Secrets come from the environment the
//! deployment injects (a secret manager, GitHub Actions secrets, a `.env` the operator keeps off
//! the repository); they are held as [`Secret`] so they never reach a log or a response.

use std::fmt;
use std::net::SocketAddr;

/// A value that must never be printed: `Debug` and `Display` show a placeholder.
#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    /// The secret itself, for the one place that sends it (an HTTP header, a signature).
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(if self.0.is_empty() {
            "Secret(<empty>)"
        } else {
            "Secret(<redacted>)"
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Development,
    Staging,
    Production,
}

impl Env {
    pub fn parse(s: &str) -> Result<Env, String> {
        match s {
            "development" | "dev" => Ok(Env::Development),
            "staging" => Ok(Env::Staging),
            "production" | "prod" => Ok(Env::Production),
            _ => Err(format!(
                "TARO_ENV must be development, staging or production, not {s:?}"
            )),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Env::Development => "development",
            Env::Staging => "staging",
            Env::Production => "production",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Google {
    pub client_id: String,
    pub client_secret: Secret,
}

#[derive(Debug, Clone)]
pub struct Stripe {
    pub secret_key: Secret,
    pub webhook_secret: Secret,
    /// live mode was asked for explicitly (TARO_STRIPE_LIVE=1); otherwise only test keys start
    pub live: bool,
    pub api_base: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub env: Env,
    pub database_url: Secret,
    /// the Customer Portal's origin, e.g. https://portal.example.com
    pub public_url: url::Url,
    /// the Provider Console's origin, e.g. https://ops.example.com
    pub ops_url: url::Url,
    pub listen: SocketAddr,
    pub ops_listen: SocketAddr,
    /// signs connection grants and derives other keys; 32 bytes or more
    pub secret_key: Secret,
    pub google: Option<Google>,
    pub google_issuer: String,
    pub google_jwks_url: String,
    pub google_token_url: String,
    pub google_auth_url: String,
    /// smtp(s)://user:pass@host:port; none in development: mail goes to the dev outbox
    pub smtp_url: Option<Secret>,
    pub mail_from: String,
    pub stripe: Option<Stripe>,
    /// the provider adapter in use; only "mock" exists today
    pub provider: String,
    /// the first platform administrator (granted at start, if that user exists or signs in)
    pub bootstrap_admin_email: Option<String>,
    /// the enterprise/ directory: the web apps are in apps/customer-portal and
    /// apps/provider-console, the shared design tokens in packages/design-tokens
    pub web_dir: std::path::PathBuf,
    /// hostnames the customer listener answers for besides the public URL's (verified custom
    /// domains are added from the database)
    pub trusted_proxies: bool,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let env = Env::parse(&var("TARO_ENV").unwrap_or_else(|| "development".into()))?;
        let database_url =
            Secret::new(var("TARO_DATABASE_URL").ok_or("TARO_DATABASE_URL is required")?);
        let url = |k: &str, default: &str| -> Result<url::Url, String> {
            let v = var(k).unwrap_or_else(|| default.into());
            let u = url::Url::parse(&v).map_err(|e| format!("{k}: {e}"))?;
            if env != Env::Development && u.scheme() != "https" {
                return Err(format!("{k} must be https outside development"));
            }
            Ok(u)
        };
        let public_url = url("TARO_PUBLIC_URL", "http://localhost:8080")?;
        let ops_url = url("TARO_OPS_URL", "http://localhost:8081")?;
        let addr = |k: &str, default: &str| -> Result<SocketAddr, String> {
            var(k)
                .unwrap_or_else(|| default.into())
                .parse()
                .map_err(|e| format!("{k}: {e}"))
        };
        let secret_key =
            match var("TARO_SECRET_KEY") {
                Some(k) if k.len() >= 32 => Secret::new(k),
                Some(_) => return Err(
                    "TARO_SECRET_KEY must be at least 32 characters (64 hex digits recommended)"
                        .into(),
                ),
                None if env == Env::Development => {
                    Secret::new("development-only-secret-key-not-for-production-use")
                }
                None => return Err("TARO_SECRET_KEY is required".into()),
            };
        let google = match (
            var("TARO_GOOGLE_CLIENT_ID"),
            var("TARO_GOOGLE_CLIENT_SECRET"),
        ) {
            (Some(id), Some(secret)) => Some(Google {
                client_id: id,
                client_secret: Secret::new(secret),
            }),
            (None, None) => None,
            _ => {
                return Err(
                    "TARO_GOOGLE_CLIENT_ID and TARO_GOOGLE_CLIENT_SECRET go together".into(),
                )
            }
        };
        let stripe = match var("TARO_STRIPE_SECRET_KEY") {
            Some(key) => {
                let live = var("TARO_STRIPE_LIVE").as_deref() == Some("1");
                let test_key = key.starts_with(concat!("sk", "_test_"))
                    || key.starts_with(concat!("rk", "_test_"));
                if !test_key && !live {
                    return Err("TARO_STRIPE_SECRET_KEY is not a test key: live billing needs TARO_STRIPE_LIVE=1 set on purpose".into());
                }
                if live && env != Env::Production {
                    return Err(
                        "live Stripe keys are only accepted with TARO_ENV=production".into(),
                    );
                }
                let webhook_secret = var("TARO_STRIPE_WEBHOOK_SECRET")
                    .ok_or("TARO_STRIPE_WEBHOOK_SECRET is required with a Stripe key")?;
                if !webhook_secret.starts_with(concat!("wh", "sec_")) {
                    return Err(concat!(
                        "TARO_STRIPE_WEBHOOK_SECRET must be a Stripe webhook signing secret (wh",
                        "sec_...)"
                    )
                    .into());
                }
                Some(Stripe {
                    secret_key: Secret::new(key),
                    webhook_secret: Secret::new(webhook_secret),
                    live,
                    api_base: var("TARO_STRIPE_API_BASE")
                        .unwrap_or_else(|| "https://api.stripe.com".into()),
                })
            }
            None => None,
        };
        let smtp_url = var("TARO_SMTP_URL").map(Secret::new);
        if smtp_url.is_none() && env != Env::Development {
            return Err(
                "TARO_SMTP_URL is required outside development (sign-in links are sent by mail)"
                    .into(),
            );
        }
        let provider = var("TARO_PROVIDER").unwrap_or_else(|| "mock".into());
        if provider != "mock" {
            return Err(format!(
                "TARO_PROVIDER {provider:?}: only the mock provider is implemented"
            ));
        }
        Ok(Config {
            env,
            database_url,
            public_url,
            ops_url,
            listen: addr("TARO_LISTEN", "127.0.0.1:8080")?,
            ops_listen: addr("TARO_OPS_LISTEN", "127.0.0.1:8081")?,
            secret_key,
            google,
            google_issuer: var("TARO_GOOGLE_ISSUER")
                .unwrap_or_else(|| "https://accounts.google.com".into()),
            google_jwks_url: var("TARO_GOOGLE_JWKS_URL")
                .unwrap_or_else(|| "https://www.googleapis.com/oauth2/v3/certs".into()),
            google_token_url: var("TARO_GOOGLE_TOKEN_URL")
                .unwrap_or_else(|| "https://oauth2.googleapis.com/token".into()),
            google_auth_url: var("TARO_GOOGLE_AUTH_URL")
                .unwrap_or_else(|| "https://accounts.google.com/o/oauth2/v2/auth".into()),
            smtp_url,
            mail_from: var("TARO_MAIL_FROM")
                .unwrap_or_else(|| "Taro Enterprise <no-reply@localhost>".into()),
            stripe,
            provider,
            bootstrap_admin_email: var("TARO_BOOTSTRAP_ADMIN_EMAIL").map(|e| e.to_lowercase()),
            web_dir: var("TARO_WEB_DIR")
                .map(Into::into)
                .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into()),
            trusted_proxies: var("TARO_TRUSTED_PROXY").as_deref() == Some("1"),
        })
    }

    /// Cookies are `Secure` unless the portal is plain http in development.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.scheme() == "https"
    }

    /// A configuration for tests: development, a given database, no external services.
    pub fn for_tests(database_url: &str) -> Config {
        Config {
            env: Env::Development,
            database_url: Secret::new(database_url),
            public_url: url::Url::parse("http://portal.test").unwrap(),
            ops_url: url::Url::parse("http://ops.test").unwrap(),
            listen: "127.0.0.1:0".parse().unwrap(),
            ops_listen: "127.0.0.1:0".parse().unwrap(),
            secret_key: Secret::new("test-secret-key-test-secret-key-0123456789"),
            google: None,
            google_issuer: "https://accounts.google.com".into(),
            google_jwks_url: String::new(),
            google_token_url: String::new(),
            google_auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            smtp_url: None,
            mail_from: "Taro Enterprise <no-reply@localhost>".into(),
            stripe: None,
            provider: "mock".into(),
            bootstrap_admin_email: None,
            web_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into(),
            trusted_proxies: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let s = Secret::new(concat!("sk", "_test_123"));
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        let c = Config::for_tests("postgres://u:p@h/db");
        let text = format!("{c:?}");
        assert!(
            !text.contains("u:p@h") && !text.contains("test-secret-key"),
            "{text}"
        );
    }
}
