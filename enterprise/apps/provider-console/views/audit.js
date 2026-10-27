// The audit log: who did what to which resource, when, and how it ended. Newest first, paged by
// id; filtered by organization on the server and by action, actor and result on the page.
import { h, container, pageHeader, button } from "../_console/console.js";
import { api, can, st, when, table, toolbar, searchBox, selectBox, humanize, orgNames, orgLink } from "../lib.js";

function resultState(r) {
  return r === "success" ? st("succeeded", "Success") : r === "denied" ? st("error", "Denied") : st("failed", "Failure");
}

function details(d) {
  if (!d || typeof d !== "object" || !Object.keys(d).length) return h("span", { class: "muted" }, "–");
  const txt = JSON.stringify(d);
  if (txt.length <= 60) return h("code", { class: "small" }, txt);
  return h("details", { class: "audit-details" }, h("summary", {}, "Details"), h("pre", {}, JSON.stringify(d, null, 2)));
}

export async function page(ctx) {
  const org = ctx.query.get("organization_id") || "";
  const before = ctx.query.get("before") || "";
  const limit = ctx.query.get("limit") || "100";
  const q = (ctx.query.get("q") || "").toLowerCase();
  const result = ctx.query.get("result") || "";
  const actor = ctx.query.get("actor") || "";
  const qs = new URLSearchParams({ limit });
  if (org) qs.set("organization_id", org);
  if (before) qs.set("before", before);
  const [r, names] = await Promise.all([api("GET", "api/v1/ops/audit-events?" + qs), orgNames()]);
  let rows = r.events;
  if (q) rows = rows.filter((e) => [e.action, e.resource_type, e.resource_id || "", e.actor_email || "", e.request_id || "", JSON.stringify(e.details)].join(" ").toLowerCase().includes(q));
  if (result) rows = rows.filter((e) => e.result === result);
  if (actor) rows = rows.filter((e) => e.actor_kind === actor);
  const last = r.events.length ? r.events[r.events.length - 1].id : null;
  const full = r.events.length >= Number(limit);
  const orgOptions = [["", "All organizations"], ...[...names.entries()].sort((a, b) => a[1].localeCompare(b[1])).map(([id, n]) => [id, n])];
  if (org && !names.has(org)) orgOptions.push([org, org.slice(0, 8)]);

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Security"], ["Audit log"]]),
    pageHeader({ title: "Audit log", desc: "Every change and sign-in, and every refused request. Records cannot be changed or deleted.",
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    container({ title: "Events", count: rows.length, flush: true,
      desc: before ? `Older than event #${before}.` : "Newest first.",
      foot: h("div", { class: "pager" },
        h("span", { class: "muted" }, r.events.length ? `Events #${r.events[0].id} to #${last}` : "No events"),
        h("span", { class: "spacer" }),
        before ? button("Newest", { kind: "sm link", id: "audit-newest", onclick: () => { ctx.setQuery({ before: null }); ctx.refresh(); } }) : null,
        button("Older", { kind: "sm", id: "audit-older", disabled: !full || !last, onclick: () => { ctx.setQuery({ before: String(last) }); ctx.refresh(); } })) },
      toolbar(
        searchBox({ label: "Filter this page", value: ctx.query.get("q") || "", id: "audit-search", placeholder: "Action, resource, actor, request ID", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        can("ops.orgs.read") ? selectBox({ label: "Organization", value: org, id: "audit-org", options: orgOptions, onchange: (v) => { ctx.setQuery({ organization_id: v, before: null }); ctx.refresh(); } }) : null,
        selectBox({ label: "Actor", value: actor, id: "audit-actor", options: [["", "Anyone"], ["operator", "Operators"], ["user", "Customers"], ["system", "System"], ["stripe", "Stripe"], ["provider", "Provider"]], onchange: (v) => { ctx.setQuery({ actor: v }); ctx.refresh(); } }),
        selectBox({ label: "Result", value: result, id: "audit-result", options: [["", "Any result"], ["success", "Success"], ["denied", "Denied"], ["failure", "Failure"]], onchange: (v) => { ctx.setQuery({ result: v }); ctx.refresh(); } }),
        selectBox({ label: "Page size", value: limit, id: "audit-limit", options: [["50", "50"], ["100", "100"], ["200", "200"]], onchange: (v) => { ctx.setQuery({ limit: v === "100" ? null : v, before: null }); ctx.refresh(); } })),
      table([
        { label: "#", cell: (e) => h("span", { class: "mono muted" }, String(e.id)), class: "num" },
        { label: "Time", cell: (e) => when(e.at), class: "nowrap" },
        { label: "Actor", cell: (e) => h("div", {}, e.actor_email || humanize(e.actor_kind), e.actor_email ? h("div", { class: "muted small" }, humanize(e.actor_kind)) : null) },
        { label: "Action", cell: (e) => h("span", { class: "mono" }, e.action) },
        { label: "Resource", cell: (e) => h("div", {}, humanize(e.resource_type), e.resource_id ? h("div", { class: "mono muted small clip", title: e.resource_id }, e.resource_id) : null) },
        { label: "Result", cell: (e) => resultState(e.result) },
        { label: "Organization", cell: (e) => e.organization_id ? orgLink(e.organization_id, names) : h("span", { class: "muted" }, "–") },
        { label: "Details", cell: (e) => details(e.details) },
        { label: "Request", cell: (e) => e.request_id ? h("span", { class: "mono muted small", title: e.request_id }, e.request_id.slice(0, 8)) : h("span", { class: "muted" }, "–") },
      ], rows, { caption: "Audit events", rowClass: (e) => e.result === "denied" ? "warn-row" : e.result === "failure" ? "fail" : null,
        empty: { title: "No events", text: q || result || actor ? "Nothing on this page matches the filters." : "Nothing has been recorded yet." } })));
}
