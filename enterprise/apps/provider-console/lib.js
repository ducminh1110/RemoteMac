// Taro Operations (the Provider Console): what every page shares. The API client (session
// cookie + X-CSRF-Token), the signed-in operator and their permissions, formatting, and small
// components on top of the shared console helpers. All DOM through createElement/textContent.
import { h, icon, status, container, alert, button, busy, flash } from "./_console/console.js";

export const S = { me: null, providers: null, branding: null, orgNames: null, setup: null };

// Where the console is mounted: "/" on its own origin, "/console/" on the portal's (one server).
// The server says so with <base href>; every URL the console uses is relative to it: links are
// written without a leading slash, API paths as "api/v1/…". Routes inside the app ("/macs") are
// paths below the base.
export const BASE = new URL(document.querySelector("base[href]") ? document.baseURI : "/", location.href).pathname.replace(/\/?$/, "/");
/** A route ("/macs?x=1") as a link from the page, relative to the base ("./macs?x=1"). */
export const rel = (route) => { const s = String(route); return s.startsWith("/") ? "./" + s.slice(1) : s; };
/** The route of a path of this site ("/console/macs" -> "/macs"), or null when it is not the console's. */
export function routeOf(pathname) {
  const root = BASE.slice(0, -1); // "" or "/console"
  if (pathname === root) return "/";
  if (!pathname.startsWith(BASE)) return null;
  return "/" + pathname.slice(BASE.length);
}
/** The product's name (the provider's branding). */
export const product = () => (S.branding && S.branding.displayName) || "Taro Enterprise";

export class ApiError extends Error {
  constructor(httpStatus, code, message, requestId) {
    super(message);
    this.status = httpStatus; this.code = code; this.requestId = requestId;
  }
}

let onUnauthenticated = () => {};
export function setUnauthenticatedHandler(fn) { onUnauthenticated = fn; }

/** Call the API. JSON in and out; a state-changing call carries the session's CSRF token. */
export async function api(method, path, body, { raw, contentType, quiet401 } = {}) {
  const headers = { Accept: "application/json" };
  const opts = { method, headers, credentials: "same-origin" };
  if (raw !== undefined) { opts.body = raw; headers["Content-Type"] = contentType || "application/octet-stream"; }
  else if (body !== undefined) { opts.body = JSON.stringify(body); headers["Content-Type"] = "application/json"; }
  if (method !== "GET" && S.me && S.me.csrf_token) headers["X-CSRF-Token"] = S.me.csrf_token;
  let res;
  try { res = await fetch(new URL(String(path).replace(/^\//, ""), location.origin + BASE), opts); }
  catch (_) { throw new ApiError(0, "network", "The console cannot reach the API. Check your connection and try again."); }
  const text = await res.text();
  let data = null;
  if (text) { try { data = JSON.parse(text); } catch (_) { data = null; } }
  if (!res.ok) {
    const e = (data && data.error) || {};
    const err = new ApiError(res.status, e.code || "http_" + res.status, e.message || `The request failed (${res.status}).`, e.request_id);
    if (res.status === 401 && !quiet401) onUnauthenticated();
    throw err;
  }
  return data;
}

/** The operator may do `p` (a permission key such as ops.jobs.manage). */
export const can = (p) => !!(S.me && (S.me.operator_permissions || []).includes(p));

export const ROLE_LABELS = {
  platform_admin: "Platform admin", operations_operator: "Operations operator", support_operator: "Support operator",
  billing_operator: "Billing operator", security_auditor: "Security auditor",
};
export const roleLabel = (r) => ROLE_LABELS[r] || humanize(r);

// ---------------------------------------------------------------- formatting

export function humanize(s) {
  if (s == null || s === "") return "–";
  const t = String(s).replace(/[_.]+/g, " ");
  return t.charAt(0).toUpperCase() + t.slice(1);
}

const DT = new Intl.DateTimeFormat("en-GB", { day: "2-digit", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });
const D = new Intl.DateTimeFormat("en-GB", { day: "2-digit", month: "short", year: "numeric" });

/** A date and time as a <time> element (the ISO value on hover). */
export function when(iso, { dateOnly = false, rel = false } = {}) {
  if (!iso) return h("span", { class: "muted" }, "–");
  const d = new Date(iso);
  if (isNaN(d)) return String(iso);
  const text = rel ? ago(d) : (dateOnly ? D : DT).format(d);
  return h("time", { datetime: d.toISOString(), title: DT.format(d) + " (" + d.toISOString() + ")" }, text);
}

export function ago(d) {
  const s = Math.round((Date.now() - new Date(d).getTime()) / 1000);
  const future = s < 0, a = Math.abs(s);
  const f = (n, u) => `${n} ${u}${n === 1 ? "" : "s"}`;
  const t = a < 45 ? "just now" : a < 3600 ? f(Math.round(a / 60), "minute") : a < 86400 ? f(Math.round(a / 3600), "hour") : f(Math.round(a / 86400), "day");
  if (t === "just now") return t;
  return future ? "in " + t : t + " ago";
}

const ZERO_DECIMAL = ["jpy", "krw", "vnd", "clp", "isk", "huf"];
export function money(minor, currency) {
  if (minor == null) return "–";
  const cur = (currency || "usd").toLowerCase();
  const v = ZERO_DECIMAL.includes(cur) ? minor : minor / 100;
  try { return new Intl.NumberFormat("en", { style: "currency", currency: cur.toUpperCase() }).format(v); }
  catch (_) { return v.toFixed(2) + " " + cur.toUpperCase(); }
}
export const minorUnits = (cur) => ZERO_DECIMAL.includes(String(cur || "").toLowerCase()) ? 0 : 2;

/** A UUID shortened, the full value on hover and for copying. */
export function shortId(id) {
  if (!id) return h("span", { class: "muted" }, "–");
  return h("span", { class: "mono id", title: String(id) }, String(id).slice(0, 8));
}

export const num = (n) => n == null ? "–" : new Intl.NumberFormat("en").format(n);

// ---------------------------------------------------------------- states as status indicators

const KIND = {
  // inventory
  available: "ok", assigned: "info", reserved: "pending", maintenance: "warn", unavailable: "fail", releasing: "pending",
  released: "stopped", discovered: "pending", error: "fail", in_use: "info", free: "ok", offline: "fail", missing: "fail",
  // jobs
  queued: "pending", running: "info", waiting_provider: "pending", succeeded: "ok", retryable_failed: "warn", permanent_failed: "fail", cancelled: "stopped",
  // rentals
  draft: "pending", pending_payment: "pending", paid: "info", provisioning: "info", ready: "ok", suspended: "warn",
  cancel_pending: "warn", expired: "stopped", failed: "fail",
  // billing
  none: "stopped", checkout_open: "pending", past_due: "warn", refunded: "warn", canceled: "stopped",
  // webhooks
  pending: "pending", processed: "ok", ignored: "stopped", dead: "fail",
  // domains
  pending_verification: "pending", verified: "info", provisioning_tls: "pending", active: "ok", disabled: "stopped",
  // incidents
  open: "fail", mitigated: "warn", resolved: "ok",
  // health
  healthy: "ok", degraded: "warn", down: "fail", unknown: "pending",
};
const LABEL = {
  retryable_failed: "Waiting to retry", permanent_failed: "Failed", waiting_provider: "Waiting on provider",
  pending_verification: "Pending verification", provisioning_tls: "Provisioning TLS", cancel_pending: "Cancel pending",
  pending_payment: "Pending payment", checkout_open: "Checkout open", past_due: "Past due", in_use: "In use",
};
export const stateLabel = (s) => LABEL[s] || humanize(s);
export const stateKind = (s) => KIND[s] || "pending";
/** A state as an icon + text in its colour. */
export const st = (s, text) => s == null ? h("span", { class: "muted" }, "–") : status(stateKind(s), text || stateLabel(s));

export function sevBadge(sev) {
  const c = { sev1: "red", sev2: "red", sev3: "amber", sev4: "blue" }[sev] || "";
  return h("span", { class: "badge sev " + c + " " + sev, title: { sev1: "Critical", sev2: "Major", sev3: "Minor", sev4: "Low" }[sev] || "" }, String(sev || "").toUpperCase());
}

// ---------------------------------------------------------------- components

/**
 * A table. columns: [{ label, cell(row) -> node|string, class, th }]; empty: { title, text, action }.
 * Wide tables scroll inside their container, never the page.
 */
export function table(columns, rows, { empty, rowClass, caption, id } = {}) {
  if (!rows || !rows.length) {
    const e = empty || {};
    return h("div", { class: "table-empty" }, h("strong", {}, e.title || "Nothing here"), e.text || null, e.action || null);
  }
  return h("div", { class: "table-wrap", tabindex: "0", role: "region", "aria-label": caption || "Table", id },
    h("table", { class: "table dense" },
      caption ? h("caption", { class: "sr-only" }, caption) : null,
      h("thead", {}, h("tr", {}, columns.map((c) => h("th", { scope: "col", class: c.class || null }, c.label)))),
      h("tbody", {}, rows.map((r) => h("tr", { class: rowClass ? rowClass(r) : null },
        columns.map((c) => h("td", { class: c.class || null }, c.cell(r))))))));
}

let fieldSeq = 0;
/**
 * A form field: { label, name, type, value, desc, hint, placeholder, options:[[value,label]], opt, required, rows, attrs }.
 * Returns the .field element; the control is .control on it.
 */
export function field(o) {
  const id = o.id || "f-" + (o.name || "x") + "-" + (++fieldSeq);
  const described = [o.desc ? id + "-desc" : null, o.hint ? id + "-hint" : null, id + "-err"].filter(Boolean).join(" ");
  let control;
  const common = { id, name: o.name, "aria-describedby": described, required: o.required || null, disabled: o.disabled || null, ...(o.attrs || {}) };
  if (o.type === "select") {
    control = h("select", common, (o.options || []).map(([v, l]) => h("option", { value: v, selected: String(o.value ?? "") === String(v) || null }, l)));
  } else if (o.type === "textarea") {
    control = h("textarea", { ...common, rows: o.rows || 3, placeholder: o.placeholder, maxlength: o.maxlength });
    control.value = o.value ?? "";
  } else {
    control = h("input", { ...common, type: o.type || "text", value: o.value ?? "", placeholder: o.placeholder, autocomplete: o.autocomplete || "off",
      inputmode: o.inputmode, maxlength: o.maxlength, min: o.min, max: o.max, step: o.step, spellcheck: o.spellcheck, class: o.mono ? "mono" : null });
  }
  if (o.oninput) control.addEventListener("input", o.oninput);
  if (o.onchange) control.addEventListener("change", o.onchange);
  const err = h("p", { class: "error", id: id + "-err", hidden: true });
  const el = h("div", { class: "field" + (o.wide ? " wide" : "") + (o.inline ? " inline" : "") },
    h("div", { class: "label-row" }, h("label", { for: id }, o.label), o.opt ? h("span", { class: "opt" }, "– optional") : null),
    o.desc ? h("p", { class: "desc", id: id + "-desc" }, o.desc) : null,
    control,
    o.hint ? h("p", { class: "hint", id: id + "-hint" }, o.hint) : null,
    err);
  el.control = control;
  el.setError = (msg) => {
    err.replaceChildren(...(msg ? [icon("err"), msg] : []));
    err.hidden = !msg;
    control.setAttribute("aria-invalid", msg ? "true" : "false");
  };
  return el;
}

/** A checkbox with a label and an optional description. */
export function checkbox({ label, desc, checked, name, disabled, onchange }) {
  const input = h("input", { type: "checkbox", name, checked: checked || null, disabled: disabled || null, onchange });
  const el = h("label", { class: "check" }, input, h("span", {}, label, desc ? h("span", { class: "d" }, desc) : null));
  return el; // label.control is already the input inside it
}

/**
 * A dialog that stays open while its action runs and shows the API's error inside it.
 * submit() resolves to close the dialog (its value is returned); throwing keeps it open.
 */
export function formDialog({ title, body, confirm = "Save", danger = false, submit, wide = false }) {
  return new Promise((resolve) => {
    const prev = document.activeElement;
    const errBox = h("div", { class: "dialog-error", role: "alert" });
    const close = (v) => { back.remove(); removeEventListener("keydown", onKey, true); if (prev && prev.focus) prev.focus(); resolve(v); };
    const onKey = (e) => {
      if (e.key === "Escape") { e.preventDefault(); close(undefined); }
      if (e.key === "Tab") trapFocus(e, back);
    };
    const ok = button(confirm, { kind: danger ? "primary danger" : "primary", type: "submit" });
    const form = h("form", { class: "modal" + (wide ? " wide" : ""), role: "dialog", "aria-modal": "true", "aria-labelledby": "dlg-title", novalidate: true,
      onsubmit: async (e) => {
        e.preventDefault();
        errBox.replaceChildren();
        await busy(ok, "Working…", async () => {
          try { const v = await submit(); close(v === undefined ? true : v); }
          catch (err) { errBox.replaceChildren(alert("err", null, err.message || String(err))); }
        });
      } },
    h("div", { class: "m-head" }, h("h2", { id: "dlg-title" }, title),
      h("button", { class: "btn link sm", type: "button", "aria-label": "Close", onclick: () => close(undefined) }, icon("close"))),
    h("div", { class: "m-body" }, errBox, body),
    h("div", { class: "m-foot" }, button("Cancel", { kind: "link", onclick: () => close(undefined) }), ok));
    const back = h("div", { class: "modal-back", onclick: (e) => e.target === back && close(undefined) }, form);
    document.body.append(back);
    addEventListener("keydown", onKey, true);
    const first = form.querySelector(".m-body input:not([type=hidden]), .m-body select, .m-body textarea");
    (first || ok).focus();
  });
}

/** A dialog that only shows something (a code to copy, steps to follow); resolves when closed. */
export function infoDialog({ title, body, close: closeLabel = "Done", id }) {
  return new Promise((resolve) => {
    const prev = document.activeElement;
    const close = () => { back.remove(); removeEventListener("keydown", onKey, true); if (prev && prev.focus) prev.focus(); resolve(true); };
    const onKey = (e) => {
      if (e.key === "Escape") { e.preventDefault(); close(); }
      if (e.key === "Tab") trapFocus(e, back);
    };
    const ok = button(closeLabel, { kind: "primary", onclick: close });
    const back = h("div", { class: "modal-back", onclick: (e) => e.target === back && close() },
      h("div", { class: "modal", role: "dialog", "aria-modal": "true", "aria-labelledby": "info-title", id },
        h("div", { class: "m-head" }, h("h2", { id: "info-title" }, title),
          h("button", { class: "btn link sm", type: "button", "aria-label": "Close", onclick: close }, icon("close"))),
        h("div", { class: "m-body" }, body),
        h("div", { class: "m-foot" }, ok)));
    document.body.append(back);
    addEventListener("keydown", onKey, true);
    ok.focus();
  });
}

function trapFocus(e, root) {
  const f = [...root.querySelectorAll("button, [href], input, select, textarea, [tabindex]:not([tabindex='-1'])")].filter((x) => !x.disabled && x.offsetParent !== null);
  if (!f.length) return;
  const first = f[0], last = f[f.length - 1];
  if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
}

/** Run an action from a button: spinner, a flash with the result or the error, then `after`. */
export async function act(btn, label, fn, { ok, after } = {}) {
  const go = async () => {
    try {
      const r = await fn();
      if (ok) { flash("ok", typeof ok === "function" ? ok(r) : ok); announce(typeof ok === "function" ? ok(r) : ok); }
      if (after) await after(r);
      return r;
    } catch (e) {
      flash("err", errorText(e));
      announce(errorText(e));
    }
  };
  return btn ? busy(btn, label, go) : go();
}

export const errorText = (e) => (e && e.message ? e.message : "Something went wrong.") + (e && e.requestId && e.status >= 500 ? ` (request ${e.requestId})` : "");

export function announce(t) {
  const l = document.getElementById("live");
  if (!l) return;
  l.textContent = "";
  setTimeout(() => { l.textContent = t; }, 30);
}

/** A filter bar above a table. */
export function toolbar(...items) {
  return h("div", { class: "toolbar" }, items);
}

export function searchBox({ label = "Search", value = "", placeholder, oninput, id }) {
  const input = h("input", { type: "search", id, value, placeholder: placeholder || label, "aria-label": label, autocomplete: "off", spellcheck: "false" });
  let t = null;
  input.addEventListener("input", () => { clearTimeout(t); t = setTimeout(() => oninput(input.value), 200); });
  return h("div", { class: "tb-search" }, icon("search"), input);
}

export function selectBox({ label, value, options, onchange, id }) {
  const sel = h("select", { "aria-label": label, id, onchange: (e) => onchange(e.target.value) },
    options.map(([v, l]) => h("option", { value: v, selected: String(value ?? "") === String(v) || null }, l)));
  return h("label", { class: "tb-select" }, h("span", { class: "tb-label" }, label), sel);
}

/** A read-only note for pages the role may see but not change. */
export function readOnlyNote(what) {
  return alert("info", "Read only", `Your role can view ${what} but not change it.`);
}

/** The page could not load its data. */
export function loadError(e, retry) {
  return alert("err", "This page could not be loaded", errorText(e), retry ? h("div", { class: "alert-actions" }, button("Try again", { icon: "refresh", onclick: retry })) : null);
}

export function linkTo(href, text, title) {
  return h("a", { href, title: title || null }, text);
}

/** Key figures for a page: [[label, value, kind?]] */
export function stats(items) {
  return h("div", { class: "stat-row" }, items.filter(Boolean).map(([label, value, kind]) =>
    h("div", { class: "stat" + (kind ? " " + kind : "") }, h("span", { class: "s-label" }, label), h("span", { class: "s-value" }, value))));
}

/** Organization names by id (cached for the session; operators with ops.orgs.read). */
export async function orgNames(force) {
  if (S.orgNames && !force) return S.orgNames;
  const m = new Map();
  if (can("ops.orgs.read")) {
    try { const r = await api("GET", "api/v1/ops/organizations"); for (const o of r.organizations) m.set(o.id, o.name); }
    catch (_) { /* names are a convenience: ids still show */ }
  }
  S.orgNames = m;
  return m;
}

export function orgLink(id, names) {
  if (!id) return h("span", { class: "muted" }, "Platform");
  const name = names && names.get(id);
  if (!can("ops.orgs.read")) return name || shortId(id);
  return h("a", { href: "organizations/" + id }, name || String(id).slice(0, 8));
}

export function copyButton(text, label = "Copy") {
  return h("button", { type: "button", class: "btn link sm copy", title: "Copy " + text, "aria-label": label + " " + text,
    onclick: async (e) => {
      const btn = e.currentTarget; // (gone from the event after the await)
      try { await navigator.clipboard.writeText(text); flash("ok", "Copied."); }
      catch (_) { flash("warn", "Copy is not available here."); }
      btn.blur();
    } }, icon("copy"));
}

export { h, icon, status, container, alert, button, busy, flash };
