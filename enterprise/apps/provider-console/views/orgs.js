// Organizations: the customers. A searchable list, and a detail page with members, rentals,
// billing state, entitlements (granted or revoked by platform admins), domains and audit.
import { h, icon, container, pageHeader, kv, button, confirmDialog, alert } from "../_console/console.js";
import { api, can, st, when, money, table, shortId, toolbar, searchBox, selectBox, act, humanize, stats, copyButton } from "../lib.js";

const MODULES = [
  ["my_macs", "My Macs", "Members see and connect to the Macs assigned to them"],
  ["rentals", "Rentals", "Rent Macs from the catalog"],
  ["billing", "Billing", "Invoices and payment methods"],
  ["team", "Team", "Members and invitations"],
];

export async function list(ctx) {
  const q = ctx.query.get("q") || "";
  const filter = ctx.query.get("status") || "";
  const r = await api("GET", "api/v1/ops/organizations" + (q ? "?q=" + encodeURIComponent(q) : ""));
  let rows = r.organizations;
  if (filter === "active") rows = rows.filter((o) => !o.suspended_at);
  if (filter === "suspended") rows = rows.filter((o) => o.suspended_at);
  const live = r.organizations.reduce((a, o) => a + o.live_rentals, 0);

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Organizations"]]),
    pageHeader({ title: "Organizations", desc: "Customer organizations, newest first. Open one for its members, rentals, billing and domains.",
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    stats([["Organizations", r.organizations.length], ["Live rentals", live], ["Suspended", r.organizations.filter((o) => o.suspended_at).length, r.organizations.some((o) => o.suspended_at) ? "warn" : null]]),
    container({ title: "Organizations", count: rows.length, flush: true,
      desc: r.organizations.length >= 200 ? "The first 200 matches are shown: search to narrow them." : null },
    toolbar(
      searchBox({ label: "Search by name or slug", value: q, id: "org-search", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
      selectBox({ label: "Status", value: filter, id: "org-status", options: [["", "All"], ["active", "Active"], ["suspended", "Suspended"]], onchange: (v) => { ctx.setQuery({ status: v }); ctx.refresh(); } })),
    table([
      { label: "Name", cell: (o) => h("a", { href: "organizations/" + o.id, class: "strong" }, o.name) },
      { label: "Slug", cell: (o) => h("span", { class: "mono" }, o.slug) },
      { label: "Status", cell: (o) => o.suspended_at ? st("suspended") : st("active", "Active") },
      { label: "Members", cell: (o) => o.members, class: "num" },
      { label: "Live rentals", cell: (o) => o.live_rentals, class: "num" },
      { label: "Created", cell: (o) => when(o.created_at), class: "nowrap" },
    ], rows, { caption: "Organizations", empty: { title: q ? "No organization matches" : "No organizations yet", text: q ? "Try another name or slug." : "Customers create organizations in the customer portal." } })));
}

export async function detail(ctx) {
  const id = ctx.params[0];
  const [d, domains, audit] = await Promise.all([
    api("GET", "api/v1/ops/organizations/" + id),
    can("ops.overview.read") ? api("GET", "api/v1/ops/domains").catch(() => null) : null,
    can("ops.audit.read") ? api("GET", "api/v1/ops/audit-events?limit=15&organization_id=" + id).catch(() => null) : null,
  ]);
  const o = d.organization;
  const rentals = d.rentals;
  const count = (f) => rentals.filter(f).length;
  const paid = {};
  for (const r of rentals) if (["paid", "past_due", "canceled"].includes(r.billing_state)) paid[r.currency] = (paid[r.currency] || 0) + r.amount_minor;
  const live = count((r) => ["ready", "suspended", "provisioning", "paid"].includes(r.status));
  const owners = d.members.filter((m) => m.role === "owner").map((m) => m.email);

  const ent = new Map(d.entitlements.map((e) => [e.feature, e.source]));
  const manage = can("ops.config.manage");
  const entRows = MODULES.map(([key, label, desc]) => ({ key, label, desc, source: ent.get(key) }));

  const myDomains = domains ? domains.domains.filter((x) => x.organization_id === id) : null;

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Organizations", "/organizations"], [o.name]]),
    pageHeader({ step: "Organization", title: o.name,
      desc: h("span", {}, h("span", { class: "mono" }, o.slug), " · created ", when(o.created_at, { dateOnly: true }), " · ", o.suspended_at ? st("suspended") : st("active", "Active")),
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    o.suspended_at ? alert("warn", "Suspended", "This organization was suspended on ", when(o.suspended_at), ".") : null,
    container({ title: "Details" }, kv([
      ["Organization ID", h("span", { class: "id-row" }, h("span", { class: "mono" }, o.id), copyButton(o.id, "Copy ID"))],
      ["Slug", h("span", { class: "mono" }, o.slug)],
      ["Status", o.suspended_at ? st("suspended") : st("active", "Active")],
      ["Created", when(o.created_at)],
      ["Members", String(d.members.length)],
      ["Owners", owners.length ? owners.join(", ") : "–"],
    ], 3)),
    h("div", { class: "cols-even" },
      container({ title: "Billing state", desc: "From the organization's last 100 rentals." }, kv([
        ["Live rentals", String(live)],
        ["Paid", String(count((r) => r.billing_state === "paid"))],
        ["Past due", count((r) => r.billing_state === "past_due") ? h("span", { class: "status warn" }, icon("warn"), String(count((r) => r.billing_state === "past_due"))) : "0"],
        ["Refunded", String(count((r) => r.billing_state === "refunded"))],
        ["Checkout open", String(count((r) => r.billing_state === "checkout_open"))],
        ["Billed in total", Object.keys(paid).length ? Object.entries(paid).map(([c, v]) => money(v, c)).join(" · ") : "–"],
      ], 3)),
      container({ title: "Entitlements", desc: "Modules the organization may use (and brand).", flush: true },
        table([
          { label: "Module", cell: (e) => h("div", {}, h("strong", {}, e.label), h("div", { class: "muted small" }, e.desc)) },
          { label: "Status", cell: (e) => e.source ? status2(true, humanize(e.source)) : status2(false) },
          manage ? { label: "", class: "actions", cell: (e) => e.source
            ? button("Revoke", { kind: "sm danger", onclick: async (ev) => {
              const b = ev.currentTarget;
              if (!await confirmDialog({ title: `Revoke ${e.label}?`, body: h("p", {}, `${o.name} loses the ${e.label} module now. Its members stop seeing it in the portal.`), confirm: "Revoke", danger: true })) return;
              await act(b, "Revoking…", () => api("DELETE", `api/v1/ops/organizations/${id}/entitlements/${e.key}`), { ok: `${e.label} revoked.`, after: ctx.refresh });
            } })
            : button("Grant", { kind: "sm", onclick: (ev) => act(ev.currentTarget, "Granting…", () => api("POST", `api/v1/ops/organizations/${id}/entitlements/${e.key}`), { ok: `${e.label} granted.`, after: ctx.refresh }) }) } : null,
        ].filter(Boolean), entRows, { caption: "Entitlements" }))),
    container({ title: "Members", count: d.members.length, flush: true },
      table([
        { label: "Email", cell: (m) => m.email },
        { label: "Role", cell: (m) => h("span", { class: "badge" + (m.role === "owner" ? " blue" : "") }, humanize(m.role)) },
      ], d.members, { caption: "Members", empty: { title: "No members" } })),
    container({ title: "Rentals", count: rentals.length, flush: true, desc: "Newest first (up to 100)." },
      table([
        { label: "Rental", cell: (r) => shortId(r.id) },
        { label: "Plan", cell: (r) => r.plan_name },
        { label: "Status", cell: (r) => h("div", {}, st(r.status), r.status_reason && r.status !== "ready" ? h("div", { class: "muted small clip" }, r.status_reason) : null) },
        { label: "Billing", cell: (r) => st(r.billing_state) },
        { label: "Amount", cell: (r) => h("span", {}, money(r.amount_minor, r.currency), h("span", { class: "muted" }, r.billing === "monthly" ? " / month" : r.duration_hours ? ` / ${r.duration_hours} h` : "")), class: "num nowrap" },
        { label: "Mac", cell: (r) => r.resource_name ? h("span", {}, r.resource_name, h("span", { class: "muted" }, " · " + (r.resource_region || ""))) : h("span", { class: "muted" }, "–") },
        { label: "Assigned to", cell: (r) => r.assigned_email || h("span", { class: "muted" }, "–") },
        { label: "Job", cell: (r) => r.job_state ? st(r.job_state) : h("span", { class: "muted" }, "–") },
        { label: "Created", cell: (r) => when(r.created_at), class: "nowrap" },
      ], rentals, { caption: "Rentals", empty: { title: "No rentals", text: "This organization has not rented a Mac yet." } })),
    myDomains ? container({ title: "Domains", count: myDomains.length, flush: true, actions: h("a", { class: "btn sm", href: "domains" }, "All domains") },
      table([
        { label: "Hostname", cell: (x) => h("span", { class: "mono" }, x.hostname) },
        { label: "Kind", cell: (x) => humanize(x.kind) },
        { label: "State", cell: (x) => st(x.state) },
        { label: "Last error", cell: (x) => x.last_error ? h("span", { class: "err-text" }, x.last_error) : h("span", { class: "muted" }, "–") },
        { label: "Added", cell: (x) => when(x.created_at, { dateOnly: true }), class: "nowrap" },
      ], myDomains, { caption: "Domains", empty: { title: "No custom domains" } })) : null,
    audit ? container({ title: "Recent activity", flush: true, actions: h("a", { class: "btn sm", href: "audit?organization_id=" + id }, "Full audit log") },
      table([
        { label: "Time", cell: (e) => when(e.at), class: "nowrap" },
        { label: "Actor", cell: (e) => e.actor_email || humanize(e.actor_kind) },
        { label: "Action", cell: (e) => h("span", { class: "mono" }, e.action) },
        { label: "Result", cell: (e) => st(e.result === "success" ? "succeeded" : e.result === "denied" ? "error" : "failed", humanize(e.result)) },
      ], audit.events, { caption: "Recent activity", empty: { title: "No activity yet" } })) : null);
}

function status2(on, text) {
  return on ? h("span", { class: "status ok" }, icon("ok"), text ? "Granted · " + text.toLowerCase() : "Granted") : h("span", { class: "status stopped" }, icon("stop"), "Not granted");
}
