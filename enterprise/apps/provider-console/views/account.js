// Account: the signed-in operator, their roles and permissions, their console sessions (end one,
// end all others) and sign-out. Console sessions last 12 hours and end after 2 idle hours.
import { h, container, pageHeader, kv, button, confirmDialog } from "/_console/console.js";
import { S, api, when, table, act, field, roleLabel } from "/lib.js";

function device(ua) {
  if (!ua) return "Unknown device";
  const b = /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Firefox\//.test(ua) ? "Firefox" : /Safari\//.test(ua) ? "Safari" : /curl|reqwest|node|undici/i.test(ua) ? "Script" : "Browser";
  const os = /Mac OS X/.test(ua) ? "macOS" : /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Linux/.test(ua) ? "Linux" : "";
  return os ? `${b} on ${os}` : b;
}

export async function page(ctx) {
  const [me, s] = await Promise.all([api("GET", "/api/v1/me"), api("GET", "/api/v1/auth/sessions")]);
  S.me = me;
  const others = s.sessions.filter((x) => x.id !== s.current).length;
  const name = field({ label: "Display name", name: "display_name", value: me.display_name, maxlength: 80, placeholder: "Your name", desc: "Shown in the top bar and to other operators." });
  const saveName = button("Save", { kind: "sm", type: "submit", id: "save-name" });

  const revoke = async (btn, x) => {
    if (!await confirmDialog({ title: "End this session?", confirm: "End session", danger: true, body: h("p", {}, `${device(x.user_agent)} from ${x.ip || "an unknown address"} is signed out.`) })) return;
    await act(btn, "Ending…", () => api("POST", `/api/v1/auth/sessions/${x.id}/revoke`), { ok: "Session ended.", after: ctx.refresh });
  };
  const revokeOthers = async (e) => {
    const btn = e.currentTarget;
    if (!await confirmDialog({ title: "Sign out everywhere else?", confirm: "End other sessions", danger: true, body: h("p", {}, `${others} other session${others === 1 ? "" : "s"} end now. This one stays.`) })) return;
    await act(btn, "Ending…", () => api("POST", "/api/v1/auth/sessions/revoke-others"), { ok: (r) => `${r.revoked} session${r.revoked === 1 ? "" : "s"} ended.`, after: ctx.refresh });
  };

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Account"]]),
    pageHeader({ title: "Account", desc: "Your operator account and where you are signed in to this console.",
      actions: button("Sign out", { kind: "primary", id: "account-sign-out", onclick: () => ctx.signOut() }) }),
    h("div", { class: "cols" },
      container({ title: "You" },
        kv([["Email", me.email], ["Roles", (me.operator_roles || []).map(roleLabel).join(", ") || "None"], ["User ID", h("span", { class: "mono" }, me.id)]], 1),
        h("form", { class: "name-form", novalidate: true, onsubmit: async (e) => {
          e.preventDefault();
          await act(saveName, "Saving…", () => api("PATCH", "/api/v1/me", { display_name: name.control.value.trim() }), { ok: "Name saved.", after: async () => { await ctx.reloadMe(); } });
        } }, name, h("div", { class: "form-actions" }, saveName))),
      container({ title: "Permissions", desc: "From your roles; the API checks them on every request." },
        h("ul", { class: "perm-list cols2" }, (me.operator_permissions || []).map((p) => h("li", {}, h("code", {}, p)))))),
    container({ title: "Console sessions", count: s.sessions.length, flush: true, desc: "Sessions last 12 hours and end after 2 hours without use. The customer portal has its own sessions.",
      actions: button("Sign out other sessions", { kind: "sm", id: "revoke-others", onclick: revokeOthers, disabled: !others }) },
    table([
      { label: "Device", cell: (x) => h("div", {}, h("strong", {}, device(x.user_agent)), x.id === s.current ? h("span", { class: "badge green ml" }, "This session") : null, h("div", { class: "muted small clip", title: x.user_agent || "" }, x.user_agent || "–")) },
      { label: "IP address", cell: (x) => h("span", { class: "mono" }, x.ip || "–") },
      { label: "Signed in", cell: (x) => when(x.created_at), class: "nowrap" },
      { label: "Last active", cell: (x) => when(x.last_seen_at, { rel: true }), class: "nowrap" },
      { label: "Expires", cell: (x) => when(x.expires_at), class: "nowrap" },
      { label: "Actions", class: "actions", cell: (x) => x.id === s.current ? button("Sign out", { kind: "sm", onclick: () => ctx.signOut() }) : button("End", { kind: "sm danger", onclick: (e) => revoke(e.currentTarget, x) }) },
    ], s.sessions, { caption: "Console sessions" })));
}
