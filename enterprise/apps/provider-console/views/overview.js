// Overview: the platform's key figures (from /api/v1/ops/overview, each with its definition,
// window and source), the fleet's capacity, provider health, open incidents and failed jobs.
import { h, icon, container, pageHeader, button, alert } from "../_console/console.js";
import { S, api, can, st, sevBadge, when, table, num, shortId, stateLabel } from "../lib.js";

function kpi({ label, value, sub, metric, href, perm, tone }) {
  const tip = metric ? `${metric.definition} · ${metric.window} · source: ${metric.source}` : null;
  const body = [
    h("span", { class: "k-label" }, label),
    h("span", { class: "k-value" }, value),
    sub ? h("span", { class: "k-sub" }, sub) : null,
    metric ? h("span", { class: "k-def" }, metric.definition) : null,
  ];
  const cls = "kpi" + (tone ? " " + tone : "");
  if (href && (!perm || can(perm))) return h("a", { class: cls + " link", href, title: tip }, body, h("span", { class: "k-go", "aria-hidden": "true" }, icon("angle")));
  return h("div", { class: cls, title: tip }, body);
}

export async function page(ctx) {
  const optional = (perm, path) => can(perm) ? api("GET", path).catch(() => null) : Promise.resolve(null);
  const [o, res, failed, inc] = await Promise.all([
    api("GET", "api/v1/ops/overview"),
    optional("ops.inventory.read", "api/v1/ops/mac-resources"),
    optional("ops.jobs.read", "api/v1/ops/provisioning-jobs?state=permanent_failed&limit=50"),
    optional("ops.incidents.read", "api/v1/ops/incidents"),
  ]);
  const sevs = o.open_incidents.value || [];
  const openInc = sevs.reduce((a, s) => a + s.count, 0);
  const worst = sevs.map((s) => s.severity).sort()[0];
  const rate = o.provisioning_failure_rate.value;
  const relay = o.relay_health.value;
  const resources = res ? res.resources : null;
  const failedJobs = failed ? failed.jobs : null;

  const kpis = h("div", { class: "kpis", id: "kpis" },
    kpi({ label: "Active rentals", value: num(o.active_rentals.value), metric: o.active_rentals, href: "organizations", perm: "ops.orgs.read" }),
    kpi({ label: "Macs available", value: num(o.available_resources.value), sub: resources ? `of ${resources.length} in the fleet` : null, metric: o.available_resources, href: "macs", perm: "ops.inventory.read",
      tone: o.available_resources.value === 0 ? "warn" : null }),
    kpi({ label: "Jobs in progress", value: num(o.provisioning_jobs_running.value), metric: o.provisioning_jobs_running, href: "provisioning-jobs", perm: "ops.jobs.read" }),
    kpi({ label: "Provisioning failure rate", value: rate == null ? "–" : (rate * 100).toFixed(rate > 0 && rate < 0.1 ? 1 : 0) + "%", sub: rate == null ? "No job finished in 24 hours" : (failedJobs ? `${failedJobs.length} failed job${failedJobs.length === 1 ? "" : "s"} listed` : null),
      metric: o.provisioning_failure_rate, href: "provisioning-jobs?state=permanent_failed", perm: "ops.jobs.read", tone: rate > 0.05 ? "bad" : null }),
    kpi({ label: "Open incidents", value: num(openInc), sub: sevs.length ? h("span", { class: "sev-list" }, sevs.map((s) => h("span", {}, sevBadge(s.severity), " ", s.count))) : "None open",
      metric: o.open_incidents, href: "incidents", perm: "ops.incidents.read", tone: worst === "sev1" || worst === "sev2" ? "bad" : openInc ? "warn" : null }),
    kpi({ label: "Domains failing TLS", value: num(o.tls_failures.value), metric: o.tls_failures, href: "domains", perm: "ops.overview.read", tone: o.tls_failures.value ? "bad" : null }),
    kpi({ label: "Relays reachable", value: relay.nodes ? `${relay.reachable} / ${relay.nodes}` : "–", sub: relay.nodes ? (relay.checked_at ? "Checked " + new Date(relay.checked_at).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" }) : "Not checked yet") : "No relay nodes",
      metric: o.relay_health, href: "relay-nodes", perm: "ops.relays.read", tone: relay.nodes && relay.reachable < relay.nodes ? "bad" : null }),
  );

  // fleet capacity by state, from the inventory
  let capacity = null;
  if (resources) {
    const by = (k) => resources.reduce((m, r) => (m[r[k]] = (m[r[k]] || 0) + 1, m), {});
    const states = by("state"), regions = by("region"), chips = by("chip");
    const total = resources.length || 1;
    const order = ["available", "assigned", "reserved", "maintenance", "unavailable", "releasing", "error", "discovered", "released"];
    const keys = Object.keys(states).sort((a, b) => order.indexOf(a) - order.indexOf(b));
    const bar = h("div", { class: "capbar", role: "img", "aria-label": keys.map((k) => `${states[k]} ${stateLabel(k)}`).join(", ") },
      keys.map((k) => { const s = h("span", { class: "seg s-" + k, title: `${stateLabel(k)}: ${states[k]}` }); s.style.flexGrow = String(states[k] / total); return s; }));
    capacity = container({ title: "Fleet capacity", count: resources.length, desc: "Macs by state, region and chip (Taro's view, reconciled with the provider every minute).",
      actions: can("ops.inventory.read") ? h("a", { class: "btn sm", href: "macs" }, "Open the fleet") : null },
      resources.length ? [bar,
        h("ul", { class: "legend" }, keys.map((k) => h("li", {}, h("span", { class: "sw s-" + k, "aria-hidden": "true" }), stateLabel(k), h("strong", {}, states[k])))),
        h("div", { class: "cap-cols" },
          h("div", {}, h("h3", {}, "By region"), h("ul", { class: "plain" }, Object.entries(regions).map(([k, v]) => h("li", {}, h("span", {}, k), h("strong", {}, v))))),
          h("div", {}, h("h3", {}, "By chip"), h("ul", { class: "plain" }, Object.entries(chips).map(([k, v]) => h("li", {}, h("span", {}, k), h("strong", {}, v))))))]
        : h("div", { class: "table-empty" }, h("strong", {}, "No Macs yet"), "The first reconciliation adds the provider's Macs."));
  }

  const providers = container({ title: "Provider health", desc: o.provider_health.definition + ".", flush: true,
    actions: can("ops.providers.read") ? h("a", { class: "btn sm", href: "providers" }, "Providers") : null },
  table([
    { label: "Provider", cell: (r) => h("strong", {}, r.name) },
    { label: "Adapter", cell: (r) => r.kind },
    { label: "Health", cell: (r) => st(r.health) },
    { label: "Checked", cell: (r) => when(r.checked_at, { rel: true }) },
  ], o.provider_health.value, { caption: "Provider health", empty: { title: "No provider", text: "No provider integration is configured." } }));

  const incidents = inc ? container({ title: "Open incidents", count: inc.incidents.filter((i) => i.status !== "resolved").length, flush: true,
    actions: h("a", { class: "btn sm", href: "incidents" }, "All incidents") },
  table([
    { label: "Severity", cell: (r) => sevBadge(r.severity), class: "st" },
    { label: "Incident", cell: (r) => h("a", { href: "incidents/" + r.id }, r.title) },
    { label: "Status", cell: (r) => st(r.status) },
    { label: "Last seen", cell: (r) => when(r.last_seen, { rel: true }), class: "nowrap" },
  ], inc.incidents.filter((i) => i.status !== "resolved").slice(0, 6), { caption: "Open incidents", empty: { title: "No open incidents", text: "Everything is quiet." } })) : null;

  const jobsBox = failedJobs ? container({ title: "Failed provisioning jobs", count: failedJobs.length, flush: true,
    actions: h("a", { class: "btn sm", href: "provisioning-jobs?state=permanent_failed" }, "All failed jobs") },
  table([
    { label: "Job", cell: (r) => shortId(r.id) },
    { label: "Operation", cell: (r) => r.operation },
    { label: "Organization", cell: (r) => r.organization },
    { label: "Error", cell: (r) => h("span", { class: "clip" }, r.last_error || "–") },
    { label: "Completed", cell: (r) => when(r.completed_at, { rel: true }), class: "nowrap" },
  ], failedJobs.slice(0, 5), { caption: "Failed provisioning jobs", empty: { title: "No failed jobs", text: "No provisioning job has failed." } })) : null;

  const simulated = [o.simulated_provider ? h("span", { class: "badge amber", id: "badge-sim-provider" }, "Simulated provider") : null];

  return h("div", {},
    ctx.crumbs([["Taro Operations"], ["Overview"]]),
    pageHeader({ title: "Overview", desc: h("span", { class: "hdr-desc" }, "The platform at a glance. Every figure links to its area; hover a figure for its definition and source. ", simulated),
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    o.simulated_provider ? h("p", { class: "sim-note" }, icon("info"), "Macs come from the mock provider: figures are demo data.") : null,
    S.setup && S.setup.mail === false ? h("div", { id: "mail-note" }, alert("info", "No mail server yet",
      "Customers sign in to the portal with a code sent by email, so the portal needs a mail server: ", h("code", {}, "smtp_url"), " in the relay's portal settings. Administrators sign in with their password.")) : null,
    kpis,
    h("div", { class: "cols-even" }, capacity, providers),
    h("div", { class: "cols-even" }, incidents, jobsBox),
    h("p", { class: "as-of muted" }, "Figures as of ", when(o.active_rentals.as_of), "."));
}

