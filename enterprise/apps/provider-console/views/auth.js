// Sign in (email and password; an email code or Google when configured), the first-run form that
// makes the first administrator, the email link, and the no-access page.
import { h, icon, alert, button, busy, flash } from "../_console/console.js";
import { S, api, field, BASE, product } from "../lib.js";

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

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
  if (S.setup && S.me && (S.me.operator_roles || []).includes("platform_admin")) S.setup.needs_admin = false;
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

  const outbox = BASE + "api/v1/dev/outbox";
  const devNote = p.environment === "development"
    ? alert("info", "Development", "Sign-in codes are not mailed. Read them at ", h("a", { href: "api/v1/dev/outbox", target: "_blank", rel: "noopener" }, h("code", {}, "GET " + outbox)), ".")
    : null;

  function brand() {
    return h("div", { class: "auth-brand" }, h("span", { class: "mark", "aria-hidden": "true" }, product().charAt(0).toUpperCase()),
      h("div", {}, h("p", { class: "auth-product" }, product()), h("p", { class: "auth-svc" }, "Operations console")));
  }
  function head(sub, title = "Sign in") {
    return [brand(), h("h1", { class: "auth-title" }, title), h("p", { class: "lead" }, sub)];
  }
  const google = () => p.google ? [h("div", { class: "or" }, h("span", {}, "or")),
    h("a", { class: "btn google", href: "api/v1/auth/google/start?return_to=" + encodeURIComponent(safeReturn(back)) }, icon("google"), "Sign in with Google")] : null;

  // ---- the first visit: make the administrator (with the code printed on the server)
  function setupStep() {
    const code = field({ label: "Setup code", name: "setup_code", autocomplete: "off", spellcheck: "false", required: true, mono: true, placeholder: "XXXX-XXXX-XXXX", id: "setup-code",
      hint: h("span", {}, "Find it on the server: ", h("code", {}, "sudo taro-relay portal setup-code")) });
    const name = field({ label: "Your name", name: "name", autocomplete: "name", maxlength: 80, required: true, id: "setup-name" });
    const mail = field({ label: "Email", name: "email", type: "email", autocomplete: "username", placeholder: "you@company.com", required: true, inputmode: "email", spellcheck: "false", id: "setup-email" });
    const pw = field({ label: "Password", name: "password", type: "password", autocomplete: "new-password", required: true, id: "setup-password", desc: "At least 12 characters." });
    const pw2 = field({ label: "Confirm password", name: "password2", type: "password", autocomplete: "new-password", required: true, id: "setup-password2" });
    const errBox = h("div", { role: "alert" });
    const go = button("Create administrator", { kind: "primary", type: "submit", id: "setup-submit" });
    const all = [code, name, mail, pw, pw2];
    const form = h("form", { class: "auth-form", id: "setup-form", novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      errBox.replaceChildren();
      all.forEach((f) => f.setError(null));
      const v = { setup_code: code.control.value.trim(), name: name.control.value.trim(), email: mail.control.value.trim(), password: pw.control.value };
      const bad = !v.setup_code ? [code, "Type the setup code."]
        : !v.name ? [name, "Type your name."]
          : !EMAIL.test(v.email) ? [mail, "Type your email address."]
            : [...v.password].length < 12 ? [pw, "Choose a password of at least 12 characters."]
              : pw2.control.value !== v.password ? [pw2, "The two passwords are not the same."] : null;
      if (bad) { bad[0].setError(bad[1]); bad[0].control.focus(); return; }
      await busy(go, "Creating…", async () => {
        try {
          await api("POST", "api/v1/setup/admin", v);
          if (S.setup) S.setup.needs_admin = false;
          await finish(ctx, "/");
        } catch (err) {
          if (err.status === 409) { if (S.setup) S.setup.needs_admin = false; passwordStep(err.message); return; }
          // the server's words, at the field they are about
          const m = err.message || "";
          const at = err.status === 422 ? (/setup code/i.test(m) ? code : /email/i.test(m) ? mail : /name/i.test(m) ? name : /password/i.test(m) ? pw : null) : null;
          if (at) { at.setError(m); at.control.focus(); } else errBox.replaceChildren(alert("err", null, m));
        }
      });
    } }, errBox, code, name, mail, pw, pw2, h("div", { class: "auth-actions" }, go));
    root.replaceChildren(card(...head("Create the first administrator of this console. You sign in with this email and password afterwards.", "Set up " + product()),
      form,
      h("p", { class: "auth-alt" }, "Already an administrator? ", h("button", { type: "button", class: "linkbtn", id: "setup-sign-in", onclick: () => passwordStep() }, "Sign in"))));
    setTimeout(() => code.control.focus(), 0);
  }

  // ---- email and password (the usual way for administrators: no mail server needed)
  function passwordStep(msg) {
    const f = field({ label: "Work email", name: "email", type: "email", value: email, autocomplete: "username", placeholder: "you@company.com", required: true, inputmode: "email", spellcheck: "false", id: "pw-email" });
    const pw = field({ label: "Password", name: "password", type: "password", autocomplete: "current-password", required: true, id: "pw-password" });
    const go = button("Sign in", { kind: "primary", type: "submit", id: "password-submit" });
    const form = h("form", { class: "auth-form", id: "password-form", novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      const v = f.control.value.trim();
      f.setError(null); pw.setError(null);
      if (!EMAIL.test(v)) { f.setError("Type your email address."); f.control.focus(); return; }
      if (!pw.control.value) { pw.setError("Type your password."); pw.control.focus(); return; }
      email = v;
      await busy(go, "Signing in…", async () => {
        try { await api("POST", "api/v1/auth/password", { email: v, password: pw.control.value }); await finish(ctx, back); }
        catch (err) {
          if (err.status === 403) { ctx.navigate("/no-access", { replace: true }); return; }
          pw.setError(err.message); pw.control.select(); pw.control.focus();
        }
      });
    } }, f, pw, h("div", { class: "auth-actions" }, go));
    root.replaceChildren(card(...head("Operators only. Sign in with your email and password."),
      gerr && GOOGLE_ERRORS[gerr] ? alert("err", null, GOOGLE_ERRORS[gerr]) : null,
      msg ? alert("info", null, msg) : null,
      form,
      h("p", { class: "auth-alt" }, h("button", { type: "button", class: "linkbtn", id: "use-code", onclick: () => emailStep() }, "Sign in with an email code instead")),
      google()));
    setTimeout(() => (email ? pw : f).control.focus(), 0);
  }

  // ---- a one-time code by mail
  function emailStep(msg) {
    const f = field({ label: "Work email", name: "email", type: "email", value: email, autocomplete: "username", placeholder: "you@company.com", required: true, inputmode: "email", spellcheck: "false" });
    const go = button("Continue", { kind: "primary", type: "submit", id: "email-continue" });
    const form = h("form", { class: "auth-form", novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      const v = f.control.value.trim();
      if (!v || !v.includes("@")) { f.setError("Type your email address."); f.control.focus(); return; }
      f.setError(null);
      await busy(go, "Sending…", async () => {
        try { await api("POST", "api/v1/auth/email/start", { email: v }); email = v; codeStep(); }
        catch (err) { f.setError(err.message); f.control.focus(); }
      });
    } }, f, h("div", { class: "auth-actions" }, go));
    root.replaceChildren(card(...head("We send a one-time code to your email."),
      gerr && GOOGLE_ERRORS[gerr] ? alert("err", null, GOOGLE_ERRORS[gerr]) : null,
      msg ? alert("info", null, msg) : null,
      form,
      h("p", { class: "auth-alt" }, h("button", { type: "button", class: "linkbtn", id: "use-password", onclick: () => passwordStep() }, "Sign in with your password instead")),
      google(),
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
        try { await api("POST", "api/v1/auth/email/verify", { email, code }); await finish(ctx, back); }
        catch (err) {
          if (err.status === 403) { ctx.navigate("/no-access", { replace: true }); return; }
          f.setError(err.message); f.control.focus();
        }
      });
    } }, f, h("div", { class: "auth-actions" },
      button("Use another email", { kind: "link", onclick: () => emailStep() }),
      button("Send a new code", { kind: "link", onclick: async (e) => {
        await busy(e.currentTarget, "Sending…", async () => {
          try { await api("POST", "api/v1/auth/email/start", { email }); flash("ok", "A new code is on its way."); }
          catch (err) { flash("err", err.message); }
        });
      } }),
      go));
    root.replaceChildren(card(...head("Check your email."), form, devNote));
    setTimeout(() => f.control.focus(), 0);
  }

  if (S.setup && S.setup.needs_admin) setupStep(); else passwordStep();
  return root;
}

/** /auth/verify?token=… : the link in the sign-in mail. */
export async function verify(ctx) {
  const token = ctx.query.get("token");
  if (!token) return card(h("h1", { class: "auth-title" }, "Sign in"), alert("err", null, "This link has no sign-in token."), h("a", { class: "btn primary", href: "sign-in" }, "Go to sign in"));
  try {
    await api("POST", "api/v1/auth/email/verify", { token });
    history.replaceState(null, "", BASE);
    await finish(ctx, "/");
    return h("div");
  } catch (err) {
    if (err.status === 403) { ctx.navigate("/no-access", { replace: true }); return h("div"); }
    return card(h("h1", { class: "auth-title" }, "Sign in"), alert("err", "The link did not work", err.message), h("a", { class: "btn primary", href: "sign-in" }, "Ask for a new code"));
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
      if (S.me) { try { await api("POST", "api/v1/auth/sign-out"); } catch (_) { /* gone already */ } S.me = null; }
      ctx.navigate("/sign-in", { replace: true });
    } })));
}
