// Taro Operations (the Provider Console): the shell (top bar, side navigation by area), the
// router (History API; the API serves index.html for every unknown path) and sign-in state.
// The console may live under a path (/console/ on the portal's origin): routes are paths below
// the base the server puts in <base href>, links are relative to it (see BASE in lib.js).
import { h, icon, breadcrumbs, button, flash } from "./_console/console.js";
import { S, api, can, roleLabel, setUnauthenticatedHandler, announce, loadError, BASE, rel, routeOf } from "./lib.js";

import * as auth from "./views/auth.js";
import * as overview from "./views/overview.js";
import * as orgs from "./views/orgs.js";
import * as fleet from "./views/fleet.js";
import * as jobs from "./views/jobs.js";
import * as relays from "./views/relays.js";
import * as assignments from "./views/assignments.js";
import * as plans from "./views/plans.js";
import * as incidents from "./views/incidents.js";
import * as audit from "./views/audit.js";
import * as access from "./views/access.js";
import * as flags from "./views/flags.js";
import * as domains from "./views/domains.js";
import * as branding from "./views/branding.js";
import * as account from "./views/account.js";

const $ = (id) => document.getElementById(id);

// The side navigation, grouped by area; each item is shown only to roles that can read it.
const NAV = [
  { items: [
    { href: "/", label: "Overview", perm: "ops.overview.read" },
    { href: "/assignments", label: "Assignments", perm: "ops.orgs.read" },
  ] },
  { section: "Customers", items: [
    { href: "/organizations", label: "Organizations", perm: "ops.orgs.read" },
    { href: "/domains", label: "Domains", perm: "ops.overview.read" },
  ] },
  { section: "Infrastructure", items: [
    { href: "/macs", label: "Mac fleet", perm: "ops.inventory.read" },
    { href: "/provisioning-jobs", label: "Provisioning jobs", perm: "ops.jobs.read" },
    { href: "/providers", label: "Providers", perm: "ops.providers.read" },
    { href: "/relay-nodes", label: "Relay nodes", perm: "ops.relays.read" },
  ] },
  { section: "Catalog", items: [
    { href: "/plans", label: "Plans", perm: "ops.overview.read" },
    { href: "/branding", label: "Branding", perm: "ops.config.manage" },
  ] },
  { section: "Operations", items: [
    { href: "/incidents", label: "Incidents", perm: "ops.incidents.read" },
    { href: "/feature-flags", label: "Feature flags", perm: "ops.flags.read" },
  ] },
  { section: "Security", items: [
    { href: "/access", label: "Access", perm: "ops.access.read" },
    { href: "/audit", label: "Audit log", perm: "ops.audit.read" },
  ] },
];

// [pattern, view, permission, nav item it belongs to]
const ROUTES = [
  [/^\/$/, overview.page, "ops.overview.read", "/"],
  [/^\/organizations$/, orgs.list, "ops.orgs.read", "/organizations"],
  [/^\/organizations\/([0-9a-f-]{36})$/, orgs.detail, "ops.orgs.read", "/organizations"],
  [/^\/domains$/, domains.page, "ops.overview.read", "/domains"],
  [/^\/macs$/, fleet.macs, "ops.inventory.read", "/macs"],
  [/^\/macs\/([0-9a-f-]{36})$/, fleet.mac, "ops.inventory.read", "/macs"],
  [/^\/providers$/, fleet.providers, "ops.providers.read", "/providers"],
  [/^\/provisioning-jobs$/, jobs.page, "ops.jobs.read", "/provisioning-jobs"],
  [/^\/relay-nodes$/, relays.page, "ops.relays.read", "/relay-nodes"],
  [/^\/assignments$/, assignments.page, "ops.orgs.read", "/assignments"],
  [/^\/plans$/, plans.list, "ops.overview.read", "/plans"],
  [/^\/plans\/new$/, plans.create, "ops.config.manage", "/plans"],
  [/^\/incidents$/, incidents.list, "ops.incidents.read", "/incidents"],
  [/^\/incidents\/([0-9a-f-]{36})$/, incidents.detail, "ops.incidents.read", "/incidents"],
  [/^\/audit$/, audit.page, "ops.audit.read", "/audit"],
  [/^\/access$/, access.page, "ops.access.read", "/access"],
  [/^\/feature-flags$/, flags.page, "ops.flags.read", "/feature-flags"],
  [/^\/branding$/, branding.page, "ops.config.manage", "/branding"],
  [/^\/branding\/new$/, branding.create, "ops.config.manage", "/branding"],
  [/^\/account$/, account.page, null, "/account"],
];
const PUBLIC = { "/sign-in": auth.signIn, "/auth/verify": auth.verify, "/no-access": auth.noAccess };

let renderSeq = 0;

// ---------------------------------------------------------------- navigation

/** The current route: the path below the base, without a trailing slash ("/" at the base). */
export function currentRoute() {
  return (routeOf(location.pathname) || "/").replace(/(.)\/+$/, "$1");
}

/** Go to a route ("/macs?x=1"; below the base). */
export function navigate(path, { replace = false } = {}) {
  const url = BASE + String(path).replace(/^\//, "");
  if (replace) history.replaceState(null, "", url); else history.pushState(null, "", url);
  closeNav();
  render();
}

/** Update the query string of the current page without a new history entry or re-render. */
export function setQuery(params) {
  const u = new URL(location.href);
  for (const [k, v] of Object.entries(params)) { if (v == null || v === "") u.searchParams.delete(k); else u.searchParams.set(k, v); }
  history.replaceState(null, "", u.pathname + u.search);
}

document.addEventListener("click", (e) => {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = e.target.closest("a[href]");
  if (!a || a.target || a.hasAttribute("download")) return;
  const raw = a.getAttribute("href");
  if (raw.startsWith("#")) {
    // with <base>, "#main" would be the base page's: go to the element on this page instead
    e.preventDefault();
    const t = document.getElementById(raw.slice(1));
    if (t) { t.focus(); t.scrollIntoView(); }
    return;
  }
  const u = new URL(a.href, location.href);
  const route = u.origin === location.origin ? routeOf(u.pathname) : null;
  if (route == null || route.startsWith("/api/") || route.startsWith("/_") || route.startsWith("/brand/")) return;
  e.preventDefault();
  if (u.pathname + u.search === location.pathname + location.search && !u.hash) { render(); return; }
  navigate(route + u.search);
});
addEventListener("popstate", () => render());

// ---------------------------------------------------------------- the shell

function openNav() { document.body.classList.add("nav-open"); $("nav-toggle").setAttribute("aria-expanded", "true"); $("nav-scrim").hidden = false; }
function closeNav() { document.body.classList.remove("nav-open"); $("nav-toggle").setAttribute("aria-expanded", "false"); $("nav-scrim").hidden = true; }
$("nav-toggle").addEventListener("click", () => document.body.classList.contains("nav-open") ? closeNav() : openNav());
$("nav-scrim").addEventListener("click", closeNav);
addEventListener("keydown", (e) => { if (e.key === "Escape" && document.body.classList.contains("nav-open")) closeNav(); });

function badges() {
  const p = S.providers || {};
  const out = [];
  if (p.environment && p.environment !== "production") out.push(h("span", { class: "badge env", title: "Environment" }, p.environment === "development" ? "Development" : p.environment));
  if (p.simulated_provider) out.push(h("span", { class: "badge sim", title: "Macs come from the mock provider" }, "Simulated provider"));
  $("top-badges").replaceChildren(...out);
}

function shell(signedIn) {
  const brand = S.branding || {};
  $("brand-name").textContent = brand.displayName || "Taro Enterprise";
  const brandEl = $("brand");
  const mark = brandEl.querySelector(".mark, img");
  if (brand.logoAssetId) {
    if (!mark || mark.tagName !== "IMG" || !mark.src.endsWith(brand.logoAssetId)) mark.replaceWith(h("img", { src: "api/v1/assets/" + brand.logoAssetId, alt: "" }));
  } else if (mark && mark.tagName === "IMG") {
    mark.replaceWith(h("span", { class: "mark", "aria-hidden": "true" }, (brand.displayName || "T").trim().charAt(0).toUpperCase()));
  } else if (mark) {
    mark.textContent = (brand.displayName || "T").trim().charAt(0).toUpperCase();
  }
  if (brand.faviconAssetId) $("favicon").setAttribute("href", "api/v1/assets/" + brand.faviconAssetId);
  badges();
  $("layout").classList.toggle("no-nav", !signedIn);
  $("sidenav").hidden = !signedIn;
  $("nav-toggle").hidden = !signedIn;
  if (!signedIn) { $("top-user").replaceChildren(); return; }
  const me = S.me;
  const roles = me.operator_roles || [];
  $("top-user").replaceChildren(
    h("a", { class: "item user", href: "account", title: "Your account and sessions" }, icon("user"),
      h("span", { class: "u-text" }, h("strong", {}, me.display_name || me.email), h("span", { class: "u-role" }, roles.map(roleLabel).join(", ")))),
    h("span", { class: "divider hide-sm" }),
    h("button", { type: "button", class: "item linkish hide-sm", id: "sign-out", onclick: signOut }, "Sign out"));
  const nav = $("sidenav");
  nav.replaceChildren(
    h("div", { class: "head" }, h("p", { class: "title" }, "Taro Operations"), h("p", { class: "sub" }, "Provider console")),
    ...NAV.flatMap((g) => {
      const items = g.items.filter((i) => can(i.perm));
      if (!items.length) return null;
      return [g.section ? h("div", { class: "section" }, g.section) : null,
        h("ul", { class: "links" }, items.map((i) => h("li", {}, h("a", { href: rel(i.href), "data-nav": i.href }, i.label))))].filter(Boolean);
    }),
    h("div", { class: "section" }, "Account"),
    h("ul", { class: "links" }, h("li", {}, h("a", { href: "account", "data-nav": "/account" }, "Account and sessions")),
      h("li", { class: "show-sm" }, h("button", { type: "button", class: "nav-btn", onclick: signOut }, "Sign out"))),
    h("div", { class: "foot" }, h("dl", {},
      h("dt", {}, "Signed in"), h("dd", {}, me.email),
      h("dt", {}, "Environment"), h("dd", {}, (S.providers && S.providers.environment) || "–"))));
}

function markNav(navHref) {
  for (const a of document.querySelectorAll("#sidenav a[data-nav]")) {
    if (a.dataset.nav === navHref) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
  }
}

export async function signOut() {
  try { await api("POST", "api/v1/auth/sign-out"); } catch (_) { /* the session may be gone already */ }
  S.me = null;
  S.setup = await api("GET", "api/v1/setup/status").catch(() => S.setup);
  flash("ok", "You are signed out.");
  navigate("/sign-in", { replace: true });
}

// ---------------------------------------------------------------- loading the operator

async function loadMe() {
  try {
    S.me = await api("GET", "api/v1/me", undefined, { quiet401: true });
  } catch (e) {
    if (e.status === 401) S.me = null; else throw e;
  }
  return S.me;
}
export async function reloadMe() { await loadMe(); shell(!!S.me && hasAnyRole()); }
const hasAnyRole = () => S.me && (S.me.operator_roles || []).length > 0;

export async function refreshBranding() {
  try { S.branding = (await api("GET", "api/v1/branding")).branding; } catch (_) { S.branding = null; }
  shell(!!S.me && hasAnyRole());
}

setUnauthenticatedHandler(() => {
  if (!S.me) return;
  S.me = null;
  flash("warn", "Your session has ended. Sign in again.");
  const back = currentRoute() + location.search;
  navigate("/sign-in?return=" + encodeURIComponent(back), { replace: true });
});

// ---------------------------------------------------------------- rendering a page

/** The context a view gets: its params, the query, helpers to set the page and re-render. */
function ctx(params, seq) {
  const q = new URL(location.href).searchParams;
  return {
    params, query: q, navigate, setQuery, signOut, reloadMe, refreshBranding,
    refresh: () => render({ keepScroll: true }),
    stale: () => seq !== renderSeq,
    crumbs: (items) => breadcrumbs(items.map(([label, href]) => href ? h("a", { href: rel(href) }, label) : label)),
  };
}

function pageEl() { return $("page"); }

export async function render({ keepScroll = false } = {}) {
  const seq = ++renderSeq;
  const path = currentRoute();
  const scroll = keepScroll ? scrollY : 0;
  const page = pageEl();

  if (PUBLIC[path]) {
    shell(false);
    markNav(null);
    const node = await PUBLIC[path](ctx([], seq));
    if (seq !== renderSeq) return;
    page.replaceChildren(node);
    document.title = (page.querySelector("h1") ? page.querySelector("h1").textContent + " · " : "") + "Taro Operations";
    if (!keepScroll) scrollTo(0, 0);
    return;
  }
  if (!S.me) {
    await loadMe().catch(() => null);
    if (!S.me) { navigate("/sign-in" + (path !== "/" ? "?return=" + encodeURIComponent(path + location.search) : ""), { replace: true }); return; }
  }
  if (!hasAnyRole()) { navigate("/no-access", { replace: true }); return; }
  shell(true);
  let match = null;
  for (const [re, view, perm, navHref] of ROUTES) {
    const m = path.match(re);
    if (m) { match = { view, perm, navHref, params: m.slice(1) }; break; }
  }
  if (!match) {
    markNav(null);
    page.replaceChildren(notFound("This page does not exist", "Check the address, or go back to the overview."));
    document.title = "Not found · Taro Operations";
    return;
  }
  markNav(match.navHref);
  if (match.perm && !can(match.perm)) {
    page.replaceChildren(notFound("Your role cannot open this page", "Ask a platform administrator for a role that includes it (Access page)."));
    document.title = "No access · Taro Operations";
    return;
  }
  if (!keepScroll) page.replaceChildren(h("p", { class: "loading" }, h("span", { class: "spinner", "aria-hidden": "true" }), "Loading…"));
  let node;
  try {
    node = await match.view(ctx(match.params, seq));
  } catch (e) {
    if (e && e.status === 401) return;
    node = h("div", {}, loadError(e, () => render()));
  }
  if (seq !== renderSeq) return; // a newer navigation won
  // a re-render (a filter typed, an action done) keeps the focused control focused
  const focused = keepScroll && document.activeElement && document.activeElement.id && page.contains(document.activeElement) ? document.activeElement : null;
  const sel = focused && typeof focused.selectionStart === "number" ? [focused.selectionStart, focused.selectionEnd] : null;
  page.replaceChildren(node);
  if (focused) {
    const again = document.getElementById(focused.id);
    if (again) { again.focus({ preventScroll: true }); if (sel && again.setSelectionRange) try { again.setSelectionRange(sel[0], sel[1]); } catch (_) { /* not a text field */ } }
  }
  const h1 = page.querySelector("h1");
  document.title = (h1 ? h1.textContent + " · " : "") + "Taro Operations";
  if (keepScroll) scrollTo(0, scroll);
  else { scrollTo(0, 0); $("main").focus({ preventScroll: true }); if (h1) announce(h1.textContent); }
}

function notFound(title, text) {
  return h("div", { class: "empty-page" }, h("h1", {}, title), h("p", { class: "lead" }, text), button("Go to the overview", { kind: "primary", onclick: () => navigate("/") }));
}

async function boot() {
  const [p, setup] = await Promise.all([
    api("GET", "api/v1/auth/providers").catch(() => ({})),
    api("GET", "api/v1/setup/status").catch(() => null),
    loadMe().catch(() => null),
  ]);
  S.providers = p;
  S.setup = setup;
  try { S.branding = (await api("GET", "api/v1/branding")).branding; } catch (_) { S.branding = null; }
}

await boot();
render();
