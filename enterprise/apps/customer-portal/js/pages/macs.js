// Customer Portal: My Macs, the Macs the provider assigned to the signed-in user.
import { h, icon, container, button, flash } from "/_console/console.js";
import { S, api } from "../core.js";
import { run, announce, emptyState, regionName, rentalStatus, FINAL, TRANSITIONAL, macName, when } from "../ui.js";

/** The Macs assigned to me, across all my organizations. */
async function loadMine() {
  const { organizations } = await api("GET", "/api/v1/organizations");
  const lists = await Promise.all((organizations || []).map((o) => api("GET", `/api/v1/organizations/${o.id}/rentals`).then((r) => r.rentals || [], () => [])));
  const me = (S.me.email || "").toLowerCase();
  return lists.flat().filter((r) => (r.assigned_email || "").toLowerCase() === me && !FINAL.has(r.status))
    .sort((a, b) => (b.created_at || "").localeCompare(a.created_at || ""));
}

function stateLabel(r) {
  if (r.status === "ready") return r.connection_state === "connectable" ? rentalStatus("ready") : h("span", { class: "status warn" }, "Ready, not reachable yet");
  if (TRANSITIONAL.has(r.status) || r.status === "draft") return h("span", { class: "status info" }, h("span", { class: "spinner sm", "aria-hidden": "true" }), "Being prepared");
  return rentalStatus(r.status);
}

export async function pageMacs(ctx) {
  const dl = S.providers.app_download_url;
  const downloadBtn = dl ? h("a", { class: "btn primary", id: "download-app", href: dl, "data-external": "", rel: "noopener" }, icon("download"), "Download the app") : null;
  const hint = h("p", { class: "app-hint", id: "connect-hint", hidden: true }, icon("info"),
    h("span", {}, "Taro did not open? ", dl ? h("a", { href: dl, "data-external": "", rel: "noopener", id: "hint-download" }, "Download the app") : "Install the Taro app", " and try again."));
  const list = h("div", { id: "mac-list" });
  ctx.inner.replaceChildren(
    h("div", { class: "page-header" }, h("div", { class: "text" }, h("h1", {}, "My Macs"), h("p", { class: "desc" }, "The Macs your provider assigned to you.")),
      h("div", { class: "actions" }, downloadBtn)),
    list, hint);

  async function connect(btn, r) {
    const g = await run(btn, "Opening…", () => api("POST", `/api/v1/rentals/${r.id}/connect-grant`));
    if (!g || !g.taro_link) return;
    hint.hidden = false;
    announce("Opening the Taro app.");
    location.href = g.taro_link;
  }

  function card(r) {
    const ready = r.connection_state === "connectable";
    const b = button("Connect", { kind: "primary", id: "connect-" + r.id, disabled: !ready });
    b.addEventListener("click", () => connect(b, r));
    const meta = [["Plan", r.plan_name], ["Model", r.resource_model], ["Region", r.resource_region ? regionName(r.resource_region) : null], ["Until", r.ends_at ? when(r.ends_at) : null]]
      .filter(([, v]) => v);
    return h("article", { class: "mac-card", "data-status": r.status },
      h("div", { class: "mac-main" },
        h("h2", {}, macName(r)),
        h("p", { class: "mac-state" }, stateLabel(r)),
        h("dl", { class: "mac-meta" }, meta.map(([k, v]) => [h("dt", {}, k), h("dd", {}, v)]).flat())),
      h("div", { class: "mac-act" }, b));
  }

  async function load(first) {
    let rows;
    try { rows = await loadMine(); } catch (e) { if (first) throw e; return; }
    if (!ctx.alive()) return;
    list.replaceChildren(rows.length
      ? h("div", { class: "mac-grid" }, rows.map(card))
      : container({ flush: true }, emptyState("No Mac is assigned to you yet.", "Your provider assigns them.")));
    if (rows.some((r) => TRANSITIONAL.has(r.status))) timer ||= ctx.every(5000, () => load(false));
    else if (timer) { clearInterval(timer); timer = null; }
  }
  let timer = null;
  await load(true);
}
