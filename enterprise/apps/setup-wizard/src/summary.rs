//! The downloadable build summary (JSON and Markdown). Built only from serialisable state,
//! which cannot hold a `Secret`: the relay key appears masked (it is, of course, in the
//! provider.json the summary describes).

use crate::state::Wizard;
use serde_json::{json, Value};

pub fn summary_json(w: &Wizard) -> Value {
    let sel = w.components.selected.as_ref();
    json!({
        "title": "Taro Enterprise package build summary",
        "generated_at": crate::util::rfc3339(crate::util::now_secs()),
        "generated_by": crate::config::BUILT_BY,
        "identity": w.identity,
        "branding": {
            "primary_color": w.branding.primary_color,
            "theme": w.branding.theme,
            "tokens": rm_brand::Color::parse(&w.branding.primary_color).ok().map(rm_brand::Tokens::derive),
            "logo": w.branding.logo.as_ref().map(|u| json!({"width": u.info.width, "height": u.info.height, "bytes": u.info.bytes, "sha256": u.sha256})),
            "icon": w.branding.icon.as_ref().map(|u| json!({"width": u.info.width, "height": u.info.height, "bytes": u.info.bytes, "sha256": u.sha256})),
        },
        "relay": {
            "host": w.relay.host,
            "port": w.relay.port,
            "admission_key": w.relay.key_masked,
            "client_updates": w.relay.client_updates,
        },
        "release": sel.map(|s| json!({
            "tag": s.tag,
            "enterprise_version": s.enterprise_version,
            "taro_version": s.taro_version,
            "relay_version": s.relay_version,
            "signed_by": s.signed_by,
        })),
        "platforms": w.components.platforms,
        "steps": w.steps(),
        "build": w.build.result.as_ref().map(|r| json!({
            "dir": r.dir,
            "built_at": r.built_at,
            "provider_json_sha256": r.provider_json_sha256,
            "artifacts": r.artifacts,
        })),
        "validation": w.validation,
    })
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

pub fn summary_markdown(w: &Wizard) -> String {
    let mut md = String::new();
    md.push_str("# Taro Enterprise package build summary\n\n");
    md.push_str(&format!(
        "Generated {} by {}. The relay admission key is shown masked.\n\n",
        crate::util::rfc3339(crate::util::now_secs()),
        crate::config::BUILT_BY
    ));
    md.push_str("## Provider\n\n| Setting | Value |\n|---|---|\n");
    let mut row = |k: &str, v: &str| md.push_str(&format!("| {k} | {} |\n", cell(v)));
    if let Some(i) = &w.identity {
        row("Provider ID", &i.provider_id);
        row("Product name", &i.name);
        row("Short name", &i.short_name);
        row("Vendor", &i.vendor);
        row("Support URL", i.support_url.as_deref().unwrap_or("-"));
        row("Support email", i.support_email.as_deref().unwrap_or("-"));
    }
    row("Primary colour", &w.branding.primary_color);
    row(
        "Theme",
        &serde_json::to_value(w.branding.theme)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
    );
    row(
        "Logo",
        if w.branding.logo.is_some() {
            "logo.png"
        } else {
            "-"
        },
    );
    row(
        "Icon",
        if w.branding.icon.is_some() {
            "icon.png"
        } else {
            "-"
        },
    );
    row("Relay", &format!("{}:{}", w.relay.host, w.relay.port));
    row(
        "Admission key",
        w.relay.key_masked.as_deref().unwrap_or("-"),
    );
    row(
        "Client updates",
        &serde_json::to_value(w.relay.client_updates)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
    );
    if let Some(s) = &w.components.selected {
        row("Enterprise release", &s.tag);
        row("Taro version (clients)", &s.taro_version);
        row("Relay version", s.relay_version.as_deref().unwrap_or("-"));
        row("Component manifest signed by", &s.signed_by);
    }
    md.push_str("\n## Steps\n\n| Step | Status | Summary |\n|---|---|---|\n");
    for st in w.steps() {
        md.push_str(&format!(
            "| {} | {} | {} |\n",
            cell(st["title"].as_str().unwrap_or("")),
            cell(st["status"].as_str().unwrap_or("")),
            cell(st["summary"].as_str().unwrap_or(""))
        ));
    }
    if let Some(r) = &w.build.result {
        md.push_str(&format!(
            "\n## Output\n\n`{}` (built {})\n\n| File | sha256 | Size |\n|---|---|---|\n",
            r.dir, r.built_at
        ));
        for a in &r.artifacts {
            md.push_str(&format!(
                "| {} | `{}` | {} |\n",
                cell(&a.path),
                a.sha256,
                crate::util::human_size(a.size)
            ));
        }
    }
    if let Some(v) = &w.validation {
        md.push_str("\n## Validation\n\n| Check | Result | Detail |\n|---|---|---|\n");
        for c in &v.checks {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                cell(&c.name),
                c.status,
                cell(&c.detail)
            ));
        }
    }
    md
}
