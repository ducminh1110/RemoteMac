// Sign in (email code, Google when configured), the email link, and the no-access page.
import { h, icon, alert, button, busy, flash } from "/_console/console.js";
import { S, api, field } from "/lib.js";

const GOOGLE_ERRORS = {
  cancelled: "Google sign-in was cancelled.",
  invalid: "Google sign-in did not work. Try again.",
  expired: "The Google sign-in took too long. Try again.",
  unverified_email: "Your Google account's email address is not verified.",
};

function safeReturn(p) {
  return p && p.startsWith("/") && !p.startsWith("//") && !p.startsWith("/sign-in") && !p.startsWith("/auth/") && !p.startsWith("/no-access") ? p : "/";
}

function card(...body) {
  return h("div", { class: "auth-wrap" }, h("section", { class: "auth-card container" }, h("div", { class: "c-body" }, body)));
}

async function finish(ctx, back) {
  await ctx.reloadMe();
  if (!S.me || !(S.me.operator_roles || []).length) { ctx.navigate("/no-access", { replace: true }); return; }
  ctx.navigate(safeReturn(back), { replace: true });
}

export async function signIn(ctx) {
  const back = ctx.query.get("return") || "/";
  if (S.me && (S.me.operator_roles || []).length) { ctx.navigate(safeReturn(back), { replace: true }); return h("div"); }
  const gerr = ctx.query.get("error");
  if (gerr === "not_an_operator") { ctx.navigate("/no-access", { replace: true }); return h("div"); }
  const p = S.providers || {};
  let email = "";
  const root = h("div");

  const devNote = p.environment === "development"
    ? alert("info", "Development", "Sign-in codes are not mailed. Read them at ", h("a", { href: "/api/v1/dev/outbox", target: "_blank", rel: "noopener" }, h("code", {}, "GET /api/v1/dev/outbox")), ".")
    : null;

  function head(sub) {
    return [h("div", { class: "auth-brand" }, h("span", { class: "mark", "aria-hidden": "true" }, ((S.branding && S.branding.displayName) || "T").charAt(0).toUpperCase()),
      h("div", {}, h("p", { class: "auth-product" }, (S.branding && S.branding.displayName) || "Taro Enterprise"), h("p", { class: "auth-svc" }, "Operations console"))),
    h("h1", { class: "auth-title" }, "Sign in"), h("p", { class: "lead" }, sub)];
  }

  function emailStep(msg) {
    const f = field({ label: "Work email", name: "email", type: "email", value: email, autocomplete: "username", placeholder: "you@company.com", required: true, inputmode: "email", spellcheck: "false" });
    const go = button("Continue", { kind: "primary", type: "submit", id: "email-continue" });
    const form = h("form", { class: "auth-form", novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      const v = f.control.value.trim();
      if (!v || !v.includes("@")) { f.setError("Type your email address."); f.control.focus(); return; }
      f.setError(null);
      await busy(go, "Sending…", async () => {
        try { await api("POST", "/api/v1/auth/email/start", { email: v }); email = v; codeStep(); }
        catch (err) { f.setError(err.message); f.control.focus(); }
      });
    } }, f, h("div", { class: "auth-actions" }, go));
    root.replaceChildren(card(...head("Operators only. We send a one-time code to your email."),
      gerr && GOOGLE_ERRORS[gerr] ? alert("err", null, GOOGLE_ERRORS[gerr]) : null,
      msg ? alert("info", null, msg) : null,
      form,
      p.google ? [h("div", { class: "or" }, h("span", {}, "or")),
        h("a", { class: "btn google", href: "/api/v1/auth/google/start?return_to=" + encodeURIComponent(safeReturn(back)) }, icon("google"), "Sign in with Google")] : null,
      devNote));
    setTimeout(() => f.control.focus(), 0);
  }

  function codeStep() {
    const f = field({ label: "Sign-in code", name: "code", value: "", autocomplete: "one-time-code", inputmode: "numeric", maxlength: 6, placeholder: "6 digits", required: true, mono: true,
      desc: "We sent a 6-digit code to " + email + ". It works once, for 15 minutes." });
    const go = button("Sign in", { kind: "primary", type: "submit", id: "code-submit" });
    const form = h("form", { class: "auth-form", novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      const code = f.control.value.replace(/\s+/g, "");
      if (!/^\d{6}$/.test(code)) { f.setError("The code is 6 digits."); f.control.focus(); return; }
      f.setError(null);
      await busy(go, "Signing in…", async () => {
        try { await api("POST", "/api/v1/auth/email/verify", { email, code }); await finish(ctx, back); }
        catch (err) {
          if (err.status === 403) { ctx.navigate("/no-access", { replace: true }); return; }
          f.setError(err.message); f.control.focus();
        }
      });
    } }, f, h("div", { class: "auth-actions" },
      button("Use another email", { kind: "link", onclick: () => emailStep() }),
      button("Send a new code", { kind: "link", onclick: async (e) => {
        await busy(e.currentTarget, "Sending…", async () => {
          try { await api("POST", "/api/v1/auth/email/start", { email }); flash("ok", "A new code is on its way."); }
          catch (err) { flash("err", err.message); }
        });
      } }),
      go));
    root.replaceChildren(card(...head("Check your email."), form, devNote));
    setTimeout(() => f.control.focus(), 0);
  }

  emailStep();
  return root;
}

/** /auth/verify?token=… : the link in the sign-in mail. */
export async function verify(ctx) {
  const token = ctx.query.get("token");
  if (!token) return card(h("h1", { class: "auth-title" }, "Sign in"), alert("err", null, "This link has no sign-in token."), h("a", { class: "btn primary", href: "/sign-in" }, "Go to sign in"));
  try {
    await api("POST", "/api/v1/auth/email/verify", { token });
    history.replaceState(null, "", "/");
    await finish(ctx, "/");
    return h("div");
  } catch (err) {
    if (err.status === 403) { ctx.navigate("/no-access", { replace: true }); return h("div"); }
    return card(h("h1", { class: "auth-title" }, "Sign in"), alert("err", "The link did not work", err.message), h("a", { class: "btn primary", href: "/sign-in" }, "Ask for a new code"));
  }
}

export async function noAccess(ctx) {
  const signedIn = S.me && S.me.email;
  return card(
    h("div", { class: "no-access-icon" }, icon("lock")),
    h("h1", { class: "auth-title" }, "No access to Taro Operations"),
    h("p", { class: "lead" }, signedIn ? `${S.me.email} has no operator role.` : "This account has no operator role, or has not signed in to the customer portal yet."),
    h("p", {}, "The provider console is for the provider's operators. A platform administrator can grant you a role on the Access page. Customers manage their Macs in the customer portal."),
    h("div", { class: "auth-actions" }, button("Sign in with another account", { kind: "primary", onclick: async () => {
      if (S.me) { try { await api("POST", "/api/v1/auth/sign-out"); } catch (_) { /* gone already */ } S.me = null; }
      ctx.navigate("/sign-in", { replace: true });
    } })));
}
