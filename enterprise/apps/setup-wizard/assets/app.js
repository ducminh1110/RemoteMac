// Taro Enterprise Setup (Wizard 1, the package builder): the console UI.
// Built with the shared console helpers; all DOM through createElement/textContent (no
// innerHTML with data). Colours are set through the CSSOM (element.style.setProperty), which the
// strict CSP allows; there are no inline styles or scripts.
import { h, icon, status, container, kv, alert, pageHeader, breadcrumbs, button, busy, flash, confirmDialog, download } from "/common/console.js";

const TOKEN = new URL(location.href).searchParams.get("t") || "";
// in TaroEnterpriseSetup's own window (not a browser tab): its title bar closes it
const APP_WINDOW = new URL(location.href).searchParams.get("shell") === "app";
if (APP_WINDOW) document.documentElement.classList.add("app-window");
let S = null; // last state from the server
let current = "preflight";
const drafts = {}; // input drafts by element id, survive re-renders
const errors = {}; // field errors by step
let pollTimer = null;
const images = { logo: { sha: null, url: null }, icon: { sha: null, url: null } };
const $ = (id) => document.getElementById(id);
function announce(t) { const l = $("live"); l.textContent = ""; setTimeout(() => { l.textContent = t; }, 30); }
const kb = (n) => n >= 1e6 ? (n / 1e6).toFixed(1) + " MB" : n >= 1e3 ? Math.round(n / 1e3) + " KB" : n + " bytes";

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
  if (data && data.state) S = data.state;
  if (!res.ok || data.ok === false) throw data;
  return data;
}

/** Run a request from a button: spinner, field errors, a flash with the result, re-render. */
async function run(btn, label, step, fn) {
  const go = async () => {
    errors[step] = {};
    try {
      const r = await fn();
      if (r && r.message) { flash("ok", r.message); announce(r.message); }
      return r;
    } catch (e) {
      const msg = (e && e.error) || "Something went wrong.";
      errors[step] = (e && e.fields) || {};
      flash("err", msg);
      announce(msg);
    } finally {
      render(); schedulePoll();
    }
  };
  return btn ? busy(btn, label, go) : go();
}

async function fetchDownload(path, name) {
  const res = await fetch(path, { headers: { "X-Setup-Token": TOKEN } });
  if (!res.ok) { flash("err", "Download failed (" + res.status + ")"); return; }
  download(await res.blob(), name);
}

const draft = (id, fallback) => id in drafts ? drafts[id] : (fallback == null ? "" : fallback);
const fid = (name) => "f-" + name.replace(/[^a-z0-9]+/gi, "-");
const val = (name) => { const e = $(fid(name)); return e ? e.value : ""; };

/** A console form field: label, description, input, hint, error (from the server's field errors). */
function field(step, o) {
  const id = o.id || fid(o.name);
  const err = (errors[step] || {})[o.name];
  const input = h("input", {
    id, name: o.name, type: o.type || "text", class: o.mono ? "mono" : null, value: draft(id, o.value),
    placeholder: o.placeholder, autocomplete: "off", spellcheck: o.spellcheck || null, inputmode: o.inputmode, maxlength: o.maxlength,
    "aria-describedby": [o.desc ? id + "-desc" : null, o.hint ? id + "-hint" : null, err ? id + "-err" : null].filter(Boolean).join(" ") || null,
    "aria-invalid": err ? "true" : null,
    oninput: (e) => { drafts[id] = e.target.value; if (o.oninput) o.oninput(e); },
  });
  return h("div", { class: "field" },
    h("div", { class: "label-row" }, h("label", { for: id }, o.label), o.opt ? h("span", { class: "opt" }, "– optional") : null),
    o.desc ? h("p", { class: "desc", id: id + "-desc" }, o.desc) : null,
    input,
    o.hint ? h("p", { class: "hint", id: id + "-hint" }, o.hint) : null,
    err ? h("p", { class: "error", id: id + "-err" }, icon("err"), err) : null);
}

/** Radio choices as tiles. */
function tiles(name, label, value, items, onpick) {
  return h("div", { class: "tiles", role: "radiogroup", "aria-label": label },
    items.map((it) => h("label", { class: "tile" + (it.disabled ? " disabled" : "") },
      h("input", { type: "radio", name, value: it.value, checked: value === it.value, disabled: it.disabled, onchange: () => { drafts[name] = it.value; if (onpick) onpick(it.value); } }),
      h("span", { class: "t" }, it.label), it.note ? h("span", { class: "d" }, it.note) : null)));
}

function checkTable(list, empty) {
  if (!list || !list.length) return h("div", { class: "table-empty" }, h("strong", {}, "Not run yet"), empty);
  return h("div", { class: "table-wrap" }, h("table", { class: "table" },
    h("thead", {}, h("tr", {}, h("th", {}, "Status"), h("th", {}, "Check"), h("th", {}, "Result"))),
    h("tbody", {}, list.map((c) => h("tr", { class: c.status },
      h("td", { class: "st" }, status(c.status)),
      h("td", { class: "name" }, c.name || c.label),
      h("td", {}, c.detail, c.hint && c.status !== "ok" ? h("div", { class: "action" }, icon(c.status === "fail" ? "err" : "warn"), h("span", {}, h("strong", {}, "Recommended action: "), c.hint)) : null))))));
}

const counts = (list) => {
  const n = (s) => (list || []).filter((c) => c.status === s).length;
  return h("span", { class: "counts" }, [n("ok") && status("ok", `${n("ok")} passed`), n("warn") && status("warn", `${n("warn")} warnings`), n("fail") && status("fail", `${n("fail")} failed`)].filter(Boolean));
};

// ---------------------------------------------------------------- colour maths (picker only)
// The authoritative tokens come from rm-brand on the server (/api/branding/tokens).
const PRESETS = [
  ["Blue", "#2563EB"], ["Indigo", "#4F46E5"], ["Purple", "#9333EA"], ["Violet", "#7C3AED"],
  ["Teal", "#0D9488"], ["Cyan", "#0891B2"], ["Green", "#16A34A"], ["Emerald", "#059669"],
  ["Orange", "#EA580C"], ["Amber", "#D97706"], ["Red", "#DC2626"], ["Rose", "#E11D48"],
  ["Pink", "#DB2777"], ["Slate", "#475569"],
];

// The same rules as rm_brand::Color::parse (the server enforces them again).
function parseHex(s) {
  const t = String(s).trim();
  const x = t.startsWith("#") ? t.slice(1) : t;
  if (!x) return { error: "Enter a colour code such as #7C3AED." };
  if (x.length !== 6) return { error: "“" + t + "” is not a 6-digit hex colour (like #7C3AED). Three-digit codes are not accepted." };
  if (!/^[0-9a-fA-F]{6}$/.test(x)) return { error: "“" + t + "” has characters other than 0-9 and A-F." };
  return { hex: "#" + x.toUpperCase() };
}
const rgbOf = (hex) => { const n = parseInt(hex.slice(1), 16); return [(n >> 16) & 255, (n >> 8) & 255, n & 255]; };
const hexOf = (r, g, b) => "#" + [r, g, b].map((v) => Math.round(Math.max(0, Math.min(255, v))).toString(16).padStart(2, "0")).join("").toUpperCase();
function hsvToHex(hh, s, v) {
  const f = (n) => { const k = (n + hh / 60) % 6; return v - v * s * Math.max(0, Math.min(k, 4 - k, 1)); };
  return hexOf(f(5) * 255, f(3) * 255, f(1) * 255);
}
function hexToHsv(hex, keepHue) {
  const [r, g, b] = rgbOf(hex).map((x) => x / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
  let hh = keepHue || 0;
  if (d > 0) {
    if (max === r) hh = 60 * (((g - b) / d) % 6);
    else if (max === g) hh = 60 * ((b - r) / d + 2);
    else hh = 60 * ((r - g) / d + 4);
    if (hh < 0) hh += 360;
  }
  return { h: hh, s: max === 0 ? 0 : d / max, v: max };
}
function hslToHex(hh, s, l) {
  s /= 100; l /= 100;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => { const k = (n + hh / 30) % 12; return l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1)); };
  return hexOf(f(0) * 255, f(8) * 255, f(4) * 255);
}

// ---------------------------------------------------------------- tokens from the server
const tokenCache = new Map();
function tokensFor(hex) {
  if (tokenCache.has(hex)) return tokenCache.get(hex);
  const p = api("POST", "/api/branding/tokens", { color: hex }).then((r) => ({ tokens: r.tokens, contrast: r.contrast }));
  p.catch(() => tokenCache.delete(hex));
  tokenCache.set(hex, p);
  return p;
}
/** The accent variables of an element, from rm-brand's tokens (light or dark variant). */
function paint(el, t, dark) {
  const set = (k, v) => el.style.setProperty(k, v);
  if (dark) {
    set("--primary", t.primary_dark); set("--primary-hover", t.primary_dark_hover); set("--primary-active", t.primary_dark_hover);
    set("--primary-border", t.primary_dark_hover); set("--on-primary", t.on_primary_dark); set("--primary-soft", t.primary_soft);
  } else {
    set("--primary", t.primary); set("--primary-hover", t.primary_hover); set("--primary-active", t.primary_active);
    set("--primary-border", t.primary_border); set("--on-primary", t.on_primary); set("--primary-soft", t.primary_soft);
  }
}

// ---------------------------------------------------------------- the colour component
// Built once and kept across renders, so typing, focus and the picker are never interrupted.
const B = { applied: null, typed: null, theme: null, dirty: false, saveTimer: null, els: null };

function brandInit() {
  if (B.els) return;
  const els = {};
  els.swatches = PRESETS.map(([name, hex], i) => {
    const b = h("button", { type: "button", role: "radio", class: "swatch-btn", id: "sw-" + name.toLowerCase(),
      "aria-checked": "false", "aria-label": name + " " + hex, title: name + " " + hex, tabindex: i === 0 ? "0" : "-1", "data-hex": hex,
      onclick: () => setColor(hex, "preset"), onkeydown: (e) => swatchKey(e, i) },
    h("span", { class: "sw-chip", "aria-hidden": "true" }), h("span", { class: "sw-name" }, name), h("span", { class: "sw-hex" }, hex));
    b.firstChild.style.setProperty("--sw", hex);
    return b;
  });
  els.palette = h("div", { class: "palette", role: "radiogroup", "aria-labelledby": "palette-label", id: "palette" }, els.swatches);
  els.chip = h("span", { class: "hex-chip", "aria-hidden": "true" });
  els.hex = h("input", { id: "hex", type: "text", class: "mono hex", maxlength: "12", autocomplete: "off", spellcheck: "false",
    "aria-describedby": "hex-hint hex-msg", "aria-invalid": "false",
    oninput: (e) => onHexInput(e.target.value),
    onblur: () => { if (!parseHex(els.hex.value).error) { els.hex.value = B.applied; B.typed = B.applied; } } });
  els.custom = button("Customize…", { id: "customize", onclick: openPanel });
  els.custom.setAttribute("aria-haspopup", "dialog");
  els.custom.setAttribute("aria-expanded", "false");
  els.msg = h("p", { id: "hex-msg", class: "hex-msg", role: "status", "aria-live": "polite" });
  els.contrast = h("p", { class: "hint", id: "contrast" });
  els.warn = h("div", { id: "contrast-warn", hidden: true });
  els.colorRoot = h("div", { class: "color-ui" },
    h("div", { class: "field wide" }, h("span", { class: "label", id: "palette-label" }, "Presets"),
      h("p", { class: "desc" }, "Common brand colours. A colour you type or customize is kept exactly, even when it is close to a preset."),
      els.palette),
    h("div", { class: "field" },
      h("div", { class: "label-row" }, h("label", { for: "hex" }, "Hex colour")),
      h("p", { class: "desc" }, "6 digits, with or without #, any case."),
      h("div", { class: "hex-row" }, h("span", { class: "hex-wrap" }, els.chip, els.hex), els.custom),
      h("p", { class: "hint", id: "hex-hint" }, "Saved as branding.primary_color in provider.json, upper case, with the tokens derived from it."),
      els.msg),
    els.contrast, els.warn);
  els.mocks = [mock(false), mock(true)];
  els.preview = h("div", { class: "previews", id: "preview" }, els.mocks.map((m) => m.root));
  B.els = els;
  buildPanel();
}

/** A small mock of the client window and of the console top bar, accents from --primary* only. */
function mock(dark) {
  const m = { dark };
  m.name = h("span", { class: "m-name" });
  m.name2 = h("span", { class: "m-h" });
  m.icon = h("span", { class: "m-icon", "aria-hidden": "true" });
  m.used = h("span", { class: "m-used" });
  m.root = h("figure", { class: "mock " + (dark ? "dark" : "light"), "data-variant": dark ? "dark" : "light", role: "img" },
    h("figcaption", { class: "m-cap" }, h("strong", {}, dark ? "Dark mode" : "Light mode"), m.used),
    h("div", { class: "m-frame", "aria-hidden": "true" },
      h("div", { class: "m-win" },
        h("div", { class: "m-title" }, m.icon, m.name, h("span", { class: "m-dots" }, h("i"), h("i"), h("i"))),
        h("div", { class: "m-toolbar" },
          h("span", { class: "m-btn m-primary" }, "Connect"),
          h("span", { class: "m-btn m-outline" }, "Share"),
          h("span", { class: "m-badge" }, "2 online")),
        h("div", { class: "m-wbody" },
          h("div", { class: "m-side" },
            h("span", { class: "m-item sel" }, "Macs"), h("span", { class: "m-item" }, "Recent"), h("span", { class: "m-item" }, "Settings")),
          h("div", { class: "m-main" },
            m.name2,
            h("span", { class: "m-input" }, "ID 123 456 789"),
            h("span", { class: "m-line" }, "Trouble connecting? ", h("span", { class: "m-link" }, "Connection help")),
            h("span", { class: "m-line" }, h("span", { class: "m-toggle on" }, h("i")), "Start with Windows"),
            h("span", { class: "m-states" },
              h("span", { class: "m-btn m-primary sm" }, "Normal"),
              h("span", { class: "m-btn m-primary sm hover" }, "Hover"),
              h("span", { class: "m-btn m-primary sm active" }, "Pressed"))))),
      h("div", { class: "m-console" },
        h("div", { class: "m-topnav" }, h("span", { class: "m-brand" }, "Relay"), h("span", { class: "m-svc" }, "Relay setup"), h("span", { class: "m-badge2" }, "First run")),
        h("div", { class: "m-cbody" },
          h("span", { class: "m-step" }, h("i"), "Certificate"),
          h("span", { class: "m-cbtn" }, "Next")))));
  return m;
}

function swatchKey(e, i) {
  const n = PRESETS.length;
  let j = null;
  if (e.key === "ArrowRight" || e.key === "ArrowDown") j = (i + 1) % n;
  else if (e.key === "ArrowLeft" || e.key === "ArrowUp") j = (i - 1 + n) % n;
  else if (e.key === "Home") j = 0;
  else if (e.key === "End") j = n - 1;
  if (j === null) return;
  e.preventDefault();
  setColor(PRESETS[j][1], "preset");
  B.els.swatches[j].focus();
}

function onHexInput(text) {
  B.typed = text;
  const r = parseHex(text);
  const e = B.els;
  if (r.error) {
    e.hex.setAttribute("aria-invalid", "true");
    e.msg.replaceChildren(icon("err"), " " + r.error + " The colour stays " + B.applied + ".");
    e.msg.className = "hex-msg error";
    return; // the last valid colour stays applied; what was typed is left alone
  }
  e.hex.setAttribute("aria-invalid", "false");
  e.msg.replaceChildren();
  e.msg.className = "hex-msg";
  setColor(r.hex, "input");
}

function setColor(hex, source) {
  B.applied = hex;
  B.dirty = true;
  syncColor(source);
  scheduleSave();
}

/** Field, swatches and preview follow B.applied. */
function syncColor(source) {
  const e = B.els, hex = B.applied;
  if (source !== "input") {
    e.hex.value = hex; B.typed = hex;
    e.hex.setAttribute("aria-invalid", "false"); e.msg.replaceChildren(); e.msg.className = "hex-msg";
  }
  e.chip.style.setProperty("--sw", hex);
  let sel = -1;
  e.swatches.forEach((b, i) => {
    const on = b.dataset.hex === hex; // exact matches only: a custom colour selects no preset
    if (on) sel = i;
    b.setAttribute("aria-checked", on ? "true" : "false");
  });
  e.swatches.forEach((b, i) => b.setAttribute("tabindex", i === (sel < 0 ? 0 : sel) ? "0" : "-1"));
  updatePreview();
}

function updatePreview() {
  const e = B.els, hex = B.applied;
  const name = (S.identity && S.identity.name) || "Your product";
  for (const m of e.mocks) {
    m.name.textContent = name; m.name2.textContent = name;
    m.root.setAttribute("aria-label", name + " preview, " + (m.dark ? "dark" : "light") + " mode, primary colour " + hex);
    m.icon.replaceChildren(images.icon.url ? h("img", { src: images.icon.url, alt: "" }) : name.slice(0, 1).toUpperCase());
    m.icon.classList.toggle("has-img", !!images.icon.url);
    const variant = m.dark ? "dark" : "light";
    const used = B.theme === "system" ? "when the system is " + variant : B.theme === variant ? "used (theme: " + B.theme + ")" : "not used (theme: " + B.theme + ")";
    m.used.textContent = used;
    m.root.classList.toggle("unused", used.startsWith("not"));
  }
  tokensFor(hex).then((r) => {
    if (B.applied !== hex) return; // a newer colour is on its way
    for (const m of e.mocks) paint(m.root, r.tokens, m.dark);
    const c = r.contrast;
    e.contrast.textContent = "Text on the primary colour: " + c.on_primary.toFixed(2) + ":1 in light mode (" + r.tokens.on_primary + " on " + r.tokens.primary + "), " +
      c.on_primary_dark.toFixed(2) + ":1 in dark mode (" + r.tokens.on_primary_dark + " on " + r.tokens.primary_dark + "). Tokens derived by rm-brand.";
    const low = !c.light_ok || !c.dark_ok;
    e.warn.hidden = !low;
    e.warn.replaceChildren(low ? alert("warn", "Low contrast", "Below 4.5:1 (WCAG AA for text): labels on this colour are hard to read. You can still use it; a darker or lighter shade reads better.") : "");
  }).catch(() => {});
}

function scheduleSave() {
  clearTimeout(B.saveTimer);
  B.saveTimer = setTimeout(saveBranding, 250);
}

async function saveBranding() {
  const hex = B.applied, theme = B.theme;
  try {
    await api("POST", "/api/branding", { primary_color: hex, theme });
    if (B.applied === hex && B.theme === theme) B.dirty = false;
    renderNav();
  } catch (e) {
    B.els.msg.replaceChildren(icon("err"), " " + ((e && e.error) || "Could not save the colour."));
    B.els.msg.className = "hex-msg error";
  }
}

// ---------------------------------------------------------------- the Customize picker (.modal)
const P = { before: null, hex: null, hsv: { h: 0, s: 0, v: 0 }, dragging: false, prevFocus: null };

function buildPanel() {
  const e = B.els;
  const hues = [0, 25, 45, 60, 90, 140, 165, 190, 210, 230, 260, 290, 320];
  const lights = [92, 84, 74, 62, 50, 40, 31, 22];
  const cols = hues.length;
  const cells = [];
  for (const l of lights) for (const hu of hues) cells.push(hslToHex(hu, hu === 60 ? 85 : 75, l));
  for (let i = 0; i < cols; i++) { const v = 255 - Math.round(i * 255 / (cols - 1)); cells.push(hexOf(v, v, v)); }
  e.gridCells = cells.map((hex, i) => {
    const b = h("button", { type: "button", class: "g-cell", "aria-label": hex, title: hex, tabindex: i === 0 ? "0" : "-1", "data-hex": hex, "aria-pressed": "false",
      onclick: () => panelSet(hex, "grid"),
      onkeydown: (ev) => {
        let j = null;
        if (ev.key === "ArrowRight") j = i + 1; else if (ev.key === "ArrowLeft") j = i - 1;
        else if (ev.key === "ArrowDown") j = i + cols; else if (ev.key === "ArrowUp") j = i - cols;
        else if (ev.key === "Home") j = i - (i % cols); else if (ev.key === "End") j = i - (i % cols) + cols - 1;
        if (j === null || j < 0 || j >= cells.length) return;
        ev.preventDefault();
        e.gridCells.forEach((c) => c.setAttribute("tabindex", "-1"));
        e.gridCells[j].setAttribute("tabindex", "0"); e.gridCells[j].focus();
      } });
    b.style.setProperty("--sw", hex);
    return b;
  });
  e.grid = h("div", { class: "g-grid", role: "group", "aria-label": "Colour grid (arrow keys move, Enter picks)" }, e.gridCells);
  e.grid.style.setProperty("--cols", String(cols));
  e.svThumb = h("span", { class: "sv-thumb", "aria-hidden": "true" });
  e.sv = h("div", { class: "sv", id: "sv", role: "slider", tabindex: "0", "aria-label": "Saturation and brightness",
    "aria-valuemin": "0", "aria-valuemax": "100", "aria-describedby": "sv-help",
    onpointerdown: svDown, onpointermove: svMove, onpointerup: svUp, onpointercancel: svUp, onkeydown: svKey }, e.svThumb);
  e.hue = h("input", { type: "range", class: "hue", id: "hue", min: "0", max: "359", step: "1", "aria-label": "Hue",
    oninput: (ev) => { P.hsv.h = Number(ev.target.value); panelSet(hsvToHex(P.hsv.h, P.hsv.s, P.hsv.v), "hue"); } });
  e.pHex = h("input", { type: "text", class: "mono hex", id: "p-hex", maxlength: "12", autocomplete: "off", spellcheck: "false", "aria-describedby": "p-msg",
    oninput: (ev) => {
      const r = parseHex(ev.target.value);
      e.pMsg.replaceChildren(r.error ? icon("err") : "", r.error ? " " + r.error : "");
      e.pHex.setAttribute("aria-invalid", r.error ? "true" : "false");
      if (!r.error) panelSet(r.hex, "hex");
    } });
  e.pMsg = h("p", { class: "hex-msg error", id: "p-msg", role: "status", "aria-live": "polite" });
  e.native = h("input", { type: "color", id: "p-native", "aria-label": "System colour picker", oninput: (ev) => panelSet(ev.target.value.toUpperCase(), "native") });
  e.before = h("span", { class: "ba-chip", "aria-hidden": "true" }, "Aa");
  e.after = h("span", { class: "ba-chip", "aria-hidden": "true" }, "Aa");
  e.beforeHex = h("span", { class: "mono" }); e.afterHex = h("span", { class: "mono" });
  e.afterInfo = h("span", { class: "muted" });
  e.apply = button("Apply", { kind: "primary", id: "p-apply", onclick: applyPanel });
  e.cancel = button("Cancel", { kind: "link", id: "p-cancel", onclick: cancelPanel });
  e.modal = h("div", { class: "modal picker", role: "dialog", "aria-modal": "true", "aria-labelledby": "cz-title", id: "customize-panel", onkeydown: trapKeys },
    h("div", { class: "m-head" }, h("h2", { id: "cz-title" }, "Customize the primary colour"),
      h("button", { class: "btn link sm", type: "button", "aria-label": "Cancel", onclick: cancelPanel }, icon("close"))),
    h("div", { class: "m-body" },
      h("div", { class: "cz-cols" },
        h("div", {}, h("p", { class: "label" }, "Colour grid"), e.grid),
        h("div", {},
          h("p", { class: "label" }, "Shade (saturation and brightness)"), e.sv,
          h("p", { class: "hint", id: "sv-help" }, "Drag, or use the arrow keys (Shift for bigger steps)."),
          h("label", { class: "label", for: "hue" }, "Hue"), e.hue,
          h("div", { class: "cz-inputs" },
            h("div", { class: "field" }, h("label", { for: "p-hex" }, "Hex"), e.pHex),
            h("div", { class: "field" }, h("label", { for: "p-native" }, "System picker"), e.native)),
          e.pMsg)),
      h("div", { class: "ba", "aria-live": "polite" },
        h("span", { class: "ba-item" }, e.before, h("span", {}, h("span", { class: "muted" }, "Before "), e.beforeHex)),
        h("span", { class: "ba-arrow", "aria-hidden": "true" }, "→"),
        h("span", { class: "ba-item" }, e.after, h("span", {}, h("span", { class: "muted" }, "After "), e.afterHex)),
        e.afterInfo)),
    h("div", { class: "m-foot" }, e.cancel, e.apply));
  e.back = h("div", { class: "modal-back", hidden: true, onclick: (ev) => { if (ev.target === e.back) cancelPanel(); } }, e.modal);
  document.body.append(e.back);
}

function trapKeys(ev) {
  if (ev.key === "Escape") { ev.preventDefault(); cancelPanel(); return; }
  if (ev.key !== "Tab") return;
  const f = [...B.els.modal.querySelectorAll("button, input, [tabindex='0']")].filter((x) => !x.disabled && x.offsetParent !== null && x.getAttribute("tabindex") !== "-1");
  if (!f.length) return;
  const first = f[0], last = f[f.length - 1];
  if (ev.shiftKey && document.activeElement === first) { ev.preventDefault(); last.focus(); }
  else if (!ev.shiftKey && document.activeElement === last) { ev.preventDefault(); first.focus(); }
}

const panelOpen = () => B.els && !B.els.back.hidden;

function openPanel() {
  const e = B.els;
  P.prevFocus = document.activeElement;
  P.before = B.applied; P.hsv = hexToHsv(B.applied);
  e.before.style.setProperty("--sw", P.before); e.beforeHex.textContent = P.before;
  tokensFor(P.before).then((r) => paint(e.before, r.tokens, false)).catch(() => {});
  panelSet(B.applied, "open");
  e.pMsg.replaceChildren();
  e.custom.setAttribute("aria-expanded", "true");
  e.back.hidden = false;
  $("layout").inert = true; $("topnav").inert = true;
  const cell = e.gridCells.find((c) => c.dataset.hex === B.applied);
  (cell || e.sv).focus();
}

function closePanel() {
  const e = B.els;
  e.back.hidden = true;
  $("layout").inert = false; $("topnav").inert = false;
  e.custom.setAttribute("aria-expanded", "false");
  (e.custom.isConnected ? e.custom : P.prevFocus || document.body).focus();
}

function cancelPanel() { closePanel(); announce("Cancelled. The colour stays " + B.applied + "."); }
function applyPanel() { const hex = P.hex; closePanel(); if (hex !== B.applied) setColor(hex, "panel"); flash("ok", "Primary colour " + hex + " applied."); }

function panelSet(hex, source) {
  const e = B.els;
  P.hex = hex;
  if (source !== "sv" && source !== "hue") P.hsv = hexToHsv(hex, P.hsv.h);
  if (source !== "hex") { e.pHex.value = hex; e.pHex.setAttribute("aria-invalid", "false"); e.pMsg.replaceChildren(); }
  if (source !== "native") e.native.value = hex.toLowerCase();
  if (source !== "hue") e.hue.value = String(Math.round(P.hsv.h) % 360);
  e.sv.style.setProperty("--hue-color", hsvToHex(P.hsv.h, 1, 1));
  e.svThumb.style.setProperty("left", (P.hsv.s * 100).toFixed(2) + "%");
  e.svThumb.style.setProperty("top", ((1 - P.hsv.v) * 100).toFixed(2) + "%");
  e.svThumb.style.setProperty("--sw", hex);
  e.sv.setAttribute("aria-valuenow", String(Math.round(P.hsv.s * 100)));
  e.sv.setAttribute("aria-valuetext", "saturation " + Math.round(P.hsv.s * 100) + "%, brightness " + Math.round(P.hsv.v * 100) + "%, " + hex);
  e.gridCells.forEach((c) => c.setAttribute("aria-pressed", c.dataset.hex === hex ? "true" : "false"));
  e.after.style.setProperty("--sw", hex); e.afterHex.textContent = hex;
  tokensFor(hex).then((r) => {
    if (P.hex !== hex) return;
    paint(e.after, r.tokens, false);
    e.afterInfo.textContent = "Text on it: " + r.contrast.on_primary.toFixed(2) + ":1" + (r.contrast.light_ok ? "" : " (below 4.5:1)");
  }).catch(() => {});
}

function svAt(ev) {
  const r = B.els.sv.getBoundingClientRect();
  P.hsv.s = Math.max(0, Math.min(1, (ev.clientX - r.left) / r.width));
  P.hsv.v = Math.max(0, Math.min(1, 1 - (ev.clientY - r.top) / r.height));
  panelSet(hsvToHex(P.hsv.h, P.hsv.s, P.hsv.v), "sv");
}
function svDown(ev) { P.dragging = true; B.els.sv.setPointerCapture(ev.pointerId); B.els.sv.focus(); svAt(ev); ev.preventDefault(); }
function svMove(ev) { if (P.dragging) svAt(ev); }
function svUp(ev) { P.dragging = false; try { B.els.sv.releasePointerCapture(ev.pointerId); } catch (_) {} }
function svKey(ev) {
  const step = ev.shiftKey ? 0.1 : 0.01;
  let { s, v } = P.hsv;
  if (ev.key === "ArrowRight") s += step; else if (ev.key === "ArrowLeft") s -= step;
  else if (ev.key === "ArrowUp") v += step; else if (ev.key === "ArrowDown") v -= step;
  else return;
  ev.preventDefault();
  P.hsv.s = Math.max(0, Math.min(1, s)); P.hsv.v = Math.max(0, Math.min(1, v));
  panelSet(hsvToHex(P.hsv.h, P.hsv.s, P.hsv.v), "sv");
}

// ---------------------------------------------------------------- images
async function refreshImages() {
  for (const slot of ["logo", "icon"]) {
    const up = S && S.branding[slot];
    const sha = up ? up.sha256 : null;
    if (images[slot].sha === sha) continue;
    if (images[slot].url) URL.revokeObjectURL(images[slot].url);
    images[slot] = { sha, url: null };
    if (!sha) continue;
    try {
      const res = await fetch("/api/branding/" + slot + "/image", { headers: { "X-Setup-Token": TOKEN } });
      if (res.ok) images[slot] = { sha, url: URL.createObjectURL(await res.blob()) };
    } catch (_) {}
  }
}

// ---------------------------------------------------------------- steps
const STEPS = [
  { id: "preflight", name: "Preflight" },
  { id: "identity", name: "Product identity" },
  { id: "branding", name: "Branding" },
  { id: "relay", name: "Relay endpoint" },
  { id: "signing", name: "Signing and release" },
  { id: "components", name: "Components and versions" },
  { id: "build", name: "Build" },
  { id: "validate", name: "Validate and export" },
];
const idx = () => STEPS.findIndex((s) => s.id === current);
const next = () => idx() < STEPS.length - 1 && go(STEPS[idx() + 1].id);
const prev = () => idx() > 0 && go(STEPS[idx() - 1].id);
const stepState = (id) => (S.steps.find((s) => s.id === id) || {});

/** One step's page: breadcrumbs, header, content and the wizard footer (Previous, actions, Next). */
function page({ title, desc, actions, nextLabel, nextDisabled }, ...body) {
  const i = idx();
  return h("div", { class: "inner" },
    breadcrumbs(["Taro Enterprise", "Package builder", STEPS[i].name]),
    pageHeader({ step: `Step ${i + 1} of ${STEPS.length}`, title, desc }),
    body,
    h("div", { class: "form-actions" },
      h("div", { class: "left" }, button("Diagnostic log", { kind: "link", icon: "download", onclick: () => fetchDownload("/api/diagnostics.txt", "taro-setup-diagnostics.txt") })),
      i > 0 ? button("Previous", { onclick: prev, id: "prev" }) : null,
      actions,
      i < STEPS.length - 1 ? button(nextLabel || "Next", { kind: "primary", onclick: next, id: "next", disabled: nextDisabled }) : null));
}

const views = {
  preflight() {
    const p = S.preflight;
    const fails = p ? p.checks.filter((c) => c.status === "fail").length : 0;
    return page({ title: "Preflight", desc: "Checks that this PC can reach the GitHub hosts the release files come from and has room for the packages." },
      p && fails ? alert("err", `${fails} check${fails > 1 ? "s" : ""} failed`, "Each row says what to do. The build needs the download hosts and free space.") : null,
      container({ title: "System checks", count: p ? p.checks.length : null, desc: p ? counts(p.checks) : "Operating system, HTTPS to github.com and objects.githubusercontent.com, free space in the output folder, loopback-only listener.",
        actions: button(p ? "Run again" : "Run checks", { kind: p ? "" : "primary", icon: p ? "refresh" : null, id: "run-preflight", onclick: (ev) => run(ev.currentTarget, "Checking…", "preflight", () => api("POST", "/api/preflight/run", {})) }), flush: true },
      checkTable(p && p.checks, "Run the checks to inspect this PC.")),
      p && p.os ? container({ title: "This PC" }, kv([["System", p.os.name + " " + p.os.version], ["Build", p.os.build], ["Architecture", p.os.arch]])) : null);
  },

  identity() {
    const i = S.identity || {};
    const st = "identity";
    const save = button("Save", { kind: "primary", id: "save-identity", onclick: (ev) => run(ev.currentTarget, "Saving…", st, () => api("POST", "/api/identity", {
      provider_id: val("provider_id"), name: val("product.name"), short_name: val("product.short_name"), vendor: val("product.vendor"),
      support_url: val("product.support_url"), support_email: val("product.support_email"),
    })) });
    return page({ title: "Product identity", desc: "Your product's names, written into provider.json for every client and the relay.", nextDisabled: !S.identity },
      Object.keys(errors[st] || {}).length ? alert("err", "Some fields need attention", "Each field below says what is wrong.") : null,
      container({ title: "Identity", foot: h("div", { class: "foot-actions" }, save) },
        h("div", { class: "split" },
          field(st, { name: "product.name", label: "Product name", value: i.name, placeholder: "Acme Connect", desc: "Window titles, About, the Mac's messages (up to 40 characters).",
            oninput: (e) => {
              if (drafts.idTouched || i.provider_id) return; // suggest an ID until it is edited by hand
              const v = e.target.value.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
              const el = $("f-provider-id"); if (el) { el.value = v; drafts["f-provider-id"] = v; }
            } }),
          field(st, { name: "provider_id", label: "Provider ID", value: i.provider_id, placeholder: "acme-cloud", mono: true, spellcheck: "false", desc: "Lower-case letters, digits and dashes.", hint: "Names the output folder and the relay package.", oninput: () => { drafts.idTouched = true; } }),
          field(st, { name: "product.short_name", label: "Short name", value: i.short_name, placeholder: "Acme", spellcheck: "false", desc: "Used in file names: Acme.exe, Acme-1.4.1-windows-x64.zip.", hint: "Letters, digits, spaces, dots, dashes, underscores; up to 16." }),
          field(st, { name: "product.vendor", label: "Vendor", value: i.vendor, placeholder: "Acme Cloud Ltd", desc: "Your company." }),
          field(st, { name: "product.support_url", label: "Support URL", opt: true, type: "url", value: i.support_url, placeholder: "https://acme.example/help", inputmode: "url" }),
          field(st, { name: "product.support_email", label: "Support email", opt: true, type: "email", value: i.support_email, placeholder: "help@acme.example", inputmode: "email" }))),
      container({ title: "Rebuilding for a newer release?", desc: "Load the provider.json of an earlier build: identity, colour, relay endpoint and the same admission key are filled in (clients and relay must share it)." },
        h("div", { class: "field" }, h("label", { for: "import-file" }, "provider.json"),
          h("input", { id: "import-file", type: "file", accept: ".json,application/json", onchange: (ev) => {
            const f = ev.target.files[0]; if (!f) return;
            run(null, "", st, async () => {
              if (f.size > 60000) throw { error: "That file is too large to be a provider.json." };
              const r = await api("POST", "/api/provider/import", { json: await f.text() });
              for (const k of Object.keys(drafts)) delete drafts[k];
              B.applied = null; B.dirty = false; B.typed = null;
              return r;
            });
          } }))));
  },

  branding() {
    brandInit();
    const b = S.branding;
    if (!B.dirty || !B.applied) { B.applied = b.primary_color; B.theme = b.theme; }
    if (!B.theme) B.theme = b.theme;
    const typing = B.typed !== null && (document.activeElement === B.els.hex || parseHex(B.typed).error);
    syncColor(typing ? "input" : "load");
    const uploader = (slot, label, desc) => {
      const up = b[slot];
      const id = "file-" + slot;
      return h("div", { class: "field" },
        h("label", { for: id }, label), h("p", { class: "desc", id: id + "-desc" }, desc),
        h("input", { id, type: "file", accept: "image/png", "aria-describedby": id + "-desc", onchange: (ev) => {
          const f = ev.target.files[0]; if (!f) return;
          run(null, "", "branding", async () => { const r = await api("POST", "/api/branding/" + slot, undefined, f); await refreshImages(); return r; });
        } }),
        up ? h("div", { class: "upload-row" },
          images[slot].url ? h("img", { class: "thumb", src: images[slot].url, alt: label + " preview" }) : null,
          status("ok", slot + ".png " + up.info.width + "×" + up.info.height + ", " + kb(up.info.bytes)),
          button("Remove", { kind: "link", onclick: (ev) => run(ev.currentTarget, "Removing…", "branding", async () => { const r = await api("DELETE", "/api/branding/" + slot); await refreshImages(); return r; }) })) : null);
    };
    return page({ title: "Branding", desc: "One primary colour (hover, pressed, border, soft and dark-mode variants are derived from it), the theme, a logo and an icon." },
      h("div", { class: "cols brand-cols" },
        h("div", {},
          container({ title: "Primary colour", desc: "Pick a preset, type a hex code, or open Customize for any colour." }, B.els.colorRoot),
          container({ title: "Theme", desc: "How your clients start." },
            tiles("theme", "Theme", B.theme, [
              { value: "system", label: "System", note: "Follow Windows or macOS." },
              { value: "light", label: "Light" },
              { value: "dark", label: "Dark" }], (v) => { B.theme = v; B.dirty = true; updatePreview(); scheduleSave(); })),
          container({ title: "Logo and icon", desc: "PNG only: copied unchanged into provider/ as logo.png and icon.png, which every client reads. JPEG and WebP are refused, not converted." },
            uploader("logo", "Logo (logo.png)", "PNG, up to 1 MB and 1024×1024 px, any shape."),
            uploader("icon", "Icon (icon.png)", "Square PNG, up to 1 MB and 1024×1024 px (256×256 or 512×512 works well)."))),
        h("div", { class: "preview-col" },
          container({ title: "Live preview", desc: "Your client and the relay's console with this colour, as provider.json makes them look." }, B.els.preview))));
  },

  relay() {
    const r = S.relay;
    const st = "relay";
    const shown = drafts.key_shown;
    const keyErr = (errors[st] || {})["relay.key"];
    const save = button("Save", { kind: "primary", id: "save-relay", onclick: (ev) => run(ev.currentTarget, "Saving…", st, () => api("POST", "/api/relay", {
      host: val("relay.host"), port: val("relay.port"), key: ($("key-entry") || {}).value || null, client_updates: draft("client_updates", r.client_updates),
      portal: val("account.portal") || null,
    })) });
    return page({ title: "Relay endpoint", desc: "Where your clients connect, and the key they send.", nextDisabled: !r.saved, actions: save },
      container({ title: "Relay server", desc: "Nothing is checked here: the relay's own setup (Wizard 2, on the server) verifies DNS and gets the TLS certificate." },
        h("div", { class: "split" },
          field(st, { name: "relay.host", label: "Relay DNS name", value: r.host, placeholder: "relay.acme.example", mono: true, spellcheck: "false", inputmode: "url", desc: "The host name your relay server will have." }),
          field(st, { name: "relay.port", label: "Port", value: String(r.port || S.config.default_port), inputmode: "numeric", desc: "TCP and UDP.", hint: "Default " + S.config.default_port + "." }))),
      container({ title: "Admission key", desc: "The shared client admission key, not a server administration credential." },
        alert("info", "Every client of yours sends this key", "Your relay admits only clients that do, like the key built into standard Taro. It is in provider.json in every package, so it is not secret from your users. Server administration credentials, TLS keys and passwords are never part of the packages."),
        h("div", { class: "field wide" },
          h("label", { for: "key-view" }, "Current key"),
          h("div", { class: "key-row" },
            h("input", { id: "key-view", type: "text", class: "mono", readonly: true, value: shown || r.key_masked || "(none yet)", "aria-describedby": "key-hint", spellcheck: "false" }),
            button(shown ? "Hide" : "Reveal", { id: "reveal", disabled: !r.key_masked, onclick: (ev) => {
              if (shown) { delete drafts.key_shown; render(); return; }
              run(ev.currentTarget, "…", st, async () => { const x = await api("POST", "/api/relay/key/reveal", {}); drafts.key_shown = x.key; });
            } }),
            button("Copy", { id: "copy", disabled: !r.key_masked, onclick: (ev) => run(ev.currentTarget, "…", st, async () => {
              const x = await api("POST", "/api/relay/key/reveal", {});
              try { await navigator.clipboard.writeText(x.key); } catch (_) { drafts.key_shown = x.key; throw { error: "The browser did not allow copying; the key is shown so you can select it." }; }
              return { message: "Key copied to the clipboard." };
            }) }),
            button("Generate new", { id: "generate", kind: r.key_masked ? "" : "primary", onclick: async (ev) => {
              const btn = ev.currentTarget;
              if (r.key_masked && !(await confirmDialog({ title: "Generate a new admission key?", confirm: "Generate new key", danger: true,
                body: h("p", {}, "Clients built with the current key will not be admitted by a relay that uses the new one.") }))) return;
              delete drafts.key_shown;
              run(btn, "Generating…", st, () => api("POST", "/api/relay/key/generate", {}));
            } })),
          h("p", { class: "hint", id: "key-hint" }, r.key_source ? "Source: " + r.key_source + ". Generated keys are " + S.config.key_len + " random characters [A-Za-z0-9_-]." : "Generate one, or enter the key your relay already uses below."),
          keyErr ? h("p", { class: "error" }, icon("err"), keyErr) : null),
        h("details", { open: !!drafts.key_open, ontoggle: (ev) => { drafts.key_open = ev.target.open; } },
          h("summary", {}, "Use an existing key"),
          field(st, { name: "relay.key.entry", id: "key-entry", label: "Existing key", value: "", mono: true, placeholder: "16-128 letters, digits, - or _", spellcheck: "false", hint: "For a rebuild: the key your relay and earlier clients use." }))),
      container({ title: "Sign-in and machine management", desc: "Optional. Written as account.portal in provider.json." },
        field(st, { name: "account.portal", label: "Portal address", opt: true, value: r.portal || (r.host ? `https://${r.host}` : ""), placeholder: "https://relay.acme.example", mono: true, spellcheck: "false", inputmode: "url",
          desc: "Your Taro Enterprise portal: since 0.2 your relay server serves it, at https://<relay host>/ (the console at /console/). Your users sign in there from the client and see the Macs assigned to them; the client connects to them through your relay.",
          hint: "Without it the client works as standard Taro does: the user enters a Mac's ID and password." })),
      container({ title: "Client updates", desc: "Written as updates.clients in provider.json." },
        tiles("client_updates", "Client updates", draft("client_updates", r.client_updates), [
          { value: "notify", label: "Look for new Taro releases", note: "Offered to the user and installed in place; the provider folder stays." },
          { value: "off", label: "Never look", note: "You ship new packages built with this wizard." }])));
  },

  signing() {
    const g = S.signing;
    const st = "signing";
    const r = g.repo;
    const signedIn = !!(r || g.fork_to);
    const where = r ? r.full_name : g.fork_to;
    const build = drafts.build !== false;
    const ready = ["identity", "branding", "relay"].every((id) => ["ok", "warn"].includes(stepState(id).status));
    const statusOf = (it) => status(it.status === "running" ? "info" : it.status === "skipped" ? "skip" : it.status === "manual" ? "warn" : it.status, { running: "In progress", pending: "Pending", ok: "Done", fail: "Failed", skipped: "Kept", manual: "Do it yourself" }[it.status]);
    const link = (url, text) => h("a", { href: url, target: "_blank", rel: "noreferrer" }, text);
    const detail = (d) => {
      if (!d) return "";
      const m = d.match(/^(.*?)(https:\/\/\S+)$/);
      return m ? [m[1], link(m[2], m[2].includes("/actions/runs/") ? "the run on GitHub" : m[2].replace("https://github.com/", ""))] : d;
    };
    const connectBtn = button(signedIn ? "Connect again" : "Connect", { kind: signedIn ? "" : "primary", id: "gh-connect",
      onclick: (ev) => run(ev.currentTarget, "Connecting…", st, async () => {
        const x = await api("POST", "/api/signing/connect", { token: val("github.token"), repo: drafts.adv ? val("github.repo") : "" });
        drafts[fid("github.token")] = ""; return x;
      }) });
    const plan = () => {
      const items = [];
      if (g.fork_to) items.push("make your own copy of " + g.upstream + " (" + g.fork_to + ") and turn on GitHub Actions in it,");
      else if (r && r.parent === g.upstream) items.push("bring your copy " + r.full_name + " up to date with " + g.upstream + ",");
      if (!g.has_secret || drafts.new_key) items.push("make a release signing key on this PC, store it as the Actions secret TARO_ENTERPRISE_SIGNING_KEY (sealed: only GitHub Actions can read it), commit its public half and keep an encrypted backup on this PC,");
      else items.push("keep the repository's signing key,");
      items.push("release Taro Enterprise " + (g.version || "") + " with GitHub Actions (built and signed there, free), unless it is released already,");
      if (build) items.push("check the release's signature, build your packages and validate them.");
      return items;
    };
    const startBtn = signedIn ? button(g.done ? "Start again" : "Start", { kind: "primary", id: "gh-start", disabled: g.running || (build && !ready),
      onclick: async (ev) => {
        const btn = ev.currentTarget;
        if (!(await confirmDialog({ title: "Start on " + where + "?", confirm: "Start", body: [h("p", {}, "This will:"), h("ul", {}, plan().map((x) => h("li", {}, x))),
          drafts.new_key && g.releases.length ? alert("warn", "A new key next to the old one", "Relays installed from earlier releases trust only the keys they were built with; the old key stays in release-signing.pub.") : null] }))) return;
        run(btn, "Starting…", st, () => api("POST", "/api/signing/start", { taro_version: drafts.adv ? val("signing.taro") || null : null, new_key: !!drafts.new_key, build }));
      } }) : null;
    return page({ title: "Signing and release", desc: "One GitHub token does the rest: your own copy of the Taro repository, the signing key, the release (built and signed by GitHub Actions, free) and your packages, built and validated.", actions: startBtn },
      g.error ? alert("err", "Stopped", detail(g.error)) : null,
      g.done ? alert("ok", build ? "Done: your packages are built and valid" : "Released and signed",
        h("span", {}, build ? "They are in " : "Pick the release in the next step. ", build && S.build.result ? h("span", { class: "path" }, S.build.result.dir) : null, " ",
          g.run ? link(g.run.html_url, "Open the release run") : null)) : null,
      signedIn && build && !ready ? alert("info", "Fill in Steps 2 to 4 first", "Your packages carry the product name, the branding and the relay. Or untick “Build the packages”.") : null,
      container({ title: "GitHub", desc: "The token stays in this program's memory and is sent only to api.github.com.", actions: connectBtn },
        field(st, { name: "github.token", label: "GitHub token", type: "password", value: "", mono: true, spellcheck: "false",
          desc: h("span", {}, "A classic token (ghp_…) with the repo and workflow scopes. ", link(g.new_token_url || "https://github.com/settings/tokens/new?scopes=repo,workflow&description=TaroEnterpriseSetup", "Make one on GitHub"), " (the scopes are ticked for you)."),
          hint: g.login ? "Signed in as " + g.login + "." : null }),
        button(drafts.adv ? "Hide advanced options" : "Advanced options", { kind: "link", id: "gh-adv", onclick: () => { drafts.adv = !drafts.adv; render(); } }),
        drafts.adv ? h("div", { class: "split" },
          field(st, { name: "github.repo", label: "Repository", opt: true, value: r ? r.full_name : "", mono: true, spellcheck: "false", placeholder: "your own copy, made for you",
            desc: "owner/repository with the Enterprise release workflow. Empty: your copy of " + (g.upstream || S.config.release_repo) + "." }),
          field(st, { name: "signing.taro", label: "Taro version for the clients", opt: true, value: "", mono: true, placeholder: "the one in Cargo.toml", desc: "The Taro release whose Windows and Mac clients your packages carry." })) : null,
        signedIn ? kv([
          ["Account", g.login],
          ["Your repository", r ? link(r.html_url, r.full_name) : h("span", {}, g.fork_to, h("span", { class: "muted" }, " (made by Start)"))],
          ["Signing key", g.has_secret ? status("ok", "Set") : status("pending", "Made by Start")],
          ["Version", g.version ? (g.releases.includes("enterprise-v" + g.version) ? g.version + " (released; used as it is)" : g.version) : "–"],
          ["Releases", g.releases.length ? g.releases.join(", ") : "none yet"],
          ["Key from", g.key_source || "–"]], 3) : null),
      signedIn ? container({ title: "Options" },
        h("label", { class: "check" }, h("input", { type: "checkbox", id: "do-build", checked: build, onchange: (e) => { drafts.build = e.target.checked; render(); } }),
          h("span", {}, "Build the packages when the release is ready", h("span", { class: "d" }, "Checks the release's signature, then Steps 6 to 8 run by themselves."))),
        g.has_secret ? h("label", { class: "check" }, h("input", { type: "checkbox", id: "new-key", checked: !!drafts.new_key, onchange: (e) => { drafts.new_key = e.target.checked; render(); } }),
          h("span", {}, "Make a new signing key", h("span", { class: "d" }, "Only if the old one is lost or exposed. Its public half is added next to the old ones."))) : null) : null,
      g.items.length ? container({ title: g.running ? "Running" : g.error ? "Stopped" : "Done", flush: true },
        h("div", { class: "table-wrap" }, h("table", { class: "table" },
          h("thead", {}, h("tr", {}, h("th", {}, "Status"), h("th", {}, "Action"), h("th", {}, "Result"))),
          h("tbody", {}, g.items.map((it) => h("tr", { class: it.status },
            h("td", { class: "st" }, statusOf(it)), h("td", { class: "name" }, it.label),
            h("td", { class: "muted" }, detail(it.detail)))))))) : null,
      g.manual_secret && r ? manualSecret(r, st) : null,
      g.backup_path ? container({ title: "Key backup" }, alert("info", "Keep this file safe", h("span", {}, "The signing key is backed up, encrypted for your Windows account, at ", h("span", { class: "path" }, g.backup_path), ". Your relays trust only this key and the ones before it: keep the backup if you change PC."))) : null);
  },

  components() {
    const c = S.components;
    const rels = c.releases || [];
    const firstUsable = rels.find((r) => r.usable);
    const pick = draft("release", (c.selected && c.selected.tag) || (firstUsable && firstUsable.tag));
    const s = c.selected;
    const discover = button(c.releases ? "Look again" : "Look for releases", { kind: c.releases ? "" : "primary", icon: c.releases ? "refresh" : null, id: "discover",
      onclick: (ev) => run(ev.currentTarget, "Looking…", "components", () => api("POST", "/api/components/discover", {})) });
    const select = c.releases && rels.length ? button("Verify and use " + (pick || ""), { kind: s && s.tag === pick ? "" : "primary", id: "select", disabled: !pick,
      onclick: (ev) => run(ev.currentTarget, "Verifying signature…", "components", () => api("POST", "/api/components/select", { tag: pick })) }) : null;
    return page({ title: "Components and versions", desc: "The signed Taro Enterprise release your packages are built from.", nextDisabled: !s },
      c.error ? alert("err", "Not usable", c.error) : null,
      container({ title: "Enterprise release", desc: "Releases tagged enterprise-v… of " + (S.signing.repo ? S.signing.repo.full_name : S.config.release_repo) + " on GitHub, newest first. Each lists the exact client and relay files it was tested with, signed with a trusted release key.", actions: [discover, select] },
        c.releases ? (rels.length ? tiles("release", "Release", pick, rels.map((r) => ({
          value: r.tag, disabled: !r.usable,
          label: r.tag + (r.prerelease ? " (pre-release)" : "") + (r === firstUsable ? " · newest" : ""),
          note: (r.published_at ? "Published " + r.published_at.slice(0, 10) + ". " : "") + (r.note || ""),
        })), () => render()) : h("div", { class: "table-empty" }, h("strong", {}, "No Enterprise release yet"), "None is published on GitHub."))
          : h("div", { class: "table-empty" }, h("strong", {}, "Not looked up yet"), "Look for releases to list them.")),
      s ? [
        alert("ok", "Signature valid", h("span", {}, s.tag + "'s component manifest is signed by ", h("code", {}, s.signed_by), ".")),
        s.warnings.length ? alert("warn", "Warnings", h("ul", {}, s.warnings.map((w) => h("li", {}, w)))) : null,
        container({ title: "Pinned versions" }, kv([["Enterprise", s.enterprise_version], ["Taro (clients)", s.taro_version], ["Relay", s.relay_version || "–"], ["Released", s.released_at]], 4)),
        container({ title: "Platforms", count: c.platforms.length, desc: "Each selected package is downloaded, verified (size and sha256) and assembled.", flush: true },
          h("div", { class: "table-wrap" }, h("table", { class: "table" },
            h("thead", {}, h("tr", {}, h("th", {}, "Build"), h("th", {}, "Component"), h("th", {}, "File"), h("th", {}, "Size"), h("th", {}, "Source"))),
            h("tbody", {}, S.config.component_ids.map((id) => {
              const comp = s.components.find((x) => x.id === id);
              const on = c.platforms.includes(id) && !!comp;
              return h("tr", { class: comp && !comp.url_ok ? "fail" : "" },
                h("td", { class: "st" }, h("input", { type: "checkbox", class: "plat", id: "plat-" + id, "aria-label": "Build " + (comp ? comp.label : id), checked: on, disabled: !comp || S.build.running,
                  onchange: (ev) => {
                    const set = new Set(c.platforms);
                    if (ev.target.checked) set.add(id); else set.delete(id);
                    run(null, "", "components", () => api("POST", "/api/components/platforms", { platforms: [...set] }));
                  } })),
                h("td", { class: "name" }, comp ? comp.label + " " + comp.version : id),
                h("td", {}, comp ? h("code", {}, comp.file) : h("span", { class: "muted" }, "Not in this release")),
                h("td", {}, comp ? kb(comp.size) : "–"),
                h("td", {}, comp ? (comp.url_ok ? status("ok", new URL(comp.url).host) : [status("fail", "Refused"), h("div", { class: "action" }, icon("err"), h("span", {}, comp.url_error))]) : "–"));
            })))))] : null);
  },

  build() {
    const b = S.build;
    const s = S.components.selected;
    const start = button("Build packages", { kind: "primary", id: "start-build", disabled: b.running || !s,
      onclick: (ev) => run(ev.currentTarget, "Starting…", "build", () => api("POST", "/api/build/start", { output_dir: val("output_dir"), replace: !!drafts.replace })) });
    const cancel = b.running ? button("Cancel build", { kind: "danger", id: "cancel-build", onclick: (ev) => run(ev.currentTarget, "Cancelling…", "build", () => api("POST", "/api/build/cancel", {})) }) : null;
    return page({ title: "Build", desc: "Downloads the official files, verifies them against the signed manifest and assembles your packages.", nextDisabled: !b.result, actions: [cancel, start] },
      S.stale ? alert("warn", "Inputs changed since the last build", "Build again with “Replace an existing build” so the packages match.") : null,
      !s ? alert("info", "Choose a release first", "Components and versions picks the signed release to build from.") : null,
      container({ title: "Output", desc: s ? `Builds ${(S.identity && S.identity.provider_id) || "<provider>"}-${s.taro_version} in this folder: first as a .partial folder, renamed only when everything is written.` : null },
        field("build", { name: "output_dir", label: "Output folder", value: b.output_dir, mono: true, spellcheck: "false", hint: "A full path. It is created if needed." }),
        h("label", { class: "check" }, h("input", { type: "checkbox", id: "replace", checked: !!drafts.replace, onchange: (e) => { drafts.replace = e.target.checked; } }),
          h("span", {}, "Replace an existing build of the same version", h("span", { class: "d" }, "The old folder is removed only after the new one is complete.")))),
      b.items.length ? container({ title: b.running ? "Building: " + b.phase : b.cancelled ? "Cancelled" : b.error ? "Build failed" : "Built", flush: true },
        b.error ? h("div", { class: "pad" }, alert("err", "The build was stopped", b.error, h("div", {}, "Nothing was left in the output folder."))) : null,
        b.cancelled ? h("div", { class: "pad" }, alert("info", "Cancelled", "Nothing was left in the output folder.")) : null,
        h("div", { class: "table-wrap" }, h("table", { class: "table" },
          h("thead", {}, h("tr", {}, h("th", {}, "Status"), h("th", {}, "Part"), h("th", {}, "Progress"))),
          h("tbody", {}, b.items.map((it) => h("tr", { class: it.status },
            h("td", { class: "st" }, status(it.status === "running" ? "info" : it.status === "skipped" ? "skip" : it.status, { running: "In progress", pending: "Pending", ok: "Done", fail: "Failed", skipped: "Skipped" }[it.status])),
            h("td", { class: "name" }, it.label),
            h("td", {}, it.total ? h("progress", { max: String(it.total), value: String(Math.min(it.done, it.total)), "aria-label": it.label + " download" }) : null,
              h("div", { class: "muted" }, it.total ? kb(it.done) + " of " + kb(it.total) + (it.detail ? " · " + it.detail : "") : it.detail || "")))))))) : null,
      container({ title: "About the macOS package" },
        alert("warn", "Not signed with your Apple Developer ID", "The macOS package is a tar.gz with Taro's official, ad-hoc signed program, unchanged. This builder has no Apple Developer ID credentials or notarization service, and changing the program would break its signature. Users remove the download quarantine once (INSTALL.md shows how).")));
  },

  validate() {
    const v = S.validation;
    const r = S.build.result;
    if (!r) return page({ title: "Validate and export", desc: "Every package is opened again and checked; then export a summary." },
      container({ title: "Nothing built yet" }, h("div", { class: "table-empty" }, h("strong", {}, "No packages"), "Build them in the previous step.")));
    const fails = v ? v.checks.filter((c) => c.status === "fail").length : 0;
    return page({ title: "Validate and export", desc: "Each archive was opened again: provider.json byte-identical and valid, the official programs unchanged, the layout as expected, SHA256SUMS right.",
      actions: [
        button("Open folder", { icon: "ext", id: "open-folder", onclick: (ev) => run(ev.currentTarget, "Opening…", "validate", () => api("POST", "/api/output/open", {})) }),
        button("Validate again", { icon: "refresh", id: "revalidate", onclick: (ev) => run(ev.currentTarget, "Validating…", "validate", () => api("POST", "/api/validate/run", {})) })] },
      v ? (v.ok ? alert("ok", `All ${v.checks.length} checks passed`, "These packages are ready to ship.") : alert("err", `${fails} check(s) failed`, "Do not ship these packages.")) : null,
      container({ title: "Validation", count: v ? v.checks.length : null, desc: v ? counts(v.checks) : null, flush: true }, checkTable(v && v.checks, "Validate the output.")),
      container({ title: "Output", desc: h("span", { class: "path" }, r.dir), flush: true },
        h("div", { class: "table-wrap" }, h("table", { class: "table" },
          h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", {}, "Size"), h("th", {}, "sha256"))),
          h("tbody", {}, r.artifacts.map((a) => h("tr", {}, h("td", { class: "name" }, h("code", {}, a.path)), h("td", {}, kb(a.size)), h("td", {}, h("code", { title: a.sha256 }, a.sha256.slice(0, 16) + "…")))))))),
      container({ title: "Export", desc: "A summary of this build without secrets (the admission key appears masked)." },
        h("div", { class: "row" },
          button("Download JSON", { icon: "download", id: "dl-json", onclick: () => fetchDownload("/api/summary.json", "taro-provider-build-summary.json") }),
          button("Download Markdown", { icon: "download", id: "dl-md", onclick: () => fetchDownload("/api/summary.md", "taro-provider-build-summary.md") }))));
  },
};

// ---------------------------------------------------------------- shell
function renderNav() {
  if (!S) return;
  $("steps").replaceChildren(...STEPS.map((s, i) => {
    const st = stepState(s.id).status;
    const cls = { ok: "done", warn: "warn", fail: "bad" }[st] || "";
    const mark = cls === "bad" || cls === "warn" ? "!" : String(i + 1);
    return h("li", { class: [cls, s.id === current ? "current" : ""].join(" ").trim() },
      h("span", { class: "marker", "aria-hidden": "true" }, cls === "done" ? h("span", { class: "i i-ok" }) : st === "running" ? h("span", { class: "spinner" }) : mark),
      h("button", { type: "button", "aria-current": s.id === current ? "step" : false, onclick: () => go(s.id) },
        h("span", { class: "n" }, `Step ${i + 1}`), h("span", { class: "name" }, s.name),
        h("span", { class: "sum" }, stepState(s.id).summary || "")));
  }));
  const done = S.steps.filter((x) => x.status === "ok" || x.status === "warn").length;
  $("side-foot").replaceChildren(h("dl", {},
    h("dt", {}, "Progress"), h("dd", {}, `${done} of ${STEPS.length} steps complete`),
    h("dt", {}, "Release"), h("dd", {}, S.components.selected ? S.components.selected.tag : "–"),
    h("dt", {}, "Output"), h("dd", { class: "path" }, S.build.output_dir)));
  $("version").textContent = "TaroEnterpriseSetup " + S.version;
}

function render() {
  if (!S) return;
  renderNav();
  const active = document.activeElement && document.activeElement.id;
  $("main").replaceChildren(views[current]());
  if (active && $(active) && !panelOpen()) $(active).focus();
}

function go(id) {
  current = id;
  try { sessionStorage.setItem("taroStep", id); } catch (_) {}
  render();
  scrollTo(0, 0);
  $("main").focus();
}

const needsPoll = () => S && (S.build.running || S.components.loading || S.components.verifying || S.signing.running);

function schedulePoll() {
  if (pollTimer || !needsPoll()) return;
  pollTimer = setTimeout(async () => {
    pollTimer = null;
    const before = JSON.stringify(S);
    try { await api("GET", "/api/state"); } catch (_) {}
    if (JSON.stringify(S) !== before && !panelOpen()) render();
    schedulePoll();
  }, 400);
}

$("quit").addEventListener("click", async () => {
  if (!(await confirmDialog({ title: "Quit the wizard?", confirm: "Quit", danger: true,
    body: h("p", {}, "Unsaved inputs are lost. The packages you built stay in the output folder.") }))) return;
  try { await api("POST", "/api/shutdown", {}); } catch (_) {}
  $("main").replaceChildren(h("div", { class: "inner" }, pageHeader({ title: "The wizard has stopped", desc: "You can close this tab." })));
});

(async () => {
  try { const s = sessionStorage.getItem("taroStep"); if (s && STEPS.some((x) => x.id === s)) current = s; } catch (_) {}
  try { await api("GET", "/api/state"); await refreshImages(); render(); schedulePoll(); }
  catch (e) {
    $("main").replaceChildren(h("div", { class: "inner" }, pageHeader({ title: "Cannot load the wizard" }),
      alert("err", (e && e.error) || "Unknown error", "Open the link printed in the TaroEnterpriseSetup window, including its token.")));
  }
})();

// GitHub refused the Actions secret: the operator adds it in the repository's settings. The key
// is copied only from here, only while it waits to be added.
function manualSecret(r, st) {
  const NAME = "TARO_ENTERPRISE_SIGNING_KEY";
  const newUrl = r.html_url + "/settings/secrets/actions/new";
  const copy = (text, what) => navigator.clipboard.writeText(text).then(() => ({ message: what + " copied to the clipboard." }),
    () => { throw { error: "The browser did not allow copying." }; });
  const a = (url, text) => h("a", { href: url, target: "_blank", rel: "noreferrer" }, text);
  return container({ title: "Add the secret yourself", desc: "GitHub did not let this program store the signing key as an Actions secret (an organization policy, or a token without access to secrets). Add it in " + r.full_name + "'s settings; it takes a minute." },
    alert("warn", "Only in GitHub's secret form", "The key signs your releases: paste it nowhere else, never in a file, chat or issue. It is still kept only in this program's memory; the encrypted backup is made once the secret is in place."),
    h("ol", { class: "manual-steps" },
      h("li", {}, "Open ", a(newUrl, "New repository secret"), " (Settings → Secrets and variables → Actions → New repository secret) in " + r.full_name + ". Sign in to GitHub as its owner if asked."),
      h("li", {}, "Name: ", h("code", {}, NAME), " ", button("Copy the name", { id: "copy-secret-name", onclick: (ev) => run(ev.currentTarget, "…", st, () => copy(NAME, "The name")) })),
      h("li", {}, "Secret: ", button("Copy the key", { kind: "primary", id: "copy-secret-key", onclick: (ev) => run(ev.currentTarget, "…", st, async () => {
        const x = await api("POST", "/api/signing/manual-key", {});
        return copy(x.key, "The key");
      }) }), " and paste it into the Secret box as it is (one line, starting with ed25519)."),
      h("li", {}, "Press ", h("strong", {}, "Add secret"), " on GitHub."),
      h("li", {}, "Come back here and press ", h("strong", {}, "Start again"), ": it sees the secret, uses the same key, commits its public half, makes the backup and goes on with the release.")),
    h("p", { class: "muted" }, "Workflows cannot publish either? In ", a(r.html_url + "/settings/actions", "Settings → Actions → General"), ", choose Workflow permissions: Read and write permissions, then Save."));
}
