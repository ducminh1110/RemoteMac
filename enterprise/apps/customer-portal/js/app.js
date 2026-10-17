// Taro Enterprise Customer Portal: boot, the top bar and the router (history routing: the API
// serves index.html for every unknown path, so /auth/verify and /device open here).
import { h, icon, flash } from "/_console/console.js";
import { S, api, refreshMe, navigate, setRenderer, onUnauthenticated, saveReturn, isDev } from "./core.js";
import { announce, loading, initials } from "./ui.js";
import { pageSignIn, pageVerifyLink, pageDevice, signOut } from "./pages/auth.js";
import { pageMacs } from "./pages/macs.js";
import { pageAccount } from "./pages/account.js";

// public: no session needed (the page decides); bare: narrow, centred layout
const ROUTES = [
  { re: /^\/sign-in\/?$/, page: pageSignIn, public: true, bare: true, title: "Sign in" },
  { re: /^\/auth\/verify\/?$/, page: pageVerifyLink, public: true, bare: true, title: "Signing in" },
  { re: /^\/device\/?$/, page: pageDevice, public: true, bare: true, title: "Sign in to the Taro app" },
  { re: /^\/$/, page: pageMacs, nav: "macs", title: "My Macs" },
  { re: /^\/account\/?$/, page: pageAccount, nav: "account", title: "Account" },
  { re: /^\/(macs|rentals|my-macs)(\/.*)?$/, redirect: "/" },
];

const $ = (id) => document.getElementById(id);
let seq = 0;          // the current render; older ones stop updating the page
let timers = [];
let firstRender = true;

// ------------------------------------------------------------------ branding (public, by hostname)

function applyBranding() {
  const b = S.branding || {};
  const fav = $("favicon");
  if (fav && b.faviconAssetId) { fav.href = "/api/v1/assets/" + b.faviconAssetId; fav.removeAttribute("type"); }
  document.body.classList.toggle("density-compact", b.density === "compact");
}
const productName = () => (S.branding && S.branding.displayName) || "Taro Enterprise";

function brandMark() {
  const b = S.branding || {};
  if (b.logoAssetId) return h("img", { src: "/api/v1/assets/" + b.logoAssetId, alt: "" });
  return h("span", { class: "mark", "aria-hidden": "true" }, productName().slice(0, 1).toUpperCase());
}

// ------------------------------------------------------------------ the shell

function renderTop(bare, current) {
  const top = $("topnav");
  const kids = [h("a", { class: "brand", href: "/", "aria-label": productName() + ", home" }, brandMark(), h("span", { class: "brand-name" }, productName()))];
  kids.push(h("span", { class: "service hide-sm" }, "Customer Portal"));
  if (isDev()) kids.push(h("span", { class: "badge hide-sm", title: "Development: simulated Macs" }, "Development"));
  if (S.me && !bare) {
    kids.push(h("nav", { class: "topmenu", "aria-label": "Portal" },
      h("a", { href: "/", "aria-current": current === "macs" ? "page" : null, "data-nav": "macs" }, "My Macs"),
      h("a", { href: "/account", "aria-current": current === "account" ? "page" : null, "data-nav": "account" }, "Account")));
  }
  kids.push(h("span", { class: "spacer" }));
  if (S.me) {
    kids.push(h("a", { class: "item user-item", href: "/account", title: S.me.email, "aria-label": "Account: " + S.me.email },
      h("span", { class: "avatar", "aria-hidden": "true" }, initials(S.me.display_name || S.me.email)),
      h("span", { class: "hide-sm ellipsis user-name" }, S.me.email)));
    kids.push(h("button", { type: "button", class: "item linkish", id: "sign-out", onclick: () => signOut() }, "Sign out"));
  }
  top.replaceChildren(...kids);
}

// ------------------------------------------------------------------ the router

function match(path) {
  for (const r of ROUTES) {
    const m = path.match(r.re);
    if (m) return { route: r, params: m.slice(1) };
  }
  return null;
}

function clearTimers() { timers.forEach((t) => clearInterval(t)); timers = []; }

async function render() {
  const my = ++seq;
  clearTimers();
  const path = location.pathname;
  const m = match(path);
  if (m && m.route.redirect) { navigate(m.route.redirect, { replace: true }); return; }
  const route = m ? m.route : { page: pageNotFound, title: "Not found", bare: !S.me, public: true };
  if (!route.public && !S.me) {
    saveReturn(location.pathname + location.search);
    navigate("/sign-in", { replace: true });
    return;
  }
  if (route.page === pageSignIn && S.me) { navigate("/", { replace: true }); return; }
  const bare = !!route.bare;
  renderTop(bare, route.nav);
  const main = $("main");
  const inner = h("div", { class: "inner" + (bare ? " narrow" : "") });
  inner.append(loading());
  main.replaceChildren(inner);
  const ctx = {
    inner,
    params: m ? m.params : [],
    query: new URLSearchParams(location.search),
    alive: () => my === seq,
    every(ms, fn) { const t = setInterval(() => { if (my === seq) fn(); else clearInterval(t); }, ms); timers.push(t); return t; },
    title(t) { if (my === seq) document.title = t + " · " + productName(); },
    refreshShell() { if (my !== seq) return; renderTop(bare, route.nav); },
  };
  ctx.title(route.title);
  try {
    await route.page(ctx);
  } catch (e) {
    if (my !== seq) return;
    if (e && e.status === 401) return;
    inner.replaceChildren(errorPage(e));
  }
  if (my !== seq) return;
  if (!firstRender) {
    main.focus({ preventScroll: true });
    window.scrollTo(0, 0);
    const h1 = inner.querySelector("h1");
    announce((h1 ? h1.textContent : route.title) + " page");
  }
  firstRender = false;
}

function errorPage(e) {
  const nf = e && e.status === 404;
  return h("div", { class: "page-error" },
    h("div", { class: "page-header" }, h("div", { class: "text" }, h("h1", {}, nf ? "Not found" : "This page could not be loaded"))),
    h("div", { class: "alert " + (nf ? "info" : "err"), role: nf ? null : "alert" }, icon(nf ? "info" : "err"),
      h("div", { class: "body" }, h("strong", {}, nf ? "It does not exist, or you do not have access to it." : "Something went wrong."), h("p", {}, (e && e.message) || String(e)),
        e && e.requestId ? h("p", { class: "muted small" }, "Request id: ", h("code", {}, e.requestId)) : null)),
    h("div", { class: "row" }, h("button", { type: "button", class: "btn", onclick: () => render() }, icon("refresh"), "Try again"), h("a", { class: "btn link", href: "/" }, "Go to My Macs")));
}

async function pageNotFound(ctx) {
  ctx.inner.replaceChildren(
    h("div", { class: "page-header" }, h("div", { class: "text" }, h("h1", {}, "Page not found"), h("p", { class: "desc" }, "There is no page at ", h("code", {}, location.pathname), "."))),
    h("a", { class: "btn primary", href: S.me ? "/" : "/sign-in" }, S.me ? "Go to My Macs" : "Sign in"));
}

// ------------------------------------------------------------------ boot

function interceptLinks() {
  document.addEventListener("click", (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target.closest && e.target.closest("a[href]");
    if (!a || a.target || a.hasAttribute("download") || a.dataset.external != null) return;
    const href = a.getAttribute("href");
    if (!href || href.startsWith("#")) return;
    const u = new URL(href, location.href);
    if (u.origin !== location.origin || u.pathname.startsWith("/api/")) return;
    e.preventDefault();
    navigate(u.pathname + u.search + u.hash);
  });
  addEventListener("popstate", () => render());
}

async function boot() {
  const [prov, brand] = await Promise.all([
    api("GET", "/api/v1/auth/providers", undefined, { allow401: true }).catch(() => ({})),
    api("GET", "/api/v1/branding", undefined, { allow401: true }).catch(() => ({})),
  ]);
  S.providers = prov || {};
  S.branding = (brand && brand.branding) || {};
  applyBranding();
  try { await refreshMe(); } catch (_) { S.me = null; }
  onUnauthenticated(() => {
    if (!S.me) return;
    S.me = null; S.csrf = ""; S.org = null; S.perms = new Set();
    saveReturn(location.pathname + location.search);
    flash("info", "Your session has ended. Sign in again.");
    navigate("/sign-in", { replace: true });
  });
  setRenderer(render);
  interceptLinks();
  await render();
}

boot();
