// Taro Enterprise Customer Portal: shared state, the API client and navigation.
// No framework. Every state-changing request carries the session's CSRF token (X-CSRF-Token,
// from GET /api/v1/me); the browser adds the Origin header the API checks.

export const S = {
  me: null,          // GET /api/v1/me, or null when signed out
  csrf: "",
  providers: {},     // GET /api/v1/auth/providers
  branding: {},      // GET /api/v1/branding (the look for this hostname)
  org: null,         // the organization in use: { id, name, slug, role }
  perms: new Set(),  // the caller's permissions in S.org
  unread: 0,
};

function storage(kind) {
  return {
    get(k) { try { return window[kind].getItem(k); } catch (_) { return null; } },
    set(k, v) { try { if (v == null) window[kind].removeItem(k); else window[kind].setItem(k, v); } catch (_) { /* private mode */ } },
  };
}
export const store = storage("localStorage");
export const sstore = storage("sessionStorage");

export class ApiError extends Error {
  constructor(status, code, message, requestId) {
    super(message);
    this.status = status;
    this.code = code;
    this.requestId = requestId || "";
  }
}

function sentence(m) {
  const t = String(m || "").trim();
  if (!t) return "";
  const s = t[0].toUpperCase() + t.slice(1);
  return /[.!?)]$/.test(s) ? s : s + ".";
}

function message(status, err) {
  const code = err.code || "";
  if (code === "unauthenticated") return "Your session has ended. Sign in again.";
  if (code === "forbidden") return "Your role in this organization does not allow this.";
  if (code === "not_found") return "Not found, or you do not have access to it.";
  if (code === "rate_limited") return "Too many attempts. Wait a few minutes, then try again.";
  if (code === "csrf") return "Your session changed in another tab. Reload the page and try again.";
  if (code === "internal") return sentence(err.message) + (err.request_id ? " Request id: " + err.request_id : "");
  if (err.message) return sentence(err.message);
  return status ? `The server answered ${status}. Try again.` : "Something went wrong. Try again.";
}

let unauthenticated = () => {};
/** What happens when a request finds no session (the app sends the person to sign in). */
export function onUnauthenticated(f) { unauthenticated = f; }

/**
 * A JSON request to the API. Throws ApiError with a readable message. Options:
 * raw (a Blob/ArrayBuffer body), contentType, headers, allow401 (no redirect to sign-in).
 */
export async function api(method, path, body, opt = {}) {
  const headers = { Accept: "application/json" };
  const init = { method, headers, credentials: "same-origin", cache: "no-store" };
  if (method !== "GET" && method !== "HEAD") headers["X-CSRF-Token"] = S.csrf || "";
  if (opt.raw !== undefined) {
    init.body = opt.raw;
    headers["Content-Type"] = opt.contentType || "application/octet-stream";
  } else if (body !== undefined) {
    init.body = JSON.stringify(body);
    headers["Content-Type"] = "application/json";
  }
  Object.assign(headers, opt.headers || {});
  let res;
  try {
    res = await fetch(path, init);
  } catch (_) {
    throw new ApiError(0, "network", "The server cannot be reached. Check your connection and try again.");
  }
  const text = await res.text();
  let data = null;
  if (text) { try { data = JSON.parse(text); } catch (_) { data = null; } }
  if (res.ok) return data;
  const err = (data && data.error) || {};
  const e = new ApiError(res.status, err.code || "http_" + res.status, message(res.status, err), err.request_id);
  if (e.code === "csrf" && !opt.retried) {
    // the token went stale (a new sign-in in another tab): fetch it again, once
    await refreshMe();
    if (S.me) return api(method, path, body, { ...opt, retried: true });
  }
  if (res.status === 401 && !opt.allow401) unauthenticated();
  throw e;
}

const ORG_KEY = "taro.portal.org";

/** Reload the signed-in user (null when signed out) and keep the chosen organization. */
export async function refreshMe() {
  try {
    const me = await api("GET", "/api/v1/me", undefined, { allow401: true, retried: true });
    S.me = me;
    S.csrf = me.csrf_token || "";
  } catch (e) {
    if (e.status !== 401) throw e;
    S.me = null; S.csrf = ""; S.org = null; S.perms = new Set();
    return null;
  }
  selectOrg(S.org ? S.org.id : store.get(ORG_KEY));
  return S.me;
}

/** Use an organization (the first one when `id` is not one of the user's). */
export function selectOrg(id) {
  const orgs = (S.me && S.me.organizations) || [];
  const o = orgs.find((x) => x.id === id) || orgs[0] || null;
  S.org = o;
  store.set(ORG_KEY, o ? o.id : null);
  const p = o && (S.me.organization_permissions || []).find((x) => x.organization_id === o.id);
  S.perms = new Set(p ? p.permissions : []);
}

export const can = (p) => S.perms.has(p);

let renderer = null;
export function setRenderer(f) { renderer = f; }

/** Go to a path of this app (history routing); other origins are a full navigation. */
export function navigate(to, { replace = false } = {}) {
  const u = new URL(to, location.origin);
  if (u.origin !== location.origin) { location.assign(u.href); return; }
  const next = u.pathname + u.search;
  if (replace) history.replaceState(null, "", next);
  else if (location.pathname + location.search !== next) history.pushState(null, "", next);
  if (renderer) renderer();
}

const RETURN_KEY = "taro.portal.return";
export function saveReturn(p) { sstore.set(RETURN_KEY, p); }
export function peekReturn() { return sstore.get(RETURN_KEY); }
export function takeReturn() {
  const p = sstore.get(RETURN_KEY);
  sstore.set(RETURN_KEY, null);
  return p && p.startsWith("/") && !p.startsWith("//") && !p.startsWith("/sign-in") ? p : null;
}

export const isDev = () => S.providers && S.providers.environment === "development";
