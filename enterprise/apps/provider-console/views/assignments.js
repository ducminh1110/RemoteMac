// Assignments: the console's main job. Give a customer (by email) a Mac of a plan, with no
// checkout, and end it again. The provider's own systems can do the same with the ops API key.
import { h, container, pageHeader, button, confirmDialog, alert } from "/_console/console.js";
import { api, can, st, when, table, field, act, readOnlyNote, stats, stateLabel } from "/lib.js";

const BUSY = ["paid", "provisioning", "cancel_pending"];
const ENDED = ["cancelled", "expired", "failed"];
const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export async function page(ctx) {
  const manage = can("ops.inventory.manage");
  const [list, plansRes] = await Promise.all([
    api("GET", "/api/v1/ops/assignments"),
    manage ? api("GET", "/api/v1/ops/plans").catch(() => ({ plans: [] })) : { plans: [] },
  ]);
  let rows = list.assignments;
  const plans = plansRes.plans.filter((p) => p.active);
  const holder = h("div", { id: "assignments-table" });
  const figures = h("div", { id: "assignments-stats" });

  const end = async (btn, a) => {
    if (!await confirmDialog({ title: "End this assignment?", confirm: "End assignment", danger: true,
      body: h("p", {}, `${a.email || a.organization}'s ${a.plan} Mac is released and the customer loses access to it.`) })) return;
    await act(btn, "Ending…", () => api("POST", `/api/v1/ops/assignments/${a.rental_id}/end`), { ok: "The assignment is ending.", after: reload });
  };

  function draw() {
    const live = rows.filter((a) => !ENDED.includes(a.status));
    figures.replaceChildren(stats([["Assigned", live.length], ["Ready", rows.filter((a) => a.status === "ready").length, "ok"],
      ["Preparing", rows.filter((a) => ["paid", "provisioning"].includes(a.status)).length], ["Ended", rows.length - live.length]]));
    holder.replaceChildren(table([
      { label: "Customer", cell: (a) => h("div", {}, h("strong", {}, a.email || "–"), a.organization && a.organization !== a.email ? h("div", { class: "muted small" }, a.organization) : null) },
      { label: "Plan", cell: (a) => a.plan },
      { label: "Status", cell: (a) => st(a.status, stateLabel(a.status)) },
      { label: "Mac", cell: (a) => a.mac_name || h("span", { class: "muted" }, "–") },
      { label: "Assigned", cell: (a) => when(a.created_at, { rel: true }), class: "nowrap" },
      manage ? { label: "Actions", class: "actions", cell: (a) => ENDED.includes(a.status) || a.status === "cancel_pending" ? null :
        button("End", { kind: "sm danger", onclick: (e) => end(e.currentTarget, a) }) } : null,
    ].filter(Boolean), rows, { caption: "Assignments", id: "assignments-grid", rowClass: (a) => ENDED.includes(a.status) ? "retired" : null,
      empty: { title: "No assignments yet", text: manage ? "Assign a Mac to a customer with the form above." : "Nothing has been assigned." } }));
  }

  let timer = null;
  function poll() {
    clearTimeout(timer);
    if (ctx.stale() || !rows.some((a) => BUSY.includes(a.status))) return;
    timer = setTimeout(reload, 5000);
  }
  async function reload() {
    if (ctx.stale()) return;
    try { rows = (await api("GET", "/api/v1/ops/assignments")).assignments; draw(); } catch (_) { /* try again next time */ }
    poll();
  }

  let form = null;
  if (manage) {
    const fEmail = field({ label: "Customer email", name: "email", type: "email", required: true, placeholder: "name@company.com", id: "assign-email",
      desc: "An account and organization are made if the customer has none." });
    const fPlan = field({ label: "Plan", name: "plan_id", type: "select", required: true, id: "assign-plan",
      options: plans.map((p) => [p.id, `${p.name} · ${p.chip} · ${p.region}`]) });
    const errBox = h("div", { role: "alert" });
    const submit = button("Assign Mac", { kind: "primary", type: "submit", id: "assign-submit" });
    form = container({ title: "Assign a Mac" }, h("form", { novalidate: true, id: "assign-form", onsubmit: async (e) => {
      e.preventDefault();
      errBox.replaceChildren(); fEmail.setError(null);
      const email = fEmail.control.value.trim();
      if (!EMAIL.test(email)) { fEmail.setError("Type the customer's email address."); fEmail.control.focus(); return; }
      if (!fPlan.control.value) { fPlan.setError("Choose a plan."); return; }
      await act(submit, "Assigning…", async () => {
        try { await api("POST", "/api/v1/ops/assignments", { email, plan_id: fPlan.control.value }); }
        catch (x) { errBox.replaceChildren(alert("err", "The Mac was not assigned", x.message)); throw x; }
      }, { ok: `A ${plans.find((p) => p.id === fPlan.control.value).name} Mac is being prepared for ${email}.`, after: () => { fEmail.control.value = ""; return reload(); } });
    } },
    errBox,
    plans.length ? null : alert("warn", "No active plan", "Create a plan first (Catalog, Plans)."),
    h("div", { class: "form-grid assign" }, fEmail, fPlan),
    h("div", { class: "form-actions start" }, submit)));
  }

  draw();
  poll();
  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Customers"], ["Assignments"]]),
    pageHeader({ title: "Assignments", desc: "Give customers Macs and take them back. The customer sees an assigned Mac in the portal and in the Taro app once it is ready.",
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    manage ? null : readOnlyNote("assignments"),
    form,
    figures,
    container({ title: "Assignments", count: rows.length, flush: true, foot: h("span", {}, "Your own systems can assign automatically: the same endpoint with ", h("code", {}, "Authorization: Bearer <TARO_OPS_API_KEY>"), ".") }, holder));
}
