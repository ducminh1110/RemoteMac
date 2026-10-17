// Domains: every custom domain of every organization, with its verification and HTTPS state.
// Customers add and verify their domains in the portal; operators see which ones fail.
import { h, container, pageHeader, button, alert } from "/_console/console.js";
import { api, st, when, table, toolbar, searchBox, selectBox, humanize, stats, orgNames, orgLink } from "/lib.js";

const STATES = ["pending_verification", "verified", "provisioning_tls", "active", "error", "disabled"];

export async function page(ctx) {
  const state = ctx.query.get("state") || "";
  const q = (ctx.query.get("q") || "").toLowerCase();
  const [r, names] = await Promise.all([api("GET", "/api/v1/ops/domains"), orgNames()]);
  let rows = r.domains;
  if (state) rows = rows.filter((d) => d.state === state);
  if (q) rows = rows.filter((d) => (d.hostname + " " + (names.get(d.organization_id) || "") + " " + (d.last_error || "")).toLowerCase().includes(q));
  const n = (s) => r.domains.filter((d) => d.state === s).length;
  const soon = r.domains.filter((d) => d.tls_expires_at && new Date(d.tls_expires_at) - Date.now() < 14 * 86400e3).length;

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Customers"], ["Domains"]]),
    pageHeader({ title: "Domains", desc: "Custom domains customers use for their portal. HTTPS is checked every 5 minutes; verification tokens are the customers' and are not shown.",
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    stats([["Domains", r.domains.length], ["Active", n("active"), "ok"], ["Pending", n("pending_verification") + n("verified") + n("provisioning_tls")], ["Failing", n("error"), n("error") ? "bad" : null], ["TLS expiring in 14 days", soon, soon ? "warn" : null]]),
    n("error") ? alert("err", `${n("error")} domain${n("error") === 1 ? " is" : "s are"} failing`, "Their HTTPS check fails: the customer's DNS may have changed. The last error says why.") : null,
    container({ title: "Domains", count: rows.length, flush: true },
      toolbar(
        searchBox({ label: "Search domains", value: ctx.query.get("q") || "", id: "domain-search", placeholder: "Hostname, organization or error", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        selectBox({ label: "State", value: state, id: "domain-state", options: [["", "All states"], ...STATES.map((s) => [s, humanize(s)])], onchange: (v) => { ctx.setQuery({ state: v }); ctx.refresh(); } })),
      table([
        { label: "Hostname", cell: (d) => h("span", { class: "mono strong" }, d.hostname) },
        { label: "Organization", cell: (d) => orgLink(d.organization_id, names) },
        { label: "Kind", cell: (d) => humanize(d.kind) },
        { label: "State", cell: (d) => st(d.state) },
        { label: "Verified", cell: (d) => when(d.verified_at, { dateOnly: true }), class: "nowrap" },
        { label: "TLS expires", cell: (d) => d.tls_expires_at ? when(d.tls_expires_at, { dateOnly: true }) : h("span", { class: "muted" }, "–"), class: "nowrap" },
        { label: "Last check", cell: (d) => when(d.last_check_at, { rel: true }), class: "nowrap" },
        { label: "Last error", cell: (d) => d.last_error ? h("span", { class: "err-text" }, d.last_error) : h("span", { class: "muted" }, "–") },
        { label: "Added", cell: (d) => when(d.created_at, { dateOnly: true }), class: "nowrap" },
      ], rows, { caption: "Domains", rowClass: (d) => d.state === "error" ? "fail" : null,
        empty: { title: state || q ? "No domain matches the filters" : "No custom domains yet", text: state || q ? "Clear a filter to see more." : "Customers add their domains in the portal." } })));
}
