// Provisioning jobs (provision and release, worked by the provisioning worker), retry and
// cancel, and reconciliation on demand with what it found (its incidents).
import { h, container, pageHeader, button, confirmDialog } from "../_console/console.js";
import { api, can, st, when, table, shortId, toolbar, searchBox, selectBox, act, readOnlyNote, stats, sevBadge } from "../lib.js";

const STATES = ["queued", "running", "waiting_provider", "retryable_failed", "permanent_failed", "succeeded", "cancelled"];
const LABELS = { queued: "Queued", running: "Running", waiting_provider: "Waiting on provider", retryable_failed: "Waiting to retry", permanent_failed: "Failed", succeeded: "Succeeded", cancelled: "Cancelled" };

let lastReconcile = null;

export async function page(ctx) {
  const state = ctx.query.get("state") || "";
  const limit = ctx.query.get("limit") || "100";
  const q = (ctx.query.get("q") || "").toLowerCase();
  const manage = can("ops.jobs.manage");
  const [r, inc] = await Promise.all([
    api("GET", `api/v1/ops/provisioning-jobs?limit=${encodeURIComponent(limit)}${state ? "&state=" + encodeURIComponent(state) : ""}`),
    can("ops.incidents.read") ? api("GET", "api/v1/ops/incidents?state=open").catch(() => null) : null,
  ]);
  let rows = r.jobs;
  if (q) rows = rows.filter((j) => [j.id, j.rental_id, j.organization, j.last_error || "", j.correlation_id].join(" ").toLowerCase().includes(q));
  const count = (s) => r.jobs.filter((j) => j.state === s).length;
  const drift = inc ? inc.incidents.filter((i) => i.component === "reconciliation") : null;

  const retry = async (btn, j) => {
    if (!await confirmDialog({ title: "Retry this job?", confirm: "Retry job",
      body: [h("p", {}, `The ${j.operation} job for ${j.organization} is queued again with its attempts reset.`), j.last_error ? h("p", { class: "muted" }, "Last error: " + j.last_error) : null] })) return;
    await act(btn, "Retrying…", () => api("POST", `api/v1/ops/provisioning-jobs/${j.id}/retry`), { ok: "The job is queued again.", after: ctx.refresh });
  };
  const cancel = async (btn, j) => {
    if (!await confirmDialog({ title: "Cancel this job?", confirm: "Cancel job", danger: true,
      body: h("p", {}, `The ${j.operation} job for ${j.organization} will not run. Its rental stays as it is; resolving it is then up to an operator.`) })) return;
    await act(btn, "Cancelling…", () => api("POST", `api/v1/ops/provisioning-jobs/${j.id}/cancel`), { ok: "The job is cancelled.", after: ctx.refresh });
  };
  const reconcile = async (e) => {
    await act(e.currentTarget, "Reconciling…", () => api("POST", "api/v1/ops/reconcile"), {
      ok: (x) => "Reconciled at " + new Date(x.reconciled_at).toLocaleTimeString("en-GB"),
      after: (x) => { lastReconcile = x.reconciled_at; return ctx.refresh(); },
    });
  };

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Infrastructure"], ["Provisioning jobs"]]),
    pageHeader({ title: "Provisioning jobs", desc: "Provision and release steps run by the worker, newest first. Failed jobs can be retried; queued ones cancelled.",
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }), manage ? button("Reconcile now", { kind: "primary", icon: "sync", id: "reconcile", onclick: reconcile }) : null] }),
    manage ? null : readOnlyNote("provisioning jobs"),
    stats([["Listed", r.jobs.length], ["In progress", count("queued") + count("running") + count("waiting_provider")], ["Waiting to retry", count("retryable_failed"), count("retryable_failed") ? "warn" : null],
      ["Failed", count("permanent_failed"), count("permanent_failed") ? "bad" : null], ["Succeeded", count("succeeded"), "ok"]]),
    drift ? container({ id: "drift", title: "Reconciliation findings", count: drift.length,
      desc: (lastReconcile ? "Last run from here: " + new Date(lastReconcile).toLocaleString("en-GB") + ". " : "") + "Open incidents where Taro and the provider disagree about a Mac. Reconciliation runs every minute; nothing is released on a guess.", flush: true },
    table([
      { label: "Severity", cell: (i) => sevBadge(i.severity), class: "st" },
      { label: "Finding", cell: (i) => h("a", { href: "incidents/" + i.id }, i.title) },
      { label: "Seen", cell: (i) => `${i.occurrences}×`, class: "num" },
      { label: "Last seen", cell: (i) => when(i.last_seen, { rel: true }), class: "nowrap" },
    ], drift, { caption: "Reconciliation findings", empty: { title: "No drift", text: "Taro and the provider agree about every Mac." } })) : null,
    container({ title: "Jobs", count: rows.length, flush: true },
      toolbar(
        searchBox({ label: "Search jobs", value: ctx.query.get("q") || "", id: "job-search", placeholder: "Organization, job, rental or error", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        selectBox({ label: "State", value: state, id: "job-state", options: [["", "All states"], ...STATES.map((s) => [s, LABELS[s]])], onchange: (v) => { ctx.setQuery({ state: v }); ctx.refresh(); } }),
        selectBox({ label: "Show", value: limit, id: "job-limit", options: [["100", "100"], ["250", "250"], ["500", "500"]], onchange: (v) => { ctx.setQuery({ limit: v === "100" ? "" : v }); ctx.refresh(); } })),
      table([
        { label: "Job", cell: (j) => h("div", {}, shortId(j.id), h("div", { class: "muted small" }, j.operation === "provision" ? "Provision" : "Release")) },
        { label: "State", cell: (j) => st(j.state, LABELS[j.state]) },
        { label: "Organization", cell: (j) => j.organization },
        { label: "Rental", cell: (j) => h("div", {}, shortId(j.rental_id), h("div", { class: "small" }, st(j.rental_status))) },
        { label: "Attempts", cell: (j) => `${j.attempts} / ${j.max_attempts}`, class: "num nowrap" },
        { label: "Last error", cell: (j) => j.last_error ? h("span", { class: "err-text clip", title: j.last_error }, j.last_error) : h("span", { class: "muted" }, "–") },
        { label: "Created", cell: (j) => when(j.created_at), class: "nowrap" },
        { label: "Next run / done", cell: (j) => j.completed_at ? when(j.completed_at, { rel: true }) : ["queued", "retryable_failed", "waiting_provider"].includes(j.state) ? h("span", {}, "Runs ", when(j.run_after, { rel: true })) : h("span", { class: "muted" }, "–"), class: "nowrap" },
        manage ? { label: "Actions", class: "actions", cell: (j) => [
          j.retry_eligible ? button("Retry", { kind: "sm", onclick: (e) => retry(e.currentTarget, j) }) : null,
          ["queued", "retryable_failed"].includes(j.state) ? button("Cancel", { kind: "sm danger", onclick: (e) => cancel(e.currentTarget, j) }) : null,
          !j.retry_eligible && j.state === "permanent_failed" ? h("span", { class: "muted small", title: "A provision job whose rental has ended is not retried: a new rental is needed." }, "Rental ended") : null,
        ] } : null,
      ].filter(Boolean), rows, { caption: "Provisioning jobs", rowClass: (j) => j.state === "permanent_failed" ? "fail" : null,
        empty: { title: state || q ? "No job matches the filters" : "No jobs yet", text: state || q ? "Clear a filter to see more." : "A job is queued when a rental is paid." } })));
}

