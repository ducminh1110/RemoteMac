// Taro relay setup (Wizard 2). Shared core UI: it only knows the API; the provider's name,
// colour and logo come from the server (/api/state, /brand/*).
const token = new URLSearchParams(location.search).get("t") || "";
history.replaceState(null, "", "/");

const STEPS = [
  { id: "provider", name: "Provider" },
  { id: "system", name: "This server" },
  { id: "network", name: "DNS & network" },
  { id: "tls", name: "Certificate" },
  { id: "firewall", name: "Firewall" },
  { id: "updates", name: "Updates" },
  { id: "apply", name: "Review & apply" },
  { id: "done", name: "Done" },
];

let st = null;
let current = "provider";
const form = { tls_mode: "acme", email: "", agree_tos: false, staging: false, cert: "", key: "", update_mode: "manual", window: "" };

function h(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === false || v == null) continue;
    if (k === "class") e.className = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (v === true) e.setAttribute(k, "");
    else e.setAttribute(k, v);
  }
  for (const c of kids.flat()) if (c != null && c !== false) e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  return e;
}

async function api(path, body) {
  const opt = { method: body === undefined ? "GET" : "POST", headers: { "X-Setup-Token": token } };
  if (body !== undefined) { opt.headers["Content-Type"] = "application/json"; opt.body = JSON.stringify(body); }
  const r = await fetch(path, opt);
  const j = await r.json().catch(() => ({}));
  if (!r.ok) { const e = new Error(j.error || r.statusText); e.fields = j.fields; e.status = r.status; throw e; }
  return j;
}

function toast(msg) {
  const t = document.getElementById("toast");
  t.textContent = msg; t.hidden = false;
  clearTimeout(toast.t); toast.t = setTimeout(() => (t.hidden = true), 4000);
}

function stepStatus(id) {
  if (!st) return "";
  const worst = (list) => (list || []).some((c) => c.status === "fail") ? "bad" : (list || []).some((c) => c.status === "warn") ? "warn" : list && list.length ? "done" : "";
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
  const ol = document.getElementById("steps");
  ol.replaceChildren(...STEPS.map((s, i) => {
    const cls = stepStatus(s.id);
    return h("li", { class: cls },
      h("button", { type: "button", "aria-current": s.id === current ? "step" : false, onclick: () => go(s.id) },
        h("span", { class: "num", "aria-hidden": "true" }, cls === "done" ? "✓" : cls === "bad" ? "!" : String(i + 1)),
        h("span", { class: "name" }, s.name)));
  }));
}

function go(id) {
  current = id;
  render();
  document.getElementById("main").focus();
}

function next() {
  const i = STEPS.findIndex((s) => s.id === current);
  if (i < STEPS.length - 1) go(STEPS[i + 1].id);
}

function checkList(list) {
  return h("ul", { class: "checks" }, (list || []).map((c) =>
    h("li", { class: c.status === "fail" ? "fail-row" : "" },
      h("span", { class: "ic " + c.status, "aria-label": c.status }, c.status === "ok" ? "✓" : c.status === "fail" ? "✕" : c.status === "warn" ? "!" : "–"),
      h("span", { class: "label" }, c.label),
      h("span", { class: "detail" }, c.detail),
      c.fix ? h("span", { class: "fix" }, c.fix) : null)));
}

function busy(btn, text) {
  btn.disabled = true;
  const old = btn.textContent;
  btn.replaceChildren(h("span", { class: "spinner", "aria-hidden": "true" }), " ", text);
  return () => { btn.disabled = false; btn.textContent = old; };
}

async function refresh() {
  const saved = st && st.settingsSaved;
  st = await api("/api/state");
  st.settingsSaved = saved;
  if (st.settings) {
    Object.assign(form, {
      tls_mode: st.settings.tls_mode, email: st.settings.email || form.email, staging: st.settings.staging,
      update_mode: st.settings.update_mode, window: st.settings.window || "",
      cert: st.settings.tls_mode === "files" ? st.settings.cert : form.cert, key: st.settings.tls_mode === "files" ? st.settings.key : form.key,
    });
  }
  const p = st.provider;
  document.getElementById("title").textContent = p ? `${p.product} relay setup` : "Relay setup";
  document.title = p ? `${p.product} relay setup` : "Relay setup";
  const pill = document.getElementById("mode-pill");
  pill.hidden = false;
  pill.textContent = st.configured ? "Reconfiguring" : "First run";
}

async function saveSettings(errBox) {
  try {
    await api("/api/settings", form);
    st.settingsSaved = true;
    errBox && errBox.replaceChildren();
    return true;
  } catch (e) {
    if (errBox) {
      errBox.replaceChildren(h("div", { class: "note fail" }, e.message, e.fields ? h("ul", {}, e.fields.map((f) => h("li", {}, f.message))) : null));
    }
    for (const f of e.fields || []) {
      const el = document.getElementById("f-" + f.field);
      if (el) el.setAttribute("aria-invalid", "true");
    }
    return false;
  }
}

const views = {
  provider() {
    const p = st.provider;
    if (!p) return h("section", { class: "card" }, h("h2", {}, "Provider folder missing"), h("div", { class: "note fail" }, st.provider_error || "No provider.json"),
      h("p", {}, "This relay must be installed from the relay package your Taro Enterprise package builder (Wizard 1) produced: it carries your provider folder."));
    return h("section", { class: "card" },
      h("h2", {}, st.configured ? "Change this relay's settings" : "Set up this relay"),
      h("p", { class: "lead" }, st.configured
        ? "This server is already set up. You can change its settings: the relay's data (Mac IDs), its certificate and your provider folder are kept."
        : "This configures the relay you installed: it checks this server, its DNS and network, gets a certificate, opens the firewall and starts the relay as a service."),
      h("h3", {}, "From your provider package"),
      h("dl", { class: "kv" },
        h("dt", {}, "Product"), h("dd", {}, p.product),
        h("dt", {}, "Vendor"), h("dd", {}, p.vendor),
        h("dt", {}, "Relay address"), h("dd", {}, h("code", {}, `${p.relay_host}:${p.relay_port}`)),
        h("dt", {}, "Primary colour"), h("dd", {}, h("span", { class: "swatch" }), p.primary_color),
        h("dt", {}, "Installed version"), h("dd", {}, st.installed ? `${st.installed.version} (${st.installed.arch})` : "not recorded"),
        h("dt", {}, "Provider folder"), h("dd", {}, h("code", {}, st.paths.provider))),
      h("div", { class: "note" }, "The relay address is fixed by your provider package: your Windows and Mac clients were built to connect to it. To use another name, build new packages with the package builder."),
      h("div", { class: "btns" }, h("button", { class: "b primary", type: "button", onclick: next }, "Continue")));
  },

  system() {
    const out = h("div", {}, st.system && st.system.length ? checkList(st.system) : h("p", { class: "lead" }, "Not checked yet."));
    const run = h("button", { class: "b primary", type: "button", onclick: async (ev) => {
      const done = busy(ev.currentTarget, "Checking…");
      try { await api("/api/checks/system", {}); await refresh(); render(); } catch (e) { toast(e.message); done(); }
    } }, st.system && st.system.length ? "Check again" : "Check this server");
    return h("section", { class: "card" }, h("h2", {}, "This server"), h("p", { class: "lead" }, "Operating system, CPU, administrator rights, systemd, disk space and the ports the relay needs."), out,
      h("div", { class: "btns" }, run, st.system && st.system.length ? h("button", { class: "b", type: "button", onclick: next }, "Continue") : null));
  },

  network() {
    const p = st.provider || {};
    const out = st.network && st.network.length ? checkList(st.network) : h("p", { class: "lead" }, "Not checked yet.");
    const run = h("button", { class: "b primary", type: "button", onclick: async (ev) => {
      const done = busy(ev.currentTarget, "Looking up…");
      try { await api("/api/checks/network", {}); await refresh(); render(); } catch (e) { toast(e.message); done(); }
    } }, st.network && st.network.length ? "Check again" : "Check DNS and network");
    return h("section", { class: "card" }, h("h2", {}, "DNS & network"),
      h("p", { class: "lead" }, "Your clients connect to ", h("code", {}, p.relay_host || "?"), ". It must point at this server's public address."), out,
      h("div", { class: "btns" }, run, st.network && st.network.length ? h("button", { class: "b", type: "button", onclick: next }, "Continue") : null));
  },

  tls() {
    const errBox = h("div", {});
    const radio = (value, title, desc) => h("label", { class: "choice" },
      h("input", { type: "radio", name: "tls", value, checked: form.tls_mode === value, onchange: () => { form.tls_mode = value; render(); } }),
      h("span", { class: "t" }, title), h("span", { class: "d" }, desc));
    const fields = [];
    if (form.tls_mode === "acme") {
      fields.push(h("div", { class: "sub-fields" },
        h("label", { class: "field", for: "f-email" }, "Email for expiry notices"),
        h("input", { id: "f-email", type: "email", autocomplete: "email", value: form.email, oninput: (e) => { form.email = e.target.value; e.target.removeAttribute("aria-invalid"); } }),
        h("label", { class: "check" }, h("input", { id: "f-agree_tos", type: "checkbox", checked: form.agree_tos, onchange: (e) => (form.agree_tos = e.target.checked) }),
          h("span", {}, "I accept the Let's Encrypt Subscriber Agreement.")),
        h("label", { class: "check" }, h("input", { type: "checkbox", checked: form.staging, onchange: (e) => (form.staging = e.target.checked) }),
          h("span", {}, "Use the staging CA (testing: no rate limits, the certificate is not trusted)."))));
    } else if (form.tls_mode === "files") {
      fields.push(h("div", { class: "sub-fields" },
        h("label", { class: "field", for: "f-cert" }, "Certificate chain (PEM)"),
        h("input", { id: "f-cert", type: "text", class: "mono", placeholder: "/etc/ssl/relay/fullchain.pem", value: form.cert, oninput: (e) => (form.cert = e.target.value) }),
        h("label", { class: "field", for: "f-key" }, "Private key (PEM)"),
        h("input", { id: "f-key", type: "text", class: "mono", placeholder: "/etc/ssl/relay/privkey.pem", value: form.key, oninput: (e) => (form.key = e.target.value) })));
    }
    const res = st.tls ? h("div", { class: "note " + (st.tls.status === "ok" ? "ok" : st.tls.status === "fail" ? "fail" : "") }, st.tls.detail, st.tls.fix ? h("div", {}, h("strong", {}, "What to do: "), st.tls.fix) : null) : null;
    return h("section", { class: "card" }, h("h2", {}, "Certificate"),
      h("p", { class: "lead" }, "HTTPS for the relay's status page and health checks. Sessions between your clients and Macs are end-to-end encrypted either way."),
      h("div", { class: "choices", role: "radiogroup", "aria-label": "Certificate" },
        radio("acme", "Let's Encrypt (recommended)", "Obtained now by an HTTP check on port 80 and renewed by the relay itself, 30 days before it expires."),
        radio("files", "My own certificate files", "Managed elsewhere; the relay reloads them when they change."),
        radio("off", "No HTTPS", "No status page; health checks stay on this server only.")),
      fields, errBox, res,
      h("div", { class: "btns" },
        h("button", { class: "b primary", type: "button", onclick: async (ev) => {
          const done = busy(ev.currentTarget, form.tls_mode === "acme" ? "Getting the certificate…" : "Checking…");
          if (await saveSettings(errBox)) { try { await api("/api/tls", {}); } catch (e) { toast(e.message); } await refresh(); render(); } else done();
        } }, form.tls_mode === "acme" ? "Get the certificate" : form.tls_mode === "files" ? "Check the files" : "Save"),
        st.tls && st.tls.status !== "fail" ? h("button", { class: "b", type: "button", onclick: next }, "Continue") : null));
  },

  firewall() {
    const fw = st.firewall;
    const body = fw ? h("div", {},
      h("dl", { class: "kv" }, h("dt", {}, "Local firewall"), h("dd", {}, fw.kind === "none" ? "none found" : `${fw.kind}${fw.active ? " (active)" : " (inactive)"}`),
        h("dt", {}, "Status"), h("dd", {}, fw.detail), h("dt", {}, "Ports the relay needs"), h("dd", {}, fw.needed.map((p) => h("code", {}, p)).reduce((a, b) => [...a, " ", b], []))),
      h("div", { class: "note warn" }, "Open the same ports in your hosting provider's firewall or security group as well: they cannot be checked from inside this server.")) : h("p", { class: "lead" }, "Not checked yet.");
    const canOpen = fw && fw.active && fw.kind !== "none";
    return h("section", { class: "card" }, h("h2", {}, "Firewall"), h("p", { class: "lead" }, "The relay's port, TCP and UDP (video), and 80/443 for the certificate and the status page."), body,
      h("div", { class: "btns" },
        h("button", { class: "b" + (fw ? "" : " primary"), type: "button", onclick: async (ev) => { const d = busy(ev.currentTarget, "Checking…"); try { await api("/api/firewall/check", {}); await refresh(); render(); } catch (e) { toast(e.message); d(); } } }, fw ? "Check again" : "Check the firewall"),
        canOpen ? h("button", { class: "b primary", type: "button", onclick: async (ev) => {
          if (!confirm(`Allow ${fw.needed.join(", ")} in ${fw.kind}?`)) return;
          const d = busy(ev.currentTarget, "Opening…");
          try { const r = await api("/api/firewall/open", { confirm: true }); toast(r.done.length ? `Done: ${r.done.length} rule(s)` : "Nothing to change"); await api("/api/firewall/check", {}); await refresh(); render(); } catch (e) { toast(e.message); d(); }
        } }, "Open the ports") : null,
        fw ? h("button", { class: "b", type: "button", onclick: next }, "Continue") : null));
  },

  updates() {
    const errBox = h("div", {});
    const radio = (value, title, desc) => h("label", { class: "choice" },
      h("input", { type: "radio", name: "upd", value, checked: form.update_mode === value, onchange: () => { form.update_mode = value; render(); } }),
      h("span", { class: "t" }, title), h("span", { class: "d" }, desc));
    return h("section", { class: "card" }, h("h2", {}, "Updates"),
      h("p", { class: "lead" }, "Enterprise updates replace only the relay's shared core. Your provider folder, these settings, the certificate and the relay's data are never replaced. Every update is signed and checked, backed up, and rolled back if the relay does not come back healthy."),
      h("div", { class: "choices", role: "radiogroup", "aria-label": "Update policy" },
        radio("manual", "Manual", "Only when you run sudo taro-relay update."),
        radio("notify", "Check and tell me", "Checked hourly; shown by taro-relay status. Nothing is installed by itself."),
        radio("auto", "Install in a maintenance window", "Checked hourly and installed only inside the window (the relay restarts; sessions reconnect).")),
      form.update_mode === "auto" ? h("div", { class: "sub-fields" },
        h("label", { class: "field", for: "f-window" }, "Maintenance window (UTC)"),
        h("input", { id: "f-window", type: "text", placeholder: "Sun 03:00-05:00", value: form.window, oninput: (e) => { form.window = e.target.value; e.target.removeAttribute("aria-invalid"); } }),
        h("p", { class: "err" }, "Like Sun 03:00-05:00 or daily 02:00-04:00.")) : null,
      errBox,
      h("div", { class: "btns" }, h("button", { class: "b primary", type: "button", onclick: async () => { if (await saveSettings(errBox)) { await refresh(); st.settingsSaved = true; toast("Saved"); next(); } } }, "Save and continue")));
  },

  apply() {
    const s = st.settings || {};
    const fails = (st.system || []).filter((c) => c.status === "fail");
    const res = st.applied;
    const confirmBox = h("input", { type: "checkbox", id: "confirm" });
    return h("section", { class: "card" }, h("h2", {}, "Review & apply"),
      h("dl", { class: "kv" },
        h("dt", {}, "Relay"), h("dd", {}, h("code", {}, `${s.public_host}:${s.port}`), " TCP + UDP"),
        h("dt", {}, "Certificate"), h("dd", {}, s.tls_mode === "acme" ? `Let's Encrypt${s.staging ? " (staging)" : ""}, renewed automatically` : s.tls_mode === "files" ? `files ${s.cert}` : "none"),
        h("dt", {}, "Updates"), h("dd", {}, s.update_mode === "auto" ? `install in ${s.window}` : s.update_mode),
        h("dt", {}, "Settings file"), h("dd", {}, h("code", {}, st.paths.runtime))),
      h("h3", {}, "Setup will"),
      h("ol", { class: "steplist" },
        h("li", {}, "create the service account taro-relay (if missing)"),
        h("li", {}, "set the folders' owners and permissions (the admission key stays readable by the relay only)"),
        h("li", {}, "write the settings (the previous file is kept as a backup)"),
        h("li", {}, "install the systemd units taro-relay.service and taro-update.timer"),
        h("li", {}, st.service_running ? "restart the relay (sessions reconnect)" : "start the relay and enable it at boot"),
        h("li", {}, "check that it answers")),
      fails.length ? h("div", { class: "note fail" }, "Fix first: ", fails.map((f) => f.label).join(", ")) : null,
      res ? h("div", { class: "note " + (res.status === "ok" ? "ok" : "fail") },
        res.status === "ok" ? "Applied. The relay is running." : res.error,
        h("ol", { class: "steplist" }, (res.steps || []).map((x) => h("li", {}, x))),
        res.recovery ? h("p", {}, res.recovery) : null) : null,
      h("label", { class: "check" }, confirmBox, h("span", {}, "I have reviewed these settings.")),
      h("div", { class: "btns" },
        h("button", { class: "b primary", type: "button", disabled: fails.length > 0, onclick: async (ev) => {
          if (!confirmBox.checked) { toast("Tick the box to confirm"); return; }
          if (!st.settingsSaved && !(await saveSettings())) { toast("Save the certificate and update settings first"); return; }
          const d = busy(ev.currentTarget, "Applying…");
          try { await api("/api/apply", { confirm: true }); } catch (e) { toast(e.message); }
          await refresh(); render(); d();
          if (st.applied && st.applied.status === "ok") go("done");
        } }, st.configured ? "Apply changes" : "Apply and start the relay"),
        h("button", { class: "b", type: "button", onclick: diagnostics }, "Download diagnostics")));
  },

  done() {
    const p = st.provider || {};
    const s = st.settings || {};
    const ok = st.applied && st.applied.status === "ok";
    return h("section", { class: "card" }, h("h2", {}, ok ? "Your relay is running" : "Not applied yet"),
      ok ? h("p", { class: "lead" }, `${p.product} clients connect to ${s.public_host}:${s.port}. Setup is not needed again; run it only to change settings.`) : h("p", { class: "lead" }, "Go back to Review & apply."),
      h("h3", {}, "Useful commands"),
      h("pre", {}, ["taro-relay status", "journalctl -u taro-relay -f", "sudo taro-relay update check", "sudo taro-relay setup        # change settings later"].join("\n")),
      s.tls_mode !== "off" && ok ? h("p", {}, "Status page: ", h("a", { class: "link", href: `https://${s.public_host}/`, target: "_blank", rel: "noreferrer" }, `https://${s.public_host}/`)) : null,
      h("div", { class: "btns" },
        h("button", { class: "b primary", type: "button", onclick: async () => { await api("/api/finish", {}); document.getElementById("main").replaceChildren(h("section", { class: "card" }, h("h2", {}, "Setup closed"), h("p", { class: "lead" }, "You can close this page and the SSH tunnel."))); } }, "Finish"),
        h("button", { class: "b", type: "button", onclick: diagnostics }, "Download diagnostics")));
  },
};

async function diagnostics() {
  const r = await fetch("/api/diagnostics.txt", { headers: { "X-Setup-Token": token } });
  const b = await r.blob();
  const a = h("a", { href: URL.createObjectURL(b), download: "taro-relay-setup-diagnostics.txt" });
  document.body.append(a); a.click(); a.remove();
}

function render() {
  renderNav();
  document.getElementById("main").replaceChildren(views[current]());
}

(async () => {
  try { await refresh(); render(); }
  catch (e) { document.getElementById("main").replaceChildren(h("section", { class: "card" }, h("h2", {}, "Cannot open setup"), h("p", {}, e.message), h("p", {}, "Open the address printed by sudo taro-relay setup, including its token."))); }
})();
