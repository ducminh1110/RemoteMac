// Customer Portal: sign-in (email code, Google), the link from the sign-in mail, the Taro app's
// device sign-in, and signing out.
import { h, icon, alert, pageHeader, container, kv, button, flash } from "/_console/console.js";
import { S, api, refreshMe, navigate, takeReturn, peekReturn, saveReturn, isDev } from "../core.js";
import { field, run, report, ok, announce, fmtDateTime, fmtRel, loading } from "../ui.js";

const GOOGLE_ERRORS = {
  cancelled: "Google sign-in was cancelled.",
  invalid: "Google sign-in did not work. Try again.",
  expired: "The Google sign-in took too long. Try again.",
  unverified_email: "Your Google account's email address is not verified, so it cannot sign in.",
  not_an_operator: "This account cannot sign in here.",
};

/** After a successful sign-in: the page the person came for, or the first-run page. */
async function afterSignIn() {
  await refreshMe();
  const ret = takeReturn();
  if (!S.me) { navigate("/sign-in", { replace: true }); return; }
  if (ret) { navigate(ret, { replace: true }); return; }
  navigate("/", { replace: true });
}

function productName() { return (S.branding && S.branding.displayName) || "Taro Enterprise"; }

function authCard(...body) {
  const b = S.branding || {};
  const logo = b.logoAssetId
    ? h("img", { class: "auth-logo", src: "/api/v1/assets/" + b.logoAssetId, alt: "" })
    : h("span", { class: "auth-logo mark", "aria-hidden": "true" }, productName().slice(0, 1).toUpperCase());
  return h("div", { class: "auth-wrap" },
    h("div", { class: "auth-brand" }, logo, h("div", {}, h("p", { class: "auth-product" }, productName()), h("p", { class: "muted small" }, "Customer Portal"))),
    ...body);
}

export async function pageSignIn(ctx) {
  const q = ctx.query;
  const ret = peekReturn() || "";
  let email = q.get("email") || "";
  const notes = [];
  if (q.get("error")) notes.push(alert("err", "Not signed in", GOOGLE_ERRORS[q.get("error")] || "The sign-in did not work. Try again."));
  if (ret.startsWith("/device")) notes.push(alert("info", "Sign in to the Taro app", "Sign in here first. Then you approve the app's sign-in."));

  const body = h("div", { class: "auth-body" });
  const card = container({ title: "Sign in", desc: "With your email address. We send you a code; there is no password." }, body);
  ctx.inner.replaceChildren(authCard(...notes, card));

  function devNote(withCode) {
    if (!isDev()) return null;
    const show = withCode ? button("Show the code", { kind: "link sm", id: "dev-show-code", onclick: async (e) => {
      await run(e.currentTarget, "Looking…", async () => {
        const mails = await api("GET", "/api/v1/dev/outbox");
        const m = (mails || []).find((x) => x.to === email && /code: (\d{6})/.test(x.body));
        if (!m) { flash("warn", "No code for " + email + " in the dev outbox yet."); return; }
        const code = m.body.match(/code: (\d{6})/)[1];
        const inp = document.getElementById("code");
        if (inp) { inp.value = code; inp.focus(); }
        announce("Code filled in.");
      });
    } }) : null;
    return h("div", { class: "dev-note" }, icon("info"),
      h("span", {}, h("strong", {}, "Development: "), "no mail is sent. Sign-in codes are in the dev outbox (", h("code", {}, "GET /api/v1/dev/outbox"), ")."), show);
  }

  function stepEmail() {
    const f = field({ label: "Email address", name: "email", type: "email", value: email, autocomplete: "email", inputmode: "email", id: "email", spellcheck: "false" });
    const submit = button("Continue", { kind: "primary", type: "submit", id: "email-continue" });
    const form = h("form", { class: "auth-form", novalidate: true }, f, h("div", { class: "form-actions" }, submit));
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const v = f.input.value.trim().toLowerCase();
      if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(v)) { f.setError("Type a valid email address, like name@example.com."); f.input.focus(); return; }
      const r = await run(submit, "Sending…", () => api("POST", "/api/v1/auth/email/start", { email: v }), f);
      if (r) { email = v; stepCode(); }
    });
    const google = S.providers.google
      ? [h("div", { class: "or" }, h("span", {}, "or")),
        h("a", { class: "btn wide", href: "/api/v1/auth/google/start?return_to=" + encodeURIComponent(ret.startsWith("/") ? ret : "/"), "data-external": "" }, icon("google"), "Continue with Google")]
      : [];
    body.replaceChildren(form, ...google, devNote(false));
    f.input.focus();
  }

  function stepCode() {
    const f = field({ label: "Sign-in code", name: "code", id: "code", inputmode: "numeric", autocomplete: "one-time-code", maxlength: 6, inputClass: "mono code-input",
      desc: h("span", {}, "We sent a 6-digit code to ", h("strong", {}, email), ". It works once, for 15 minutes.") });
    const submit = button("Sign in", { kind: "primary", type: "submit", id: "code-submit" });
    const back = button("Use another address", { kind: "link", onclick: () => stepEmail() });
    const again = button("Send a new code", { kind: "link", onclick: async (e) => {
      const r = await run(e.currentTarget, "Sending…", () => api("POST", "/api/v1/auth/email/start", { email }), f);
      if (r) ok("A new code is on its way.");
    } });
    const form = h("form", { class: "auth-form", novalidate: true }, f, h("div", { class: "form-actions" }, h("span", { class: "left" }, back), again, submit));
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const code = f.input.value.replace(/\s+/g, "");
      if (!/^\d{6}$/.test(code)) { f.setError("The code has 6 digits."); f.input.focus(); return; }
      const r = await run(submit, "Signing in…", () => api("POST", "/api/v1/auth/email/verify", { email, code }, { allow401: true }), f);
      if (r) await afterSignIn();
    });
    body.replaceChildren(form, devNote(true));
    f.input.focus();
  }

  stepEmail();
}

/** The link in the sign-in mail: /auth/verify?token=… */
export async function pageVerifyLink(ctx) {
  const token = ctx.query.get("token");
  if (!token) {
    ctx.inner.replaceChildren(authCard(alert("err", "This link is not complete", "Open the link from the mail again, or sign in with the code."), h("a", { class: "btn primary", href: "/sign-in" }, "Sign in")));
    return;
  }
  ctx.inner.replaceChildren(authCard(container({ title: "Signing you in" }, loading("Checking the link…"))));
  try {
    await api("POST", "/api/v1/auth/email/verify", { token }, { allow401: true });
    // the token must not stay in the address bar or the history
    history.replaceState(null, "", "/");
    await afterSignIn();
  } catch (e) {
    if (!ctx.alive()) return;
    ctx.inner.replaceChildren(authCard(alert("err", "The link did not work", e.message), h("a", { class: "btn primary", href: "/sign-in" }, "Sign in with a code")));
  }
}

/** /device?code=BCDF-GHJK: approve (or deny) the Taro app's sign-in. */
export async function pageDevice(ctx) {
  const pre = (ctx.query.get("code") || "").toUpperCase();
  if (!S.me) {
    const back = location.pathname + location.search;
    if (ctx.query.get("via") === "google" && S.providers.google) {
      location.replace("/api/v1/auth/google/start?return_to=" + encodeURIComponent(back));
      return;
    }
    saveReturn(back);
    navigate("/sign-in", { replace: true });
    return;
  }
  const f = field({ label: "Code shown in the app", name: "user_code", id: "device-code", value: pre, maxlength: 9, autocomplete: "off", spellcheck: "false",
    inputClass: "mono code-input", desc: "8 letters, like BCDF-GHJK.", placeholder: "BCDF-GHJK" });
  const look = button("Continue", { kind: "primary", type: "submit", id: "device-continue" });
  const form = h("form", { novalidate: true }, f, h("div", { class: "form-actions" }, look));
  const out = h("div", { id: "device-request" });
  ctx.inner.replaceChildren(authCard(container({ title: "Sign in to the Taro app", desc: "Approve only a sign-in you started yourself, in the Taro app on your computer." }, form, out)));

  const norm = (v) => { const c = v.replace(/[^a-z]/gi, "").toUpperCase(); return c.length === 8 ? c.slice(0, 4) + "-" + c.slice(4) : null; };

  async function lookUp() {
    out.replaceChildren();
    const code = norm(f.input.value);
    if (!code) { f.setError("The code has 8 letters, like BCDF-GHJK."); f.input.focus(); return; }
    f.input.value = code;
    const r = await run(look, "Checking…", () => api("GET", "/api/v1/devices/pending?user_code=" + encodeURIComponent(code)), f);
    if (!r || !ctx.alive()) return;
    show(r.request, code);
  }

  function show(req, code) {
    form.hidden = true;
    const approve = button("Approve", { kind: "primary", id: "device-approve" });
    const deny = button("Deny", { kind: "danger", id: "device-deny" });
    const decide = async (btn, which) => {
      const r = await run(btn, which === "approve" ? "Approving…" : "Denying…", () => api("POST", "/api/v1/devices/" + which, { user_code: code }));
      if (!r) return;
      out.replaceChildren(which === "approve"
        ? alert("ok", "Approved", "You can go back to the app. ", h("span", { class: "muted" }, req.client + " is now signed in as " + S.me.email + ". You can sign it out any time under Account."))
        : alert("info", "Denied", "The app was not signed in. If you did not start this, nothing else is needed."));
      announce(which === "approve" ? "Approved. You can go back to the app." : "Denied.");
      out.append(h("div", { class: "form-actions" }, h("a", { class: "btn", href: "/account" }, "Signed-in apps"), h("a", { class: "btn link", href: "/" }, "My Macs")));
    };
    approve.addEventListener("click", () => decide(approve, "approve"));
    deny.addEventListener("click", () => decide(deny, "deny"));
    out.replaceChildren(
      h("div", { class: "device-ask" },
        h("p", { class: "device-line" }, h("strong", {}, req.client), " wants to sign in to your account",
          req.ip ? [" from ", h("span", { class: "mono" }, req.ip)] : null, ", ", h("time", { datetime: req.created_at, title: fmtDateTime(req.created_at) }, fmtRel(req.created_at)), "."),
        kv([["Account", S.me.email], ["Code", h("span", { class: "mono" }, code)]], 3),
        alert("warn", null, "If you did not start this sign-in, choose Deny.")),
      h("div", { class: "form-actions" }, deny, approve));
    approve.focus();
  }

  form.addEventListener("submit", (e) => { e.preventDefault(); lookUp(); });
  if (norm(pre)) await lookUp(); else f.input.focus();
}

export async function signOut({ keepReturn = false } = {}) {
  try { await api("POST", "/api/v1/auth/sign-out", undefined, { allow401: true }); } catch (e) { if (e.status !== 401) { report(e); return; } }
  S.me = null; S.csrf = ""; S.org = null; S.perms = new Set(); S.unread = 0;
  if (!keepReturn) takeReturn();
  flash("info", "You are signed out.");
  navigate("/sign-in", { replace: true });
}
