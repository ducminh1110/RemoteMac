// Access: who holds which operator role. Platform admins grant a role to someone who has signed
// in to the customer portal once, and revoke it (their console sessions end with their last
// role). Both need a sign-in in the last 15 minutes.
import { h, container, pageHeader, button, confirmDialog, alert, flash } from "../_console/console.js";
import { S, api, can, when, table, act, field, formDialog, readOnlyNote, roleLabel, toolbar, searchBox } from "../lib.js";

function roleBadge(r) {
  const c = { platform_admin: "red", operations_operator: "blue", support_operator: "", billing_operator: "green", security_auditor: "amber" }[r] || "";
  return h("span", { class: "badge " + c }, roleLabel(r));
}

function recentHint(e, ctx) {
  if (e && e.status === 409 && /sign in again/i.test(e.message)) {
    flash("warn", "This change needs a recent sign-in. Sign in again, then repeat it.");
    return true;
  }
  return false;
}

export async function page(ctx) {
  const r = await api("GET", "api/v1/ops/access");
  const manage = can("ops.access.manage");
  const q = (ctx.query.get("q") || "").toLowerCase();
  let rows = r.grants;
  if (q) rows = rows.filter((g) => (g.email + " " + g.role).toLowerCase().includes(q));
  const admins = r.grants.filter((g) => g.role === "platform_admin").length;
  const people = new Set(r.grants.map((g) => g.email)).size;

  const grant = async () => {
    const email = field({ label: "Email", name: "email", type: "email", required: true, placeholder: "name@company.com", inputmode: "email", spellcheck: "false",
      desc: "They must have signed in to the customer portal once." });
    const role = field({ label: "Role", name: "role", type: "select", value: "support_operator", options: r.roles.map((x) => [x.key, `${roleLabel(x.key)} · ${x.description}`]) });
    const ok = await formDialog({ title: "Grant an operator role", confirm: "Grant role", body: [email, role,
      h("p", { class: "muted small" }, "The role takes effect at their next request. They sign in to this console with the same email.")], submit: async () => {
      const v = email.control.value.trim();
      if (!v.includes("@")) { email.setError("Type their email address."); throw new Error("Type their email address."); }
      try { await api("POST", "api/v1/ops/access", { email: v, role: role.control.value }); }
      catch (e) { if (recentHint(e, ctx)) throw new Error("Sign in again (a sign-in in the last 15 minutes is needed), then repeat this."); throw e; }
      return { email: v, role: role.control.value };
    } });
    if (ok) { flash("ok", `${ok.email} is now ${roleLabel(ok.role).toLowerCase()}.`); ctx.refresh(); }
  };
  const revoke = async (btn, g) => {
    const self = S.me && g.user_id === S.me.id;
    const last = r.grants.filter((x) => x.user_id === g.user_id).length === 1;
    if (!await confirmDialog({ title: `Revoke ${roleLabel(g.role).toLowerCase()} from ${g.email}?`, confirm: "Revoke role", danger: true,
      body: [h("p", {}, last ? "This is their last operator role: their console sessions end now." : "They keep their other operator roles."),
        self ? alert("warn", "This is you", "You lose this role's access immediately.") : null] })) return;
    await act(btn, "Revoking…", async () => {
      try { await api("DELETE", `api/v1/ops/access/${g.user_id}/${g.role}`); }
      catch (e) { if (recentHint(e, ctx)) return; throw e; }
    }, { ok: `${roleLabel(g.role)} revoked from ${g.email}.`, after: async () => { if (self) await ctx.reloadMe(); ctx.refresh(); } });
  };

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Security"], ["Access"]]),
    pageHeader({ title: "Access", desc: "Operator roles in Taro Operations. Customers' roles in their own organizations are managed by the customers.",
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }), manage ? button("Grant role", { kind: "primary", icon: "plus", id: "grant-role", onclick: grant }) : null] }),
    manage ? null : readOnlyNote("operator access"),
    container({ title: "Operator grants", count: rows.length, flush: true, desc: `${people} ${people === 1 ? "person" : "people"}, ${admins} platform admin${admins === 1 ? "" : "s"}. The platform keeps at least one platform admin.` },
      toolbar(searchBox({ label: "Search grants", value: ctx.query.get("q") || "", id: "grant-search", placeholder: "Email or role", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } })),
      table([
        { label: "Email", cell: (g) => h("div", {}, h("strong", {}, g.email), S.me && g.user_id === S.me.id ? h("span", { class: "badge ml" }, "You") : null) },
        { label: "Role", cell: (g) => roleBadge(g.role) },
        { label: "Granted", cell: (g) => when(g.granted_at), class: "nowrap" },
        { label: "Granted by", cell: (g) => g.granted_by || h("span", { class: "muted" }, "System") },
        manage ? { label: "Actions", class: "actions", cell: (g) => button("Revoke", { kind: "sm danger", onclick: (e) => revoke(e.currentTarget, g),
          disabled: g.role === "platform_admin" && admins <= 1 }) } : null,
      ].filter(Boolean), rows, { caption: "Operator grants", empty: { title: "No grants match" } })),
    container({ title: "Roles", desc: "What each operator role may do." },
      h("div", { class: "role-grid" }, r.roles.map((x) => h("div", { class: "role-card" },
        h("div", { class: "rc-head" }, roleBadge(x.key)), h("p", {}, x.description),
        h("ul", { class: "perm-list" }, x.permissions.map((p) => h("li", {}, h("code", {}, p)))))))));
}


