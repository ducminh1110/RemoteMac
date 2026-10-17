// Billing operations: rentals that need a person (past due, refunded, paid but failed) and the
// Stripe webhook inbox (state, attempts, errors; replay a failed or dead event).
import { h, container, pageHeader, button, confirmDialog, alert } from "/_console/console.js";
import { S, api, can, st, when, money, table, shortId, toolbar, searchBox, selectBox, act, readOnlyNote, stats, orgNames, orgLink, copyButton } from "/lib.js";

function why(r) {
  if (r.billing_state === "past_due") return ["Past due", "The last payment failed. Stripe retries; the customer may need to update their card."];
  if (r.billing_state === "refunded") return ["Refunded", "The payment was refunded. Check that the rental ended."];
  if (r.status === "failed" && r.billing_state === "paid") return ["Paid, not delivered", "The customer paid but provisioning failed. Refunds are manual (in Stripe)."];
  return ["Review", ""];
}

function simBadge() {
  const p = S.providers || {};
  return p.billing && !p.billing_live ? h("span", { class: "badge amber" }, "Simulated billing") : null;
}

export async function attention(ctx) {
  const [r, names] = await Promise.all([api("GET", "/api/v1/ops/billing/attention"), orgNames()]);
  const rows = r.needs_attention;
  const n = (f) => rows.filter(f).length;
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Billing"], ["Needs attention"]]),
    pageHeader({ title: "Billing: needs attention", desc: h("span", {}, "Rentals whose billing needs a person: past due, refunded, or paid but not delivered. ", simBadge()),
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    stats([["Needs attention", rows.length, rows.length ? "warn" : "ok"], ["Past due", n((x) => x.billing_state === "past_due")], ["Refunded", n((x) => x.billing_state === "refunded")], ["Paid, not delivered", n((x) => x.status === "failed" && x.billing_state === "paid"), n((x) => x.status === "failed" && x.billing_state === "paid") ? "bad" : null]]),
    container({ title: "Rentals", count: rows.length, flush: true, foot: "Refunds, credits and dunning are done in the Stripe dashboard; this list shows what to look at." },
      table([
        { label: "Reason", cell: (x) => { const [t, d] = why(x); return h("div", {}, h("strong", {}, t), d ? h("div", { class: "muted small" }, d) : null); } },
        { label: "Organization", cell: (x) => orgLink(x.organization_id, names) },
        { label: "Rental", cell: (x) => h("span", { class: "id-row" }, shortId(x.id), copyButton(x.id, "Copy rental ID")) },
        { label: "Plan", cell: (x) => x.plan_name },
        { label: "Status", cell: (x) => h("div", {}, st(x.status), x.status_reason ? h("div", { class: "muted small clip", title: x.status_reason }, x.status_reason) : null) },
        { label: "Billing", cell: (x) => st(x.billing_state) },
        { label: "Amount", cell: (x) => money(x.amount_minor, x.currency) + (x.billing === "monthly" ? " / month" : ""), class: "num nowrap" },
        { label: "Updated", cell: (x) => when(x.updated_at, { rel: true }), class: "nowrap" },
      ], rows, { caption: "Rentals that need attention", empty: { title: "Nothing needs attention", text: "No rental is past due, refunded, or paid but failed." } })));
}

const EVENT_STATES = ["pending", "processed", "ignored", "failed", "dead"];

export async function events(ctx) {
  const state = ctx.query.get("state") || "";
  const q = (ctx.query.get("q") || "").toLowerCase();
  const r = await api("GET", "/api/v1/ops/billing/webhook-events" + (state ? "?state=" + state : ""));
  let rows = r.events;
  if (q) rows = rows.filter((e) => (e.id + " " + e.type + " " + (e.last_error || "")).toLowerCase().includes(q));
  const manage = can("ops.billing.manage");
  const n = (s) => r.events.filter((e) => e.state === s).length;
  const replay = async (btn, e) => {
    if (!await confirmDialog({ title: "Replay this event?", confirm: "Replay event",
      body: [h("p", {}, "The event ", h("code", {}, e.id), ` (${e.type}) is processed again from its stored payload. Processing is idempotent.`), e.last_error ? h("p", { class: "muted" }, "Last error: " + e.last_error) : null] })) return;
    await act(btn, "Replaying…", () => api("POST", `/api/v1/ops/billing/webhook-events/${encodeURIComponent(e.id)}/replay`), { ok: "The event is queued for processing.", after: ctx.refresh });
  };
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Billing"], ["Stripe events"]]),
    pageHeader({ title: "Stripe webhook events", desc: h("span", {}, "Events from Stripe as received and processed (up to 200, newest first). Payloads stay in the database. ", simBadge()),
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    manage ? null : readOnlyNote("webhook events"),
    stats([["Listed", r.events.length], ["Pending", n("pending")], ["Processed", n("processed"), "ok"], ["Failed", n("failed"), n("failed") ? "warn" : null], ["Dead", n("dead"), n("dead") ? "bad" : null]]),
    n("dead") ? alert("err", "Dead events", "Some events were given up after 8 tries. Fix the cause, then replay them.") : null,
    container({ title: "Events", count: rows.length, flush: true },
      toolbar(
        searchBox({ label: "Search events", value: ctx.query.get("q") || "", id: "evt-search", placeholder: "Event ID, type or error", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        selectBox({ label: "State", value: state, id: "evt-state", options: [["", "All states"], ...EVENT_STATES.map((s) => [s, s.charAt(0).toUpperCase() + s.slice(1)])], onchange: (v) => { ctx.setQuery({ state: v }); ctx.refresh(); } })),
      table([
        { label: "Event", cell: (e) => h("div", {}, h("span", { class: "mono small" }, e.id), e.livemode ? h("span", { class: "badge red ml" }, "Live") : h("span", { class: "badge ml" }, "Test")) },
        { label: "Type", cell: (e) => h("span", { class: "mono" }, e.type) },
        { label: "State", cell: (e) => st(e.state) },
        { label: "Attempts", cell: (e) => String(e.attempts), class: "num" },
        { label: "Last error", cell: (e) => e.last_error ? h("span", { class: "err-text clip", title: e.last_error }, e.last_error) : h("span", { class: "muted" }, "–") },
        { label: "Received", cell: (e) => when(e.received_at), class: "nowrap" },
        { label: "Processed", cell: (e) => when(e.processed_at, { rel: true }), class: "nowrap" },
        manage ? { label: "Actions", class: "actions", cell: (e) => ["failed", "dead"].includes(e.state) ? button("Replay", { kind: "sm", onclick: (ev) => replay(ev.currentTarget, e) }) : null } : null,
      ].filter(Boolean), rows, { caption: "Stripe webhook events", rowClass: (e) => e.state === "dead" ? "fail" : null,
        empty: { title: state || q ? "No event matches the filters" : "No events yet", text: state || q ? "Clear a filter to see more." : "Stripe sends events when customers pay." } })));
}
