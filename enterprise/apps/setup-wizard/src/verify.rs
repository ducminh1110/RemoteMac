//! Step 9: verification of the deployed portal.

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize)]
pub struct VCheck {
    pub id: String,
    pub name: String,
    /// ok | fail | skipped
    pub status: String,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerificationReport {
    pub portal_url: Option<String>,
    pub checks: Vec<VCheck>,
    pub at: u64,
}

pub fn vcheck(
    id: &str,
    name: &str,
    status: &str,
    detail: impl Into<String>,
    hint: Option<&str>,
) -> VCheck {
    VCheck {
        id: id.into(),
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
        hint: hint.map(str::to_string),
    }
}

async fn expect_200(
    http: &reqwest::Client,
    id: &str,
    name: &str,
    url: &str,
    hint: &str,
) -> (VCheck, Option<Value>) {
    match http.get(url).send().await {
        Ok(r) if r.status().as_u16() == 200 => {
            let v = r.json::<Value>().await.ok();
            (vcheck(id, name, "ok", format!("GET {url} -> 200"), None), v)
        }
        Ok(r) => {
            let s = r.status();
            let body = r.text().await.unwrap_or_default();
            let body = crate::redact::redact(&body.chars().take(200).collect::<String>());
            (
                vcheck(
                    id,
                    name,
                    "fail",
                    format!("GET {url} -> {s} {body}"),
                    Some(hint),
                ),
                None,
            )
        }
        Err(e) => (
            vcheck(
                id,
                name,
                "fail",
                format!("GET {url} failed: {}", e.without_url()),
                Some(hint),
            ),
            None,
        ),
    }
}

/// Names of enabled login methods from `/api/v1/auth/providers`. Accepts either a list of
/// strings / objects, or `{ "providers": [...] }`.
pub fn provider_names(v: &Value) -> Option<Vec<String>> {
    let list = v
        .as_array()
        .or_else(|| v.get("providers").and_then(Value::as_array))?;
    Some(
        list.iter()
            .filter_map(|p| {
                p.as_str()
                    .map(str::to_string)
                    .or_else(|| p.get("id").and_then(Value::as_str).map(str::to_string))
                    .or_else(|| p.get("name").and_then(Value::as_str).map(str::to_string))
            })
            .collect(),
    )
}

pub async fn portal_checks(http: &reqwest::Client, portal: &str) -> Vec<VCheck> {
    let base = portal.trim_end_matches('/');
    let (h, _) = expect_200(
        http,
        "healthz",
        "Portal health (/healthz)",
        &format!("{base}/healthz"),
        "The portal is not answering. Check `docker compose ps` and the portal logs on the server.",
    )
    .await;
    let (r, _) = expect_200(
        http,
        "readyz",
        "Database migrated (/readyz)",
        &format!("{base}/readyz"),
        "The portal runs but is not ready: usually the database migrations have not finished or the database is unreachable.",
    )
    .await;
    let (mut p, body) = expect_200(
        http,
        "providers",
        "Login methods (/api/v1/auth/providers)",
        &format!("{base}/api/v1/auth/providers"),
        "The auth API is not answering; check the portal logs.",
    )
    .await;
    if p.status == "ok" {
        match body.as_ref().and_then(provider_names) {
            Some(n) if !n.is_empty() => p.detail = format!("Enabled: {}", n.join(", ")),
            Some(_) => {
                p.status = "fail".into();
                p.detail = "No login methods are enabled".into();
                p.hint =
                    Some("Enable at least one login method in the portal configuration.".into());
            }
            None => {
                p.status = "fail".into();
                p.detail = "Response was not a list of providers".into();
                p.hint = None;
            }
        }
    }
    vec![h, r, p]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn providers_shapes() {
        assert_eq!(
            provider_names(&json!(["password", "github"])).unwrap(),
            vec!["password", "github"]
        );
        assert_eq!(
            provider_names(&json!({"providers":[{"id":"oidc"},{"name":"Email"}]})).unwrap(),
            vec!["oidc", "Email"]
        );
        assert!(provider_names(&json!({"x":1})).is_none());
    }
}
