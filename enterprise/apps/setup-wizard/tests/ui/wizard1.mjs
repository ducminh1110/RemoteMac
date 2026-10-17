// Drives the real wizard UI in headless Chromium against the fake GitHub of tests/ui_demo.rs.
// Not part of `cargo test` (it needs Node and Playwright). To run it:
//
//   cd enterprise
//   UI_DEMO_SLOW=1 cargo test -p taro-enterprise-setup --test ui_demo -- --ignored --nocapture &
//   # wait for the UI_DEMO_URL=... line, then:
//   PW_DIR=/path/with/node_modules/playwright-core CHROME=/path/to/chrome \
//     node apps/setup-wizard/tests/ui/wizard1.mjs "$UI_DEMO_URL" /tmp/wizard1-shots
//
// It clicks presets, types valid and invalid hex codes, opens Customize, picks from the grid,
// drags the shade area, cancels (value restored), applies, checks that the preview uses the
// server's rm-brand tokens, runs the whole flow to the export, and checks that there are no
// console errors, no CSP violations and no horizontal scroll at 390 px. Then it photographs every
// step at 1440x900 in light and dark mode, the open Customize picker, and the phone layout.
import { createRequire } from "node:module";
import { mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const require = createRequire((process.env.PW_DIR || process.cwd()) + "/");
const { chromium } = require("playwright-core");

const URL_ = process.argv[2];
const SHOTS = process.argv[3] || "./wizard1-shots";
if (!URL_) { console.error("usage: node wizard1.mjs <wizard url> [screenshot dir]"); process.exit(2); }
mkdirSync(SHOTS, { recursive: true });

// ---- a real PNG (solid colour with a diagonal), so the thumbnails render
function crc32(buf) {
  let c, crc = 0xffffffff;
  for (let n = 0; n < buf.length; n++) {
    c = (crc ^ buf[n]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function png(w, hgt, [r, g, b]) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(hgt, 4); ihdr[8] = 8; ihdr[9] = 6;
  const raw = Buffer.alloc((w * 4 + 1) * hgt);
  for (let y = 0; y < hgt; y++) {
    raw[y * (w * 4 + 1)] = 0;
    for (let x = 0; x < w; x++) {
      const o = y * (w * 4 + 1) + 1 + x * 4;
      const edge = Math.abs(x - y * (w / hgt)) < 3;
      raw[o] = edge ? 255 : r; raw[o + 1] = edge ? 255 : g; raw[o + 2] = edge ? 255 : b; raw[o + 3] = 255;
    }
  }
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: !!ok, detail: detail === undefined ? "" : String(detail) });
  console.log((ok ? "PASS " : "FAIL ") + name + (detail !== undefined ? " - " + detail : ""));
}

const browser = await chromium.launch({ executablePath: process.env.CHROME || undefined, headless: true });
const consoleErrors = [];
const pages = [];
async function newPage(viewport, colorScheme = "light") {
  const ctx = await browser.newContext({ viewport, colorScheme, acceptDownloads: true });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    window.__csp = [];
    document.addEventListener("securitypolicyviolation", (e) => window.__csp.push(e.violatedDirective + " " + e.blockedURI));
  });
  page.on("console", (m) => { if (m.type() === "error") consoleErrors.push(m.text()); });
  page.on("pageerror", (e) => consoleErrors.push("pageerror: " + e.message));
  pages.push(page);
  return page;
}
const shot = (page, name) => page.screenshot({ path: `${SHOTS}/${name}.png` });
const apiState = (page) => page.evaluate(async () => {
  const t = new URL(location.href).searchParams.get("t");
  return (await (await fetch("/api/state", { headers: { "X-Setup-Token": t } })).json()).state;
});
const serverTokens = (page, hex) => page.evaluate(async (hex) => {
  const t = new URL(location.href).searchParams.get("t");
  const r = await fetch("/api/branding/tokens", { method: "POST", headers: { "X-Setup-Token": t, "Content-Type": "application/json" }, body: JSON.stringify({ color: hex }) });
  return (await r.json()).tokens;
}, hex);
const mockVar = (page, variant, v) => page.evaluate(([variant, v]) => getComputedStyle(document.querySelector(`.mock[data-variant=${variant}]`)).getPropertyValue(v).trim(), [variant, v]);
const step = async (page, n) => { await page.locator("#steps li button").nth(n).click(); await page.waitForTimeout(120); };
async function waitPreview(page, hex) {
  const t = await serverTokens(page, hex);
  await page.waitForFunction(([p, pd]) => {
    const l = document.querySelector(".mock[data-variant=light]"), d = document.querySelector(".mock[data-variant=dark]");
    return l && d && getComputedStyle(l).getPropertyValue("--primary").trim() === p && getComputedStyle(d).getPropertyValue("--primary").trim() === pd;
  }, [t.primary, t.primary_dark], { timeout: 5000 });
  return t;
}
const checked = (page) => page.$$eval("#palette [role=radio][aria-checked=true]", (els) => els.map((e) => e.querySelector(".sw-name").textContent));
const panelOpen = (page) => page.evaluate(() => !document.querySelector(".modal-back:has(#customize-panel)").hidden);
const flashText = async (page, kind) => { await page.waitForSelector(`.flash.${kind}`); return page.locator(`.flash.${kind}`).last().innerText(); };
const clearFlashes = (page) => page.evaluate(() => document.querySelectorAll(".flash").forEach((f) => f.remove()));

const page = await newPage({ width: 1440, height: 900 });
await page.goto(URL_);
await page.waitForSelector("#steps li button");

// 1. preflight
await page.click("#run-preflight");
await page.waitForSelector(".table tbody tr");
check("preflight shows checks", (await page.locator(".table tbody tr").count()) >= 4);

// 2. identity (an invalid ID first: the field error comes from rm-brand)
await step(page, 1);
await page.fill("#f-product-name", "Acme Connect");
check("provider ID suggested from the name", (await page.inputValue("#f-provider-id")) === "acme-connect");
await page.fill("#f-provider-id", "Acme Cloud");
await page.fill("#f-product-short-name", "Acme");
await page.fill("#f-product-vendor", "Acme Cloud Ltd");
await page.fill("#f-product-support-url", "https://acme.example/help");
await page.click("#save-identity");
await page.waitForSelector("#f-provider-id-err");
check("field error for the provider ID", (await page.textContent("#f-provider-id-err")).includes("lower-case"));
check("invalid field marked", (await page.getAttribute("#f-provider-id", "aria-invalid")) === "true");
await shot(page, "light-02-identity-field-error");
await page.fill("#f-provider-id", "acme-cloud");
await page.click("#save-identity");
check("identity saved", (await flashText(page, "ok")).includes("saved"));
await clearFlashes(page);

// 3. branding: presets
await step(page, 2);
await page.waitForSelector("#palette");
check("14 preset swatches", (await page.locator("#palette [role=radio]").count()) === 14);
check("the default (Taro's colour) selects no preset", (await checked(page)).length === 0, await page.inputValue("#hex"));
await page.click("#sw-blue");
check("Blue selected", JSON.stringify(await checked(page)) === JSON.stringify(["Blue"]));
check("hex field follows the preset", (await page.inputValue("#hex")) === "#2563EB");
const tk = await waitPreview(page, "#2563EB");
check("preview uses the server's tokens", (await mockVar(page, "light", "--primary-hover")) === tk.primary_hover && (await mockVar(page, "dark", "--on-primary")) === tk.on_primary_dark, tk.primary_hover);
check("the wizard's own accent is untouched", (await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--primary").trim())) === "#7C4AE8");
await page.focus("#sw-blue");
await page.keyboard.press("ArrowRight");
check("arrow key moves to Indigo", JSON.stringify(await checked(page)) === JSON.stringify(["Indigo"]) && (await page.evaluate(() => document.activeElement.id)) === "sw-indigo");
await page.keyboard.press("ArrowLeft");
await page.keyboard.press("ArrowLeft");
check("arrow keys wrap (Slate)", JSON.stringify(await checked(page)) === JSON.stringify(["Slate"]));
await page.click("#sw-teal");
await waitPreview(page, "#0D9488");
await page.setInputFiles("#file-logo", { name: "logo.png", mimeType: "image/png", buffer: png(240, 80, [13, 148, 136]) });
await page.waitForSelector("img.thumb");
await page.setInputFiles("#file-icon", { name: "icon.png", mimeType: "image/png", buffer: png(128, 128, [13, 148, 136]) });
await page.waitForFunction(() => document.querySelectorAll("img.thumb").length === 2);
await page.waitForSelector(".m-icon.has-img img");
await clearFlashes(page);
await page.setInputFiles("#file-icon", { name: "wide.png", mimeType: "image/png", buffer: png(200, 100, [0, 0, 0]) });
check("a non-square icon is refused", (await flashText(page, "err")).includes("square"));
await clearFlashes(page);

// invalid hex: message, last valid colour kept, typed text kept
await page.fill("#hex", "#12G");
await page.waitForSelector("#hex-msg.error");
check("invalid hex: inline message", (await page.textContent("#hex-msg")).includes("6-digit"));
check("invalid hex: aria-invalid", (await page.getAttribute("#hex", "aria-invalid")) === "true");
check("invalid hex: typed text kept", (await page.inputValue("#hex")) === "#12G");
check("invalid hex: colour kept (Teal)", JSON.stringify(await checked(page)) === JSON.stringify(["Teal"]) && (await mockVar(page, "light", "--primary")) === "#0D9488");
await page.fill("#hex", "#12345G");
check("non-hex characters named", (await page.textContent("#hex-msg")).includes("0-9 and A-F"));
await page.fill("#hex", "#fff");
check("3-digit code refused", (await page.textContent("#hex-msg")).includes("Three-digit"));
await shot(page, "light-03-branding-invalid-hex");

// a custom colour: no swatch selected, preview follows, saved unchanged
await page.fill("#hex", "3d7a5b");
await waitPreview(page, "#3D7A5B");
check("custom colour: no swatch snapped", (await checked(page)).length === 0);
check("lower case without # accepted", (await page.getAttribute("#hex", "aria-invalid")) === "false");
await page.waitForTimeout(600);
check("custom colour saved by the server", (await apiState(page)).branding.primary_color === "#3D7A5B");

// Customize: grid, shade drag, Cancel restores
await page.click("#customize");
await page.waitForFunction(() => !document.querySelector(".modal-back:has(#customize-panel)").hidden);
check("Customize opens a modal", (await page.getAttribute("#customize-panel", "aria-modal")) === "true" && await page.evaluate(() => document.getElementById("layout").inert));
check("focus moves into the picker", await page.evaluate(() => document.getElementById("customize-panel").contains(document.activeElement)));
check("before shows the current colour", (await page.textContent(".ba-item:first-child .mono")) === "#3D7A5B");
await page.locator(".g-cell").nth(13 * 4 + 9).click();
const gridHex = await page.inputValue("#p-hex");
check("grid pick shown as after", (await page.textContent(".ba-item:nth-child(3) .mono")) === gridHex, gridHex);
const sv = await page.locator("#sv").boundingBox();
await page.mouse.move(sv.x + sv.width * 0.2, sv.y + sv.height * 0.2);
await page.mouse.down();
await page.mouse.move(sv.x + sv.width * 0.5, sv.y + sv.height * 0.4, { steps: 5 });
await page.mouse.move(sv.x + sv.width * 0.85, sv.y + sv.height * 0.3, { steps: 5 });
await page.mouse.up();
const dragHex = await page.inputValue("#p-hex");
check("dragging the shade area changes the colour", dragHex !== gridHex, gridHex + " -> " + dragHex);
await page.focus("#sv");
await page.keyboard.press("Shift+ArrowLeft");
check("shade area keyboard operable", (await page.inputValue("#p-hex")) !== dragHex);
await page.fill("#hue", "200");
await page.dispatchEvent("#hue", "input");
check("hue slider changes the colour", (await page.inputValue("#p-hex")) !== dragHex);
check("main field untouched while the picker is open", (await page.inputValue("#hex")) === "#3D7A5B");
await page.keyboard.press("Tab");
check("Tab stays in the picker", await page.evaluate(() => document.getElementById("customize-panel").contains(document.activeElement)));
await shot(page, "light-customize-picker");
await page.click("#p-cancel");
check("Cancel closes", !(await panelOpen(page)));
check("Cancel restores exactly", (await page.inputValue("#hex")) === "#3D7A5B" && (await mockVar(page, "light", "--primary")) === "#3D7A5B");
check("focus returns to Customize", (await page.evaluate(() => document.activeElement.id)) === "customize");
await page.click("#customize");
await page.locator(".g-cell").nth(3).click();
await page.keyboard.press("Escape");
check("Escape cancels", !(await panelOpen(page)) && (await page.inputValue("#hex")) === "#3D7A5B");
await page.click("#customize");
await page.locator(".g-cell").nth(13 * 5 + 2).click();
const sv2 = await page.locator("#sv").boundingBox();
await page.mouse.click(sv2.x + sv2.width * 0.7, sv2.y + sv2.height * 0.35);
const applyHex = await page.inputValue("#p-hex");
await page.fill("#p-hex", "#zz");
check("picker hex validates", (await page.textContent("#p-msg")).length > 0);
await page.fill("#p-hex", applyHex.toLowerCase());
check("native colour input present", (await page.locator("#p-native[type=color]").count()) === 1);
await page.click("#p-apply");
check("Apply commits", !(await panelOpen(page)) && (await page.inputValue("#hex")) === applyHex, applyHex);
await waitPreview(page, applyHex);
check("Apply: preview updated", (await mockVar(page, "light", "--primary")) === applyHex);
await page.fill("#hex", "#787878");
await waitPreview(page, "#787878");
await page.waitForFunction(() => !document.querySelector("#contrast-warn").hidden);
check("low contrast warns", (await page.textContent("#contrast-warn")).includes("4.5:1"));
await page.fill("#hex", "#3D7A5B");
await waitPreview(page, "#3D7A5B");
await page.check("input[name=theme][value=system]");
await page.waitForTimeout(600);
const st1 = await apiState(page);
check("final colour and theme saved", st1.branding.primary_color === "#3D7A5B" && st1.branding.theme === "system");
await clearFlashes(page);

// 4. relay
await step(page, 3);
await page.fill("#f-relay-host", "relay.acme.example");
await page.click("#generate");
await page.waitForFunction(() => document.querySelector("#key-view").value.includes("•"));
check("key shown masked", (await page.inputValue("#key-view")).includes("•"));
await page.click("#reveal");
await page.waitForFunction(() => !document.querySelector("#key-view").value.includes("•"));
const key = await page.inputValue("#key-view");
check("reveal shows a 32-char key", /^[A-Za-z0-9_-]{32}$/.test(key), key.length);
await page.click("#reveal");
await page.click("#generate");
await page.waitForSelector(".modal-back .modal:has-text('Generate a new admission key?')");
await page.getByRole("button", { name: "Cancel" }).last().click();
check("generate again asks first (confirmDialog), Cancel keeps the key", (await page.evaluate(async () => {
  const t = new URL(location.href).searchParams.get("t");
  return (await (await fetch("/api/relay/key/reveal", { method: "POST", headers: { "X-Setup-Token": t, "Content-Type": "application/json" }, body: "{}" })).json()).key;
})) === key);
await page.fill("#f-relay-host", "relay.acme.example");
await page.click("#save-relay");
check("relay saved", (await flashText(page, "ok")).includes("saved"));
check("state never carries the key", !JSON.stringify(await apiState(page)).includes(key));
await clearFlashes(page);

// 5. one token: Start makes the copy, the key, the release, and builds the packages
await step(page, 4);
await page.fill("#f-github-token", "github-token-for-tests-0123456789");
await page.click("#gh-connect");
await page.waitForSelector("#gh-start");
check("signed in to GitHub; the copy will be made", (await flashText(page, "ok")).includes("octo/RemoteMac"));
await shot(page, "light-05-signing-connected");
await page.click("#gh-start");
await page.waitForSelector(".modal:not(.picker)");
check("Start asks first and lists what it will do", (await page.locator(".modal:not(.picker) li").count()) >= 4);
await page.waitForTimeout(400);
await shot(page, "light-05-signing-confirm");
await page.click(".modal:not(.picker) .btn.primary");
await page.waitForFunction(() => [...document.querySelectorAll("tbody tr.ok")].length >= 3, null, { timeout: 30000 });
await shot(page, "light-05-signing-running");
for (let i = 0; i < 3000; i++) {
  const s = await apiState(page);
  if (!s.signing.running && (s.signing.done || s.signing.error)) break;
  await page.waitForTimeout(100);
}
const sg = await apiState(page);
check("Start did it all: copy, key, release, verified, built", sg.signing.done === true, sg.signing.error || "");
check("every action done", sg.signing.items.every((i) => i.status === "ok"), sg.signing.items.map((i) => i.id + ":" + i.status).join(" "));
check("the packages are valid", sg.validation && sg.validation.ok);
check("the key never reaches the page", !JSON.stringify(sg).includes("ed25519-secret") && !JSON.stringify(sg.signing).includes("key_secret"));
await page.waitForSelector(".alert.ok:has-text('packages are built')");
await shot(page, "light-05-signing-done");
await clearFlashes(page);

// 6. components: the copy's release is the one chosen
await step(page, 5);
await page.waitForSelector(".alert.ok:has-text('Signature valid')");
await page.click("#discover");
await page.waitForSelector("input[name=release]");
check("the copy's release preselected", await page.isChecked("input[name=release][value='enterprise-v0.3.0']"));
await page.click("#select");
await page.waitForSelector(".alert.ok:has-text('Signature valid')");
check("4 platforms offered", (await page.locator("input.plat").count()) === 4);
await clearFlashes(page);

// 7. build again by hand (the fake streams slowly: progress is visible)
await step(page, 6);
await page.check("#replace");
await page.click("#start-build");
await page.waitForSelector("progress");
await page.waitForFunction(() => [...document.querySelectorAll("progress")].some((p) => p.value > 0));
check("cancel offered while building", (await page.locator("#cancel-build").count()) === 1);
await clearFlashes(page);
await shot(page, "light-07-build-progress");
for (let i = 0; i < 600; i++) {
  const s = await apiState(page);
  if (!s.build.running) break;
  await page.waitForTimeout(250);
}
const st2 = await apiState(page);
check("build finished", !!st2.build.result, st2.build.error || "");
check("validation passed", st2.validation && st2.validation.ok, st2.validation && st2.validation.checks.length + " checks");
const uiDone = await page.waitForFunction(() => document.querySelectorAll("#steps li")[6].classList.contains("done"), null, { timeout: 8000 }).then(() => true, () => false);
check("the UI follows the build to its end (polling)", uiDone);

// 8. validate & export
await step(page, 7);
await page.waitForSelector(".alert.ok:has-text('checks passed')");
const [dl] = await Promise.all([page.waitForEvent("download"), page.click("#dl-json")]);
await dl.saveAs(`${SHOTS}/summary.json`);
const sum = JSON.parse(readFileSync(`${SHOTS}/summary.json`, "utf8"));
check("summary has the colour", sum.branding.primary_color === "#3D7A5B");
check("summary masks the key", !JSON.stringify(sum).includes(key) && String(sum.relay.admission_key).includes("•"));
const [dl2] = await Promise.all([page.waitForEvent("download"), page.click("#dl-md")]);
await dl2.saveAs(`${SHOTS}/summary.md`);
check("Markdown summary masks the key", !readFileSync(`${SHOTS}/summary.md`, "utf8").includes(key));

// every step at 1440x900, light and dark, and the open picker
const NAMES = ["preflight", "identity", "branding", "relay", "signing", "components", "build", "validate"];
for (const scheme of ["light", "dark"]) {
  const p = await newPage({ width: 1440, height: 900 }, scheme);
  await p.goto(URL_);
  await p.waitForSelector("#steps li button");
  for (let i = 0; i < NAMES.length; i++) {
    await step(p, i);
    if (NAMES[i] === "branding") await waitPreview(p, "#3D7A5B");
    await p.waitForTimeout(150);
    await shot(p, `${scheme}-${String(i + 1).padStart(2, "0")}-${NAMES[i]}`);
  }
  await step(p, 2);
  await p.click("#customize");
  await p.locator(".g-cell").nth(13 * 3 + 8).click();
  await p.waitForTimeout(200);
  await shot(p, `${scheme}-customize-picker`);
  await p.click("#p-cancel");
}

// 390 px: every step without horizontal scroll, the colour step and the picker usable
for (const scheme of ["light", "dark"]) {
  const m = await newPage({ width: 390, height: 844 }, scheme);
  await m.goto(URL_);
  await m.waitForSelector("#steps li button");
  const overflow = [];
  for (let i = 0; i < NAMES.length; i++) {
    await step(m, i);
    const sw = await m.evaluate(() => document.documentElement.scrollWidth);
    if (sw > 390) overflow.push(i + ":" + sw);
  }
  check(`no horizontal scroll at 390 px on any step (${scheme})`, overflow.length === 0, overflow.join(","));
  await step(m, 2);
  await m.waitForSelector("#palette");
  await m.locator("#palette").scrollIntoViewIfNeeded();
  await shot(m, `phone-${scheme}-branding`);
  await m.click("#customize");
  const dlgOverflow = await m.evaluate(() => { const d = document.getElementById("customize-panel"); return d.scrollWidth > d.clientWidth + 1 || document.documentElement.scrollWidth > 390; });
  check(`Customize picker fits at 390 px (${scheme})`, !dlgOverflow);
  const svm = await m.locator("#sv").boundingBox();
  await m.mouse.click(svm.x + svm.width * 0.5, svm.y + svm.height * 0.5);
  await shot(m, `phone-${scheme}-customize`);
  await m.click("#p-cancel");
  if (scheme === "light") { await step(m, 7); await shot(m, "phone-light-validate"); }
}

const csp = (await Promise.all(pages.map((p) => p.evaluate(() => window.__csp).catch(() => [])))).flat();
check("no CSP violations", csp.length === 0, JSON.stringify(csp));
// the refused non-square icon upload is a 400 the page reports itself: not a script error
const realErrors = consoleErrors.filter((e) => !/status of 400/.test(e));
check("no console errors", realErrors.length === 0, JSON.stringify(realErrors));

await browser.close();
writeFileSync(`${SHOTS}/results.json`, JSON.stringify(results, null, 2));
const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} UI checks passed`);
process.exit(failed.length ? 1 : 0);
