// Customer Portal: page building blocks on top of the shared console helpers (fields with
// errors, tables, empty states, formatting, rental and domain status).
import { h, icon, status, flash, busy } from "/_console/console.js";
import { ApiError, S } from "./core.js";

export function announce(text) {
  const l = document.getElementById("live");
  if (!l) return;
  l.textContent = "";
  setTimeout(() => { l.textContent = text; }, 30);
}

let uid = 0;
/**
 * A console form field: label, description, input, hint and an error line.
 * o: { label, name, type (text|email|select|textarea|...), value, options [[value, label]], desc,
 *      hint, optional, placeholder, autocomplete, inputmode, maxlength, mono, disabled, wide }
 * The element gets .input and .setError(message | null).
 */
export function field(o) {
  const id = o.id || "f-" + (o.name || "x") + "-" + (++uid);
  const descId = id + "-desc", hintId = id + "-hint", errId = id + "-err";
  let input;
  if (o.type === "select") {
    input = h("select", { id, name: o.name, disabled: o.disabled },
      (o.options || []).map(([v, l]) => h("option", { value: v, selected: String(v) === String(o.value ?? "") }, l)));
  } else if (o.type === "textarea") {
    input = h("textarea", { id, name: o.name, placeholder: o.placeholder, maxlength: o.maxlength, disabled: o.disabled });
    input.value = o.value ?? "";
  } else {
    input = h("input", {
      id, name: o.name, type: o.type || "text", placeholder: o.placeholder, autocomplete: o.autocomplete || "off",
      inputmode: o.inputmode, maxlength: o.maxlength, disabled: o.disabled, readonly: o.readonly,
      class: o.mono ? "mono" : o.inputClass, spellcheck: o.spellcheck, required: o.required,
    });
    input.value = o.value ?? "";
  }
  const base = [o.desc ? descId : null, o.hint ? hintId : null].filter(Boolean);
  if (base.length) input.setAttribute("aria-describedby", base.join(" "));
  const err = h("p", { class: "error", id: errId, hidden: true });
  const wrap = h("div", { class: "field" + (o.wide ? " wide" : "") + (o.class ? " " + o.class : "") },
    h("div", { class: "label-row" }, h("label", { for: id }, o.label), o.optional ? h("span", { class: "opt" }, "– optional") : null),
    o.desc ? h("p", { class: "desc", id: descId }, o.desc) : null,
    o.wrapInput ? o.wrapInput(input) : input,
    o.hint ? h("p", { class: "hint", id: hintId }, o.hint) : null,
    err);
  wrap.input = input;
  wrap.setError = (msg) => {
    if (!msg) {
      err.hidden = true; err.replaceChildren();
      input.removeAttribute("aria-invalid");
      if (base.length) input.setAttribute("aria-describedby", base.join(" ")); else input.removeAttribute("aria-describedby");
      return;
    }
    err.replaceChildren(icon("err"), h("span", {}, msg));
    err.hidden = false;
    input.setAttribute("aria-invalid", "true");
    input.setAttribute("aria-describedby", [...base, errId].join(" "));
  };
  input.addEventListener("input", () => wrap.setError(null));
  input.addEventListener("change", () => wrap.setError(null));
  return wrap;
}

/** Show an error: under the field when it belongs to one, else in the flash bar. */
export function report(e, fld) {
  const msg = (e && e.message) || "Something went wrong. Try again.";
  if (e && e.status === 401) return;
  if (fld && e instanceof ApiError && (e.status === 422 || e.status === 409)) {
    fld.setError(msg);
    fld.input.focus();
  } else {
    flash("err", msg);
  }
  announce(msg);
}

/** Run a request from a button: a spinner meanwhile, errors reported. Resolves to the result or undefined. */
export async function run(btn, text, fn, fld) {
  const go = async () => {
    try { return await fn(); } catch (e) { report(e, fld); return undefined; }
  };
  return btn ? busy(btn, text, go) : go();
}

export function ok(msg) { flash("ok", msg); announce(msg); }

/** A link that looks like a button (navigation stays in the app). */
export function linkButton(label, href, { kind = "", icon: ic, id } = {}) {
  return h("a", { class: "btn" + (kind ? " " + kind : ""), href, id }, ic ? icon(ic) : null, label);
}

export function emptyState(title, text, action) {
  return h("div", { class: "table-empty" }, h("strong", {}, title), text ? h("p", { class: "empty-text" }, text) : null, action || null);
}

/** A table: cols [{ label, cell(row), class }]. On a phone each row stacks as label: value. */
export function table(cols, rows, { empty, label, rowClass } = {}) {
  if (!rows || !rows.length) return empty || emptyState("Nothing here yet");
  return h("div", { class: "table-wrap" }, h("table", { class: "table stack", "aria-label": label },
    h("thead", {}, h("tr", {}, cols.map((c) => h("th", { scope: "col" }, c.label)))),
    h("tbody", {}, rows.map((r) => h("tr", { class: rowClass ? rowClass(r) : null },
      cols.map((c) => h("td", { class: c.class, "data-label": c.label }, c.cell(r))))))));
}

export function loading(text = "Loading…") {
  return h("p", { class: "loading" }, h("span", { class: "spinner", "aria-hidden": "true" }), text);
}

/** A copy button for a value; works without clipboard permission too. */
export function copyButton(value, label = "Copy", { describe } = {}) {
  const b = h("button", { type: "button", class: "btn sm", "aria-label": describe ? "Copy " + describe : null }, icon("copy"), label);
  b.addEventListener("click", async () => {
    const v = typeof value === "function" ? value() : value;
    let done = false;
    try { if (navigator.clipboard && window.isSecureContext) { await navigator.clipboard.writeText(v); done = true; } } catch (_) { done = false; }
    if (!done) {
      const t = h("textarea", { class: "sr-only", readonly: true, "aria-hidden": "true", tabindex: "-1" });
      t.value = v;
      document.body.append(t);
      t.select();
      try { done = document.execCommand("copy"); } catch (_) { done = false; }
      t.remove();
    }
    if (done) ok("Copied to the clipboard."); else flash("warn", "Copying is blocked here. Select the text and copy it yourself.");
    b.focus();
  });
  return b;
}

// ------------------------------------------------------------------ formatting

const dtf = new Intl.DateTimeFormat("en", { dateStyle: "medium", timeStyle: "short" });
const df = new Intl.DateTimeFormat("en", { dateStyle: "medium" });
const tf = new Intl.DateTimeFormat("en", { timeStyle: "medium" });
export const fmtDateTime = (t) => (t ? dtf.format(new Date(t)) : "–");
export const fmtDate = (t) => (t ? df.format(new Date(t)) : "–");
export const fmtTime = (t) => (t ? tf.format(new Date(t)) : "–");

/** A time as text, with the exact time as its tooltip. */
export function when(t, rel = false) {
  if (!t) return h("span", { class: "muted" }, "–");
  return h("time", { datetime: t, title: fmtDateTime(t) }, rel ? fmtRel(t) : fmtDateTime(t));
}

export function fmtRel(t) {
  const s = Math.round((Date.now() - new Date(t).getTime()) / 1000);
  const a = Math.abs(s);
  const f = (n, u) => (s >= 0 ? `${n} ${u}${n === 1 ? "" : "s"} ago` : `in ${n} ${u}${n === 1 ? "" : "s"}`);
  if (a < 45) return s >= 0 ? "just now" : "in a moment";
  if (a < 3600) return f(Math.round(a / 60), "minute");
  if (a < 86400) return f(Math.round(a / 3600), "hour");
  if (a < 86400 * 30) return f(Math.round(a / 86400), "day");
  return fmtDate(t);
}

export function fmtMoney(minor, currency) {
  const cur = String(currency || "usd").toUpperCase();
  try {
    const nf = new Intl.NumberFormat("en", { style: "currency", currency: cur });
    const digits = nf.resolvedOptions().maximumFractionDigits;
    return nf.format((Number(minor) || 0) / 10 ** digits);
  } catch (_) {
    return ((Number(minor) || 0) / 100).toFixed(2) + " " + cur;
  }
}

export function regionName(r) {
  if (!r) return "–";
  return r.split("-").map((p) => (p.length <= 2 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1))).join(" ");
}

/** "$19.00 for 24 hours" / "$99.00 per month". */
export function priceText(minor, currency, billing, hours) {
  const p = fmtMoney(minor, currency);
  if (billing === "monthly") return p + " per month";
  if (hours) return `${p} for ${hours >= 48 && hours % 24 === 0 ? hours / 24 + " days" : hours + " hours"}`;
  return p + ", paid once";
}

export function billingText(billing, hours) {
  if (billing === "monthly") return "Monthly, renews until cancelled";
  return hours ? `One-time, ${hours >= 48 && hours % 24 === 0 ? hours / 24 + " days" : hours + " hours"}` : "One-time";
}

export const ROLES = {
  owner: { label: "Owner", desc: "Everything, including owners and billing." },
  admin: { label: "Admin", desc: "Manages members, rentals, branding and domains." },
  member: { label: "Member", desc: "Uses the Macs assigned to them." },
  billing_manager: { label: "Billing manager", desc: "Manages billing and invoices; sees rentals." },
};
export const roleLabel = (r) => (ROLES[r] ? ROLES[r].label : r);
/** Which roles a role may hand out (the server's Role::may_assign). */
export function mayAssign(mine, target) {
  if (mine === "owner") return true;
  if (mine === "admin") return target !== "owner";
  return false;
}

// ------------------------------------------------------------------ rental status

export const RENTAL = {
  draft: { kind: "pending", label: "Draft", means: "The rental is being created." },
  pending_payment: { kind: "warn", label: "Awaiting payment", means: "Pay to start. The Mac is prepared as soon as the payment is confirmed." },
  paid: { kind: "info", label: "Paid", busy: true, means: "Payment received. A Mac is being reserved for you." },
  provisioning: { kind: "info", label: "Preparing", busy: true, means: "Your Mac is being prepared. This usually takes a few minutes." },
  ready: { kind: "ok", label: "Ready", means: "The Mac is ready. Choose Connect to open it in the Taro app." },
  suspended: { kind: "warn", label: "Suspended", means: "Access is paused, usually because a payment failed. Check Billing." },
  cancel_pending: { kind: "pending", label: "Ending", busy: true, means: "Cancelled. The Mac is being released and erased." },
  expired: { kind: "stopped", label: "Expired", means: "The rental period has ended. The Mac was released." },
  failed: { kind: "fail", label: "Failed", means: "The Mac could not be prepared. Contact support; the payment is handled by your provider." },
  cancelled: { kind: "stopped", label: "Cancelled", means: "The rental was cancelled." },
};
export const FINAL = new Set(["expired", "failed", "cancelled"]);
export const TRANSITIONAL = new Set(["paid", "provisioning", "cancel_pending"]);

export function rentalStatus(st) {
  const s = RENTAL[st] || { kind: "pending", label: st };
  if (s.busy) return h("span", { class: "status " + s.kind }, h("span", { class: "spinner sm", "aria-hidden": "true" }), s.label);
  return status(s.kind, s.label);
}

export const macName = (r) => r.resource_name || r.plan_name;

// ------------------------------------------------------------------ misc

export function badge(text, color) { return h("span", { class: "badge" + (color ? " " + color : "") }, text); }

export function supportLink() {
  const c = (S.branding && S.branding.supportContact) || "";
  if (!c) return null;
  const href = c.startsWith("https://") ? c : "mailto:" + c;
  return h("a", { href, rel: "noopener", target: c.startsWith("https://") ? "_blank" : null }, c);
}

/** Initials for a person or an organization. */
export function initials(s) {
  const t = String(s || "?").replace(/@.*/, "").split(/[\s._-]+/).filter(Boolean);
  return ((t[0] || "?")[0] + (t[1] ? t[1][0] : "")).toUpperCase();
}
