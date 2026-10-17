// Taro Enterprise console: small DOM helpers shared by the administration pages. No framework, no
// inline styles or scripts (the pages' Content-Security-Policy allows their own origin only).

/** h("div", { class: "x", onclick }, ...children): an element. false/null attributes and children are skipped. */
export function h(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === false || v == null) continue;
    if (k === "class") e.className = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (v === true) e.setAttribute(k, "");
    else e.setAttribute(k, v);
  }
  for (const c of kids.flat(Infinity)) if (c != null && c !== false) e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  return e;
}

export const icon = (name) => h("span", { class: "i i-" + name, "aria-hidden": "true" });

const STATUS = {
  ok: ["ok", "Passed"], pass: ["ok", "Passed"], success: ["ok", "Success"], running: ["ok", "Running"],
  warn: ["warn", "Warning"], fail: ["err", "Failed"], err: ["err", "Error"], error: ["err", "Error"],
  info: ["info", "Info"], pending: ["pending", "Pending"], skip: ["stop", "Skipped"], stopped: ["stop", "Stopped"],
};

/** A status indicator: icon + text in the status colour (status: ok, warn, fail, info, pending, skip). */
export function status(kind, text) {
  const [ic, label] = STATUS[kind] || ["pending", kind];
  return h("span", { class: "status " + kind }, icon(ic), text ?? label);
}

/** A container: { title, desc, actions, foot, flush } + body. */
export function container({ title, count, desc, actions, foot, flush, id } = {}, ...body) {
  return h("section", { class: "container", id },
    title ? h("div", { class: "c-head" },
      h("div", { class: "text" }, h("h2", {}, title, count != null ? h("span", { class: "count" }, ` (${count})`) : null), desc ? h("p", { class: "desc" }, desc) : null),
      actions ? h("div", { class: "actions" }, actions) : null) : null,
    h("div", { class: "c-body" + (flush ? " flush" : "") }, body),
    foot ? h("div", { class: "c-foot" }, foot) : null);
}

/** Key-value pairs: [[label, value], ...] in `cols` columns. */
export function kv(pairs, cols = 3) {
  return h("dl", { class: "kv cols-" + cols }, pairs.filter(Boolean).map(([k, v]) => h("div", {}, h("dt", {}, k), h("dd", {}, v ?? "–"))));
}

/** An alert inside the page: kind ok | warn | err | info. */
export function alert(kind, title, ...body) {
  const ic = { ok: "ok", warn: "warn", err: "err", info: "info" }[kind] || "info";
  return h("div", { class: "alert " + kind, role: kind === "err" ? "alert" : null }, icon(ic), h("div", { class: "body" }, title ? h("strong", {}, title) : null, body));
}

/** A page header: { step, title, desc, actions }. */
export function pageHeader({ step, title, desc, actions }) {
  return h("div", { class: "page-header" },
    h("div", { class: "text" }, step ? h("p", { class: "step" }, step) : null, h("h1", {}, title), desc ? h("p", { class: "desc" }, desc) : null),
    actions ? h("div", { class: "actions" }, actions) : null);
}

export function breadcrumbs(items) {
  return h("ol", { class: "breadcrumbs", "aria-label": "Breadcrumbs" }, items.map((x) => h("li", {}, x)));
}

/** A button: { kind: "primary" | "link" | "", icon, onclick, disabled }. */
export function button(label, { kind = "", icon: ic, onclick, disabled, type = "button", id } = {}) {
  return h("button", { class: "btn" + (kind ? " " + kind : ""), type, onclick, disabled, id }, ic ? icon(ic) : null, label);
}

/** Show a spinner in a button while `work` runs; the button comes back afterwards. */
export async function busy(btn, text, work) {
  const old = [...btn.childNodes];
  btn.disabled = true;
  btn.replaceChildren(h("span", { class: "spinner", "aria-hidden": "true" }), text);
  try {
    return await work();
  } finally {
    if (btn.isConnected) { btn.disabled = false; btn.replaceChildren(...old); }
  }
}

/** A notification in the flash bar (top right); errors stay until dismissed. */
export function flash(kind, message) {
  let bar = document.getElementById("flashbar");
  if (!bar) { bar = h("div", { class: "flashbar", id: "flashbar", role: "status", "aria-live": "polite" }); document.body.append(bar); }
  const ic = { ok: "ok", err: "err", warn: "warn" }[kind] || "info";
  const el = h("div", { class: "flash " + kind }, icon(ic), h("div", { class: "msg" }, message),
    h("button", { type: "button", "aria-label": "Dismiss", onclick: () => el.remove() }, icon("close")));
  bar.append(el);
  if (kind !== "err") setTimeout(() => el.remove(), 5000);
  return el;
}

/** A confirmation dialog; resolves true when confirmed. */
export function confirmDialog({ title, body, confirm = "Confirm", danger = false }) {
  return new Promise((resolve) => {
    const prev = document.activeElement;
    const close = (v) => { back.remove(); removeEventListener("keydown", onKey, true); prev && prev.focus && prev.focus(); resolve(v); };
    const onKey = (e) => { if (e.key === "Escape") { e.preventDefault(); close(false); } };
    const ok = button(confirm, { kind: danger ? "primary danger" : "primary", onclick: () => close(true) });
    const back = h("div", { class: "modal-back", onclick: (e) => e.target === back && close(false) },
      h("div", { class: "modal", role: "dialog", "aria-modal": "true", "aria-labelledby": "modal-title" },
        h("div", { class: "m-head" }, h("h2", { id: "modal-title" }, title),
          h("button", { class: "btn link sm", type: "button", "aria-label": "Close", onclick: () => close(false) }, icon("close"))),
        h("div", { class: "m-body" }, body),
        h("div", { class: "m-foot" }, button("Cancel", { kind: "link", onclick: () => close(false) }), ok)));
    document.body.append(back);
    addEventListener("keydown", onKey, true);
    ok.focus();
  });
}

/** Save a Blob as a file. */
export function download(blob, name) {
  const a = h("a", { href: URL.createObjectURL(blob), download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}
