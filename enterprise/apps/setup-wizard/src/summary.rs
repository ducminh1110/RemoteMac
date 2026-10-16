//! Non-secret documents: `deploy/deployment.json` (committed to the new repository) and the
//! downloadable deployment summary (JSON + Markdown). None of these contain credentials:
//! they are built only from serialisable state, which cannot hold a `Secret`.

use crate::state::Wizard;
use serde_json::{json, Value};

pub fn logo_path(w: &Wizard) -> Option<String> {
    w.branding
        .logo
        .as_ref()
        .map(|u| format!("deploy/branding/logo.{}", u.info.kind.ext()))
}

pub fn favicon_path(w: &Wizard) -> Option<String> {
    w.branding
        .favicon
        .as_ref()
        .map(|u| format!("deploy/branding/favicon.{}", u.info.kind.ext()))
}

/// Contents of `deploy/deployment.json`.
pub fn deployment_json(w: &Wizard) -> Value {
    let d = w.deployment.as_ref();
    let i = w.identity.as_ref();
    json!({
        "schema_version": 1,
        "generated_by": format!("TaroEnterpriseSetup {}", env!("CARGO_PKG_VERSION")),
        "generated_at": crate::util::rfc3339(crate::util::now_secs()),
        "environment": d.map(|d| d.environment),
        "profile": d.map(|d| d.profile.clone()),
        "portal_url": d.map(|d| d.portal_url.clone()),
        "ops_url": d.map(|d| d.ops_url.clone()),
        "provider_adapter": d.map(|d| d.provider_adapter.clone()),
        "bootstrap_admin_email": d.map(|d| d.admin_email.clone()),
        "deployment_name": i.map(|i| i.deployment_name.clone()),
        "operator_name": i.map(|i| i.operator_name.clone()),
        "support_contact": i.map(|i| i.support_contact.clone()),
        "modules": i.map(|i| i.modules.clone()).unwrap_or_default(),
        "branding": {
            "primary_color": w.branding.primary_color,
            "theme": w.branding.theme,
            "density": w.branding.density,
            "logo": logo_path(w),
            "favicon": favicon_path(w),
        },
        "domain": {
            "strategy": w.domain.strategy,
            "name": w.domain.domain,
            "server_target": w.domain.target,
        },
        "billing": {
            "provider": if w.billing_enabled() { Some("stripe") } else { None },
            "mode": w.billing.mode,
            "webhook_path": "/webhooks/stripe",
            "webhook_events": crate::stripe::WEBHOOK_EVENTS,
        }
    })
}

/// Downloadable summary (JSON). Includes check results but no secrets.
pub fn summary_json(w: &Wizard) -> Value {
    json!({
        "title": "Taro Enterprise deployment summary",
        "generated_at": crate::util::rfc3339(crate::util::now_secs()),
        "deployment": deployment_json(w),
        "steps": w.steps(),
        "stripe_account": w.billing.account.as_ref().map(|a| json!({"id": a.id, "display_name": a.display_name})),
        "stripe_webhook_endpoint": w.billing.webhook.as_ref().map(|h| json!({"id": h.id, "url": h.url})),
        "github": {
            "login": w.github.user.as_ref().map(|u| u.login.clone()),
            "repository": w.github.repo.as_ref().map(|r| json!({"full_name": r.full_name, "url": r.html_url, "private": r.private})),
            "actions_secrets_set": w.github.plan.as_ref().map(|p| p.iter().filter(|a| a.id.starts_with("secret:") && a.status == "ok").map(|a| a.id.trim_start_matches("secret:").to_string()).collect::<Vec<_>>()),
        },
        "release": w.release.release.as_ref().map(|r| json!({
            "tag": r.tag_name, "url": r.html_url,
            "assets": r.assets.iter().map(|a| json!({"name": a.name, "size": a.size})).collect::<Vec<_>>()
        })),
        "domain_verification": w.domain.verification,
        "verification": w.verification,
    })
}

pub fn summary_markdown(w: &Wizard) -> String {
    let d = deployment_json(w);
    let s = |v: &Value| match v {
        Value::Null => "-".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut md = String::new();
    md.push_str("# Taro Enterprise deployment summary\n\n");
    md.push_str(&format!(
        "Generated {} by TaroEnterpriseSetup {}. Contains no secrets.\n\n",
        crate::util::rfc3339(crate::util::now_secs()),
        env!("CARGO_PKG_VERSION")
    ));
    md.push_str("## Deployment\n\n| Setting | Value |\n|---|---|\n");
    for (k, label) in [
        ("deployment_name", "Deployment name"),
        ("environment", "Environment"),
        ("profile", "Profile"),
        ("portal_url", "Portal URL"),
        ("ops_url", "Ops console URL"),
        ("provider_adapter", "Provider adapter"),
        ("bootstrap_admin_email", "Bootstrap admin"),
        ("operator_name", "Operator"),
        ("support_contact", "Support contact"),
    ] {
        md.push_str(&format!("| {label} | {} |\n", s(&d[k]).replace('|', "\\|")));
    }
    md.push_str(&format!("| Modules | {} |\n", s(&d["modules"])));
    md.push_str(&format!("| Domain | {} |\n", s(&d["domain"]["name"])));
    md.push_str(&format!(
        "| Primary colour | {} ({}, {}) |\n",
        s(&d["branding"]["primary_color"]),
        s(&d["branding"]["theme"]),
        s(&d["branding"]["density"])
    ));
    md.push_str("\n## Steps\n\n| Step | Status | Summary |\n|---|---|---|\n");
    for st in w.steps() {
        md.push_str(&format!(
            "| {} | {} | {} |\n",
            s(&st["title"]),
            s(&st["status"]),
            s(&st["summary"]).replace('|', "\\|")
        ));
    }
    if let Some(r) = &w.github.repo {
        md.push_str(&format!(
            "\n## Repository\n\n{} ({}) - {}\n",
            r.full_name,
            if r.private { "private" } else { "PUBLIC" },
            r.html_url
        ));
    }
    if let Some(r) = &w.release.release {
        md.push_str(&format!(
            "\n## Release\n\n{} - {}\n\n",
            r.tag_name, r.html_url
        ));
        for a in &r.assets {
            md.push_str(&format!("- {} ({} bytes)\n", a.name, a.size));
        }
    }
    if let Some(v) = &w.verification {
        md.push_str("\n## Verification\n\n| Check | Result | Detail |\n|---|---|---|\n");
        for c in &v.checks {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                c.name,
                c.status,
                c.detail.replace('|', "\\|")
            ));
        }
    }
    md
}
