// Incidents: what operators must look at, opened by the platform (a failed paid rental, a
// provider or relay down, drift between Taro and the provider). Operators take ownership,
// write the mitigation, and resolve with what fixed it.
import { h, container, pageHeader, kv, button, alert } from "/_console/console.js";
import { S, api, can, st, when, table, toolbar, searchBox, selectBox, act, field, readOnlyNote, stats, sevBadge, humanize, checkbox, shortId } from "/lib.js";

const SEV = { sev1: "Critical", sev2: "Major", sev3: "Minor", sev4: "Low" };

export async function list(ctx) {
  const status = ctx.query.has("status") ? ctx.query.get("status") : "unresolved";
  const sev = ctx.query.get("sev") || "";
  const q = (ctx.query.get("q") || "").toLowerCase();
  const r = await api("GET", "/api/v1/ops/incidents" + (status && status !== "unresolved" ? "?state=" + status : ""));
  let rows = r.incidents;
  if (status === "unresolved") rows = rows.filter((i) => i.status !== "resolved");
  if (sev) rows = rows.filter((i) => i.severity === sev);
  if (q) rows = rows.filter((i) => [i.title, i.component, i.impact, i.owner_email || ""].join(" ").toLowerCase().includes(q));
  const open = r.incidents.filter((i) => i.status === "open").length;
  const mitigated = r.incidents.filter((i) => i.status === "mitigated").length;

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Operations"], ["Incidents"]]),
    pageHeader({ title: "Incidents", desc: "Opened by the platform when something needs an operator. A repeat of the same problem bumps the open incident instead of opening a new one.",
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    can("ops.incidents.manage") ? null : readOnlyNote("incidents"),
    status === "unresolved" || status === "" ? stats([["Open", open, open ? "bad" : "ok"], ["Mitigated", mitigated, mitigated ? "warn" : null],
      ...["sev1", "sev2", "sev3", "sev4"].map((s) => [SEV[s] + " (" + s.toUpperCase() + ")", r.incidents.filter((i) => i.severity === s && i.status !== "resolved").length])]) : null,
    container({ title: "Incidents", count: rows.length, flush: true },
      toolbar(
        searchBox({ label: "Search incidents", value: ctx.query.get("q") || "", id: "inc-search", placeholder: "Title, component, owner", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        selectBox({ label: "Status", value: status, id: "inc-status", options: [["unresolved", "Open or mitigated"], ["open", "Open"], ["mitigated", "Mitigated"], ["resolved", "Resolved"], ["", "All"]],
          onchange: (v) => { ctx.setQuery({ status: v === "unresolved" ? null : v || "" }); if (v === "") { const u = new URL(location.href); u.searchParams.set("status", ""); history.replaceState(null, "", u.pathname + u.search); } ctx.refresh(); } }),
        selectBox({ label: "Severity", value: sev, id: "inc-sev", options: [["", "All"], ...Object.entries(SEV).map(([k, v]) => [k, `${k.toUpperCase()} · ${v}`])], onchange: (v) => { ctx.setQuery({ sev: v }); ctx.refresh(); } })),
      table([
        { label: "Severity", cell: (i) => sevBadge(i.severity), class: "st" },
        { label: "Incident", cell: (i) => h("div", {}, h("a", { href: "/incidents/" + i.id, class: "strong" }, i.title), i.impact ? h("div", { class: "muted small" }, i.impact) : null) },
        { label: "Component", cell: (i) => humanize(i.component) },
        { label: "Status", cell: (i) => st(i.status) },
        { label: "Seen", cell: (i) => `${i.occurrences}×`, class: "num" },
        { label: "First seen", cell: (i) => when(i.first_seen), class: "nowrap" },
        { label: "Last seen", cell: (i) => when(i.last_seen, { rel: true }), class: "nowrap" },
        { label: "Owner", cell: (i) => i.owner_email || h("span", { class: "muted" }, "Nobody") },
      ], rows, { caption: "Incidents", rowClass: (i) => i.status === "resolved" ? "retired" : null,
        empty: { title: "No incidents", text: status === "unresolved" ? "Nothing is open. Everything is quiet." : "No incident matches the filters." } })));
}

function linked(l) {
  if (!l || typeof l !== "object") return null;
  const out = [];
  for (const [k, v] of Object.entries(l)) {
    if (v == null) continue;
    let val = h("span", { class: "mono" }, String(v));
    if (k === "organization_id" && can("ops.orgs.read")) val = h("a", { href: "/organizations/" + v, class: "mono" }, String(v));
    if (k === "resource_id" && can("ops.inventory.read")) val = h("a", { href: "/macs/" + v, class: "mono" }, String(v));
    if (k === "job_id" && can("ops.jobs.read")) val = h("a", { href: "/provisioning-jobs?q=" + v, class: "mono" }, String(v));
    if (k === "relay_id" && can("ops.relays.read")) val = h("a", { href: "/relay-nodes", class: "mono" }, String(v));
    if (k === "provider_id" && can("ops.providers.read")) val = h("a", { href: "/providers", class: "mono" }, String(v));
    out.push([humanize(k.replace(/_id$/, "")), val]);
  }
  return out;
}

export async function detail(ctx) {
  const id = ctx.params[0];
  const r = await api("GET", "/api/v1/ops/incidents?limit=200");
  let i = r.incidents.find((x) => x.id === id);
  if (!i) i = (await api("GET", "/api/v1/ops/incidents?state=resolved")).incidents.find((x) => x.id === id);
  if (!i) return h("div", {}, ctx.crumbs([["Taro Operations", "/"], ["Incidents", "/incidents"], ["Not found"]]), alert("err", "No such incident", "It may be older than the 200 incidents listed."));
  const manage = can("ops.incidents.manage");
  const mine = S.me && i.owner_email === S.me.email;
  const links = linked(i.linked);

  let form = null;
  if (manage) {
    const fStatus = field({ label: "Status", name: "status", type: "select", value: i.status, options: [["open", "Open"], ["mitigated", "Mitigated"], ["resolved", "Resolved"]] });
    const fMit = field({ label: "Mitigation", name: "mitigation", type: "textarea", rows: 3, value: i.mitigation, desc: "What has been done to limit the impact." });
    const fRes = field({ label: "Resolution", name: "resolution", type: "textarea", rows: 3, value: i.resolution, desc: "What fixed it. Required to resolve." });
    const own = mine ? null : checkbox({ label: "Take ownership", desc: i.owner_email ? `Now owned by ${i.owner_email}.` : "Nobody owns it yet.", checked: !i.owner_email });
    const save = button("Save changes", { kind: "primary", type: "submit", id: "incident-save" });
    form = h("form", { novalidate: true, id: "incident-form", onsubmit: async (e) => {
      e.preventDefault();
      fRes.setError(null);
      const status = fStatus.control.value;
      if (status === "resolved" && !fRes.control.value.trim()) { fRes.setError("Write what resolved it."); fRes.control.focus(); return; }
      const body = { status, mitigation: fMit.control.value, resolution: fRes.control.value };
      if (own && own.control.checked) body.take_ownership = true;
      await act(save, "Saving…", () => api("PATCH", "/api/v1/ops/incidents/" + i.id, body), { ok: status === "resolved" ? "Incident resolved." : "Incident updated.", after: ctx.refresh });
    } }, h("div", { class: "form-grid" }, fStatus), fMit, fRes, own, h("div", { class: "form-actions" }, save));
  }

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Incidents", "/incidents"], [shortId(i.id)]]),
    pageHeader({ step: "Incident", title: i.title, desc: h("span", { class: "hdr-desc" }, sevBadge(i.severity), " ", SEV[i.severity] || "", " · ", st(i.status), " · ", humanize(i.component)),
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    i.status === "resolved" ? alert("ok", "Resolved", "Resolved ", when(i.resolved_at), i.resolution ? ": " + i.resolution : ".") : null,
    h("div", { class: "cols" },
      h("div", {},
        container({ title: "Details" }, kv([
          ["Severity", h("span", {}, sevBadge(i.severity), " ", SEV[i.severity] || "")],
          ["Status", st(i.status)],
          ["Component", humanize(i.component)],
          ["First seen", when(i.first_seen)],
          ["Last seen", when(i.last_seen)],
          ["Occurrences", String(i.occurrences)],
          ["Owner", i.owner_email || "Nobody"],
          ["Resolved", when(i.resolved_at)],
        ], 2), i.impact ? [h("h3", {}, "Impact"), h("p", {}, i.impact)] : null),
        links && links.length ? container({ title: "Linked resources" }, kv(links, 2)) : null,
        !manage ? container({ title: "Mitigation and resolution" }, kv([["Mitigation", i.mitigation || "–"], ["Resolution", i.resolution || "–"]], 1)) : null),
      manage ? container({ title: "Update", desc: "Changes are written to the audit log." }, form) : h("div", {}, readOnlyNote("this incident"))));
}


