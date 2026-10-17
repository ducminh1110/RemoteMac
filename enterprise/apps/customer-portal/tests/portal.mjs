// Drives the Customer Portal in headless Chromium against a running development server.
// Not part of `cargo test` (needs Node and Playwright). To run it:
//
//   cd enterprise
//   TARO_APP_DOWNLOAD_URL=https://example.com/Acme-Setup.exe setsid scripts/dev-server.sh portal2 8580 > /tmp/portal2.log 2>&1 &
//   PW_DIR=/path/with/node_modules/playwright-core CHROME=/path/to/chrome \
//     node apps/customer-portal/tests/portal.mjs http://127.0.0.1:8580 http://127.0.0.1:8581 /tmp/portal-shots
//
// An operator (ops@example.test, on the console API) assigns a Mac to a new customer. The test
// signs that customer in, sees the Mac become Ready, connects (a taro:// navigation), checks the
// download link, approves and denies a Taro app sign-in, revokes a session and an app, signs out.
// Throughout: no console errors, no CSP violations, no horizontal scroll at 390 px.
import { createRequire } from "node:module";
import { mkdirSync } from "node:fs";

const require = createRequire((process.env.PW_DIR || process.cwd()) + "/");
const { chromium } = require("playwright-core");

const BASE = (process.argv[2] || "http://127.0.0.1:8580").replace(/\/$/, "");
const OPS = (process.argv[3] || "http://127.0.0.1:8581").replace(/\/$/, "");
const SHOTS = process.argv[4] || "./portal-shots";
const CHROME = process.env.CHROME || "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
mkdirSync(SHOTS, { recursive: true });
const EMAIL = `alice-${Date.now().toString(36)}@example.test`;

let pass = 0, total = 0;
function check(name, ok, detail) {
  total++; if (ok) pass++;
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${!ok && detail !== undefined ? "  -- " + detail : ""}`);
}
async function step(name, fn) { try { await fn(); } catch (e) { check(name + " (threw)", false, (e && e.stack) || e); } }

// ---- a small API client with a cookie jar (the operator, and the "Taro app")
async function jar(base) {
  let cookie = "", csrf = "";
  const call = async (method, path, body) => {
    const res = await fetch(base + path, { method, redirect: "manual", headers: { Origin: base, Cookie: cookie, "X-CSRF-Token": csrf, ...(body ? { "content-type": "application/json" } : {}) }, body: body ? JSON.stringify(body) : undefined });
    for (const c of res.headers.getSetCookie()) cookie = (cookie ? cookie + "; " : "") + c.split(";")[0];
    const t = await res.text(); let j = null; try { j = JSON.parse(t); } catch (_) { /* not json */ }
    return { status: res.status, body: j };
  };
  return { call, setCsrf: (t) => { csrf = t; } };
}
async function code(base, email) {
  for (let i = 0; i < 20; i++) {
    const r = await (await fetch(base + "/api/v1/dev/outbox")).json();
    const m = r.find((x) => x.to === email && /type this code: (\d{6})/.test(x.body));
    if (m) return m.body.match(/type this code: (\d{6})/)[1];
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("no code for " + email);
}

const BAD = [];
const CHECK_SCROLL = `document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1`;
const browser = await chromium.launch({ executablePath: CHROME, args: ["--no-sandbox"] });
async function newPage(opt = {}) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, ...opt });
  const page = await ctx.newPage();
  // (the 401 of the signed-out GET /api/v1/me at boot is expected)
  page.on("console", (m) => { if (m.type() === "error" && !(/\/api\/v1\/me$/.test(m.location().url || "") && /401/.test(m.text()))) BAD.push("console: " + m.text()); });
  page.on("pageerror", (e) => BAD.push("pageerror: " + e.message));
  return page;
}
const shot = (p, name) => p.screenshot({ path: `${SHOTS}/${name}.png`, fullPage: true });
const settle = (p) => p.waitForLoadState("networkidle").catch(() => {});

// ---- the provider assigns a Mac
let rentalId = "";
await step("operator assigns a Mac", async () => {
  const ops = await jar(OPS);
  await ops.call("POST", "/api/v1/auth/email/start", { email: "ops@example.test" });
  const v = await ops.call("POST", "/api/v1/auth/email/verify", { email: "ops@example.test", code: await code(OPS, "ops@example.test") });
  const me = await ops.call("GET", "/api/v1/me"); ops.setCsrf(me.body.csrf_token);
  const plans = await ops.call("GET", "/api/v1/ops/plans");
  const a = await ops.call("POST", "/api/v1/ops/assignments", { email: EMAIL, plan_id: plans.body.plans[0].id });
  rentalId = a.body && a.body.rental && a.body.rental.id;
  check("operator: assignment created", v.status === 200 && a.status < 300 && rentalId, JSON.stringify(a.body));
});

const P = await newPage();
const wanted = [];
await P.addInitScript(() => { window.addEventListener("securitypolicyviolation", (e) => console.error("CSP violation: " + e.violatedDirective)); });

await step("sign in page", async () => {
  await P.goto(BASE + "/"); await settle(P);
  check("signed out: sent to /sign-in", new URL(P.url()).pathname === "/sign-in", P.url());
  check("no Google button (not configured)", (await P.locator("text=Continue with Google").count()) === 0);
  check("sign-in has no horizontal scroll", await P.evaluate(CHECK_SCROLL));
  await shot(P, "signin-light");
  await P.emulateMedia({ colorScheme: "dark" }); await shot(P, "signin-dark"); await P.emulateMedia({ colorScheme: "light" });
});

await step("sign in with an email code", async () => {
  await P.fill("#email", "not-an-email"); await P.click("#email-continue");
  check("bad email: inline error", (await P.locator(".error:visible").count()) > 0);
  await P.fill("#email", EMAIL); await P.click("#email-continue");
  await P.waitForSelector("#code");
  await P.fill("#code", await code(BASE, EMAIL)); await P.click("#code-submit");
  await P.waitForSelector("h1:has-text('My Macs')");
  check("signed in: My Macs", new URL(P.url()).pathname === "/");
  check("top bar: only My Macs and Account", (await P.locator(".topmenu a").allInnerTexts()).join() === "My Macs,Account");
});

await step("the assigned Mac", async () => {
  const connectSel = `#connect-${rentalId}`;
  await P.waitForSelector(connectSel);
  check("the Mac shows", (await P.locator(".mac-card").count()) === 1);
  await P.waitForFunction((s) => !document.querySelector(s).disabled, connectSel, { timeout: 40000 });
  check("it becomes Ready and Connect is enabled", (await P.locator(".mac-state").innerText()).includes("Ready"));
  const href = await P.getAttribute("#download-app", "href");
  check("Download the app uses app_download_url", href === "https://example.com/Acme-Setup.exe", href);
  check("no other pages in nav (no billing/rent)", (await P.locator("a[href='/billing'], a[href='/rent']").count()) === 0);
  check("hint hidden before Connect", await P.locator("#connect-hint").isHidden());
  await shot(P, "macs-light");
  await P.emulateMedia({ colorScheme: "dark" }); await shot(P, "macs-dark"); await P.emulateMedia({ colorScheme: "light" });
  // Connect: the page asks the browser to open a taro:// link
  const cdp = await P.context().newCDPSession(P);
  await cdp.send("Page.enable");
  const nav = [];
  cdp.on("Page.frameRequestedNavigation", (e) => nav.push(e.url));
  const grant = P.waitForResponse((r) => r.url().includes("/connect-grant"));
  await P.click(connectSel);
  const g = await (await grant).json();
  await P.waitForTimeout(500);
  check("Connect: grant returns a taro link", /^taro:\/\//.test(g.taro_link), g.taro_link);
  check("Connect: browser navigates to the taro:// link", nav.some((u) => u.startsWith("taro://")), nav.join());
  check("Connect: 'Taro did not open? Download the app' hint shown", await P.locator("#connect-hint").isVisible() && (await P.locator("#connect-hint").innerText()).includes("Download the app"));
});

await step("device approval", async () => {
  const app = await jar(BASE);
  const a = await app.call("POST", "/api/v1/devices/authorize", { client: "Taro 1.4.1 on Windows" });
  check("app: authorize gives a user code", a.status === 201 && /^[A-Z]{4}-[A-Z]{4}$/.test(a.body.user_code), JSON.stringify(a.body));
  await P.goto(a.body.verification_uri_complete.replace(/^https?:\/\/[^/]+/, BASE)); await settle(P);
  await P.waitForSelector("#device-approve");
  check("device page names the app and account", (await P.locator("#device-request").innerText()).includes("Taro 1.4.1 on Windows") && (await P.locator("#device-request").innerText()).includes(EMAIL));
  await shot(P, "device-light");
  await P.emulateMedia({ colorScheme: "dark" }); await shot(P, "device-dark"); await P.emulateMedia({ colorScheme: "light" });
  await P.click("#device-approve");
  await P.waitForSelector("#device-request .alert.ok", { timeout: 8000 }).catch(async () => { console.log("DEBUG", await P.locator("body").innerText()); });
  check("approved: 'You can go back to the app.'", (await P.locator("#device-request").innerText()).includes("You can go back to the app."));
  const tok = await app.call("POST", "/api/v1/devices/token", { device_code: a.body.device_code });
  check("app: token issued after approval", tok.status === 200 && !!tok.body.access_token, JSON.stringify(tok.body));
  const b = await app.call("POST", "/api/v1/devices/authorize", { client: "Taro 1.4.1 on macOS" });
  await P.goto(BASE + "/device?code=" + b.body.user_code); await settle(P);
  await P.click("#device-deny"); await P.waitForSelector("#device-request .alert.info");
  check("denied: 'Denied'", (await P.locator("#device-request").innerText()).includes("Denied"));
  const bt = await app.call("POST", "/api/v1/devices/token", { device_code: b.body.device_code });
  check("app: denied token is not issued", bt.status !== 200, JSON.stringify(bt.body));
  // signed out + via=google without Google configured: goes to the normal sign-in
  const Q = await newPage();
  const c = await app.call("POST", "/api/v1/devices/authorize", { client: "Taro", via: "google" });
  await Q.goto(BASE + "/device?code=" + c.body.user_code + "&via=google"); await settle(Q);
  check("device signed out: sign-in first", new URL(Q.url()).pathname === "/sign-in", Q.url());
  await Q.context().close();
});

await step("account", async () => {
  // a second browser session to revoke
  const Q = await newPage();
  await Q.goto(BASE + "/sign-in"); await Q.fill("#email", EMAIL); await Q.click("#email-continue"); await Q.waitForSelector("#code");
  await Q.fill("#code", await code(BASE, EMAIL)); await Q.click("#code-submit"); await Q.waitForSelector("h1:has-text('My Macs')");
  await P.goto(BASE + "/account"); await settle(P);
  await P.waitForSelector("#revoke-others:not([disabled])");
  check("account shows email", (await P.locator("main").innerText()).includes(EMAIL));
  check("signed-in app listed", (await P.locator("[id^=revoke-app-]").count()) >= 1);
  await shot(P, "account-light");
  await P.click("[id^=revoke-app-] >> nth=0"); await P.click("dialog button.danger, [role=dialog] button.danger");
  await P.waitForFunction(() => document.querySelectorAll("[id^=revoke-app-]").length === 0);
  check("revoke app: list empty", true);
  await P.click("[id^=revoke-session-], [id^=revoke-]:not(#revoke-others):not([id^=revoke-app-]) >> nth=0"); await P.click("dialog button.danger, [role=dialog] button.danger");
  await P.waitForFunction(() => document.querySelectorAll("table tbody tr").length === 1);
  check("revoke session: other browser session gone", true);
  await Q.reload(); await settle(Q);
  check("revoked browser is signed out", new URL(Q.url()).pathname === "/sign-in", Q.url());
  await Q.context().close();
});

await step("phone layout", async () => {
  const M = await newPage({ viewport: { width: 390, height: 800 } });
  await M.goto(BASE + "/sign-in"); await M.fill("#email", EMAIL); await M.click("#email-continue"); await M.waitForSelector("#code");
  await M.fill("#code", await code(BASE, EMAIL)); await M.click("#code-submit"); await M.waitForSelector(".mac-card");
  check("390px: My Macs has no horizontal scroll", await M.evaluate(CHECK_SCROLL));
  await shot(M, "macs-390");
  await M.goto(BASE + "/account"); await settle(M);
  check("390px: Account has no horizontal scroll", await M.evaluate(CHECK_SCROLL));
  await M.goto(BASE + "/sign-in"); await settle(M);
  await M.click("#sign-out").catch(() => {});
  await M.context().close();
});

await step("sign out", async () => {
  await P.goto(BASE + "/"); await settle(P);
  await P.click("#sign-out"); await P.waitForURL(/\/sign-in/);
  const me = await fetch(BASE + "/api/v1/me");
  check("sign out: back at sign-in", true);
  await P.goto(BASE + "/"); await settle(P);
  check("signed out: / redirects to sign-in", new URL(P.url()).pathname === "/sign-in");
  await shot(P, "signin-after-signout");
});

check("no console errors or CSP violations", BAD.length === 0, BAD.join(" | "));
await browser.close();
console.log(`\n${pass}/${total} checks passed`);
process.exit(pass === total ? 0 : 1);
