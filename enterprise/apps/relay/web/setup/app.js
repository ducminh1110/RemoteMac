// Taro relay setup (Wizard 2). Shared core UI: it only knows the API; the provider's name,
// colour and logo come from the server (/api/state, /brand/*).
import { h, icon, status, container, kv, alert, pageHeader, breadcrumbs, button, busy, flash, confirmDialog, download } from "/common/console.js";

const token = new URLSearchParams(location.search).get("t") || "";
history.replaceState(null, "", "/");

const STEPS = [
  { id: "provider", name: "Provider package" },
  { id: "system", name: "Server checks" },
  { id: "network", name: "DNS and network" },
  { id: "tls", name: "Certificate" },
  { id: "firewall", name: "Firewall" },
  { id: "updates", name: "Update policy" },
  { id: "apply", name: "Review and apply" },
  { id: "done", name: "Summary" },
];

let st = null;
let current = "provider";
const form = { tls_mode: "acme", email: "", agree_tos: false, staging: false, cert: "", key: "", update_mode: "manual", window: "",
  portal_enabled: true, smtp_url: "", mail_from: "", app_download_url: "" };

async function api(path, body) {
  const opt = { method: body === undefined ? "GET" : "POST", headers: { "X-Setup-Token": token } };
  if (body !== undefined) { opt.headers["Content-Type"] = "application/json"; opt.body = JSON.stringify(body); }
  const r = await fetch(path, opt);
  const j = await r.json().catch(() => ({}));
  if (!r.ok) { const e = new Error(j.error || r.statusText); e.fields = j.fields; e.status = r.status; throw e; }
  return j;
}

const worst = (list) => (list || []).some((c) => c.status === "fail") ? "bad" : (list || []).some((c) => c.status === "warn") ? "warn" : list && list.length ? "done" : "";

function stepStatus(id) {
  if (!st) return "";
  switch (id) {
    case "provider": return st.provider ? "done" : "bad";
    case "system": return worst(st.system);
    case "network": return worst(st.network);
    case "tls": return st.tls ? (st.tls.status === "fail" ? "bad" : "done") : "";
    case "firewall": return st.firewall ? "done" : "";
    case "updates": return st.settings && st.settingsSaved ? "done" : "";
    case "apply": return st.applied ? (st.applied.status === "ok" ? "done" : "bad") : "";
    case "done": return st.applied && st.applied.status === "ok" ? "done" : "";
  }
  return "";
}

function renderNav() {
  document.getElementById("steps").replaceChildren(...STEPS.map((s, i) => {
    const cls = stepStatus(s.id);
    const mark = cls === "bad" || cls === "warn" ? "!" : String(i + 1);
    return h("li", { class: [cls, s.id === current ? "current" : ""].join(" ").trim() },
      h("span", { class: "marker", "aria-hidden": "true" }, cls === "done" ? h("span", { class: "i i-ok" }) : mark),
      h("button", { type: "button", "aria-current": s.id === current ? "step" : false, onclick: () => go(s.id) },
        h("span", { class: "n" }, `Step ${i + 1}`), h("span", { class: "name" }, s.name)));
  }));
  const done = STEPS.filter((x) => stepStatus(x.id) === "done").length;
  document.getElementById("side-foot").replaceChildren(h("dl", {},
    h("dt", {}, "Progress"), h("dd", {}, `${done} of ${STEPS.length} steps complete`),
    h("dt", {}, "Service"), h("dd", {}, st.service_running ? status("running") : status("stopped"))));
}

function go(id) {
  current = id;
  render();
  scrollTo(0, 0);
  document.getElementById("main").focus();
}
const idx = () => STEPS.findIndex((s) => s.id === current);
const next = () => idx() < STEPS.length - 1 && go(STEPS[idx() + 1].id);
const prev = () => idx() > 0 && go(STEPS[idx() - 1].id);

/** One step's page: breadcrumbs, header, content and the wizard's footer. */
function page({ title, desc, actions }, ...body) {
  const i = idx();
  return h("div", { class: "inner" },
    breadcrumbs([st.provider ? st.provider.product : "Relay", "Relay setup", STEPS[i].name]),
    pageHeader({ step: `Step ${i + 1} of ${STEPS.length}`, title, desc }),
    body,
    h("div", { class: "form-actions" },
      h("div", { class: "left" }, button("Download diagnostics", { kind: "link", icon: "download", onclick: diagnostics })),
      i > 0 && current !== "done" ? button("Previous", { onclick: prev }) : null,
      actions));
}

function checkTable(list, empty) {
  if (!list || !list.length) return h("div", { class: "table-empty" }, h("strong", {}, "Not checked yet"), empty);
  return h("div", { class: "table-wrap" }, h("table", { class: "table" },
    h("thead", {}, h("tr", {}, h("th", {}, "Status"), h("th", {}, "Check"), h("th", {}, "Result"))),
    h("tbody", {}, list.map((c) => h("tr", { class: c.status },
      h("td", { class: "st" }, status(c.status)),
      h("td", { class: "name" }, c.label),
      h("td", {}, c.detail, c.fix && c.status !== "ok" ? h("div", { class: "action" }, icon(c.status === "fail" ? "err" : "warn"), h("span", {}, h("strong", {}, "Recommended action: "), c.fix)) : null))))));
}

const counts = (list) => {
  const n = (s) => (list || []).filter((c) => c.status === s).length;
  return [n("ok") && status("ok", `${n("ok")} passed`), n("warn") && status("warn", `${n("warn")} warnings`), n("fail") && status("fail", `${n("fail")} failed`)]
    .filter(Boolean);
};

async function refresh() {
  const saved = st && st.settingsSaved;
  st = await api("/api/state");
  st.settingsSaved = saved;
  if (st.settings) {
    Object.assign(form, {
      tls_mode: st.settings.tls_mode, email: st.settings.email || form.email, staging: st.settings.staging,
      update_mode: st.settings.update_mode, window: st.settings.window || "",
      portal_enabled: st.settings.portal_enabled, mail_from: st.settings.mail_from || "", app_download_url: st.settings.app_download_url || "",
      cert: st.settings.tls_mode === "files" ? st.settings.cert : form.cert, key: st.settings.tls_mode === "files" ? st.settings.key : form.key,
    });
  }
  const p = st.provider;
  document.getElementById("product").textContent = p ? p.product : "Relay";
  document.title = p ? `Relay setup · ${p.product}` : "Relay setup";
  if (p) { document.getElementById("host").hidden = false; document.getElementById("host-name").textContent = p.relay_host; }
  const mode = document.getElementById("mode");
  mode.hidden = false;
  mode.textContent = st.configured ? "Reconfigure" : "First run";
  document.getElementById("version").textContent = `taro-relay ${st.version}`;
}

async function saveSettings(errBox) {
  for (const el of document.querySelectorAll("[aria-invalid]")) el.removeAttribute("aria-invalid");
  for (const el of document.querySelectorAll(".field .error")) el.remove();
  try {
    await api("/api/settings", form);
    st.settingsSaved = true;
    errBox && errBox.replaceChildren();
    return true;
  } catch (e) {
    errBox && errBox.replaceChildren(alert("err", e.message, e.fields ? h("ul", {}, e.fields.map((f) => h("li", {}, f.message))) : null));
    for (const f of e.fields || []) {
      const el = document.getElementById("f-" + f.field);
      if (el) { el.setAttribute("aria-invalid", "true"); el.closest(".field")?.append(h("p", { class: "error" }, icon("err"), f.message)); }
    }
    return false;
  }
}

function field({ id, label, desc, hint, opt }, input) {
  return h("div", { class: "field" },
    h("div", { class: "label-row" }, h("label", { for: id }, label), opt ? h("span", { class: "opt" }, "– optional") : null),
    desc ? h("p", { class: "desc" }, desc) : null, input, hint ? h("p", { class: "hint" }, hint) : null);
}

function tiles(name, value, onpick, items) {
  return h("div", { class: "tiles", role: "radiogroup", "aria-label": name },
    items.map(([v, t, d]) => h("label", { class: "tile" },
      h("input", { type: "radio", name, value: v, checked: value === v, onchange: () => onpick(v) }),
      h("span", { class: "t" }, t), h("span", { class: "d" }, d))));
}

async function runCheck(btn, label, path) {
  await busy(btn, label, async () => {
    try { await api(path, {}); await refresh(); render(); } catch (e) { flash("err", e.message); }
  });
}

const views = {
  provider() {
    const p = st.provider;
    if (!p) return page({ title: "Provider package missing", desc: "This relay has no valid provider folder." },
      container({ title: "Provider folder" },
        alert("err", "provider.json could not be read", st.provider_error || "No provider.json"),
        h("p", {}, "Install this relay from the relay package your Taro Enterprise package builder (Wizard 1) produced: it carries your provider folder at ", h("span", { class: "path" }, st.paths.provider), ".")));
    return page({
      title: st.configured ? "Change this relay's settings" : "Set up this relay",
      desc: st.configured
        ? "This server is already set up. You can change its settings; the relay's data (Mac IDs), its certificate and your provider folder are kept."
        : "This wizard checks this server, its DNS and network, obtains a certificate, opens the firewall and starts the relay as a systemd service.",
      actions: button("Next", { kind: "primary", onclick: next }),
    },
    st.config_error ? alert("err", "The current settings file cannot be read", st.config_error) : null,
    container({ title: "Provider", desc: "From the provider package this relay was installed with. Read-only." },
      kv([
        ["Product", p.product],
        ["Vendor", p.vendor],
        ["Provider ID", h("code", {}, p.provider_id)],
        ["Relay endpoint", h("code", {}, `${p.relay_host}:${p.relay_port}`)],
        ["Primary colour", h("span", {}, h("span", { class: "swatch" }), p.primary_color)],
        ["Support", p.support_url ? h("a", { href: p.support_url, target: "_blank", rel: "noreferrer" }, p.support_url) : p.support_email || "–"],
      ])),
    container({ title: "Installation" },
      kv([
        ["Installed version", st.installed ? `${st.installed.version}` : "not recorded"],
        ["Architecture", st.installed ? st.installed.arch : "–"],
        ["Service", st.service_running ? status("running") : status("stopped")],
        ["Provider folder", h("span", { class: "path" }, st.paths.provider)],
        ["Settings file", h("span", { class: "path" }, st.paths.runtime)],
        ["State folder", h("span", { class: "path" }, st.paths.state)],
      ])),
    alert("info", "The relay endpoint is fixed by your provider package", "Your Windows and Mac clients were built to connect to it. To use another host name, build new packages with the package builder."));
  },

  system() {
    const has = st.system && st.system.length;
    const run = button(has ? "Run again" : "Run checks", { icon: has ? "refresh" : null, kind: has ? "" : "primary", onclick: (ev) => runCheck(ev.currentTarget, "Checking…", "/api/checks/system") });
    const fails = (st.system || []).filter((c) => c.status === "fail").length;
    return page({
      title: "Server checks",
      desc: "Operating system, CPU architecture, administrator rights, systemd, disk space and the ports the relay needs.",
      actions: has ? button("Next", { kind: "primary", onclick: next }) : null,
    },
    has && fails ? alert("err", `${fails} check${fails > 1 ? "s" : ""} failed`, "Fix them before applying: each row says what to do.") : null,
    container({ title: "Checks", count: has ? st.system.length : null, desc: has ? h("span", { class: "counts" }, counts(st.system)) : null, actions: run, flush: true },
      checkTable(st.system, "Run the checks to inspect this server.")));
  },

  network() {
    const p = st.provider || {};
    const has = st.network && st.network.length;
    const run = button(has ? "Run again" : "Check DNS and network", { icon: has ? "refresh" : null, kind: has ? "" : "primary", onclick: (ev) => runCheck(ev.currentTarget, "Looking up…", "/api/checks/network") });
    return page({
      title: "DNS and network",
      desc: h("span", {}, "Clients connect to ", h("code", {}, p.relay_host || "?"), ". Its DNS records must point at this server's public address."),
      actions: has ? button("Next", { kind: "primary", onclick: next }) : null,
    },
    container({ title: "Network checks", count: has ? st.network.length : null, desc: has ? h("span", { class: "counts" }, counts(st.network)) : null, actions: run, flush: true },
      checkTable(st.network, "Resolves the relay's host name and compares it with this server's public addresses.")));
  },

  tls() {
    const errBox = h("div", {});
    const pick = (v) => { form.tls_mode = v; render(); };
    let fields = null;
    if (form.tls_mode === "acme") {
      fields = container({ title: "Let's Encrypt settings" },
        field({ id: "f-email", label: "Contact email", desc: "Let's Encrypt sends expiry and policy notices here." },
          h("input", { id: "f-email", type: "email", autocomplete: "email", placeholder: "ops@example.com", value: form.email, oninput: (e) => { form.email = e.target.value; } })),
        h("label", { class: "check" }, h("input", { id: "f-agree_tos", type: "checkbox", checked: form.agree_tos, onchange: (e) => (form.agree_tos = e.target.checked) }),
          h("span", {}, "I accept the Let's Encrypt Subscriber Agreement.")),
        h("label", { class: "check" }, h("input", { type: "checkbox", checked: form.staging, onchange: (e) => (form.staging = e.target.checked) }),
          h("span", {}, "Use the staging CA", h("span", { class: "d" }, "For testing: no rate limits, but browsers do not trust the certificate."))));
    } else if (form.tls_mode === "files") {
      fields = container({ title: "Certificate files", desc: "Paths on this server, readable by root. The relay reloads them when they change." },
        field({ id: "f-cert", label: "Certificate chain (PEM)" },
          h("input", { id: "f-cert", type: "text", class: "mono", placeholder: "/etc/ssl/relay/fullchain.pem", value: form.cert, oninput: (e) => (form.cert = e.target.value) })),
        field({ id: "f-key", label: "Private key (PEM)" },
          h("input", { id: "f-key", type: "text", class: "mono", placeholder: "/etc/ssl/relay/privkey.pem", value: form.key, oninput: (e) => (form.key = e.target.value) })));
    }
    const res = st.tls ? alert(st.tls.status === "ok" ? "ok" : st.tls.status === "fail" ? "err" : "info",
      st.tls.status === "ok" ? "Certificate ready" : st.tls.status === "fail" ? "Certificate not ready" : "No certificate",
      st.tls.detail, st.tls.fix ? h("div", {}, h("strong", {}, "Recommended action: "), st.tls.fix) : null) : null;
    const cert = st.certificate;
    return page({
      title: "Certificate",
      desc: "HTTPS for the relay's status page and remote health checks. Sessions between clients and Macs are end-to-end encrypted either way.",
      actions: [
        button(form.tls_mode === "acme" ? "Request certificate" : form.tls_mode === "files" ? "Validate files" : "Save", {
          kind: st.tls && st.tls.status !== "fail" ? "" : "primary",
          onclick: (ev) => busy(ev.currentTarget, form.tls_mode === "acme" ? "Requesting…" : "Checking…", async () => {
            if (await saveSettings(errBox)) { try { await api("/api/tls", {}); } catch (e) { flash("err", e.message); } await refresh(); render(); }
          }),
        }),
        st.tls && st.tls.status !== "fail" ? button("Next", { kind: "primary", onclick: next }) : null,
      ],
    },
    res, errBox,
    container({ title: "Certificate source" },
      tiles("tls", form.tls_mode, pick, [
        ["acme", "Let's Encrypt (recommended)", "Issued now through an HTTP-01 check on port 80 and renewed by the relay 30 days before expiry."],
        ["files", "Existing certificate files", "Managed elsewhere (your own CA or tooling). The relay reloads them on change."],
        ["off", "No HTTPS", "No public status page. Health checks stay local to this server."],
      ])),
    fields,
    cert ? container({ title: "Current certificate" },
      kv([["Names", cert.names.join(", ")], ["Issuer", cert.issuer], ["Expires in", `${cert.days_left} days`]])) : null);
  },

  firewall() {
    const fw = st.firewall;
    const canOpen = fw && fw.active && fw.kind !== "none";
    return page({
      title: "Firewall",
      desc: "The relay's port over TCP and UDP (video), plus 80 and 443 for the certificate and the status page.",
      actions: [
        canOpen ? button("Open ports", {
          onclick: async (ev) => {
            const ok = await confirmDialog({ title: "Open firewall ports", confirm: "Open ports",
              body: [h("p", {}, "These rules will be added to ", h("strong", {}, fw.kind), ":"), h("ul", {}, fw.needed.map((p) => h("li", {}, h("code", {}, p))))] });
            if (!ok) return;
            await busy(ev.currentTarget, "Opening…", async () => {
              try {
                const r = await api("/api/firewall/open", { confirm: true });
                flash("ok", r.done.length ? `${r.done.length} firewall rule(s) added` : "Nothing to change: the ports were already open");
                await api("/api/firewall/check", {}); await refresh(); render();
              } catch (e) { flash("err", e.message); }
            });
          },
        }) : null,
        fw ? button("Next", { kind: "primary", onclick: next }) : null,
      ],
    },
    container({ title: "Local firewall", actions: button(fw ? "Check again" : "Check firewall", { icon: fw ? "refresh" : null, kind: fw ? "" : "primary", onclick: (ev) => runCheck(ev.currentTarget, "Checking…", "/api/firewall/check") }) },
      fw ? kv([
        ["Firewall", fw.kind === "none" ? "none found" : fw.kind],
        ["State", fw.kind === "none" ? status("skip", "Not installed") : fw.active ? status("ok", "Active") : status("stopped", "Inactive")],
        ["Status", fw.detail],
      ]) : h("div", { class: "table-empty" }, h("strong", {}, "Not checked yet"), "Detects ufw, firewalld or nftables and the rules the relay needs.")),
    fw ? container({ title: "Required ports", count: fw.needed.length, flush: true },
      h("div", { class: "table-wrap" }, h("table", { class: "table" },
        h("thead", {}, h("tr", {}, h("th", {}, "Port"), h("th", {}, "Purpose"))),
        h("tbody", {}, fw.needed.map((p) => h("tr", {}, h("td", { class: "name" }, h("code", {}, p)), h("td", { class: "muted" }, purpose(p)))))))) : null,
    alert("warn", "Cloud firewalls are not visible from this server", "Open the same ports in your hosting provider's firewall or security group as well."));
  },

  updates() {
    const errBox = h("div", {});
    return page({
      title: "Update policy",
      desc: "Enterprise updates replace only the relay's shared core. Your provider folder, these settings, the certificate and the relay's data are never replaced.",
      actions: button("Save and continue", { kind: "primary", onclick: (ev) => busy(ev.currentTarget, "Saving…", async () => {
        if (await saveSettings(errBox)) { await refresh(); st.settingsSaved = true; flash("ok", "Update policy saved"); next(); }
      }) }),
    },
    errBox,
    container({ title: "Policy" },
      tiles("upd", form.update_mode, (v) => { form.update_mode = v; render(); }, [
        ["manual", "Manual", "Only when an administrator runs sudo taro-relay update."],
        ["notify", "Check and notify", "Checked hourly and reported by taro-relay status. Nothing is installed automatically."],
        ["auto", "Maintenance window", "Checked hourly, installed only inside the window. The relay restarts; sessions reconnect."],
      ]),
      form.update_mode === "auto" ? field({ id: "f-window", label: "Maintenance window (UTC)", hint: "For example Sun 03:00-05:00 or daily 02:00-04:00." },
        h("input", { id: "f-window", type: "text", class: "mono", placeholder: "Sun 03:00-05:00", value: form.window, oninput: (e) => { form.window = e.target.value; } })) : null),
    container({ title: "Portal and console", desc: "One web site on this server's HTTPS: customers use the portal, administrators the console. The relay port for the apps stays as it is." },
      h("label", { class: "check" }, h("input", { type: "checkbox", id: "f-portal_enabled", checked: form.portal_enabled, onchange: (e) => { form.portal_enabled = e.target.checked; render(); } }),
        h("span", {}, "Serve the Customer Portal and the Provider Console",
          h("span", { class: "d" }, `${siteOf()}/ (customers) and ${siteOf()}/console/ (administrators). Needs a certificate (step 4).`))),
      form.portal_enabled ? [
        field({ id: "f-smtp_url", label: "Mail server", opt: true, desc: "Customers sign in with a code sent by mail. Administrators sign in with a password and need no mail.",
          hint: (st.settings && st.settings.smtp_set ? "A mail server is set (leave empty to keep it, type - to remove it). " : "") + "smtps://user:password@smtp.example.com:465" },
          h("input", { id: "f-smtp_url", type: "password", class: "mono", autocomplete: "off", placeholder: "smtps://user:password@smtp.example.com:465", value: form.smtp_url, oninput: (e) => { form.smtp_url = e.target.value; } })),
        field({ id: "f-mail_from", label: "Sender", opt: true, hint: "For example Acme Cloud <no-reply@acme.example>." },
          h("input", { id: "f-mail_from", type: "text", value: form.mail_from, oninput: (e) => { form.mail_from = e.target.value; } })),
        field({ id: "f-app_download_url", label: "App download address", opt: true, desc: "Where the portal's Download button sends customers: your Windows package from the package builder." },
          h("input", { id: "f-app_download_url", type: "url", class: "mono", placeholder: "https://…", value: form.app_download_url, oninput: (e) => { form.app_download_url = e.target.value; } })),
      ] : null),
    container({ title: "How updates are installed" },
      h("ol", { class: "steps-list" },
        h("li", {}, "The signed manifest is verified against the release key; unsigned or altered packages are refused."),
        h("li", {}, "Only core files are accepted (core/bin, core/web, core/share); any other path is rejected."),
        h("li", {}, "The current core is backed up, the new one staged and swapped in atomically."),
        h("li", {}, "The relay restarts and must pass its health check, otherwise the backup is restored."))));
  },

  apply() {
    const s = st.settings || {};
    const fails = (st.system || []).filter((c) => c.status === "fail");
    const res = st.applied;
    const confirmBox = h("input", { type: "checkbox", id: "confirm" });
    return page({
      title: "Review and apply",
      desc: "Review the configuration. Applying writes the settings, installs the services and starts the relay.",
      actions: button(st.configured ? "Apply changes" : "Apply and start relay", {
        kind: "primary", disabled: fails.length > 0,
        onclick: (ev) => {
          if (!confirmBox.checked) { flash("warn", "Confirm that you reviewed the settings"); confirmBox.focus(); return; }
          return busy(ev.currentTarget, "Applying…", async () => {
            if (!st.settingsSaved && !(await saveSettings())) { flash("err", "Save the certificate and update settings first"); return; }
            try { await api("/api/apply", { confirm: true }); } catch (e) { flash("err", e.message); }
            await refresh(); render();
            if (st.applied && st.applied.status === "ok") go("done");
          });
        },
      }),
    },
    fails.length ? alert("err", "Resolve the failed server checks first", fails.map((f) => f.label).join(", ")) : null,
    res ? alert(res.status === "ok" ? "ok" : "err", res.status === "ok" ? "Applied. The relay is running." : "Apply failed", res.error || null,
      h("ol", {}, (res.steps || []).map((x) => h("li", {}, x))), res.recovery ? h("p", {}, res.recovery) : null) : null,
    container({ title: "Relay", actions: button("Edit", { kind: "link", onclick: () => go("provider") }) },
      kv([["Endpoint", h("code", {}, `${s.public_host}:${s.port}`)], ["Protocols", "TCP + UDP"], ["Service", st.service_running ? status("running") : status("stopped")]])),
    container({ title: "Certificate", actions: button("Edit", { kind: "link", onclick: () => go("tls") }) },
      kv([["Source", s.tls_mode === "acme" ? "Let's Encrypt" : s.tls_mode === "files" ? "Certificate files" : "None"],
        ["Details", s.tls_mode === "acme" ? (s.staging ? "Staging CA, renewed automatically" : "Renewed automatically") : s.tls_mode === "files" ? h("code", {}, s.cert) : "No status page"],
        ["Status", st.tls ? status(st.tls.status) : status("pending", "Not requested")]])),
    container({ title: "Updates", actions: button("Edit", { kind: "link", onclick: () => go("updates") }) },
      kv([["Policy", { manual: "Manual", notify: "Check and notify", auto: "Maintenance window" }[s.update_mode] || s.update_mode], ["Window", s.update_mode === "auto" ? s.window : "–"], ["Settings file", h("span", { class: "path" }, st.paths.runtime)]])),
    container({ title: "Portal and console", actions: button("Edit", { kind: "link", onclick: () => go("updates") }) },
      kv([["Portal", s.portal_enabled ? h("code", {}, `${siteOf()}/`) : "Off"], ["Console", s.portal_enabled ? h("code", {}, `${siteOf()}/console/`) : "Off"],
        ["Database", s.portal_enabled ? "Built in (PostgreSQL in /var/lib/taro/pg)" : "–"], ["Mail", s.smtp_set ? "Set" : "None (administrators use their password)"]])),
    container({ title: "Actions to be performed" },
      h("ol", { class: "steps-list" },
        h("li", {}, "Create the service account taro-relay (if missing)."),
        h("li", {}, "Set folder owners and permissions; the admission key stays readable by the relay only."),
        h("li", {}, "Write the settings; the previous file is kept as a backup."),
        s.portal_enabled ? h("li", {}, "Prepare the portal: its database, the server key and the console's setup code.") : null,
        h("li", {}, "Install the systemd units taro-relay.service and taro-update.timer."),
        h("li", {}, st.service_running ? "Restart the relay (sessions reconnect)." : "Start the relay and enable it at boot."),
        h("li", {}, "Verify that the relay answers its health check.")),
      h("label", { class: "check" }, confirmBox, h("span", {}, "I have reviewed these settings."))));
  },

  done() {
    const p = st.provider || {};
    const s = st.settings || {};
    const ok = st.applied && st.applied.status === "ok";
    return page({
      title: ok ? "Relay is running" : "Not applied yet",
      desc: ok ? `${p.product} clients connect to ${s.public_host}:${s.port}. Run setup again only to change settings.` : "Go back to Review and apply.",
      actions: button("Finish", { kind: "primary", onclick: async () => {
        await api("/api/finish", {});
        document.getElementById("main").replaceChildren(h("div", { class: "inner" },
          pageHeader({ title: "Setup closed", desc: "You can close this page and the SSH tunnel." })));
      } }),
    },
    ok ? alert("ok", "Setup complete", "The relay is installed as a service and passed its health check.") : null,
    container({ title: "Relay" },
      kv([["Endpoint", h("code", {}, `${s.public_host}:${s.port}`)], ["Service", st.service_running ? status("running") : status("stopped")],
        ["Status page", s.tls_mode !== "off" && ok ? (() => { const u = s.portal_enabled ? `${siteOf()}/relay/` : `${siteOf()}/`; return h("a", { href: u, target: "_blank", rel: "noreferrer" }, `${u} `, icon("ext")); })() : "–"]])),
    ok && s.portal_enabled ? container({ title: "Create the administrator", desc: "The first visit to the console makes its administrator. It asks for this setup code, so nobody else can." },
      kv([["Console", h("a", { href: `${siteOf()}/console/`, target: "_blank", rel: "noreferrer" }, `${siteOf()}/console/ `, icon("ext"))],
        ["Setup code", st.setup_code ? h("code", { class: "mono" }, st.setup_code) : "Used (an administrator exists)"],
        ["Portal", h("a", { href: `${siteOf()}/`, target: "_blank", rel: "noreferrer" }, `${siteOf()}/ `, icon("ext"))],
        ["Relay page", h("a", { href: `${siteOf()}/relay/`, target: "_blank", rel: "noreferrer" }, `${siteOf()}/relay/ `, icon("ext"))]])) : null,
    container({ title: "Useful commands" },
      h("pre", {}, ["taro-relay status", "journalctl -u taro-relay -f", "sudo taro-relay portal setup-code   # the console's first-run code", "sudo taro-relay update check", "sudo taro-relay setup        # change settings later"].join("\n"))));
  },
};

function siteOf() {
  const s = (st && st.settings) || {};
  return s.site || `https://${s.public_host || "this-server"}`;
}

function purpose(p) {
  if (p.startsWith("80/")) return "Certificate validation (HTTP-01)";
  if (p.startsWith("443/")) return "Portal, console and status page (HTTPS)";
  if (p.endsWith("/udp")) return "Relay media (video, audio)";
  return "Relay sessions";
}

async function diagnostics() {
  const r = await fetch("/api/diagnostics.txt", { headers: { "X-Setup-Token": token } });
  if (!r.ok) { flash("err", "Diagnostics are not available"); return; }
  download(await r.blob(), "taro-relay-setup-diagnostics.txt");
}

function render() {
  renderNav();
  document.getElementById("main").replaceChildren(views[current]());
}

(async () => {
  try { await refresh(); render(); }
  catch (e) {
    document.getElementById("main").replaceChildren(h("div", { class: "inner" },
      pageHeader({ title: "Cannot open setup" }),
      alert("err", e.message, "Open the address printed by sudo taro-relay setup, including its token.")));
  }
})();
