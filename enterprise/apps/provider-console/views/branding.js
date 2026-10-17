// Branding (provider-wide): the look of the customer portal and this console for every
// organization without its own. Versioned: a new version is a draft until published; publishing
// an older version is the rollback.
import { h, icon, container, pageHeader, kv, button, confirmDialog, alert, flash } from "/_console/console.js";
import { api, when, table, act, field, checkbox, humanize } from "/lib.js";

const MODULE_LABELS = { my_macs: "My Macs", rentals: "Rentals", billing: "Billing", team: "Team" };

// ---- colour helpers (for the preview; the server derives the real tokens)
const parseHex = (s) => { const m = /^#?([0-9a-f]{6})$/i.exec(String(s).trim()); return m ? "#" + m[1].toUpperCase() : null; };
const rgb = (hex) => { const n = parseInt(hex.slice(1), 16); return [(n >> 16) & 255, (n >> 8) & 255, n & 255]; };
const lum = (hex) => { const f = (c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; }; const [r, g, b] = rgb(hex).map(f); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
export const contrastWithWhite = (hex) => 1.05 / (lum(hex) + 0.05);
const mix = (hex, to, t) => { const a = rgb(hex), b = rgb(to); return "#" + a.map((v, i) => Math.round(v + (b[i] - v) * t).toString(16).padStart(2, "0")).join("").toUpperCase(); };

function paint(el, hex) {
  const on = contrastWithWhite(hex) >= 4.5 ? "#FFFFFF" : "#111111";
  el.style.setProperty("--primary", hex);
  el.style.setProperty("--primary-hover", mix(hex, "#000000", 0.1));
  el.style.setProperty("--primary-active", mix(hex, "#000000", 0.2));
  el.style.setProperty("--primary-border", mix(hex, "#000000", 0.28));
  el.style.setProperty("--primary-soft", mix(hex, "#FFFFFF", 0.88));
  el.style.setProperty("--on-primary", on);
}

function swatch(hex) {
  const s = h("span", { class: "swatch", "aria-hidden": "true" });
  s.style.background = hex;
  return h("span", { class: "nowrap" }, s, h("span", { class: "mono" }, hex));
}

function thumb(id, label) {
  return id ? h("img", { class: "thumb", src: "/api/v1/assets/" + id, alt: label }) : h("span", { class: "muted" }, "None");
}

/** A small picture of the portal in a colour: top bar, a selected nav item, a button, a link. */
function preview(c) {
  const p = h("div", { class: "brand-preview", "aria-label": "Preview", role: "img" },
    h("div", { class: "bp-top" }, c.logoAssetId ? h("img", { src: "/api/v1/assets/" + c.logoAssetId, alt: "" }) : h("span", { class: "bp-mark" }, (c.displayName || "T").charAt(0).toUpperCase()),
      h("span", { class: "bp-name" }, c.displayName || "Taro Enterprise"), h("span", { class: "bp-sp" }), h("span", { class: "bp-user" }, "alex@customer.com")),
    h("div", { class: "bp-body" },
      h("div", { class: "bp-side" }, ["My Macs", "Rentals", "Billing", "Team"].map((t, i) => h("span", { class: "bp-item" + (i === 0 ? " sel" : "") }, t))),
      h("div", { class: "bp-main" }, h("span", { class: "bp-h" }, "My Macs"), h("span", { class: "bp-line" }, "Mac mini 001 · ", h("span", { class: "bp-link" }, "Connect")),
        h("span", { class: "bp-btns" }, h("span", { class: "bp-btn primary" }, "Rent a Mac"), h("span", { class: "bp-btn" }, "Details")))));
  paint(p, parseHex(c.primaryColor) || "#6D5AE6");
  return p;
}

export async function page(ctx) {
  const r = await api("GET", "/api/v1/ops/branding");
  const c = r.current;
  const live = r.published_version;
  const publish = async (btn, v) => {
    const older = live != null && v.version < live;
    if (!await confirmDialog({ title: older ? `Roll back to version ${v.version}?` : `Publish version ${v.version}?`, confirm: older ? "Roll back" : "Publish",
      body: [h("p", {}, "The customer portal and this console use it at once, for every organization without its own branding."), preview(v.config)] })) return;
    await act(btn, "Publishing…", () => api("POST", `/api/v1/ops/branding/${v.version}/publish`), { ok: `Version ${v.version} is live.`, after: async () => { reloadTokens(); await ctx.refreshBranding(); ctx.refresh(); } });
  };
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Catalog"], ["Branding"]]),
    pageHeader({ title: "Branding", desc: "The provider's look in the customer portal and this console. Organizations with their own branding (if entitled) override it.",
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }), h("a", { class: "btn primary", href: "/branding/new", id: "new-branding" }, icon("plus"), "New version")] }),
    live == null ? alert("info", "Nothing published yet", "The built-in Taro Enterprise look is in use.") : null,
    h("div", { class: "cols" },
      container({ title: "Live", desc: live != null ? `Version ${live}` : "Built-in default" }, kv([
        ["Display name", c.displayName],
        ["Primary colour", swatch(c.primaryColor)],
        ["Theme", humanize(c.themeMode)],
        ["Density", humanize(c.density)],
        ["Modules", c.enabledModules.map((m) => MODULE_LABELS[m] || m).join(", ") || "None"],
        ["Support contact", c.supportContact || "–"],
        ["Logo", thumb(c.logoAssetId, "Logo")],
        ["Favicon", thumb(c.faviconAssetId, "Favicon")],
      ], 2)),
      container({ title: "Preview" }, preview(c))),
    container({ title: "Versions", count: r.versions.length, flush: true, desc: "Newest first (up to 50). Publishing an older version rolls back to it." },
      table([
        { label: "Version", cell: (v) => h("strong", {}, "v" + v.version) },
        { label: "Display name", cell: (v) => v.config.displayName },
        { label: "Colour", cell: (v) => swatch(v.config.primaryColor) },
        { label: "Theme", cell: (v) => humanize(v.config.themeMode) + " · " + humanize(v.config.density) },
        { label: "Created", cell: (v) => when(v.created_at), class: "nowrap" },
        { label: "Status", cell: (v) => v.version === live ? h("span", { class: "badge green" }, "Live") : v.published_at ? h("span", { class: "badge" }, "Published before") : h("span", { class: "badge blue" }, "Draft") },
        { label: "Actions", class: "actions", cell: (v) => [
          v.version !== live ? button(live != null && v.version < live ? "Roll back" : "Publish", { kind: "sm" + (v.published_at ? "" : " primary"), onclick: (e) => publish(e.currentTarget, v) }) : null,
          h("a", { class: "btn sm link", href: "/branding/new?from=" + v.version }, "Copy"),
        ] },
      ], r.versions, { caption: "Branding versions", rowClass: (v) => v.version === live ? "current" : null,
        empty: { title: "No versions yet", text: "Create the first version with your name, colour and logo.", action: h("a", { class: "btn primary", href: "/branding/new" }, "New version") } })));
}

function reloadTokens() {
  const l = document.querySelector('link[href^="/brand/tokens.css"]');
  if (l) l.setAttribute("href", "/brand/tokens.css?v=" + Date.now());
}

async function upload(kind, file) {
  if (file.size > 1024 * 1024) throw new Error("Images are at most 1 MB.");
  if (!["image/png", "image/jpeg", "image/webp"].includes(file.type)) throw new Error("Upload a PNG, JPEG or WebP image.");
  return api("POST", "/api/v1/ops/assets?kind=" + kind, undefined, { raw: await file.arrayBuffer(), contentType: file.type });
}

export async function create(ctx) {
  const r = await api("GET", "/api/v1/ops/branding");
  const fromV = ctx.query.get("from");
  const base = (fromV && (r.versions.find((v) => String(v.version) === fromV) || {}).config) || r.current;
  const state = { ...base, enabledModules: [...base.enabledModules] };

  const prevBox = h("div", { class: "preview-slot" });
  const redraw = () => prevBox.replaceChildren(preview(state));

  const fName = field({ label: "Display name", name: "displayName", value: state.displayName, required: true, maxlength: 60, desc: "Shown in the top bar, page titles and mails. 1 to 60 characters, no < or >.",
    oninput: () => { state.displayName = fName.control.value; redraw(); } });

  const colorIn = h("input", { type: "color", value: (parseHex(state.primaryColor) || "#6D5AE6").toLowerCase(), "aria-label": "Pick a colour", id: "color-pick" });
  const fHex = field({ label: "Primary colour", name: "primaryColor", value: state.primaryColor, mono: true, maxlength: 7, required: true, desc: "A #RRGGBB colour for buttons, links and selection." });
  const contrast = h("p", { class: "hint", id: "contrast", "aria-live": "polite" });
  const setColor = (hex, from) => {
    state.primaryColor = hex;
    if (from !== "hex") fHex.control.value = hex;
    if (from !== "picker") colorIn.value = hex.toLowerCase();
    const cr = contrastWithWhite(hex);
    contrast.replaceChildren(cr >= 4.5 ? h("span", { class: "status ok" }, icon("ok"), `Contrast with white ${cr.toFixed(1)}:1 — good for text.`)
      : h("span", { class: "status warn" }, icon("warn"), `Contrast with white ${cr.toFixed(1)}:1 — below 4.5:1, white text on it may be hard to read.`));
    redraw();
  };
  colorIn.addEventListener("input", () => { fHex.setError(null); setColor(colorIn.value.toUpperCase(), "picker"); });
  fHex.control.addEventListener("input", () => { const x = parseHex(fHex.control.value); fHex.setError(x ? null : "A colour such as #6D5AE6."); if (x) setColor(x, "hex"); });
  fHex.control.parentNode.insertBefore(h("div", { class: "color-row" }, colorIn, fHex.control), fHex.control.nextSibling);
  fHex.insertBefore(contrast, fHex.querySelector(".error"));

  const fTheme = field({ label: "Theme", name: "themeMode", type: "select", value: state.themeMode, options: [["system", "Follow the system"], ["light", "Light"], ["dark", "Dark"]], desc: "The portal's colour scheme." });
  fTheme.control.addEventListener("change", () => { state.themeMode = fTheme.control.value; });
  const fDensity = field({ label: "Density", name: "density", type: "select", value: state.density, options: [["comfortable", "Comfortable"], ["compact", "Compact"]] });
  fDensity.control.addEventListener("change", () => { state.density = fDensity.control.value; });
  const mods = h("fieldset", { class: "mods" }, h("legend", {}, "Portal modules"), h("p", { class: "desc" }, "The areas customers see in the portal."),
    r.modules.map((m) => checkbox({ label: MODULE_LABELS[m] || m, name: "mod-" + m, checked: state.enabledModules.includes(m),
      onchange: (e) => { state.enabledModules = e.target.checked ? [...new Set([...state.enabledModules, m])] : state.enabledModules.filter((x) => x !== m); } })));
  const fSupport = field({ label: "Support contact", name: "supportContact", value: state.supportContact, opt: true, placeholder: "support@provider.com or https://help.provider.com", desc: "An email address or an https:// link." });

  const imageField = (kind, label, hint) => {
    const key = kind === "logo" ? "logoAssetId" : "faviconAssetId";
    const thumbBox = h("span", { class: "img-slot" });
    const show = () => thumbBox.replaceChildren(state[key] ? h("img", { class: "thumb lg", src: "/api/v1/assets/" + state[key], alt: label }) : h("span", { class: "muted" }, "None"),
      state[key] ? button("Remove", { kind: "sm link", onclick: () => { state[key] = null; show(); redraw(); } }) : null);
    const input = h("input", { type: "file", accept: "image/png,image/jpeg,image/webp", id: "file-" + kind, "aria-describedby": "file-" + kind + "-hint" });
    const msg = h("p", { class: "error", hidden: true });
    input.addEventListener("change", async () => {
      const f = input.files && input.files[0];
      if (!f) return;
      msg.hidden = true;
      try {
        const res = await upload(kind, f);
        state[key] = res.id;
        show(); redraw();
        flash("ok", `${label} uploaded (${res.width}×${res.height}).`);
      } catch (e) { msg.replaceChildren(icon("err"), e.message); msg.hidden = false; }
      input.value = "";
    });
    show();
    return h("div", { class: "field" }, h("div", { class: "label-row" }, h("label", { for: "file-" + kind }, label), h("span", { class: "opt" }, "– optional")),
      h("div", { class: "upload-row" }, thumbBox, input), h("p", { class: "hint", id: "file-" + kind + "-hint" }, hint), msg);
  };

  const publishNow = checkbox({ label: "Publish when saved", desc: "Otherwise it is saved as a draft to publish later." });
  const errBox = h("div", { role: "alert" });
  const save = button("Save version", { kind: "primary", type: "submit", id: "branding-save" });
  const form = h("form", { novalidate: true, id: "branding-form", onsubmit: async (e) => {
    e.preventDefault();
    errBox.replaceChildren();
    state.displayName = fName.control.value.trim();
    state.supportContact = fSupport.control.value.trim();
    const hex = parseHex(fHex.control.value);
    fName.setError(state.displayName && !/[<>]/.test(state.displayName) ? null : "1 to 60 characters, no < or >.");
    fHex.setError(hex ? null : "A colour such as #6D5AE6.");
    if (!state.displayName || /[<>]/.test(state.displayName) || !hex) return;
    state.primaryColor = hex;
    await act(save, "Saving…", async () => {
      let res;
      try { res = await api("POST", "/api/v1/ops/branding", state); }
      catch (x) { errBox.replaceChildren(alert("err", "Not saved", x.message)); throw x; }
      if (publishNow.control.checked) { await api("POST", `/api/v1/ops/branding/${res.version}/publish`); reloadTokens(); await ctx.refreshBranding(); }
      for (const w of res.warnings || []) flash("warn", w);
      flash("ok", publishNow.control.checked ? `Version ${res.version} saved and published.` : `Version ${res.version} saved as a draft.`);
      ctx.navigate("/branding");
    });
  } },
  errBox,
  h("div", { class: "cols" },
    h("div", {},
      container({ title: "Identity" }, fName, fSupport),
      container({ title: "Colour and layout" }, fHex, h("div", { class: "form-grid" }, fTheme, fDensity)),
      container({ title: "Images", desc: "PNG, JPEG or WebP, at most 1 MB and 1024×1024 pixels." },
        imageField("logo", "Logo", "Shown in the top bar. A square or wide image."),
        imageField("favicon", "Favicon", "The browser tab icon. Square, 16×16 pixels or more.")),
      container({ title: "Portal" }, mods)),
    h("div", { class: "sticky" }, container({ title: "Preview", desc: "Approximate: the server derives the final tints." }, prevBox),
      container({}, publishNow, h("div", { class: "form-actions" }, h("a", { class: "btn link", href: "/branding" }, "Cancel"), save)))));
  setColor(parseHex(state.primaryColor) || "#6D5AE6", null);

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Branding", "/branding"], ["New version"]]),
    pageHeader({ title: "New branding version", desc: fromV ? `Starting from version ${fromV}.` : "Starting from the live configuration." }),
    form);
}
