// Taro Enterprise Setup - wizard UI (vanilla JS, no build step).
// All DOM is built with createElement/textContent: no innerHTML with data, so nothing the
// server or a remote API returns can inject markup.
"use strict";

(function () {
  const TOKEN = new URL(location.href).searchParams.get("t") || "";
  let S = null; // last state from the server
  let current = "preflight";
  let busy = {}; // action key -> true while a request runs
  const drafts = {}; // non-secret input drafts by id, survive re-renders
  const errors = {}; // field errors by step
  let flash = null; // { kind, text } shown at top of the panel
  let pollTimer = null;
  const objectUrls = { logo: null, favicon: null };

  // ---------------------------------------------------------------- helpers
  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
      else if (k === "class") el.className = v;
      else if (k === "text") el.textContent = v;
      else if (k === "value") el.value = v;
      else if (k === "checked") el.checked = !!v;
      else if (k === "disabled") el.disabled = !!v;
      else el.setAttribute(k, v === true ? "" : String(v));
    }
    for (const k of kids.flat()) {
      if (k === null || k === undefined || k === false) continue;
      el.append(k instanceof Node ? k : document.createTextNode(String(k)));
    }
    return el;
  }
  const $ = (id) => document.getElementById(id);
  function announce(t) { const l = $("live"); l.textContent = ""; setTimeout(() => { l.textContent = t; }, 30); }

  async function api(method, path, body, raw) {
    const opts = { method, headers: { "X-Setup-Token": TOKEN } };
    if (raw !== undefined) { opts.body = raw; opts.headers["Content-Type"] = "application/octet-stream"; }
    else if (body !== undefined) { opts.body = JSON.stringify(body); opts.headers["Content-Type"] = "application/json"; }
    let res;
    try { res = await fetch(path, opts); }
    catch (e) { throw { error: "The wizard is not reachable. Is TaroEnterpriseSetup still running?" }; }
    const text = await res.text();
    let data;
    try { data = JSON.parse(text); } catch (_) { data = { ok: res.ok, error: text }; }
    if (data && data.state) { S = data.state; }
    if (!res.ok || data.ok === false) throw data;
    return data;
  }

  async function act(key, step, fn) {
    if (busy[key]) return;
    // Start the request first: fn reads the form fields synchronously, before the
    // re-render below replaces them (and clears password fields).
    let pending;
    try { pending = Promise.resolve(fn()); } catch (e) { pending = Promise.reject(e); }
    busy[key] = true; errors[step] = {}; flash = null; render();
    try {
      const r = await pending;
      if (r && r.message) { flash = { kind: "ok", text: r.message }; announce(r.message); }
    } catch (e) {
      const msg = (e && e.error) || "Something went wrong.";
      errors[step] = (e && e.fields) || {};
      flash = { kind: "fail", text: msg };
      announce(msg);
    } finally {
      busy[key] = false; render(); schedulePoll();
    }
  }

  function download(path, name) {
    act("dl:" + path, current, async () => {
      const res = await fetch(path, { headers: { "X-Setup-Token": TOKEN } });
      if (!res.ok) throw { error: "Download failed (" + res.status + ")" };
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = h("a", { href: url, download: name });
      document.body.append(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 5000);
    });
  }

  function draft(id, fallback) { return id in drafts ? drafts[id] : (fallback == null ? "" : fallback); }

  function field(step, o) {
    // o: { id, label, type, value, help, placeholder, secret, autocomplete }
    const err = (errors[step] || {})[o.name || o.id];
    const helpId = o.id + "-help", errId = o.id + "-err";
    const input = h("input", {
      id: o.id, name: o.name || o.id, type: o.type || "text",
      value: o.secret ? "" : draft(o.id, o.value),
      placeholder: o.placeholder, autocomplete: o.autocomplete || (o.secret ? "off" : null),
      spellcheck: o.secret ? "false" : null, inputmode: o.inputmode,
      "aria-describedby": [o.help ? helpId : null, err ? errId : null].filter(Boolean).join(" ") || null,
      "aria-invalid": err ? "true" : null,
      oninput: (e) => { if (!o.secret) drafts[o.id] = e.target.value; if (o.oninput) o.oninput(e); },
    });
    return h("div", { class: "field" },
      h("label", { for: o.id, text: o.label }),
      input,
      o.help ? h("div", { class: "help", id: helpId, text: o.help }) : null,
      err ? h("div", { class: "err", id: errId, text: err }) : null);
  }

  function btn(label, key, onclick, cls, extra) {
    const b = busy[key];
    return h("button", Object.assign({ type: "button", class: "btn " + (cls || ""), disabled: b, onclick, "aria-busy": b ? "true" : null }, extra || {}),
      b ? h("span", { class: "spin", "aria-hidden": "true" }) : null, label);
  }

  function badge(status) {
    const labels = { ok: "OK", warn: "Warning", fail: "Failed", skipped: "Skipped", pending: "Pending", running: "Running", todo: "To do", needs_config: "Needs config" };
    return h("span", { class: "badge " + status, text: labels[status] || status });
  }

  function checkList(items) {
    return h("ul", { class: "checks" }, items.map((c) =>
      h("li", {}, badge(c.status || (c.ok ? "ok" : "fail")),
        h("div", {},
          h("div", { class: "check-name", text: c.name || c.label }),
          c.detail ? h("div", { class: "check-detail", text: c.detail }) : null,
          c.hint ? h("div", { class: "check-hint", text: "How to fix: " + c.hint }) : null))));
  }

  function card(title, lead, ...kids) {
    return h("section", { class: "card" }, h("h2", { text: title }), lead ? h("p", { class: "lead", text: lead }) : null, ...kids);
  }

  function banner(kind, text) { return h("div", { class: "banner " + kind, role: kind === "fail" ? "alert" : "status" }, text); }

  function radioGroup(name, legend, options, value, onchange) {
    return h("fieldset", {}, h("legend", { text: legend }),
      options.map((o) => h("label", { class: "choice" + (o.disabled ? " disabled" : "") },
        h("input", { type: "radio", name, value: o.value, checked: value === o.value, disabled: o.disabled,
          onchange: () => { drafts[name] = o.value; if (onchange) onchange(o.value); } }),
        h("span", {}, o.label, o.note ? h("small", { text: o.note }) : null))));
  }

  // ---------------------------------------------------------------- steps
  const STEPS = {
    preflight: { title: "Preflight", lead: "Checks this PC can reach the services the setup needs.", render: renderPreflight },
    deployment: { title: "Deployment mode", lead: "Where and how Taro Enterprise will run.", render: renderDeployment },
    identity: { title: "Product identity", lead: "Names and modules shown to your users.", render: renderIdentity },
    branding: { title: "Branding", lead: "Logo, favicon, colour and look of the portal.", render: renderBranding },
    domain: { title: "Domain & TLS", lead: "Point a domain at your server and prove you control it.", render: renderDomain },
    billing: { title: "Billing", lead: "Connect Stripe. Test mode is the default.", render: renderBilling },
    github: { title: "GitHub bootstrap", lead: "Create the private deployment repository and store its secrets.", render: renderGithub },
    release: { title: "Build & release", lead: "Run the release workflow in your new repository.", render: renderRelease },
    verify: { title: "Verification", lead: "Check the deployed portal end to end and export a summary.", render: renderVerify },
  };

  function renderPreflight() {
    const p = S.preflight;
    return [
      card("System checks", "Operating system, HTTPS reachability of GitHub and Stripe, free disk space, and that this wizard is reachable only from this PC.",
        p ? checkList(p.checks) : h("p", { text: "Not run yet." }),
        h("div", { class: "actions" }, btn(p ? "Run again" : "Run checks", "preflight", () => act("preflight", "preflight", () => api("POST", "/api/preflight/run", {}))))),
    ];
  }

  function renderDeployment() {
    const d = S.deployment || {};
    const env = draft("environment", d.environment || "production");
    const e = errors.deployment || {};
    return [card("Environment and profile", null,
      radioGroup("environment", "Environment", [
        { value: "development", label: "Development", note: "http:// URLs allowed. For trying things out." },
        { value: "staging", label: "Staging", note: "https:// required." },
        { value: "production", label: "Production", note: "https:// required." },
      ], env, () => render()),
      h("fieldset", {}, h("legend", { text: "Profile" }),
        h("label", { class: "choice" }, h("input", { type: "radio", name: "profile", value: "single_server_compose", checked: true }),
          h("span", {}, "Single server with Docker Compose", h("small", { text: "Portal, ops console, database and Caddy (TLS) on one Linux server. Other profiles: Planned." }))),
        e.profile ? h("div", { class: "err", text: e.profile }) : null),
      h("div", { class: "grid2" },
        field("deployment", { id: "portal_url", label: "Public portal URL", type: "url", value: d.portal_url, placeholder: "https://portal.example.com", help: env === "development" ? "http:// is allowed in development." : "Must start with https://." }),
        field("deployment", { id: "ops_url", label: "Ops console URL", type: "url", value: d.ops_url, placeholder: "https://ops.example.com" })),
      h("div", { class: "field" }, h("label", { for: "provider_adapter", text: "Provider adapter" }),
        h("select", { id: "provider_adapter", "aria-describedby": "provider-help" }, h("option", { value: "mock", text: "mock - simulated Mac provider (the only adapter available today)" })),
        h("div", { class: "help", id: "provider-help", text: "Adapters for real Mac hosting providers are Planned. The mock adapter lets you run the full flow without renting hardware." }),
        e.provider_adapter ? h("div", { class: "err", text: e.provider_adapter }) : null),
      field("deployment", { id: "admin_email", label: "Bootstrap admin email", type: "email", value: d.admin_email, placeholder: "admin@example.com", autocomplete: "email", help: "The first administrator account of the portal." }),
      h("div", { class: "actions" }, btn("Save", "deployment", () => act("deployment", "deployment", () => api("POST", "/api/deployment", {
        environment: draft("environment", d.environment || "production"), profile: "single_server_compose",
        portal_url: $("portal_url").value, ops_url: $("ops_url").value, provider_adapter: $("provider_adapter").value, admin_email: $("admin_email").value,
      })))))];
  }

  function renderIdentity() {
    const i = S.identity || {};
    const mods = drafts.modules || i.modules || ["my_macs", "rentals", "billing", "team"];
    const desc = { my_macs: "My Macs - customers connect their own Macs", rentals: "Rentals - rent hosted Macs by the hour", billing: "Billing - Stripe checkout, invoices, subscriptions", team: "Team - organisations, roles and invitations" };
    const e = errors.identity || {};
    return [card("Identity", null,
      field("identity", { id: "deployment_name", label: "Deployment name", value: i.deployment_name, placeholder: "Acme Remote Macs" }),
      field("identity", { id: "operator_name", label: "Owner / operator name", value: i.operator_name, placeholder: "Acme Ltd." }),
      field("identity", { id: "support_contact", label: "Support contact", value: i.support_contact, placeholder: "support@example.com or https://help.example.com", help: "An email address or an https:// link." }),
      h("fieldset", {}, h("legend", { text: "Modules to enable" }),
        S.config.modules.map((m) => h("label", { class: "choice" },
          h("input", { type: "checkbox", value: m, checked: mods.includes(m), onchange: (ev) => {
            const cur = new Set(drafts.modules || i.modules || ["my_macs", "rentals", "billing", "team"]);
            if (ev.target.checked) cur.add(m); else cur.delete(m);
            drafts.modules = [...cur];
          } }), h("span", { text: desc[m] || m }))),
        e.modules ? h("div", { class: "err", text: e.modules }) : null),
      h("div", { class: "actions" }, btn("Save", "identity", () => act("identity", "identity", () => api("POST", "/api/identity", {
        deployment_name: $("deployment_name").value, operator_name: $("operator_name").value, support_contact: $("support_contact").value,
        modules: drafts.modules || i.modules || ["my_macs", "rentals", "billing", "team"],
      })))))];
  }

  function contrastWhite(hex) {
    const m = /^#([0-9a-f]{6})$/i.exec(hex || ""); if (!m) return null;
    const n = parseInt(m[1], 16), c = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); });
    const L = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    return 1.05 / (L + 0.05);
  }

  function renderBranding() {
    const b = S.branding;
    const color = draft("primary_color", b.primary_color);
    const theme = draft("theme", b.theme), density = draft("density", b.density);
    const ratio = contrastWhite(color);
    function uploader(kind, label) {
      const up = b[kind];
      const id = "file-" + kind;
      return h("div", { class: "field" },
        h("label", { for: id, text: label }),
        h("input", { id, type: "file", accept: "image/png,image/jpeg,image/webp", "aria-describedby": id + "-help",
          onchange: (ev) => {
            const f = ev.target.files[0]; if (!f) return;
            act("up:" + kind, "branding", async () => {
              const r = await api("POST", "/api/branding/" + kind, undefined, f);
              if (objectUrls[kind]) URL.revokeObjectURL(objectUrls[kind]);
              objectUrls[kind] = URL.createObjectURL(f);
              return r;
            });
          } }),
        h("div", { class: "help", id: id + "-help", text: "PNG, JPEG or WebP; max 1 MB and 1024x1024 px. SVG is not accepted." }),
        up ? h("div", { class: "row help" }, "Accepted: " + up.info.kind.toUpperCase() + " " + up.info.width + "x" + up.info.height + ", " + Math.round(up.info.bytes / 1024) + " KB",
          h("button", { type: "button", class: "btn ghost", onclick: () => act("rm:" + kind, "branding", () => api("DELETE", "/api/branding/" + kind)) }, "Remove")) : null);
    }
    const pv = h("div", { class: "preview " + theme + " " + density, "aria-label": "Live preview" },
      h("div", { class: "pv-bar" },
        objectUrls.logo && b.logo ? h("img", { class: "pv-logo", src: objectUrls.logo, alt: "Logo preview" }) : h("span", { class: "pv-logo", "aria-hidden": "true" }),
        h("strong", { text: (S.identity && S.identity.deployment_name) || "Your deployment" })),
      h("div", { class: "pv-body" }, h("p", {}, "Welcome back. ", h("span", { class: "pv-link", text: "View your Macs" })), h("span", { class: "pv-btn", text: "Rent a Mac" })));
    if (/^#[0-9a-f]{6}$/i.test(color)) pv.style.setProperty("--p", color);
    return [
      card("Logo and favicon", "Files are checked by their content (magic bytes), not their name.", uploader("logo", "Logo"), uploader("favicon", "Favicon")),
      card("Look", null,
        h("div", { class: "field" }, h("label", { for: "primary_color", text: "Primary colour" }),
          h("div", { class: "row" },
            h("input", { type: "color", "aria-label": "Pick primary colour", value: /^#[0-9a-f]{6}$/i.test(color) ? color : "#0a66c2", oninput: (e) => { drafts.primary_color = e.target.value.toUpperCase(); render(); } }),
            h("input", { id: "primary_color", class: "hex", type: "text", value: color, maxlength: 7, oninput: (e) => { drafts.primary_color = e.target.value; }, onchange: () => render(), "aria-describedby": "pc-help" })),
          h("div", { class: "help", id: "pc-help", text: "Format #RRGGBB." + (ratio ? " Contrast against white: " + ratio.toFixed(2) + ":1." : "") }),
          ratio && ratio < 4.5 ? h("div", { class: "banner warn", text: "Below 4.5:1 - not readable as text or button labels on white. Choose a darker colour for text use." }) : null,
          (errors.branding || {}).primary_color ? h("div", { class: "err", text: errors.branding.primary_color }) : null),
        h("div", { class: "grid2" },
          radioGroup("theme", "Theme", [{ value: "light", label: "Light" }, { value: "dark", label: "Dark" }], theme, () => render()),
          radioGroup("density", "Density", [{ value: "comfortable", label: "Comfortable" }, { value: "compact", label: "Compact" }], density, () => render())),
        h("div", { class: "field" }, h("div", { class: "label", text: "Preview" }), pv),
        b.contrast_warning && !(ratio && ratio < 4.5) ? banner("warn", b.contrast_warning) : null,
        h("div", { class: "actions" }, btn("Save branding", "branding", () => act("branding", "branding", () => api("POST", "/api/branding", { primary_color: draft("primary_color", b.primary_color), theme, density }))))),
    ];
  }

  function renderDomain() {
    const d = S.domain;
    const zone = S.config.managed_zone;
    const strat = draft("strategy", d.strategy || "custom");
    const out = [card("Domain strategy", null,
      radioGroup("strategy", "Strategy", [
        { value: "managed", label: "Taro-managed subdomain", note: zone ? "A subdomain of " + zone + "." : "Needs config: no Taro-managed DNS zone is configured (TARO_SETUP_MANAGED_ZONE).", disabled: !zone },
        { value: "custom", label: "Your own domain", note: "e.g. portal.example.com - you create the DNS records at your DNS provider." },
      ], strat, () => render()),
      strat === "managed" && zone
        ? field("domain", { id: "label", label: "Subdomain label", value: d.domain && zone && d.domain.endsWith("." + zone) ? d.domain.slice(0, -zone.length - 1) : "", placeholder: "acme", help: "Becomes <label>." + zone })
        : field("domain", { id: "domain", label: "Domain", value: d.domain, placeholder: "portal.example.com" }),
      field("domain", { id: "target", label: "Server target", value: d.target, placeholder: "203.0.113.10 or server.example.net", help: "Your server's public IP address (A/AAAA record) or host name (CNAME record)." }),
      h("div", { class: "actions" }, btn("Generate DNS records", "domain", () => act("domain", "domain", () => api("POST", "/api/domain", {
        strategy: strat, domain: $("domain") ? $("domain").value : "", label: $("label") ? $("label").value : "", target: $("target").value,
      })))))];
    if (d.records && d.records.length) {
      out.push(card("DNS records to create", "TLS is handled on the server by Caddy (on-demand certificates). The wizard only verifies the result.",
        h("table", { class: "records" }, h("thead", {}, h("tr", {}, h("th", { text: "Type" }), h("th", { text: "Name" }), h("th", { text: "Value" }), h("th", { text: "Purpose" }))),
          h("tbody", {}, d.records.map((r) => h("tr", {}, h("td", { text: r.kind }), h("td", {}, h("code", { text: r.name })), h("td", {}, h("code", { text: r.value }),
            " ", h("button", { type: "button", class: "btn ghost", "aria-label": "Copy value of " + r.kind + " record", onclick: () => { navigator.clipboard && navigator.clipboard.writeText(r.value).then(() => announce("Copied")); } }, "Copy")), h("td", { text: r.purpose }))))),
        d.verification ? h("div", {},
          banner(d.verification.dns_ok && d.verification.https_ok ? "ok" : "fail",
            d.verification.dns_ok && d.verification.https_ok ? "DNS and HTTPS verified." : "Not verified yet: complete is shown only when both DNS and HTTPS pass."),
          checkList(d.verification.checks)) : null,
        h("div", { class: "actions" }, btn("Verify", "domain-verify", () => act("domain-verify", "domain", () => api("POST", "/api/domain/verify", {}))))));
    }
    return out;
  }

  function renderBilling() {
    const b = S.billing;
    if (!S.billing_enabled) return [card("Billing is not enabled", "The billing module is switched off in Product identity, so this step is skipped.")];
    const live = !!drafts.live_confirmed;
    const portal = S.deployment ? S.deployment.portal_url + "/webhooks/stripe" : null;
    const out = [card("Stripe secret key", "Paste a test-mode secret key (sk_test_...) or restricted key (rk_test_...). It is checked with Stripe (GET /v1/account) and then kept in memory only.",
      b.account ? banner(b.mode === "live" ? "warn" : "ok", "Key valid for Stripe account " + b.account.id + (b.account.display_name ? " (" + b.account.display_name + ")" : "") + (b.mode === "live" ? " - LIVE MODE" : " - test mode")) : null,
      b.key_error ? banner("fail", b.key_error) : null,
      field("billing", { id: "key", label: b.key_present ? "Replace secret key" : "Secret key", type: "password", secret: true, placeholder: "sk_test_...", help: "Never shown again, never written to disk." }),
      h("label", { class: "choice" }, h("input", { type: "checkbox", checked: live, onchange: (e) => { drafts.live_confirmed = e.target.checked; render(); } }),
        h("span", {}, "Use live mode", h("small", { text: "Real charges. Only for a production go-live." }))),
      live ? field("billing", { id: "live_typed", label: "Type LIVE to confirm live mode", value: "", autocomplete: "off" }) : null,
      h("div", { class: "actions" }, btn("Validate key", "key", () => act("key", "billing", async () => {
        const k = $("key").value; $("key").value = "";
        return api("POST", "/api/billing/key", { key: k, live_confirmed: live, live_typed: $("live_typed") ? $("live_typed").value : "" });
      }))))];
    out.push(card("Webhook", portal ? "Endpoint URL: " + portal : "Save the Deployment mode step first.",
      b.webhook_secret_present ? banner("ok", "Webhook signing secret stored in memory (" + (b.webhook_secret_source === "created" ? "captured when the endpoint was created" : "entered") + ").") : null,
      field("billing", { id: "webhook_secret", label: "Webhook signing secret", type: "password", secret: true, placeholder: "whsec_...", help: "Stripe dashboard > Developers > Webhooks > your endpoint > Signing secret. Not needed if the wizard creates the endpoint." }),
      h("div", { class: "actions" }, btn("Store secret", "whsec", () => act("whsec", "billing", async () => {
        const v = $("webhook_secret").value; $("webhook_secret").value = "";
        return api("POST", "/api/billing/webhook-secret", { secret: v });
      }), "secondary"), btn("Check existing endpoint", "wh-check", () => act("wh-check", "billing", () => api("POST", "/api/billing/webhook/check", {})), "secondary", { disabled: !b.account || busy["wh-check"] })),
      b.webhook_note ? banner(b.webhook_missing_events.length ? "warn" : "info", b.webhook_note) : null,
      b.webhook_missing_events.length ? h("p", { class: "help", text: "Missing events: " + b.webhook_missing_events.join(", ") }) : null,
      h("details", { open: !!drafts.wh_open, ontoggle: (e) => { drafts.wh_open = e.target.open; } }, h("summary", { text: "Create the endpoint in Stripe" }),
        h("p", { class: "help", text: "Creates POST /v1/webhook_endpoints with url " + (portal || "<portal>/webhooks/stripe") + " and events: " + S.config.webhook_events.join(", ") + "." }),
        h("label", { class: "choice" }, h("input", { type: "checkbox", id: "wh-confirm", onchange: (e) => { drafts.wh_confirm = e.target.checked; render(); }, checked: !!drafts.wh_confirm }), h("span", { text: "Yes, create this webhook endpoint in my Stripe account." })),
        h("div", { class: "actions" }, btn("Create endpoint", "wh-create", () => act("wh-create", "billing", () => api("POST", "/api/billing/webhook/create", { confirm: !!drafts.wh_confirm })), "", { disabled: !drafts.wh_confirm || !b.account || busy["wh-create"] })))));
    out.push(card("Billing smoke test", "Creates a test-mode Checkout Session for 1 x 1.00 USD (100 minor units), shows what Stripe returned, then expires it. Test mode only.",
      b.smoke ? h("div", {}, banner(b.smoke.ok ? "ok" : "fail", b.smoke.detail),
        b.smoke.session_id ? h("dl", { class: "kv" }, h("dt", { text: "Session id" }), h("dd", {}, h("code", { text: b.smoke.session_id })), h("dt", { text: "Checkout URL" }), h("dd", {}, b.smoke.session_url ? h("a", { href: b.smoke.session_url, target: "_blank", rel: "noopener noreferrer", text: "open (now expired)" }) : "-"), h("dt", { text: "Expired" }), h("dd", { text: b.smoke.expired ? "yes" : "no" })) : null) : null,
      h("label", { class: "choice" }, h("input", { type: "checkbox", checked: !!drafts.smoke_confirm, onchange: (e) => { drafts.smoke_confirm = e.target.checked; render(); } }), h("span", { text: "Run the smoke test against my Stripe test account." })),
      h("div", { class: "actions" }, btn("Run smoke test", "smoke", () => act("smoke", "billing", () => api("POST", "/api/billing/smoke", { confirm: !!drafts.smoke_confirm })), "secondary", { disabled: !drafts.smoke_confirm || b.mode !== "test" || busy.smoke }))));
    return out;
  }

  const PERMS = [
    ["Administration", "Read and write", "create the repository from the template, set workflow permissions"],
    ["Contents", "Read and write", "commit deploy/deployment.json and branding files"],
    ["Actions", "Read and write", "start the release workflow and read its logs"],
    ["Secrets", "Read and write", "store Stripe and Taro secrets as Actions secrets"],
    ["Workflows", "Read and write", "the template contains workflow files"],
    ["Metadata", "Read-only", "required by GitHub for every fine-grained token"],
  ];

  function renderGithub() {
    const g = S.github;
    const out = [];
    if (g.user) {
      out.push(card("GitHub account", null, banner("ok", "Authenticated as " + g.user.login + " (" + ({ pat: "fine-grained token", device: "device flow", remembered: "remembered token" }[g.auth_method] || g.auth_method) + ")."),
        g.user.scopes ? h("p", { class: "help", text: "Token scopes: " + (g.user.scopes.join(", ") || "none") }) : null,
        h("div", { class: "actions" }, btn("Sign out", "logout", () => act("logout", "github", () => api("POST", "/api/github/logout", {})), "secondary"))));
    } else {
      const d = g.device;
      out.push(card("Sign in with GitHub (device flow)", S.config.device_flow_available ? "An OAuth client id is configured." : "Needs config: set TARO_SETUP_GITHUB_CLIENT_ID or enter the client id of your OAuth/GitHub App (device flow enabled).",
        d.phase === "waiting" ? h("div", {},
          h("p", {}, "Open ", h("a", { href: d.verification_uri, target: "_blank", rel: "noopener noreferrer", text: d.verification_uri }), " and enter this code:"),
          h("div", { class: "code-box", text: d.user_code }),
          h("p", { class: "help", text: "Waiting for you to approve on GitHub (checking every " + d.interval + " s)..." }),
          h("div", { class: "actions" }, btn("Cancel", "dev-cancel", () => act("dev-cancel", "github", () => api("POST", "/api/github/device/cancel", {})), "secondary"))) :
          h("div", {},
            S.config.device_flow_available ? null : field("github", { id: "client_id", label: "OAuth client id", value: "", placeholder: "Iv1.0123456789abcdef" }),
            d.error ? banner("fail", d.error) : null,
            g.remember_supported ? h("label", { class: "choice" }, h("input", { type: "checkbox", id: "dev-remember" }), h("span", {}, "Remember on this PC", h("small", { text: "Encrypted with Windows DPAPI for your Windows account." }))) : null,
            h("div", { class: "actions" }, btn("Start device flow", "dev-start", () => act("dev-start", "github", () => api("POST", "/api/github/device/start", { client_id: $("client_id") ? $("client_id").value : null, remember: !!($("dev-remember") && $("dev-remember").checked) })))))));
      out.push(card("Or paste a fine-grained personal access token", "Create one at github.com > Settings > Developer settings > Fine-grained tokens, with the resource owner that will own the new repository and these repository permissions:",
        h("ul", { class: "perm-list" }, PERMS.map(([n, l, why]) => h("li", {}, h("strong", { text: n + ": " + l }), " - " + why))),
        h("p", { class: "help", text: "Deleting a repository from this wizard additionally needs delete_repo (classic) or Administration write. The token is checked with GET /user; its prefix is not trusted." }),
        field("github", { id: "token", label: "Token", type: "password", secret: true, placeholder: "github_pat_...", help: "Kept in memory only and sent only to api.github.com." }),
        g.remember_supported ? h("label", { class: "choice" }, h("input", { type: "checkbox", id: "pat-remember" }), h("span", {}, "Remember on this PC", h("small", { text: "Encrypted with Windows DPAPI in %APPDATA%\\TaroEnterpriseSetup." }))) : null,
        g.auth_error ? banner("fail", g.auth_error) : null,
        h("div", { class: "actions" }, btn("Validate token", "pat", () => act("pat", "github", async () => {
          const t = $("token").value; $("token").value = "";
          return api("POST", "/api/github/pat", { token: t, remember: !!($("pat-remember") && $("pat-remember").checked) });
        })))));
      if (g.remembered_available) out.push(card("Remembered token", "A token remembered on this PC (DPAPI) is available.",
        h("div", { class: "actions" }, btn("Use remembered token", "rem-use", () => act("rem-use", "github", () => api("POST", "/api/github/remembered/use", {}))),
          btn("Forget it", "rem-forget", () => act("rem-forget", "github", () => api("POST", "/api/github/remembered/forget", {})), "secondary"))));
      return out;
    }

    const owners = [g.user.login].concat((g.orgs || []).map((o) => o.login));
    const owner = draft("owner", g.owner || g.user.login);
    const locked = g.bootstrap_running || g.bootstrap_done;
    out.push(card("Repository", "A private repository is generated from the template and configured. Nothing happens until you confirm.",
      h("div", { class: "grid2" },
        h("div", { class: "field" }, h("label", { for: "owner", text: "Owner" }),
          h("select", { id: "owner", disabled: locked, onchange: (e) => { drafts.owner = e.target.value; } },
            owners.map((o) => {
              const org = (g.orgs || []).find((x) => x.login === o);
              const note = org ? (org.can_create_repos === true ? " (org, " + org.role + ")" : org.can_create_repos === false ? " (org - you cannot create repositories)" : " (org - permission unknown)") : " (you)";
              return h("option", { value: o, selected: o === owner ? "selected" : null, text: o + note });
            })),
          h("div", { class: "help" }, btn(g.orgs ? "Reload organisations" : "Load my organisations", "orgs", () => act("orgs", "github", () => api("POST", "/api/github/orgs", {})), "ghost"))),
        field("github", { id: "repo", label: "Repository name", value: g.repo_name || "taro-enterprise-deploy" })),
      field("github", { id: "template", label: "Template repository", value: g.template, help: "Must be a template repository the token can read." }),
      h("div", { class: "actions" }, btn("Review plan", "plan", () => act("plan", "github", () => api("POST", "/api/github/plan", { owner: $("owner").value, repo: $("repo").value, template: $("template").value })), "secondary", { disabled: locked || busy.plan }))));
    if (g.plan) {
      const running = g.bootstrap_running;
      out.push(card(g.bootstrap_done ? "Bootstrap complete" : running ? "Bootstrap running" : "What will happen", g.bootstrap_done || running ? null : "Exactly these actions, in this order. Secrets are encrypted on this PC with the repository's public key before upload and are never committed.",
        checkList(g.plan),
        g.bootstrap_error ? banner("fail", g.bootstrap_error) : null,
        !running && !g.bootstrap_done ? h("div", {},
          h("label", { class: "choice" }, h("input", { type: "checkbox", checked: !!drafts.gh_confirm, onchange: (e) => { drafts.gh_confirm = e.target.checked; render(); } }),
            h("span", { text: "I confirm: create the private repository " + (g.owner || owner) + "/" + (g.repo_name || "") + " and perform the actions above." })),
          h("div", { class: "actions" }, btn(g.created_by_wizard ? "Retry remaining actions" : "Create repository", "boot", () => act("boot", "github", () => api("POST", "/api/github/bootstrap", { owner: g.owner, repo: g.repo_name, template: g.template, confirm: !!drafts.gh_confirm })), "", { disabled: !drafts.gh_confirm || busy.boot }))) : null,
        g.repo ? h("p", {}, "Repository: ", h("a", { href: g.repo.html_url, target: "_blank", rel: "noopener noreferrer", text: g.repo.full_name }), g.repo.private ? " (private)" : " (PUBLIC!)") : null));
    }
    if (g.repo && g.created_by_wizard && !g.bootstrap_running && (g.bootstrap_error || S.release.error)) {
      out.push(card("Clean up / roll back", "A step failed after the repository was created. You can fix the cause and retry, or remove what was created:",
        h("ul", { class: "perm-list" },
          h("li", { text: "Retry: fix the issue (permissions, inputs) and press \"Retry remaining actions\" - creation is not repeated." }),
          h("li", { text: "Stripe: delete the webhook endpoint in the Stripe dashboard if the wizard created one." }),
          h("li", { text: "GitHub: delete the repository below, or on github.com under Settings > General > Danger Zone." })),
        field("github", { id: "confirm_name", label: "Type " + g.repo.full_name + " to delete it", value: "", autocomplete: "off" }),
        h("div", { class: "actions" }, btn("Delete the repository I just created", "del", () => act("del", "github", () => api("POST", "/api/github/delete-repo", { confirm_name: $("confirm_name").value })), "danger"))));
    }
    return out;
  }

  function renderRelease() {
    const r = S.release, g = S.github;
    if (!g.bootstrap_done) return [card("Not ready", "Finish the GitHub bootstrap first.")];
    const out = [card("Release workflow", "Runs .github/workflows/enterprise-release.yml in " + g.repo.full_name + " via workflow_dispatch.",
      field("release", { id: "version", label: "Version", value: r.version || "0.1.0", help: "The release must be tagged with exactly this version." }),
      h("div", { class: "actions" }, btn(r.attempts ? "Retry (dispatch again)" : "Start release", "release", () => act("release", "release", () => api("POST", "/api/release/start", { version: $("version").value })), "", { disabled: r.running || busy.release })),
      r.running ? banner("info", r.phase + "...") : null,
      r.error ? banner("fail", r.error) : null)];
    if (r.run) {
      out.push(card("Run", null,
        h("p", {}, h("a", { href: r.run.html_url, target: "_blank", rel: "noopener noreferrer", text: "Run #" + r.run.id }), " - " + (r.run.status || "") + (r.run.conclusion ? " / " + r.run.conclusion : "")),
        r.jobs.map((j) => h("div", {}, h("h3", { text: j.name + " - " + (j.conclusion || j.status || "") }),
          checkList(j.steps.map((s) => ({ name: s.name, status: s.conclusion === "success" ? "ok" : s.conclusion === "skipped" ? "skipped" : s.conclusion ? "fail" : s.status === "in_progress" ? "running" : "pending" }))))),
        r.log_tail ? h("div", {}, h("h3", { text: "Log tail (redacted)" }), h("pre", { class: "log", text: r.log_tail })) : null));
    }
    if (r.release) {
      out.push(card("Release " + r.release.tag_name, "Verified: the release exists and has assets.",
        h("p", {}, h("a", { href: r.release.html_url, target: "_blank", rel: "noopener noreferrer", text: r.release.html_url })),
        h("ul", {}, r.release.assets.map((a) => h("li", { text: a.name + " (" + Math.round(a.size / 1024) + " KB)" })))));
    }
    return out;
  }

  function renderVerify() {
    const v = S.verification;
    return [card("Deployed portal", S.deployment ? "Portal: " + S.deployment.portal_url : "Save the Deployment mode step first.",
      v ? checkList(v.checks) : h("p", { text: "Not run yet." }),
      h("div", { class: "actions" }, btn(v ? "Run again" : "Run verification", "verify", () => act("verify", "verify", () => api("POST", "/api/verify/run", {}))))),
    card("Export", "Deployment summary without any secrets.",
      h("div", { class: "actions" },
        btn("Download JSON", "dl:/api/summary.json", () => download("/api/summary.json", "taro-deployment-summary.json"), "secondary"),
        btn("Download Markdown", "dl:/api/summary.md", () => download("/api/summary.md", "taro-deployment-summary.md"), "secondary")))];
  }

  // ---------------------------------------------------------------- shell
  function render() {
    if (!S) return;
    const icons = { ok: "✓", warn: "!", fail: "×", running: "…", skipped: "–", needs_config: "?", todo: "" };
    const statusText = { ok: "complete", warn: "complete with warnings", fail: "failed", running: "in progress", skipped: "skipped", needs_config: "needs configuration", todo: "not done" };
    const list = $("step-list");
    list.replaceChildren(...S.steps.map((s, i) => h("li", {},
      h("button", { type: "button", class: "step-btn", "aria-current": s.id === current ? "step" : null, onclick: () => go(s.id) },
        h("span", { class: "dot " + s.status, "aria-hidden": "true", text: icons[s.status] || String(i + 1) }),
        h("span", {}, h("span", { class: "step-title", text: (i + 1) + ". " + s.title }),
          h("span", { class: "step-sum", text: s.summary }),
          h("span", { class: "sr-only", text: " (" + (statusText[s.status] || s.status) + ")" }))))));
    const st = STEPS[current];
    const active = document.activeElement && document.activeElement.id;
    const panel = $("panel");
    const idx = S.steps.findIndex((s) => s.id === current);
    const nav = h("div", { class: "actions" },
      idx > 0 ? h("button", { type: "button", class: "btn secondary", onclick: () => go(S.steps[idx - 1].id) }, "Back") : null,
      idx < S.steps.length - 1 ? h("button", { type: "button", class: "btn secondary", onclick: () => go(S.steps[idx + 1].id) }, "Next step") : null);
    panel.replaceChildren(...[
      h("div", { class: "panel-head" }, h("h1", { text: st.title }), h("p", { text: st.lead })),
      flash ? banner(flash.kind, flash.text) : null,
      ...st.render(),
      nav].filter(Boolean));
    if (active && $(active)) $(active).focus();
    $("app-version").textContent = "Version " + S.version + " - local wizard on this PC";
  }

  function go(id) {
    current = id; flash = null;
    try { sessionStorage.setItem("taroStep", id); } catch (_) {}
    render();
    const p = $("panel"); p.focus(); window.scrollTo({ top: 0 });
  }

  function needsPoll() {
    return S && (S.github.device.phase === "waiting" || S.github.bootstrap_running || S.release.running);
  }

  function schedulePoll() {
    if (pollTimer || !needsPoll()) return;
    pollTimer = setTimeout(async () => {
      pollTimer = null;
      const before = JSON.stringify(S);
      try { await api("GET", "/api/state"); } catch (_) {}
      if (JSON.stringify(S) !== before) render();
      schedulePoll();
    }, 2000);
  }

  function initTheme() {
    let t = "light";
    try { t = localStorage.getItem("taroTheme") || "light"; } catch (_) {}
    const apply = () => {
      document.documentElement.dataset.theme = t;
      const b = $("theme-toggle");
      b.setAttribute("aria-pressed", t === "dark" ? "true" : "false");
      b.textContent = t === "dark" ? "Light mode" : "Dark mode";
    };
    apply();
    $("theme-toggle").addEventListener("click", () => { t = t === "dark" ? "light" : "dark"; try { localStorage.setItem("taroTheme", t); } catch (_) {} apply(); });
  }

  document.addEventListener("DOMContentLoaded", async () => {
    initTheme();
    $("dl-log").addEventListener("click", () => download("/api/diagnostics.txt", "taro-setup-diagnostics.txt"));
    $("quit").addEventListener("click", async () => {
      if (!confirm("Quit the wizard? Secrets held in memory (tokens, keys) are discarded.")) return;
      try { await api("POST", "/api/shutdown", {}); } catch (_) {}
      $("panel").replaceChildren(h("div", { class: "card" }, h("h2", { text: "The wizard has stopped." }), h("p", { text: "You can close this tab." })));
    });
    try { const s = sessionStorage.getItem("taroStep"); if (s && STEPS[s]) current = s; } catch (_) {}
    try { await api("GET", "/api/state"); render(); schedulePoll(); }
    catch (e) { $("panel").replaceChildren(h("div", { class: "card" }, h("h2", { text: "Cannot load the wizard" }), h("p", { text: (e && e.error) || "Unknown error" }))); }
  });
})();
