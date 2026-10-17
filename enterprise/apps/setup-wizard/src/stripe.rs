//! Minimal Stripe client: account check, webhook endpoints, checkout smoke test.
//! The key is only ever sent to the configured Stripe API base.

use crate::secret::Secret;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::Value;

/// Events the Taro portal handles; the webhook endpoint subscribes to exactly these.
pub const WEBHOOK_EVENTS: [&str; 7] = [
    "checkout.session.completed",
    "checkout.session.expired",
    "invoice.paid",
    "invoice.payment_failed",
    "customer.subscription.updated",
    "customer.subscription.deleted",
    "charge.refunded",
];

#[derive(Clone)]
pub struct Stripe {
    base: String,
    http: reqwest::Client,
    key: Secret,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountInfo {
    pub id: String,
    pub display_name: Option<String>,
    pub country: Option<String>,
    pub charges_enabled: Option<bool>,
    pub details_submitted: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookEndpoint {
    pub id: String,
    pub url: String,
    pub status: String,
    pub enabled_events: Vec<String>,
    pub livemode: bool,
}

impl WebhookEndpoint {
    pub fn missing_events(&self) -> Vec<&'static str> {
        if self.enabled_events.iter().any(|e| e == "*") {
            return vec![];
        }
        WEBHOOK_EVENTS
            .iter()
            .copied()
            .filter(|e| !self.enabled_events.iter().any(|x| x == e))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckoutSession {
    pub id: String,
    pub url: Option<String>,
    pub livemode: bool,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StripeError {
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for StripeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "Stripe API {s}: {}", self.message),
            None => write!(f, "Stripe API: {}", self.message),
        }
    }
}

pub type StripeResult<T> = Result<T, StripeError>;

fn parse_webhook(v: &Value) -> WebhookEndpoint {
    WebhookEndpoint {
        id: v["id"].as_str().unwrap_or_default().into(),
        url: v["url"].as_str().unwrap_or_default().into(),
        status: v["status"].as_str().unwrap_or_default().into(),
        enabled_events: v["enabled_events"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        livemode: v["livemode"].as_bool().unwrap_or(false),
    }
}

impl Stripe {
    pub fn new(base: &str, http: reqwest::Client, key: Secret) -> Self {
        Stripe {
            base: base.trim_end_matches('/').to_string(),
            http,
            key,
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        form: &[(String, String)],
    ) -> StripeResult<Value> {
        let mut rb = self
            .http
            .request(method.clone(), format!("{}{}", self.base, path))
            .bearer_auth(self.key.expose());
        if method != Method::GET {
            rb = rb.form(form);
        }
        let r = rb.send().await.map_err(|e| StripeError {
            status: None,
            message: if e.is_timeout() {
                "timed out".into()
            } else {
                format!("request failed ({})", e.without_url())
            },
        })?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(v);
        }
        let msg = v["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
        let hint = match status {
            StatusCode::UNAUTHORIZED => " (the key is invalid or was revoked)",
            StatusCode::FORBIDDEN => " (a restricted key needs more permissions for this call)",
            _ => "",
        };
        Err(StripeError {
            status: Some(status.as_u16()),
            message: format!("{msg}{hint}"),
        })
    }

    /// `GET /v1/account`.
    pub async fn account(&self) -> StripeResult<AccountInfo> {
        let v = self.call(Method::GET, "/v1/account", &[]).await?;
        Ok(AccountInfo {
            id: v["id"].as_str().unwrap_or_default().into(),
            display_name: v["settings"]["dashboard"]["display_name"]
                .as_str()
                .or_else(|| v["business_profile"]["name"].as_str())
                .map(str::to_string),
            country: v["country"].as_str().map(str::to_string),
            charges_enabled: v["charges_enabled"].as_bool(),
            details_submitted: v["details_submitted"].as_bool(),
        })
    }

    pub async fn list_webhooks(&self) -> StripeResult<Vec<WebhookEndpoint>> {
        let v = self
            .call(Method::GET, "/v1/webhook_endpoints?limit=100", &[])
            .await?;
        Ok(v["data"]
            .as_array()
            .map(|a| a.iter().map(parse_webhook).collect())
            .unwrap_or_default())
    }

    /// `POST /v1/webhook_endpoints`. Stripe returns the signing secret only on creation.
    pub async fn create_webhook(
        &self,
        url: &str,
    ) -> StripeResult<(WebhookEndpoint, Option<Secret>)> {
        let mut form = vec![
            ("url".to_string(), url.to_string()),
            (
                "description".to_string(),
                "Taro Enterprise portal (created by TaroEnterpriseSetup)".to_string(),
            ),
        ];
        for (i, e) in WEBHOOK_EVENTS.iter().enumerate() {
            form.push((format!("enabled_events[{i}]"), e.to_string()));
        }
        let v = self
            .call(Method::POST, "/v1/webhook_endpoints", &form)
            .await?;
        let secret = v["secret"].as_str().map(Secret::new);
        Ok((parse_webhook(&v), secret))
    }

    /// Billing smoke test: a test-mode Checkout Session for 1 x 100 minor units.
    pub async fn create_smoke_session(&self, portal: &str) -> StripeResult<CheckoutSession> {
        let form: Vec<(String, String)> = [
            ("mode", "payment".to_string()),
            ("line_items[0][quantity]", "1".to_string()),
            ("line_items[0][price_data][currency]", "usd".to_string()),
            ("line_items[0][price_data][unit_amount]", "100".to_string()),
            (
                "line_items[0][price_data][product_data][name]",
                "Taro setup smoke test".to_string(),
            ),
            (
                "success_url",
                format!("{portal}/billing/smoke-test/success"),
            ),
            ("cancel_url", format!("{portal}/billing/smoke-test/cancel")),
            ("metadata[purpose]", "taro-setup-smoke-test".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let v = self
            .call(Method::POST, "/v1/checkout/sessions", &form)
            .await?;
        Ok(CheckoutSession {
            id: v["id"].as_str().unwrap_or_default().into(),
            url: v["url"].as_str().map(str::to_string),
            livemode: v["livemode"].as_bool().unwrap_or(true),
            status: v["status"].as_str().map(str::to_string),
        })
    }

    /// `POST /v1/checkout/sessions/{id}/expire`.
    pub async fn expire_session(&self, id: &str) -> StripeResult<CheckoutSession> {
        if !id.starts_with("cs_") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(StripeError {
                status: None,
                message: "unexpected session id".into(),
            });
        }
        let v = self
            .call(
                Method::POST,
                &format!("/v1/checkout/sessions/{id}/expire"),
                &[],
            )
            .await?;
        Ok(CheckoutSession {
            id: v["id"].as_str().unwrap_or_default().into(),
            url: None,
            livemode: v["livemode"].as_bool().unwrap_or(true),
            status: v["status"].as_str().map(str::to_string),
        })
    }
}
