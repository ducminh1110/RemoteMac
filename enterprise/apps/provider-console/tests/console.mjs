// End-to-end check of the Provider Console (Taro Operations) in headless Chromium, against the
// real API. Not part of `cargo test`.
//
//   cd enterprise
//   # the mock provider, the console on its own port (portal 8680, console 8681):
//   scripts/dev-server.sh console2 8680 &
//   CONSOLE_URL=http://127.0.0.1:8681 PORTAL_URL=http://127.0.0.1:8680 node apps/provider-console/tests/console.mjs [shots]
//   # one site with Macs that enroll themselves (portal at /, console at /console/):
//   SINGLE=1 FLEET=1 scripts/dev-server.sh ui 8090 &
//   CONSOLE_URL=http://127.0.0.1:8090/console PORTAL_URL=http://127.0.0.1:8090 node apps/provider-console/tests/console.mjs [shots]
//
//   PW_DIR=/path/with/node_modules/playwright-core CHROME=/path/to/chrome (or PLAYWRIGHT_BROWSERS_PATH)
//
// The first administrator: on a server that has none yet, the first-run page makes ADMIN_EMAIL
// with ADMIN_PASSWORD and the setup code (FIRST=setup, the default), or FIRST=code signs in the
// bootstrap administrator (ops@example.test, ADMIN_EMAIL then) with an email code from the dev
// outbox and gives it its password on the Account page. Later runs sign in with the password
// (the API allows 10 password sign-ins per address in 10 minutes: restart the dev server, or
// wait, to run this more than twice in a row).
// Then: password sign-in (a wrong password first), password change (and back), and either
// - fleet (TARO_PROVIDER=fleet): Add a Mac, a Mac enrolled with that code and kept online by
//   heartbeats from this script, the list refreshing by itself, the Mac chosen in Assignments,
//   Control of the assigned Mac (confirmed; the customer's connect-grant then answers 409),
//   Release (the customer can connect again), ending the assignment, Remove; or
// - mock: assigning a Mac to a new email address, waiting for it to be ready, ending it.
// Every page of the navigation opens, every link of the console stays under the console's base,
// and throughout there are no console errors, no CSP violations and no horizontal page scroll at
// 390 px. Photographs go to the shots directory.
import { createRequire } from "node:module";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";

const require = createRequire((process.env.PW_DIR || process.cwd()) + "/");
const { chromium } = require("playwright-core");

const SHOTS = process.argv[2] || process.env.SHOTS || "./console-shots";
const CONSOLE = (process.env.CONSOLE_URL || "http://127.0.0.1:8681").replace(/\/+$/, "");
const PORTAL = (process.env.PORTAL_URL || "http://127.0.0.1:8680").replace(/\/+$/, "");
const BASE = new URL(CONSOLE + "/").pathname; // "/" or "/console/"
const ADMIN_EMAIL = process.env.ADMIN_EMAIL || "admin@example.test";
const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD || "correct horse battery staple";
const SETUP_CODE = process.env.SETUP_CODE || "ABCD-EFGH-JKLM";
const FIRST = process.env.FIRST || "setup";
mkdirSync(SHOTS, { recursive: true });

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: !!ok });
  console.log((ok ? "PASS " : "FAIL ") + name + (detail ? " - " + String(detail).slice(0, 240) : ""));
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const rand = Date.now().toString(36);

async function outboxCode(email) {
  for (let i = 0; i < 40; i++) {
    const mails = await (await fetch(CONSOLE + "/api/v1/dev/outbox")).json();
    const m = mails.find((x) => x.to === email);
    if (m) { const c = /code: (\d{6})/.exec(m.body) || /(\d{6})/.exec(m.subject); if (c) return c[1]; }
    await sleep(150);
  }
  throw new Error("no sign-in code for " + email);
}

const browser = await chromium.launch({ executablePath: process.env.CHROME || undefined, headless: true });
const consoleErrors = [];
// expected by design: the boot's /me while signed out (401), ending a Mac still being prepared
// (409), the wrong setup code and wrong passwords (422), and opening taro:// without the app
const EXPECTED = [/\/api\/v1\/me$/, /\/api\/v1\/rentals\/[0-9a-f-]+\/connect-grant$/, /\/api\/v1\/ops\/assignments\/[0-9a-f-]+\/end$/, /\/api\/v1\/setup\/admin$/, /\/api\/v1\/auth\/password(\/change)?$/];

async function newPage(viewport, colorScheme = "light") {
  const ctx = await browser.newContext({ viewport, colorScheme });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    window.__csp = [];
    document.addEventListener("securitypolicyviolation", (e) => window.__csp.push(e.violatedDirective + " " + e.blockedURI));
  });
  page.on("console", (m) => {
    if (m.type() !== "error") return;
    const url = (m.location() && m.location().url) || "";
    if (/Failed to load resource/.test(m.text()) && EXPECTED.some((re) => re.test(url))) return;
    if (/Failed to launch 'taro:/.test(m.text())) return;
    consoleErrors.push(m.text() + (url ? " @ " + url : ""));
  });
  page.on("pageerror", (e) => consoleErrors.push("pageerror: " + e.message));
  return { ctx, page };
}

async function settle(page) {
  await page.waitForFunction(() => !document.querySelector("#page > .loading"), null, { timeout: 15000 });
  await page.waitForTimeout(150);
}
/** A route of the console ("/macs") as a URL. */
const at = (route) => CONSOLE + route;
async function nav(page, route) {
  await page.evaluate((p) => { history.pushState(null, "", p); dispatchEvent(new PopStateEvent("popstate")); }, BASE + route.replace(/^\//, ""));
  await settle(page);
}
const h1 = (page) => page.$eval("h1", (e) => e.textContent.trim()).catch(() => "");
const text = (page, sel) => page.$eval(sel, (e) => e.innerText).catch(() => "");
const noHScroll = (page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1);
const shot = (page, name) => page.screenshot({ path: resolve(SHOTS, name + ".png"), fullPage: false });
const clearFlash = (page) => page.evaluate(() => document.querySelectorAll(".flash").forEach((f) => f.remove()));
/** Links of the page on this origin that leave the console's base (they would break under /console/). */
const strayLinks = (page) => page.$$eval("a[href]", (as, base) => as.map((a) => a.href)
  .filter((u) => u.startsWith(location.origin) && !new URL(u).pathname.startsWith(base) && new URL(u).pathname + "/" !== base), BASE);

async function signInPassword(page, email, password) {
  await page.goto(at("/"));
  await page.waitForSelector("#password-submit");
  await page.fill("#pw-email", email);
  await page.fill("#pw-password", password);
  await page.click("#password-submit");
  await page.waitForSelector("#sidenav a[data-nav=\"/\"]");
  await settle(page);
}
async function signOut(page) {
  await nav(page, "/account");
  await page.click("#account-sign-out");
  await page.waitForSelector("#password-submit, #setup-form");
}
async function waitText(page, sel, re, ms) {
  const end = Date.now() + ms;
  let t = "";
  while (Date.now() < end) { t = (await text(page, sel)).replace(/\s+/g, " "); if (re.test(t)) return t; await sleep(400); }
  return t;
}

const PAGES = [
  ["/", "Overview"], ["/assignments", "Assignments"], ["/organizations", "Organizations"], ["/domains", "Domains"],
  ["/macs", "Mac fleet"], ["/provisioning-jobs", "Provisioning jobs"], ["/providers", "Providers"],
  ["/relay-nodes", "Relay nodes"], ["/plans", "Plans"], ["/plans/new", "Create plan"], ["/branding", "Branding"],
  ["/branding/new", "New branding version"], ["/incidents", "Incidents"], ["/feature-flags", "Feature flags"],
  ["/access", "Access"], ["/audit", "Audit log"], ["/account", "Account"],
];

// ---------------------------------------------------------------- the customer (portal API from its own page)

async function customer(email) {
  const { ctx, page } = await newPage({ width: 1200, height: 800 });
  await page.goto(PORTAL + "/");
  const call = (method, path, body) => page.evaluate(async ([method, path, body]) => {
    const me = window.__me;
    const headers = { "Content-Type": "application/json", Accept: "application/json" };
    if (me && me.csrf_token) headers["X-CSRF-Token"] = me.csrf_token;
    const r = await fetch(path, { method, headers, credentials: "same-origin", body: body === undefined ? undefined : JSON.stringify(body) });
    let j = null; try { j = await r.json(); } catch (_) { /* empty */ }
    return { status: r.status, body: j };
  }, [method, path, body]);
  await call("POST", "/api/v1/auth/email/start", { email });
  const code = await outboxCode(email);
  const v = await call("POST", "/api/v1/auth/email/verify", { email, code });
  const me = await call("GET", "/api/v1/me");
  await page.evaluate((m) => { window.__me = m; }, me.body);
  return { ctx, page, call, signedIn: v.status < 300 && me.status === 200 };
}

// ---------------------------------------------------------------- the run

try {
  const { ctx: actx, page } = await newPage({ width: 1440, height: 900 });
  const status0 = await (await fetch(CONSOLE + "/api/v1/setup/status")).json();
  await page.goto(at("/"));
  await page.waitForSelector("#setup-form, #password-submit");
  check("the console's own files load under " + BASE, await page.evaluate(() => [...document.querySelectorAll("link[rel=stylesheet], script[src]")].every((e) => new URL(e.href || e.src).pathname.startsWith(document.querySelector("base") ? new URL(document.baseURI).pathname : "/"))));

  // ---- the first administrator
  if (status0.needs_admin) {
    check("first visit: the setup page instead of sign-in", /^Set up /.test(await h1(page)), await h1(page));
    check("setup: the code's hint names the command", /sudo taro-relay portal setup-code/.test(await text(page, "#setup-form")));
    if (FIRST === "setup") {
      await page.fill("#setup-code", "WRNG-CODE-2345");
      await page.fill("#setup-name", "Ada Admin");
      await page.fill("#setup-email", ADMIN_EMAIL);
      await page.fill("#setup-password", "short");
      await page.fill("#setup-password2", "short");
      await page.click("#setup-submit");
      check("setup: a short password is refused in the form (the rule shown)", /at least 12/.test(await text(page, "#setup-form")));
      await page.fill("#setup-password", ADMIN_PASSWORD);
      await page.fill("#setup-password2", ADMIN_PASSWORD + "x");
      await page.click("#setup-submit");
      check("setup: passwords that differ are refused", /not the same/.test(await text(page, "#setup-form")));
      await page.fill("#setup-password2", ADMIN_PASSWORD);
      await page.click("#setup-submit");
      const err = await waitText(page, "#setup-code-err", /setup code is wrong/i, 8000);
      check("setup: a wrong code shows the server's 422 message at the code field", /setup code is wrong/i.test(err), err);
      await page.fill("#setup-code", SETUP_CODE.toLowerCase());
      await shot(page, "setup");
      await page.click("#setup-submit");
      await page.waitForSelector("#sidenav a[data-nav=\"/\"]", { timeout: 15000 });
      await settle(page);
      check("setup: the new administrator is signed in on the overview", (await h1(page)) === "Overview", await h1(page));
      const me = await page.evaluate(async () => (await fetch("api/v1/me")).json());
      check("setup: the administrator is a platform admin with the name given", (me.operator_roles || []).includes("platform_admin") && me.display_name === "Ada Admin", JSON.stringify(me.operator_roles));
    } else {
      // the bootstrap administrator signs in with an email code and sets a password
      await page.click("#setup-sign-in");
      await page.waitForSelector("#use-code");
      await page.click("#use-code");
      await page.waitForSelector("#email-continue");
      await page.fill("input[name=email]", ADMIN_EMAIL);
      await page.click("#email-continue");
      await page.waitForSelector("#code-submit");
      await page.fill("input[name=code]", await outboxCode(ADMIN_EMAIL));
      await page.click("#code-submit");
      await page.waitForSelector("#sidenav a[data-nav=\"/\"]");
      await settle(page);
      check("bootstrap admin: the email code reaches the overview", (await h1(page)) === "Overview", await h1(page));
      await nav(page, "/account");
      await page.fill("#pw-new", ADMIN_PASSWORD);
      await page.fill("#pw-new2", ADMIN_PASSWORD);
      await page.click("#change-password");
      check("bootstrap admin: sets a password with the current one left empty", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
      await nav(page, "/");
    }
    const after = await (await fetch(CONSOLE + "/api/v1/setup/status")).json();
    check("the setup page closes once an administrator exists", after.needs_admin === false);
  } else {
    check("an administrator exists: the sign-in page (password first)", (await h1(page)) === "Sign in" && !!(await page.$("#use-code")));
    await signInPassword(page, ADMIN_EMAIL, ADMIN_PASSWORD);
  }
  check("overview after sign-in", (await h1(page)) === "Overview");
  if (status0.mail === false) check("overview: the short note about the mail server", /mail server/.test(await text(page, "#mail-note")));
  const kpis = await text(page, "#kpis");
  check("overview: KPIs from the API", /Active rentals/.test(kpis) && /Macs available/.test(kpis) && /Open incidents/.test(kpis), kpis.replace(/\s+/g, " "));
  check("overview: no billing KPIs", !/Stripe|Billing needs/.test(kpis));
  const navLinks = await page.$$eval("#sidenav a[data-nav]", (as) => as.map((a) => [a.dataset.nav, a.textContent.trim(), a.href]));
  check("nav: Assignments is right after Overview", navLinks[0][0] === "/" && navLinks[1][0] === "/assignments", JSON.stringify(navLinks.slice(0, 3)));
  check("nav: no billing pages", !navLinks.some(([r, label]) => /billing/.test(r) || /Stripe|Needs attention/.test(label)));
  check("nav: every link is under the console's base", navLinks.every(([r, , href]) => href === CONSOLE + "/" + r.slice(1) || (r === "/" && href === CONSOLE + "/")), navLinks.map((x) => x[2]).join(" "));

  // ---- password sign-in
  await signOut(page);
  check("signed out: the password form is the sign-in", (await h1(page)) === "Sign in" && !!(await page.$("#password-submit")));
  check("sign-in: the email code is offered as the other way", /email code instead/.test(await text(page, "#page")));
  await shot(page, "sign-in");
  await page.fill("#pw-email", ADMIN_EMAIL);
  await page.fill("#pw-password", "not the password at all");
  await page.click("#password-submit");
  const pwErr = await waitText(page, "#password-form", /wrong/, 8000);
  check("sign-in: a wrong password shows the server's message", /email address or the password is wrong/.test(pwErr), pwErr);
  await page.fill("#pw-password", ADMIN_PASSWORD);
  await page.click("#password-submit");
  await page.waitForSelector("#sidenav a[data-nav=\"/\"]");
  await settle(page);
  check("sign-in: the password reaches the overview", (await h1(page)) === "Overview");

  // ---- changing the password (and back)
  const NEW_PASSWORD = ADMIN_PASSWORD + " 2";
  await nav(page, "/account");
  check("account: the Change password form", !!(await page.$("#password-change-form")));
  await page.fill("#pw-current", "this is the wrong one");
  await page.fill("#pw-new", NEW_PASSWORD);
  await page.fill("#pw-new2", NEW_PASSWORD);
  await page.click("#change-password");
  const curErr = await waitText(page, "#password-change-form", /wrong/, 8000);
  check("password change: a wrong current password is refused at its field", /current password is wrong/.test(curErr), curErr);
  await page.fill("#pw-current", ADMIN_PASSWORD);
  await page.fill("#pw-new2", NEW_PASSWORD + "x");
  await page.click("#change-password");
  check("password change: a confirmation that differs is refused", /not the same/.test(await text(page, "#password-change-form")));
  await page.fill("#pw-new2", NEW_PASSWORD);
  await clearFlash(page);
  await page.click("#change-password");
  check("password change: done", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
  await settle(page);
  await shot(page, "account");
  await signOut(page);
  await signInPassword(page, ADMIN_EMAIL, NEW_PASSWORD);
  check("password change: the new password signs in", (await h1(page)) === "Overview");
  await nav(page, "/account");
  await page.fill("#pw-current", NEW_PASSWORD);
  await page.fill("#pw-new", ADMIN_PASSWORD);
  await page.fill("#pw-new2", ADMIN_PASSWORD);
  await clearFlash(page);
  await page.click("#change-password");
  check("password change: and back", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));

  // ---- Macs
  const fleet = await page.evaluate(async () => (await fetch("api/v1/ops/fleet")).json());
  if (fleet.provider === "fleet") await fleetFlow(page, fleet);
  else await mockFlow(page);

  // ---- every page renders, from a deep link too, and keeps its links under the base
  let stray = [];
  for (const [route, title] of PAGES) {
    await nav(page, route);
    const got = await h1(page);
    check(`page ${route} renders "${title}"`, got === title, got);
    stray = stray.concat((await strayLinks(page)).map((u) => route + " -> " + u));
  }
  check("no link leaves the console's base", stray.length === 0, stray.join(" "));
  await page.goto(at("/macs"));
  await settle(page);
  check("a deep link (reload on /macs) opens the page", (await h1(page)) === "Mac fleet");
  await page.click("#sidenav a[data-nav=\"/assignments\"]");
  await settle(page);
  check("a click in the navigation goes to the page under the base", page.url() === at("/assignments") && (await h1(page)) === "Assignments", page.url());
  await page.click("#brand");
  await settle(page);
  check("the brand link goes to the overview", page.url() === CONSOLE + "/" && (await h1(page)) === "Overview", page.url());
  const loaded = async () => !/could not be loaded/.test(await text(page, "#page"));
  await nav(page, "/organizations");
  const orgHref = await page.$eval('#page a[href^="organizations/"]', (a) => a.getAttribute("href")).catch(() => null);
  if (orgHref) {
    await page.click(`#page a[href="${orgHref}"]`); await settle(page);
    check("organization detail renders (link clicked)", (await h1(page)) !== "" && await loaded() && page.url() === CONSOLE + "/" + orgHref, page.url());
  }
  await nav(page, "/plans");
  const planHref = await page.$eval('#page a[href^="plans/new?from="]', (a) => a.getAttribute("href")).catch(() => null);
  if (planHref) { await nav(page, "/" + planHref); check("new plan version renders", /New version of/.test(await h1(page)), await h1(page)); }
  for (const route of ["/billing", "/billing/events"]) {
    await nav(page, route);
    check(`${route} is no longer a page`, /does not exist/.test(await h1(page)));
  }
  check("no CSP violations", (await page.evaluate(() => window.__csp)).length === 0, await page.evaluate(() => window.__csp.join("; ")));

  // ---- photographs: light 1440, dark 1440, phone 390
  await nav(page, "/"); await shot(page, "overview-light");
  await nav(page, "/assignments"); await shot(page, "assignments-light");
  const { ctx: dctx, page: dark } = await newPage({ width: 1440, height: 900 }, "dark");
  await dark.context().addCookies(await actx.cookies());
  await dark.goto(at("/")); await settle(dark);
  await shot(dark, "overview-dark");
  await nav(dark, "/macs"); await shot(dark, "macs-dark");
  await nav(dark, "/assignments"); await shot(dark, "assignments-dark");
  const { ctx: pctx, page: phone } = await newPage({ width: 390, height: 844 });
  await phone.context().addCookies(await actx.cookies());
  let ok = true;
  for (const [route, name] of [["/", "overview"], ["/assignments", "assignments"], ["/plans", "plans"], ["/macs", "macs"], ["/organizations", "organizations"], ["/account", "account"]]) {
    await phone.goto(at(route)); await settle(phone);
    ok = (await noHScroll(phone)) && ok;
    if (["overview", "assignments", "macs"].includes(name)) await shot(phone, name + "-390");
  }
  check("390 px: no horizontal page scroll (overview, assignments, plans, macs, organizations, account)", ok);
  const { ctx: sctx, page: signed } = await newPage({ width: 390, height: 844 });
  await signed.goto(at("/")); await signed.waitForSelector("#password-submit");
  check("390 px: the sign-in page fits", await noHScroll(signed));
  await shot(signed, "sign-in-390");
  check("no console errors", consoleErrors.length === 0, consoleErrors.join(" | "));
  await sctx.close(); await dctx.close(); await pctx.close(); await actx.close();
} catch (e) {
  check("test run completed", false, e && e.stack);
} finally {
  await browser.close();
}
const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);

// ---------------------------------------------------------------- Macs that enroll themselves

async function fleetFlow(page, fleet0) {
  const MAC = `Test Mac ${rand}`;
  const TARO_ID = "rm-" + String(Date.now()).slice(-9);
  await nav(page, "/macs");
  check("Macs: the fleet list (provider fleet)", (await h1(page)) === "Mac fleet" && !!(await page.$("#fleet-stats")), await h1(page));

  // Add a Mac: the code, its expiry, the server and the steps
  await page.click("#add-mac");
  await page.waitForSelector("#add-mac-dialog");
  const code = (await text(page, "#enroll-code")).trim();
  const dlg = await text(page, "#add-mac-dialog");
  check("Add a Mac: a code in the dialog", /^[A-Z2-9]{4}-[A-Z2-9]{4}-[A-Z2-9]{4}$/.test(code), code);
  check("Add a Mac: the server address", (await text(page, "#enroll-server")).trim() === (fleet0.enroll.server || PORTAL), await text(page, "#enroll-server"));
  check("Add a Mac: its expiry and the three steps", /works once, until/.test(dlg) && /Install .* for Mac \(Taro\.app\)/.test(dlg) && /Screen Recording and Accessibility/.test(dlg) && /Enter this code/.test(dlg), dlg.replace(/\s+/g, " "));
  check("Add a Mac: a copy button for the code", !!(await page.$('#add-mac-dialog button[aria-label^="Copy code"]')));
  await page.waitForTimeout(400); // (the dialog fades in)
  await shot(page, "add-a-mac");
  await page.click("#add-mac-dialog .m-foot .btn.primary");
  await page.waitForSelector("#add-mac-dialog", { state: "detached" });

  // the Mac enrolls with it and keeps a heartbeat
  const agent = (path, body, token) => fetch(PORTAL + "/api/v1/agent/" + path, { method: "POST",
    headers: { "Content-Type": "application/json", ...(token ? { Authorization: "Bearer " + token } : {}) }, body: JSON.stringify(body) });
  const enr = await agent("enroll", { code, name: MAC, model: "Mac mini (2023)", chip: "Apple M2 Pro", cpu_cores: 10, memory_gb: 32, storage_gb: 512, macos: "macOS 15.1", version: "1.6.0" });
  const { token } = await enr.json();
  check("the Mac enrolls with the code", enr.status === 201 && !!token, enr.status);
  check("the code works once", (await agent("enroll", { code, name: "Again" })).status === 422);
  let epoch = 0;
  const beat = async () => {
    const r = await agent("heartbeat", { taro_id: TARO_ID, applied_epoch: epoch, version: "1.6.0" }, token);
    if (r.ok) epoch = (await r.json()).epoch;
    return r.status;
  };
  await beat();
  const hb = setInterval(() => beat().catch(() => {}), 2000);
  try {
    const row = `#fleet-grid tbody tr:has-text("${MAC}")`;
    const t = await waitText(page, row, /Online/, 25000);
    check("Macs: the enrolled Mac shows up online without a reload (10 s refresh)", /Online/.test(t) && /Last seen/.test(t), t);
    check("Macs: name, chip and model, macOS, the ID at the relay", t.includes(MAC) && /M2 Pro/.test(t) && /Mac mini/.test(t) && /macOS 15\.1/.test(t) && t.includes(TARO_ID), t);
    const mac = (await page.evaluate(async () => (await fetch("api/v1/ops/fleet")).json())).macs.find((m) => m.display_name === MAC);
    await waitText(page, row, /Available/, 20000);
    await shot(page, "macs-online");

    // Assignments: choose this Mac
    const email = `fleet-${rand}@example.test`;
    await nav(page, "/assignments");
    let opts = [];
    for (let i = 0; i < 20; i++) {
      opts = await page.$$eval("#assign-mac option", (o) => o.map((x) => [x.value, x.textContent]));
      if (opts.some(([v]) => v === mac.id)) break;
      await sleep(1000); await nav(page, "/assignments");
    }
    check("Assignments: the Mac select, 'Any free Mac of the plan' by default", opts.length && opts[0][0] === "" && /Any free Mac of the plan/.test(opts[0][1]), JSON.stringify(opts[0]));
    check("Assignments: the free Mac is listed", opts.some(([v, l]) => v === mac.id && l.includes(MAC)), JSON.stringify(opts));
    await page.selectOption("#assign-mac", mac.id);
    await page.fill("#assign-email", email);
    await shot(page, "assign-mac-select");
    await page.click("#assign-submit");
    check("Assignments: assigning flashes success", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
    const arow = `#assignments-grid tbody tr:has-text("${email}")`;
    const ready = await waitText(page, arow, /Ready/, 60000);
    check("Assignments: the chosen Mac becomes ready", /Ready/.test(ready) && ready.includes(MAC), ready);
    const opts2 = await page.$$eval("#assign-mac option", (o) => o.map((x) => x.value));
    check("Assignments: the Mac is no longer offered once assigned", !opts2.includes(mac.id));
    const rental = (await page.evaluate(async () => (await fetch("api/v1/ops/assignments")).json())).assignments.find((a) => a.email === email);

    // the customer can connect
    const cust = await customer(email);
    check("customer: signed in to the portal", cust.signedIn);
    let g = await cust.call("POST", `/api/v1/rentals/${rental.rental_id}/connect-grant`, {});
    check("customer: a connect-grant before the take-over", g.status === 201, g.status + " " + JSON.stringify(g.body));

    // Control: confirm, the customer is out (409), the note with the Taro link
    await nav(page, "/macs");
    await waitText(page, row, new RegExp(email.replace(/[.]/g, "\\.")), 15000);
    check("Macs: the assigned customer and organization in the row", (await text(page, row)).includes(email));
    await page.click(`#control-${mac.id}`);
    await page.waitForSelector(".modal-back .modal");
    const confirmText = (await text(page, ".modal-back .modal")).replace(/\s+/g, " ");
    check("Control: an assigned Mac asks first", confirmText.includes(`${email} is using this Mac. Taking control disconnects them, and they cannot connect until you release it.`), confirmText);
    await page.waitForTimeout(400); // (the dialog fades in)
    await shot(page, "control-confirm");
    await page.click('.modal-back .m-foot button:has-text("Take control")');
    await page.waitForSelector("#open-in-taro", { timeout: 10000 });
    const link = await page.$eval("#open-in-taro", (a) => a.getAttribute("href"));
    check("Control: the note with Open in Taro (the taro:// link)", /^taro:\/\/connect\?server=.+&grant=/.test(link), link);
    check("Control: the page stays (the link is opened, no navigation away)", page.url().startsWith(at("/macs")), page.url());
    await page.waitForTimeout(300);
    const ctl = await waitText(page, row, /Controlled by/, 8000);
    check("Macs: the 'Controlled by <email> since' badge", ctl.includes(`Controlled by ${ADMIN_EMAIL} (you) since`), ctl);
    await shot(page, "control-note");
    g = await cust.call("POST", `/api/v1/rentals/${rental.rental_id}/connect-grant`, {});
    check("customer: connect-grant answers 409 while the administrator controls the Mac", g.status === 409 && /administrator is working on this Mac/.test(JSON.stringify(g.body)), g.status + " " + JSON.stringify(g.body));
    await nav(page, "/assignments");
    const opts3 = await page.$$eval("#assign-mac option", (o) => o.map((x) => x.value));
    check("Assignments: a controlled Mac is not offered", !opts3.includes(mac.id));

    // Release
    await nav(page, "/macs");
    await clearFlash(page);
    await page.click(`#release-${mac.id}`);
    await page.waitForSelector(".modal-back .modal");
    check("Release: asks with 'Hand it back'", !!(await page.$('.modal-back .m-foot button:has-text("Hand it back")')), await text(page, ".modal-back .modal"));
    await page.click('.modal-back .m-foot button:has-text("Hand it back")');
    check("Release: flashes success", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
    const rel = await waitText(page, row, /^(?!.*Controlled by)/, 8000);
    check("Release: the badge is gone", !/Controlled by/.test(rel), rel);
    g = await cust.call("POST", `/api/v1/rentals/${rental.rental_id}/connect-grant`, {});
    check("customer: can connect again after the release", g.status === 201, g.status + " " + JSON.stringify(g.body));
    await cust.ctx.close();

    // end the assignment, then remove the Mac
    await nav(page, "/assignments");
    await page.click(`${arow} button:has-text("End")`);
    await page.click('.modal-back .m-foot button:has-text("End assignment")');
    const ended = await waitText(page, arow, /Cancelled|Expired|Ended/i, 60000);
    check("Assignments: the assignment ends", /Cancelled|Expired|Ended/i.test(ended), ended);
    await nav(page, "/macs");
    let removable = false;
    for (let i = 0; i < 30 && !removable; i++) { removable = !!(await page.$(`#remove-${mac.id}`)); if (!removable) { await sleep(1000); await nav(page, "/macs"); } }
    check("Macs: Remove is offered once the Mac is unassigned", removable);
    if (removable) {
      await page.click(`#remove-${mac.id}`);
      await page.waitForSelector(".modal-back .modal");
      check("Remove: asks first", /leaves the fleet/.test(await text(page, ".modal-back .modal")));
      await page.click('.modal-back .m-foot button:has-text("Remove Mac")');
      await page.waitForSelector(row, { state: "detached", timeout: 10000 }).catch(() => null);
      check("Remove: the Mac leaves the list", !(await page.$(row)));
    }
  } finally {
    clearInterval(hb);
  }
  check("a removed Mac's heartbeat is refused (401)", (await beat()) === 401);
}

// ---------------------------------------------------------------- the mock provider: assign, ready, end

async function mockFlow(page) {
  const email = `customer-${rand}@example.test`;
  const rowOf = () => page.$(`#assignments-grid tbody tr:has-text("${email}")`);
  const statusOf = async () => { const r = await rowOf(); return r ? (await r.innerText()).replace(/\s+/g, " ") : ""; };
  async function waitStatus(re, ms) {
    const end = Date.now() + ms;
    let t = "";
    while (Date.now() < end) { t = await statusOf(); if (re.test(t)) return t; await sleep(500); }
    return t;
  }
  await page.click('#sidenav a[data-nav="/assignments"]');
  await page.waitForSelector("#assign-form");
  await settle(page);
  check("assignments: page renders with a form and the API note", (await h1(page)) === "Assignments" && /TARO_OPS_API_KEY/.test(await text(page, "#page")));
  check("assignments: no Mac select without the fleet provider", !(await page.$("#assign-mac")));
  const planOptions = await page.$$eval("#assign-plan option", (o) => o.length);
  check("assignments: the plan select lists active plans", planOptions > 0, planOptions);
  await page.fill("#assign-email", "not-an-email");
  await page.click("#assign-submit");
  check("assignments: an invalid email is refused in the form", /email address/.test(await text(page, "#assign-form")));
  await page.fill("#assign-email", email);
  await page.click("#assign-submit");
  check("assignments: assigning flashes success", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
  let t = await waitStatus(new RegExp(email), 8000);
  check("assignments: the new row appears", t.includes(email), t);
  t = await waitStatus(/Ready/, 60000);
  check("assignments: the row becomes ready by polling (no reload)", /Ready/.test(t), t);
  check("assignments: the ready row has a Mac name", /Mac|mock|\w+-\d+/i.test(t), t);

  await clearFlash(page);
  await (await rowOf()).$('button:has-text("End")').then((b) => b.click());
  await page.waitForSelector(".modal-back .modal");
  check("assignments: End asks for confirmation", /End this assignment/.test(await text(page, ".modal")));
  await page.click('.modal-back .m-foot button:has-text("End assignment")');
  await page.waitForSelector(".modal-back", { state: "detached" });
  check("assignments: ending flashes success", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
  t = await waitStatus(/Cancelled|Expired|Released|Ended/i, 60000);
  check("assignments: the assignment ends", /Cancelled|Expired|Released|Ended/i.test(t), t);
  check("assignments: an ended row has no End button", !(await (await rowOf()).$('button:has-text("End")')));

  // a second assignment, ended while still being prepared: the 409 shows in the flash bar
  const email2 = `second-${rand}@example.test`;
  await page.fill("#assign-email", email2);
  await page.click("#assign-submit");
  await page.waitForSelector(`#assignments-grid tbody tr:has-text("${email2}")`, { timeout: 10000 });
  const row2 = await page.$(`#assignments-grid tbody tr:has-text("${email2}")`);
  if (/Preparing|Paid|Provisioning/i.test(await row2.innerText())) {
    await (await row2.$('button:has-text("End")')).click();
    await page.click('.modal-back .m-foot button:has-text("End assignment")');
    const f = await page.waitForSelector(".flash.err", { timeout: 10000 }).catch(() => null);
    check("assignments: ending a Mac that is being prepared shows the 409 message", f && /being prepared/.test(await f.innerText()));
  } else {
    check("assignments: ending a Mac that is being prepared shows the 409 message (Mac was already ready; skipped)", true);
  }

  // clean up: end every test assignment so that the mock Macs are free for the next run
  for (let i = 0; i < 6; i++) {
    const row = await page.$('#assignments-grid tbody tr:has-text("@example.test"):has(button:has-text("End"))');
    if (!row) break;
    if (/Ready/.test(await row.innerText())) {
      await (await row.$('button:has-text("End")')).click();
      await page.click('.modal-back .m-foot button:has-text("End assignment")');
      await page.waitForSelector(".modal-back", { state: "detached" });
    }
    await sleep(1500);
  }
  await page.waitForTimeout(500);
  check("assignments: test assignments are all ended", !(await page.$('#assignments-grid tbody tr:has(button:has-text("End"))')));
  await nav(page, "/macs");
  const macHref = await page.$eval('#page a[href^="macs/"]', (a) => a.getAttribute("href")).catch(() => null);
  if (macHref) { await nav(page, "/" + macHref); check("Mac detail renders", (await h1(page)) !== "" && !/could not be loaded/.test(await text(page, "#page")), await h1(page)); }
}
