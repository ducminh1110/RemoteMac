// End-to-end check of the Provider Console (Taro Operations) in headless Chromium, against the
// real API with the mock provider. Not part of `cargo test`.
//
//   cd enterprise
//   scripts/dev-server.sh console2 8680 &       # portal on 8680, console on 8681 (mock Macs)
//   PW_DIR=/path/with/node_modules/playwright-core CHROME=/path/to/chrome \
//     node apps/provider-console/tests/console.mjs [screenshot dir]
//
// It signs in as ops@example.test (the bootstrap platform admin; the code is read from the dev
// outbox), checks the overview, assigns a Mac to a new email address, waits for it to be ready,
// ends it, opens every page of the navigation, and checks throughout that there are no console
// errors, no CSP violations and no horizontal page scroll at 390 px. Then it photographs the
// Overview and Assignments in light, dark and phone width.
import { createRequire } from "node:module";
import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";

const require = createRequire((process.env.PW_DIR || process.cwd()) + "/");
const { chromium } = require("playwright-core");

const SHOTS = process.argv[2] || process.env.SHOTS || "./console-shots";
const CONSOLE = process.env.CONSOLE_URL || "http://127.0.0.1:8681";
mkdirSync(SHOTS, { recursive: true });

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: !!ok });
  console.log((ok ? "PASS " : "FAIL ") + name + (detail ? " - " + String(detail).slice(0, 200) : ""));
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
// expected by design: the boot's /me while signed out (401), and ending a Mac still being prepared (409)
const EXPECTED = [/\/api\/v1\/me$/, /\/api\/v1\/ops\/assignments\/[0-9a-f-]+\/end$/];

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
    consoleErrors.push(m.text() + (url ? " @ " + url : ""));
  });
  page.on("pageerror", (e) => consoleErrors.push("pageerror: " + e.message));
  return { ctx, page };
}

async function settle(page) {
  await page.waitForFunction(() => !document.querySelector("#page > .loading"), null, { timeout: 15000 });
  await page.waitForTimeout(150);
}
async function nav(page, path) {
  await page.evaluate((p) => { history.pushState(null, "", p); dispatchEvent(new PopStateEvent("popstate")); }, path);
  await settle(page);
}
const h1 = (page) => page.$eval("h1", (e) => e.textContent.trim()).catch(() => "");
const noHScroll = (page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1);
const shot = (page, name) => page.screenshot({ path: resolve(SHOTS, name + ".png"), fullPage: false });

async function signIn(page, email) {
  await page.goto(CONSOLE + "/");
  await page.waitForSelector("#email-continue");
  await page.fill("input[name=email]", email);
  await page.click("#email-continue");
  await page.waitForSelector("#code-submit");
  await page.fill("input[name=code]", await outboxCode(email));
  await page.click("#code-submit");
  await page.waitForSelector("#sidenav a[data-nav=\"/\"]");
  await settle(page);
}

const PAGES = [
  ["/", "Overview"], ["/assignments", "Assignments"], ["/organizations", "Organizations"], ["/domains", "Domains"],
  ["/macs", "Mac fleet"], ["/provisioning-jobs", "Provisioning jobs"], ["/providers", "Providers"],
  ["/relay-nodes", "Relay nodes"], ["/plans", "Plans"], ["/plans/new", "Create plan"], ["/branding", "Branding"],
  ["/branding/new", "New branding version"], ["/incidents", "Incidents"], ["/feature-flags", "Feature flags"],
  ["/access", "Access"], ["/audit", "Audit log"], ["/account", "Account"],
];

const email = `customer-${Date.now().toString(36)}@example.test`;
const rowOf = (page) => page.$(`#assignments-grid tbody tr:has-text("${email}")`);
const statusOf = async (page) => { const r = await rowOf(page); return r ? (await r.innerText()).replace(/\s+/g, " ") : ""; };
async function waitStatus(page, re, ms) {
  const end = Date.now() + ms;
  let t = "";
  while (Date.now() < end) { t = await statusOf(page); if (re.test(t)) return t; await sleep(500); }
  return t;
}

try {
  const { ctx: actx, page } = await newPage({ width: 1440, height: 900 });
  await page.goto(CONSOLE + "/");
  await page.waitForSelector("#email-continue");
  check("signed out: sign-in page", (await h1(page)) === "Sign in");
  await signIn(page, "ops@example.test");
  check("sign-in reaches the overview", (await h1(page)) === "Overview");
  const kpis = await page.$eval("#kpis", (e) => e.innerText);
  check("overview: KPIs from the API", /Active rentals/.test(kpis) && /Macs available/.test(kpis) && /Open incidents/.test(kpis), kpis.replace(/\s+/g, " "));
  check("overview: no billing KPIs", !/Stripe|Billing needs/.test(kpis));

  const navLinks = await page.$$eval("#sidenav a[data-nav]", (as) => as.map((a) => [a.getAttribute("href"), a.textContent.trim()]));
  check("nav: Assignments is right after Overview", navLinks[0][0] === "/" && navLinks[1][0] === "/assignments", JSON.stringify(navLinks.slice(0, 3)));
  check("nav: no billing pages", !navLinks.some(([href, label]) => /billing/.test(href) || /Stripe|Needs attention/.test(label)));

  // ---- assign a Mac
  await page.click('#sidenav a[data-nav="/assignments"]');
  await page.waitForSelector("#assign-form");
  await settle(page);
  check("assignments: page renders with a form and the API note", (await h1(page)) === "Assignments" && /TARO_OPS_API_KEY/.test(await page.$eval("#page", (e) => e.innerText)));
  const planOptions = await page.$$eval("#assign-plan option", (o) => o.length);
  check("assignments: the plan select lists active plans", planOptions > 0, planOptions);
  await page.fill("#assign-email", "not-an-email");
  await page.click("#assign-submit");
  check("assignments: an invalid email is refused in the form", /email address/.test(await page.$eval("#assign-form", (e) => e.innerText)));
  await page.fill("#assign-email", email);
  await page.click("#assign-submit");
  check("assignments: assigning flashes success", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
  let t = await waitStatus(page, new RegExp(email), 8000);
  check("assignments: the new row appears", t.includes(email), t);
  t = await waitStatus(page, /Ready/, 60000);
  check("assignments: the row becomes ready by polling (no reload)", /Ready/.test(t), t);
  check("assignments: the ready row has a Mac name", /Mac|mock|\w+-\d+/i.test(t), t);

  // ---- end it
  await page.evaluate(() => document.querySelectorAll(".flash").forEach((f) => f.remove()));
  await (await rowOf(page)).$('button:has-text("End")').then((b) => b.click());
  await page.waitForSelector(".modal-back .modal");
  check("assignments: End asks for confirmation", /End this assignment/.test(await page.$eval(".modal", (e) => e.innerText)));
  await page.click('.modal-back .m-foot button:has-text("End assignment")');
  await page.waitForSelector(".modal-back", { state: "detached" });
  check("assignments: ending flashes success", !!(await page.waitForSelector(".flash.ok", { timeout: 10000 }).catch(() => null)));
  t = await waitStatus(page, /Cancelled|Expired|Released|Ended/i, 60000);
  check("assignments: the assignment ends", /Cancelled|Expired|Released|Ended/i.test(t) && !/End\b.*End\b/.test(t), t);
  check("assignments: an ended row has no End button", !(await (await rowOf(page)).$('button:has-text("End")')));

  // ---- a second assignment, ended while still being prepared: the 409 shows in the flash bar
  const email2 = `second-${Date.now().toString(36)}@example.test`;
  await page.fill("#assign-email", email2);
  await page.click("#assign-submit");
  await page.waitForSelector(`#assignments-grid tbody tr:has-text("${email2}")`, { timeout: 10000 });
  const row2 = await page.$(`#assignments-grid tbody tr:has-text("${email2}")`);
  const early = /Preparing|Paid|Provisioning/i.test(await row2.innerText());
  if (early) {
    await (await row2.$('button:has-text("End")')).click();
    await page.click('.modal-back .m-foot button:has-text("End assignment")');
    const f = await page.waitForSelector(".flash.err", { timeout: 10000 }).catch(() => null);
    check("assignments: ending a Mac that is being prepared shows the 409 message", f && /being prepared/.test(await f.innerText()));
  } else {
    check("assignments: ending a Mac that is being prepared shows the 409 message (Mac was already ready; skipped)", true);
  }

  // ---- every page renders
  for (const [path, title] of PAGES) {
    await nav(page, path);
    const got = await h1(page);
    check(`page ${path} renders "${title}"`, got === title, got);
  }
  const loaded = async () => !/could not be loaded/.test(await page.$eval("#page", (e) => e.innerText));
  await nav(page, "/organizations");
  const orgHref = await page.$eval('#page a[href^="/organizations/"]', (a) => a.getAttribute("href")).catch(() => null);
  if (orgHref) { await nav(page, orgHref); check("organization detail renders", (await h1(page)) !== "" && await loaded(), await h1(page)); }
  await nav(page, "/macs");
  const macHref = await page.$eval('#page a[href^="/macs/"]', (a) => a.getAttribute("href")).catch(() => null);
  if (macHref) { await nav(page, macHref); check("Mac detail renders", (await h1(page)) !== "" && await loaded(), await h1(page)); }
  await nav(page, "/plans");
  const planHref = await page.$eval('#page a[href^="/plans/new?from="]', (a) => a.getAttribute("href")).catch(() => null);
  if (planHref) { await nav(page, planHref); check("new plan version renders", /New version of/.test(await h1(page)), await h1(page)); }
  for (const path of ["/billing", "/billing/events"]) {
    await nav(page, path);
    check(`${path} is no longer a page`, /does not exist/.test(await h1(page)));
  }
  check("no CSP violations", (await page.evaluate(() => window.__csp)).length === 0, await page.evaluate(() => window.__csp.join("; ")));

  // ---- photographs: light 1440, dark 1440, phone 390
  await nav(page, "/"); await shot(page, "overview-light");
  await nav(page, "/assignments"); await shot(page, "assignments-light");
  const { ctx: dctx, page: dark } = await newPage({ width: 1440, height: 900 }, "dark");
  await dark.context().addCookies(await actx.cookies());
  await dark.goto(CONSOLE + "/"); await settle(dark);
  await shot(dark, "overview-dark");
  await nav(dark, "/assignments"); await shot(dark, "assignments-dark");
  const { ctx: pctx, page: phone } = await newPage({ width: 390, height: 844 });
  await phone.context().addCookies(await actx.cookies());
  let ok = true;
  for (const [path, name] of [["/", "overview"], ["/assignments", "assignments"], ["/plans", "plans"], ["/macs", "macs"], ["/organizations", "organizations"]]) {
    await phone.goto(CONSOLE + path); await settle(phone);
    ok = (await noHScroll(phone)) && ok;
    if (name === "overview" || name === "assignments") await shot(phone, name + "-390");
  }
  check("390 px: no horizontal page scroll (overview, assignments, plans, macs, organizations)", ok);
  check("no console errors", consoleErrors.length === 0, consoleErrors.join(" | "));
  await dctx.close(); await pctx.close(); await actx.close();
} catch (e) {
  check("test run completed", false, e && e.stack);
} finally {
  await browser.close();
}
const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);
