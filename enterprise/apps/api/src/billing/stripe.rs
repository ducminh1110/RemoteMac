//! Stripe, behind the [`Billing`] trait: the real client (REST, form-encoded, idempotency keys,
//! a pinned API version) and, in development without Stripe keys, a simulated one whose
//! "payments" go through the same webhook inbox as Stripe's, marked as simulated everywhere.

use async_trait::async_trait;
use serde::Deserialize;

use crate::config::Secret;

pub const API_VERSION: &str = "2024-06-20";

#[derive(Debug, Clone)]
pub struct CheckoutRequest {
    pub customer: String,
    /// "payment" (one-time) or "subscription" (monthly)
    pub mode: &'static str,
    pub product_name: String,
    pub amount_minor: i64,
    pub currency: String,
    pub rental_id: uuid::Uuid,
    pub organization_id: uuid::Uuid,
    pub success_url: String,
    pub cancel_url: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutSession {
    pub id: String,
    pub url: Option<String>,
    pub status: Option<String>,
}

#[async_trait]
pub trait Billing: Send + Sync {
    fn livemode(&self) -> bool;
    /// the development stand-in: no money moves
    fn simulated(&self) -> bool;
    async fn create_customer(
        &self,
        organization_id: uuid::Uuid,
        name: &str,
        email: &str,
    ) -> anyhow::Result<String>;
    async fn create_checkout(&self, r: &CheckoutRequest) -> anyhow::Result<CheckoutSession>;
    async fn get_checkout(&self, id: &str) -> anyhow::Result<CheckoutSession>;
    async fn expire_checkout(&self, id: &str) -> anyhow::Result<()>;
    async fn cancel_subscription_at_period_end(&self, id: &str) -> anyhow::Result<()>;
    async fn portal_session(&self, customer: &str, return_url: &str) -> anyhow::Result<String>;
}

pub struct Stripe {
    pub http: reqwest::Client,
    pub key: Secret,
    pub base: String,
    pub live: bool,
}

#[derive(Deserialize)]
struct StripeError {
    error: StripeErrorBody,
}
#[derive(Deserialize)]
struct StripeErrorBody {
    message: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

impl Stripe {
    async fn post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        form: &[(String, String)],
        idem: Option<&str>,
    ) -> anyhow::Result<T> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(self.key.expose())
            .header("Stripe-Version", API_VERSION)
            .form(form)
            .timeout(std::time::Duration::from_secs(20));
        if let Some(k) = idem {
            req = req.header("Idempotency-Key", k);
        }
        Self::answer(req.send().await?).await
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> anyhow::Result<T> {
        let req = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(self.key.expose())
            .header("Stripe-Version", API_VERSION)
            .timeout(std::time::Duration::from_secs(20));
        Self::answer(req.send().await?).await
    }

    async fn answer<T: for<'de> Deserialize<'de>>(r: reqwest::Response) -> anyhow::Result<T> {
        let status = r.status();
        if status.is_success() {
            return Ok(r.json().await?);
        }
        let body: Option<StripeError> = r.json().await.ok();
        let (msg, kind) = body
            .map(|b| {
                (
                    b.error.message.unwrap_or_default(),
                    b.error.kind.unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        anyhow::bail!(
            "Stripe answered {status} ({kind}): {}",
            crate::redact::redact(&msg)
        )
    }
}

fn kv(k: &str, v: impl ToString) -> (String, String) {
    (k.to_string(), v.to_string())
}

#[derive(Deserialize)]
struct Id {
    id: String,
}
#[derive(Deserialize)]
struct Url {
    url: String,
}

#[async_trait]
impl Billing for Stripe {
    fn livemode(&self) -> bool {
        self.live
    }
    fn simulated(&self) -> bool {
        false
    }

    async fn create_customer(
        &self,
        org: uuid::Uuid,
        name: &str,
        email: &str,
    ) -> anyhow::Result<String> {
        let form = vec![
            kv("name", name),
            kv("email", email),
            kv("metadata[organization_id]", org),
        ];
        let c: Id = self
            .post("/v1/customers", &form, Some(&format!("customer-{org}")))
            .await?;
        Ok(c.id)
    }

    async fn create_checkout(&self, r: &CheckoutRequest) -> anyhow::Result<CheckoutSession> {
        let mut form = vec![
            kv("mode", r.mode),
            kv("customer", &r.customer),
            kv("client_reference_id", r.rental_id),
            kv("success_url", &r.success_url),
            kv("cancel_url", &r.cancel_url),
            kv("metadata[rental_id]", r.rental_id),
            kv("metadata[organization_id]", r.organization_id),
            kv("line_items[0][quantity]", 1),
            kv("line_items[0][price_data][currency]", &r.currency),
            kv("line_items[0][price_data][unit_amount]", r.amount_minor),
            kv(
                "line_items[0][price_data][product_data][name]",
                &r.product_name,
            ),
            // the session expires after 30 minutes: an abandoned checkout stops holding the rental
            kv("expires_at", chrono::Utc::now().timestamp() + 1800),
        ];
        if r.mode == "subscription" {
            form.push(kv(
                "line_items[0][price_data][recurring][interval]",
                "month",
            ));
            form.push(kv("subscription_data[metadata][rental_id]", r.rental_id));
            form.push(kv(
                "subscription_data[metadata][organization_id]",
                r.organization_id,
            ));
        } else {
            form.push(kv("payment_intent_data[metadata][rental_id]", r.rental_id));
        }
        self.post("/v1/checkout/sessions", &form, Some(&r.idempotency_key))
            .await
    }

    async fn get_checkout(&self, id: &str) -> anyhow::Result<CheckoutSession> {
        self.get(&format!("/v1/checkout/sessions/{id}")).await
    }

    async fn expire_checkout(&self, id: &str) -> anyhow::Result<()> {
        let _: Id = self
            .post(&format!("/v1/checkout/sessions/{id}/expire"), &[], None)
            .await?;
        Ok(())
    }

    async fn cancel_subscription_at_period_end(&self, id: &str) -> anyhow::Result<()> {
        let _: Id = self
            .post(
                &format!("/v1/subscriptions/{id}"),
                &[kv("cancel_at_period_end", "true")],
                None,
            )
            .await?;
        Ok(())
    }

    async fn portal_session(&self, customer: &str, return_url: &str) -> anyhow::Result<String> {
        let u: Url = self
            .post(
                "/v1/billing_portal/sessions",
                &[kv("customer", customer), kv("return_url", return_url)],
                None,
            )
            .await?;
        Ok(u.url)
    }
}

/// Development only: checkout pages served by the portal itself, and webhooks Taro writes to its
/// own inbox when the developer presses Pay. Never used outside TARO_ENV=development.
pub struct Simulated {
    pub public_url: String,
}

#[async_trait]
impl Billing for Simulated {
    fn livemode(&self) -> bool {
        false
    }
    fn simulated(&self) -> bool {
        true
    }
    async fn create_customer(
        &self,
        org: uuid::Uuid,
        _name: &str,
        _email: &str,
    ) -> anyhow::Result<String> {
        Ok(format!("cus_sim_{}", org.simple()))
    }
    async fn create_checkout(&self, r: &CheckoutRequest) -> anyhow::Result<CheckoutSession> {
        let id = format!("cs_sim_{}", crate::crypto::token(12).replace('-', "x"));
        Ok(CheckoutSession {
            url: Some(format!("{}/dev/checkout?session={id}", self.public_url)),
            id,
            status: Some("open".into()),
        })
        .inspect(|_| tracing::info!(rental = %r.rental_id, "simulated checkout created"))
    }
    async fn get_checkout(&self, id: &str) -> anyhow::Result<CheckoutSession> {
        Ok(CheckoutSession {
            id: id.into(),
            url: Some(format!("{}/dev/checkout?session={id}", self.public_url)),
            status: Some("open".into()),
        })
    }
    async fn expire_checkout(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cancel_subscription_at_period_end(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn portal_session(&self, _customer: &str, return_url: &str) -> anyhow::Result<String> {
        Ok(format!("{return_url}?simulated_billing_portal=1"))
    }
}

/// Verify a `Stripe-Signature` header over the raw body: `t=<unix>,v1=<hex hmac>[,v1=...]`,
/// HMAC-SHA256 of "<t>.<body>" under the endpoint's signing secret, within `tolerance` seconds.
pub fn verify_signature(
    header: &str,
    body: &[u8],
    secret: &str,
    now: i64,
    tolerance: i64,
) -> Result<(), &'static str> {
    let mut t = None;
    let mut sigs = vec![];
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_string()),
            _ => {}
        }
    }
    let t = t.ok_or("no timestamp")?;
    if (now - t).abs() > tolerance {
        return Err("timestamp outside the tolerance");
    }
    let mut payload = format!("{t}.").into_bytes();
    payload.extend_from_slice(body);
    let expected = crate::crypto::mac(secret.as_bytes(), &payload);
    if sigs
        .iter()
        .filter_map(|s| hex::decode(s).ok())
        .any(|s| crate::crypto::eq(&s, &expected))
    {
        Ok(())
    } else {
        Err("no matching signature")
    }
}

/// A header as Stripe would send it (tests and the simulated billing).
pub fn sign(body: &[u8], secret: &str, t: i64) -> String {
    let mut payload = format!("{t}.").into_bytes();
    payload.extend_from_slice(body);
    format!(
        "t={t},v1={}",
        hex::encode(crate::crypto::mac(secret.as_bytes(), &payload))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        let body = br#"{"id":"evt_1","type":"invoice.paid"}"#;
        let h = sign(body, concat!("wh", "sec_test"), 1_700_000_000);
        assert!(verify_signature(&h, body, concat!("wh", "sec_test"), 1_700_000_100, 300).is_ok());
        assert_eq!(
            verify_signature(&h, body, concat!("wh", "sec_other"), 1_700_000_100, 300),
            Err("no matching signature")
        );
        assert_eq!(
            verify_signature(&h, b"{}", "whsec_test", 1_700_000_100, 300),
            Err("no matching signature"),
            "a changed body"
        );
        assert_eq!(
            verify_signature(&h, body, concat!("wh", "sec_test"), 1_700_001_000, 300),
            Err("timestamp outside the tolerance"),
            "a replay"
        );
        assert_eq!(
            verify_signature("v1=00", body, concat!("wh", "sec_test"), 0, 300),
            Err("no timestamp")
        );
        // a second (rotated) signature next to a wrong one still verifies
        let good = h.split(',').nth(1).unwrap();
        assert!(verify_signature(
            &format!("t=1700000000,v1=deadbeef,{good}"),
            body,
            concat!("wh", "sec_test"),
            1_700_000_000,
            300
        )
        .is_ok());
    }
}
