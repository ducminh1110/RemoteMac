// Customer Portal: the account (profile, browser sessions, signed-in Taro apps) and
// notifications.
import { h, container, kv, pageHeader, button, confirmDialog } from "/_console/console.js";
import { S, api } from "../core.js";
import { run, ok, table, emptyState, when, badge } from "../ui.js";
import { signOut } from "./auth.js";

/** "Chrome on macOS" from a user agent. */
export function deviceName(ua) {
  if (!ua) return "Unknown browser";
  const b = /Edg\//.test(ua) ? "Edge" : /OPR\//.test(ua) ? "Opera" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : /curl\//.test(ua) ? "curl" : "Browser";
  const os = /iPhone|iPad/.test(ua) ? "iOS" : /Android/.test(ua) ? "Android" : /Mac OS X|Macintosh/.test(ua) ? "macOS" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "";
  return os ? `${b} on ${os}` : b;
}

export async function pageAccount(ctx) {
  const sessionsBox = h("div", {});
  const appsBox = h("div", {});
  async function loadSessions() {
    const r = await api("GET", "/api/v1/auth/sessions");
    if (!ctx.alive()) return;
    const others = r.sessions.filter((s) => s.id !== r.current).length;
    const revokeOthers = button("Sign out other sessions", { kind: "sm", id: "revoke-others", disabled: !others });
    revokeOthers.addEventListener("click", async () => {
      if (!await confirmDialog({ title: "Sign out everywhere else?", body: h("p", {}, `${others} other ${others === 1 ? "session ends" : "sessions end"} now. This browser stays signed in.`), confirm: "Sign out others", danger: true })) return;
      const x = await run(revokeOthers, "Signing out…", () => api("POST", "/api/v1/auth/sessions/revoke-others"));
      if (x) { ok(x.revoked === 1 ? "1 session ended." : `${x.revoked} sessions ended.`); loadSessions(); }
    });
    sessionsBox.replaceChildren(container({ title: "Browser sessions", count: r.sessions.length, flush: true, desc: "Where you are signed in to this portal", actions: revokeOthers },
      table([
        { label: "Device", class: "name", cell: (s) => [deviceName(s.user_agent), s.id === r.current ? [" ", badge("This browser", "green")] : null] },
        { label: "IP address", cell: (s) => h("span", { class: "mono small" }, s.ip || "–") },
        { label: "Signed in", cell: (s) => when(s.created_at) },
        { label: "Last active", cell: (s) => when(s.last_seen_at, true) },
        { label: "Expires", cell: (s) => when(s.expires_at) },
        { label: "Actions", cell: (s) => s.id === r.current
          ? button("Sign out", { kind: "link sm", onclick: () => signOut() })
          : button("Revoke", { kind: "link sm", id: "revoke-" + s.id, onclick: async (e) => {
            if (!await confirmDialog({ title: "Revoke this session?", body: h("p", {}, `${deviceName(s.user_agent)} (${s.ip || "unknown IP"}) is signed out.`), confirm: "Revoke", danger: true })) return;
            const x = await run(e.currentTarget, "Revoking…", () => api("POST", `/api/v1/auth/sessions/${s.id}/revoke`));
            if (x !== undefined) { ok("Session revoked."); loadSessions(); }
          } }) },
      ], r.sessions, { label: "Browser sessions" })));
  }

  async function loadApps() {
    let r;
    try { r = await api("GET", "/api/v1/auth/devices"); } catch (e) { if (e.status === 404) { appsBox.replaceChildren(); return; } throw e; }
    if (!ctx.alive()) return;
    appsBox.replaceChildren(container({ title: "Signed-in apps", count: r.devices.length, flush: true, desc: "Taro apps you approved. Revoke one you no longer use." },
      table([
        { label: "App", class: "name", cell: (d) => d.client },
        { label: "Signed in", cell: (d) => when(d.created_at) },
        { label: "Last used", cell: (d) => when(d.last_used_at, true) },
        { label: "Expires", cell: (d) => when(d.expires_at) },
        { label: "Actions", cell: (d) => button("Revoke", { kind: "link sm", id: "revoke-app-" + d.id, onclick: async (e) => {
          if (!await confirmDialog({ title: `Sign out ${d.client}?`, body: h("p", {}, "The app is signed out now and must be approved again to sign in."), confirm: "Revoke", danger: true })) return;
          const x = await run(e.currentTarget, "Revoking…", () => api("POST", `/api/v1/auth/devices/${d.id}/revoke`));
          if (x !== undefined) { ok(`${d.client} is signed out.`); loadApps(); }
        } }) },
      ], r.devices, { label: "Signed-in apps", empty: emptyState("No apps signed in", "When you sign in to the Taro app, you approve it at /device and it shows here.") })));
  }

  ctx.inner.replaceChildren(
    pageHeader({ title: "Account", desc: "Your email address and where you are signed in.", actions: button("Sign out", { id: "account-sign-out", onclick: () => signOut() }) }),
    container({ title: "Profile" }, kv([["Email", S.me.email]], 1)),
    sessionsBox, appsBox);
  await Promise.all([loadSessions(), loadApps()]);
}

