// The Mac fleet (mac_resources, reconciled with the provider) and the provider integrations.
// Operators hold a Mac out of new rentals (with a reason) and release the hold; nothing here
// changes hardware state.
//
// With the provider's own Macs (TARO_PROVIDER=fleet: Taro for Mac enrolls them with a code made
// here and keeps a heartbeat), the page lists them from /api/v1/ops/fleet instead, refreshed every
// 10 seconds: Add a Mac (an enrollment code), Control (opens the Mac in the Taro app; a customer's
// Mac is taken over until Release) and Remove.
import { h, icon, container, pageHeader, kv, button, confirmDialog, alert, flash } from "../_console/console.js";
import { S, api, can, st, when, table, shortId, toolbar, searchBox, selectBox, act, humanize, field, formDialog, readOnlyNote, stats, copyButton, num, infoDialog, product } from "../lib.js";

const STATES = ["available", "assigned", "reserved", "maintenance", "unavailable", "releasing", "released", "error", "discovered"];

function spec(r) {
  return h("span", { class: "nowrap" }, h("strong", {}, r.chip), h("span", { class: "muted" }, ` · ${r.cpu_cores} cores · ${r.memory_gb} GB · ${r.storage_gb >= 1024 ? r.storage_gb / 1024 + " TB" : r.storage_gb + " GB"}`));
}

async function holdDialog(r, after) {
  const f = field({ label: "Reason", name: "reason", type: "textarea", rows: 3, maxlength: 200, required: true,
    desc: "Shown to other operators and kept in the audit log. Up to 200 characters.", placeholder: "e.g. Disk errors reported, waiting for the provider" });
  const ok = await formDialog({ title: `Hold ${r.display_name}`, confirm: "Hold Mac", danger: true,
    body: [h("p", {}, "The Mac is kept out of new rentals. A rental on it now goes on; nothing changes on the hardware."), f],
    submit: async () => {
      const reason = f.control.value.trim();
      if (!reason) { f.setError("Say why."); throw new Error("Say why the Mac is held."); }
      await api("POST", `api/v1/ops/mac-resources/${r.id}/hold`, { reason });
    } });
  if (ok) { flash("ok", `${r.display_name} is held.`); await after(); }
}

async function releaseHold(btn, r, after) {
  if (!await confirmDialog({ title: `Release the hold on ${r.display_name}?`, confirm: "Release hold",
    body: [h("p", {}, "The Mac goes back to unavailable; the next reconciliation makes it available again if the provider has it free."),
      r.hold_reason ? h("p", { class: "muted" }, "Held because: " + r.hold_reason) : null] })) return;
  await act(btn, "Releasing…", () => api("DELETE", `api/v1/ops/mac-resources/${r.id}/hold`), { ok: `Hold released on ${r.display_name}.`, after });
}

function actions(r, after) {
  if (!can("ops.inventory.manage")) return null;
  return r.hold_reason
    ? button("Release hold", { kind: "sm", onclick: (e) => releaseHold(e.currentTarget, r, after) })
    : button("Hold", { kind: "sm danger", onclick: () => holdDialog(r, after) });
}

// ---------------------------------------------------------------- the provider's own Macs (fleet)

const REFRESH_MS = 10000;

function online(x) {
  return h("div", { class: "online" + (x.online ? " on" : "") },
    h("span", { class: "dot", "aria-hidden": "true" }), h("span", { class: "o-text" }, x.online ? "Online" : "Offline"),
    h("div", { class: "muted small" }, x.last_seen_at ? ["Last seen ", when(x.last_seen_at, { rel: true })] : "Not seen yet"));
}

function controlBadge(x) {
  if (!x.control) return null;
  const who = (x.control.by_email || "An administrator") + (x.control.mine ? " (you)" : "");
  return h("div", { class: "control-badge" }, icon("lock"), h("span", {}, `Controlled by ${who} since `, when(x.control.since)));
}

/** The Mac's ID for buttons (focus survives a refresh). */
const bid = (what, x) => `${what}-${x.id}`;

async function control(btn, x, ui) {
  if (x.rental_id && !x.control) {
    if (!await confirmDialog({ title: `Take control of ${x.display_name}?`, confirm: "Take control", danger: true,
      body: h("p", {}, `${x.assigned_email || "A customer"} is using this Mac. Taking control disconnects them, and they cannot connect until you release it.`) })) return;
  }
  const r = await act(btn, "Opening…", () => api("POST", `api/v1/ops/fleet/${x.id}/control`), { after: ui.reload });
  if (!r || !r.taro_link) return;
  ui.opened(x, r);
  location.href = r.taro_link;
}

async function release(btn, x, ui) {
  const mine = x.control && x.control.mine;
  const who = mine ? "Your" : `${(x.control && x.control.by_email) || "The administrator"}'s`;
  if (!await confirmDialog({ title: `Release ${x.display_name}?`, confirm: "Hand it back",
    body: h("p", {}, `${who} session on it ends.`, x.assigned_email ? ` ${x.assigned_email} can connect again.` : "") })) return;
  await act(btn, "Releasing…", () => api("POST", `api/v1/ops/fleet/${x.id}/release`), { ok: `${x.display_name} is released.`, after: () => { ui.closeNote(x); return ui.reload(); } });
}

async function remove(btn, x, ui) {
  if (!await confirmDialog({ title: `Remove ${x.display_name}?`, confirm: "Remove Mac", danger: true,
    body: [h("p", {}, "The Mac leaves the fleet and its enrollment stops working. Customers can no longer get it."),
      h("p", { class: "muted" }, "To add it again, enroll it with a new code (Add a Mac).")] })) return;
  await act(btn, "Removing…", () => api("DELETE", `api/v1/ops/fleet/${x.id}`), { ok: `${x.display_name} is removed.`, after: ui.reload });
}

function fleetActions(x, ui) {
  if (!can("ops.inventory.manage")) return null;
  const free = !x.control || x.control.mine;
  return [
    x.agent_id && free ? button("Control", { kind: "sm", id: bid("control", x), onclick: (e) => control(e.currentTarget, x, ui) }) : null,
    x.control ? button("Release", { kind: "sm", id: bid("release", x), onclick: (e) => release(e.currentTarget, x, ui) }) : null,
    x.agent_id && !x.rental_id ? button("Remove", { kind: "sm danger", id: bid("remove", x), onclick: (e) => remove(e.currentTarget, x, ui) }) : null,
  ];
}

async function addMac(btn, fl, ui) {
  const r = await act(btn, "Making a code…", () => api("POST", "api/v1/ops/fleet/enrollment-codes", {}));
  if (!r) return;
  const server = r.server || (fl.enroll && fl.enroll.server) || location.origin;
  const relay = fl.enroll && fl.enroll.relay;
  await infoDialog({ title: "Add a Mac", id: "add-mac-dialog", body: [
    h("p", {}, "Enter this code in the Taro app on the Mac you add. It works once, until ", when(r.expires_at), "."),
    h("div", { class: "enroll-code" }, h("span", { class: "mono", id: "enroll-code" }, r.code), copyButton(r.code, "Copy code")),
    kv([
      ["Server", h("span", { class: "id-row" }, h("span", { class: "mono", id: "enroll-server" }, server), copyButton(server, "Copy the server address"))],
      relay ? ["Relay", h("span", { class: "mono" }, relay)] : null,
    ], 2),
    h("ol", { class: "steps" },
      h("li", {}, `Install ${product()} for Mac (Taro.app) on the Mac.`),
      h("li", {}, "Open it, allow Screen Recording and Accessibility."),
      h("li", {}, "Enter this code.")),
    h("p", { class: "muted small" }, "The Mac shows up in this list once it has enrolled."),
  ] });
  await ui.reload();
}

async function enrolled(ctx, fl) {
  const manage = can("ops.inventory.manage");
  const holder = h("div", { id: "fleet-table" });
  const figures = h("div", { id: "fleet-stats" });
  const noteBox = h("div", { id: "control-note" });
  let q = (ctx.query.get("q") || "").toLowerCase();
  let noteFor = null;
  const dl = S.providers && S.providers.app_download_url;

  const ui = {
    reload,
    opened(x, r) {
      noteFor = x.id;
      noteBox.replaceChildren(alert("info", `Opening ${x.display_name} in Taro`,
        h("p", {}, r.took_over ? `${x.assigned_email || "The customer"} is disconnected and cannot connect until you release the Mac. ` : "",
          "Taro did not open? Open it with the button below", dl ? ", or download the app first." : " (install the Taro app first).",
          " The link works for 2 minutes; press Control again for a new one."),
        h("div", { class: "alert-actions" },
          h("a", { class: "btn sm primary", href: r.taro_link, id: "open-in-taro" }, "Open in Taro"),
          dl ? h("a", { class: "btn sm ml", href: dl, target: "_blank", rel: "noopener", id: "download-app" }, icon("download"), "Download the app") : null)));
    },
    closeNote(x) { if (noteFor === x.id) { noteFor = null; noteBox.replaceChildren(); } },
  };

  function draw() {
    const macs = fl.macs;
    const n = (f) => macs.filter(f).length;
    figures.replaceChildren(stats([["Macs", macs.length], ["Online", n((x) => x.online), "ok"], ["Offline", n((x) => !x.online), n((x) => !x.online) ? "bad" : null],
      ["Assigned", n((x) => x.rental_id)], ["Controlled", n((x) => x.control), n((x) => x.control) ? "warn" : null]]));
    const rows = q ? macs.filter((x) => [x.display_name, x.model, x.chip, x.taro_id || "", x.assigned_email || "", x.organization || ""].join(" ").toLowerCase().includes(q)) : macs;
    const focused = document.activeElement && document.activeElement.id && holder.contains(document.activeElement) ? document.activeElement.id : null;
    holder.replaceChildren(table([
      { label: "Mac", cell: (x) => h("div", {}, h("a", { href: "macs/" + x.id, class: "strong" }, x.display_name), h("div", { class: "muted small" }, [x.chip, x.model].filter(Boolean).join(" · ") || "–")) },
      { label: "macOS", cell: (x) => h("div", {}, x.macos || "–", x.agent_version ? h("div", { class: "muted small" }, "Taro " + x.agent_version) : null), class: "nowrap" },
      { label: "Online", cell: online, class: "nowrap" },
      { label: "ID at the relay", cell: (x) => x.taro_id ? h("span", { class: "mono" }, x.taro_id) : h("span", { class: "muted" }, "Not reported yet"), class: "nowrap" },
      { label: "State", cell: (x) => h("div", {}, st(x.state), x.hold_reason ? h("div", { class: "hold", title: x.hold_reason }, icon("lock"), h("span", {}, "Held: " + x.hold_reason)) : null, controlBadge(x)) },
      { label: "Assigned to", cell: (x) => x.assigned_email || x.organization
        ? h("div", {}, h("strong", {}, x.assigned_email || "–"), x.organization && x.organization !== x.assigned_email ? h("div", { class: "muted small" }, x.organization) : null)
        : h("span", { class: "muted" }, "–") },
      manage ? { label: "Actions", class: "actions", cell: (x) => fleetActions(x, ui) } : null,
    ].filter(Boolean), rows, { caption: "Macs", id: "fleet-grid", rowClass: (x) => x.control ? "held" : null,
      empty: macs.length ? { title: "No Mac matches the search", text: "Clear the search to see every Mac." }
        : { title: "No Macs yet", text: "Install Taro for Mac on a Mac and enter an enrollment code (Add a Mac).",
          action: manage ? button("Add a Mac", { kind: "primary", icon: "plus", onclick: (e) => addMac(e.currentTarget, fl, ui) }) : null } }));
    if (focused) { const again = document.getElementById(focused); if (again) again.focus({ preventScroll: true }); }
  }

  let timer = null;
  async function reload() {
    if (ctx.stale()) return;
    try { const next = await api("GET", "api/v1/ops/fleet"); if (!ctx.stale()) fl = next; } catch (_) { /* try again next time */ }
    if (!ctx.stale()) draw();
  }
  function tick() {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      if (ctx.stale()) return;
      if (!document.querySelector(".modal-back")) await reload(); // not under an open dialog
      tick();
    }, REFRESH_MS);
  }

  draw();
  tick();
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Infrastructure"], ["Mac fleet"]]),
    pageHeader({ title: "Mac fleet", desc: "The Macs enrolled with Taro for Mac. Each one keeps a heartbeat with this server; the list refreshes every 10 seconds.",
      actions: [button("Refresh", { icon: "refresh", onclick: (e) => busyReload(e.currentTarget) }),
        manage ? button("Add a Mac", { kind: "primary", icon: "plus", id: "add-mac", onclick: (e) => addMac(e.currentTarget, fl, ui) }) : null] }),
    manage ? null : readOnlyNote("the fleet"),
    noteBox,
    figures,
    container({ title: "Macs", flush: true },
      toolbar(searchBox({ label: "Search Macs", value: ctx.query.get("q") || "", id: "fleet-search", placeholder: "Name, chip, relay ID, customer",
        oninput: (v) => { q = v.toLowerCase(); ctx.setQuery({ q: v }); draw(); } })),
      holder));

  async function busyReload(btn) { await act(btn, "Refreshing…", reload); }
}

export async function macs(ctx) {
  const fl = await api("GET", "api/v1/ops/fleet").catch(() => null);
  if (fl && fl.provider === "fleet") return enrolled(ctx, fl);
  const f = { state: ctx.query.get("state") || "", region: ctx.query.get("region") || "", chip: ctx.query.get("chip") || "" };
  const q = (ctx.query.get("q") || "").toLowerCase();
  const qs = new URLSearchParams(Object.entries(f).filter(([, v]) => v)).toString();
  const [r, all] = await Promise.all([
    api("GET", "api/v1/ops/mac-resources" + (qs ? "?" + qs : "")),
    qs ? api("GET", "api/v1/ops/mac-resources") : null,
  ]);
  const everything = (all || r).resources;
  const caps = r.capabilities;
  const regions = [...new Set([...(caps ? caps.regions : []), ...everything.map((x) => x.region)])].sort();
  const chips = [...new Set([...(caps ? caps.chips : []), ...everything.map((x) => x.chip)])].sort();
  let rows = r.resources;
  if (q) rows = rows.filter((x) => [x.display_name, x.external_id, x.model, x.organization || "", x.hold_reason || ""].join(" ").toLowerCase().includes(q));
  const n = (s) => everything.filter((x) => x.state === s).length;
  const held = everything.filter((x) => x.hold_reason).length;

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Infrastructure"], ["Mac fleet"]]),
    pageHeader({ title: "Mac fleet", desc: h("span", {}, "Every Mac from the provider as Taro sees it. Reconciliation compares it with the provider every minute. ",
      r.simulated_provider ? h("span", { class: "badge amber" }, "Simulated provider") : null),
    actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    can("ops.inventory.manage") ? null : readOnlyNote("the fleet"),
    stats([["Macs", everything.length], ["Available", n("available"), "ok"], ["Assigned", n("assigned")], ["Held", held, held ? "warn" : null], ["Unavailable or error", n("unavailable") + n("error"), n("unavailable") + n("error") ? "bad" : null]]),
    container({ title: "Macs", count: rows.length, flush: true },
      toolbar(
        searchBox({ label: "Search Macs", value: ctx.query.get("q") || "", id: "mac-search", placeholder: "Name, ID, model, organization", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        selectBox({ label: "State", value: f.state, id: "mac-state", options: [["", "All states"], ...STATES.map((s) => [s, humanize(s)])], onchange: (v) => { ctx.setQuery({ state: v }); ctx.refresh(); } }),
        selectBox({ label: "Region", value: f.region, id: "mac-region", options: [["", "All regions"], ...regions.map((s) => [s, s])], onchange: (v) => { ctx.setQuery({ region: v }); ctx.refresh(); } }),
        selectBox({ label: "Chip", value: f.chip, id: "mac-chip", options: [["", "All chips"], ...chips.map((s) => [s, s])], onchange: (v) => { ctx.setQuery({ chip: v }); ctx.refresh(); } })),
      table([
        { label: "Mac", cell: (x) => h("div", {}, h("a", { href: "macs/" + x.id, class: "strong" }, x.display_name), h("div", { class: "mono muted small" }, x.external_id)) },
        { label: "Specs", cell: spec },
        { label: "Region", cell: (x) => x.region },
        { label: "State", cell: (x) => h("div", {}, st(x.state), x.hold_reason ? h("div", { class: "hold", title: x.hold_reason }, icon("lock"), h("span", {}, "Held: " + x.hold_reason)) : null) },
        { label: "Provider state", cell: (x) => st(x.provider_state) },
        { label: "Rental", cell: (x) => x.rental_id ? h("div", {}, x.organization || "–", h("div", { class: "muted small" }, shortId(x.rental_id))) : h("span", { class: "muted" }, "–") },
        { label: "Synced", cell: (x) => when(x.last_synced_at, { rel: true }), class: "nowrap" },
        can("ops.inventory.manage") ? { label: "Actions", class: "actions", cell: (x) => actions(x, ctx.refresh) } : null,
      ].filter(Boolean), rows, { caption: "Macs", rowClass: (x) => x.hold_reason ? "held" : null,
        empty: { title: everything.length ? "No Mac matches the filters" : "No Macs yet", text: everything.length ? "Clear a filter to see more." : "The first reconciliation adds the provider's Macs." } })),
    caps ? container({ title: "Provider capabilities", desc: "What the active provider adapter reports." }, kv([
      ["Regions", caps.regions.join(", ") || "–"],
      ["Chips", caps.chips.join(", ") || "–"],
      ["Start and stop", caps.start_stop ? "Supported" : "Not supported"],
      ["Idempotency keys", caps.idempotency_keys ? "Supported" : "Not supported"],
    ], 4)) : null);
}

export async function mac(ctx) {
  const id = ctx.params[0];
  const r = await api("GET", "api/v1/ops/mac-resources");
  const x = r.resources.find((m) => m.id === id);
  if (!x) return h("div", {}, ctx.crumbs([["Taro Operations", "/"], ["Mac fleet", "/macs"], ["Not found"]]), alert("err", "No such Mac", "It may have been removed. Go back to the fleet."));
  let diag = null, diagErr = null;
  try { diag = (await api("GET", `api/v1/ops/mac-resources/${id}/diagnostics`)).diagnostics; } catch (e) { diagErr = e; }
  const back = async () => ctx.refresh();
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Mac fleet", "/macs"], [x.display_name]]),
    pageHeader({ step: "Mac", title: x.display_name, desc: h("span", {}, x.model, " · ", st(x.state)),
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }), actions(x, back)] }),
    x.hold_reason ? alert("warn", "Held from new rentals", x.hold_reason) : null,
    container({ title: "Details" }, kv([
      ["Taro ID", h("span", { class: "id-row" }, h("span", { class: "mono" }, x.id), copyButton(x.id, "Copy ID"))],
      ["Provider", x.provider],
      ["Provider's ID", h("span", { class: "mono" }, x.external_id)],
      ["Model", x.model],
      ["Chip", x.chip],
      ["CPU cores", num(x.cpu_cores)],
      ["Memory", x.memory_gb + " GB"],
      ["Storage", x.storage_gb + " GB"],
      ["Region", x.region],
      ["State (Taro)", st(x.state)],
      ["State (provider)", st(x.provider_state)],
      ["Last synced", when(x.last_synced_at)],
      ["Rental", x.rental_id ? h("span", { class: "mono" }, x.rental_id) : "–"],
      ["Organization", x.organization || "–"],
      ["Hold reason", x.hold_reason || "–"],
    ], 3)),
    container({ title: "Diagnostics", desc: "What the provider reports about this Mac, stripped of anything sensitive." },
      diagErr ? alert("warn", "Diagnostics are not available", diagErr.message)
        : diag && typeof diag === "object" && !Array.isArray(diag) ? kv(Object.entries(diag).map(([k, v]) => [humanize(k), typeof v === "object" && v !== null ? h("code", {}, JSON.stringify(v)) : String(v)]), 3)
          : h("pre", {}, JSON.stringify(diag, null, 2))));
}

export async function providers(ctx) {
  const r = await api("GET", "api/v1/ops/provider-integrations");
  const manage = can("ops.providers.manage");
  const add = async () => {
    const kind = field({ label: "Adapter", name: "kind", type: "select", options: r.adapters.map((a) => [a, humanize(a)]), value: r.adapters[0], desc: "Implemented adapters only." });
    const name = field({ label: "Name", name: "name", maxlength: 60, required: true, placeholder: "e.g. Provider A (EU)" });
    const secret = field({ label: "Secret reference", name: "secret_ref", opt: true, mono: true, placeholder: "vault:kv/taro/provider-a",
      desc: "Where the credentials live in the secret store (scheme:path). Never the secret itself." });
    const config = field({ label: "Configuration (JSON)", name: "config", type: "textarea", rows: 4, opt: true, placeholder: "{}", attrs: { class: "mono", spellcheck: "false" } });
    const ok = await formDialog({ title: "Add a provider", confirm: "Add provider", body: [kind, name, secret, config], submit: async () => {
      let cfg;
      const txt = config.control.value.trim();
      if (txt) { try { cfg = JSON.parse(txt); } catch (_) { config.setError("Not valid JSON."); throw new Error("The configuration is not valid JSON."); } }
      const body = { kind: kind.control.value, name: name.control.value.trim(), config: cfg, secret_ref: secret.control.value.trim() || undefined };
      await api("POST", "api/v1/ops/provider-integrations", body);
    } });
    if (ok) { flash("ok", "Provider added."); ctx.refresh(); }
  };
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Infrastructure"], ["Providers"]]),
    pageHeader({ title: "Providers", desc: "The provider integrations that supply Macs. The active one is the adapter this deployment runs with.",
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }), manage ? button("Add provider", { kind: "primary", icon: "plus", id: "add-provider", onclick: add }) : null] }),
    manage ? null : readOnlyNote("providers"),
    container({ title: "Provider integrations", count: r.providers.length, flush: true },
      table([
        { label: "Name", cell: (p) => h("div", {}, h("strong", {}, p.name), p.active ? h("span", { class: "badge green ml" }, "Active") : null) },
        { label: "Adapter", cell: (p) => p.kind },
        { label: "Enabled", cell: (p) => p.enabled ? st("active", "Enabled") : st("disabled") },
        { label: "Health", cell: (p) => st(p.health) },
        { label: "Health checked", cell: (p) => when(p.last_health_at, { rel: true }), class: "nowrap" },
        { label: "Inventory synced", cell: (p) => when(p.last_sync_at, { rel: true }), class: "nowrap" },
        { label: "Secret", cell: (p) => p.has_secret ? h("span", { class: "status ok" }, icon("lock"), "Referenced") : h("span", { class: "muted" }, "None") },
        { label: "Last error", cell: (p) => p.last_error ? h("span", { class: "err-text" }, p.last_error) : h("span", { class: "muted" }, "–") },
      ], r.providers, { caption: "Provider integrations", empty: { title: "No providers" } })),
    container({ title: "Configuration", desc: "Non-secret settings of each integration." },
      r.providers.length ? r.providers.map((p) => h("div", { class: "cfg" }, h("h3", {}, p.name), h("pre", {}, JSON.stringify(p.config, null, 2)))) : h("p", { class: "muted" }, "–")));
}

