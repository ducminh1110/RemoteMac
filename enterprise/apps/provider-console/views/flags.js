// Feature flags: the platform's kill switches. On by default; an operator turns one off in an
// incident (new rentals, checkout, connection grants, the provisioning worker).
import { h, container, pageHeader, button, confirmDialog, alert } from "/_console/console.js";
import { api, can, when, table, act, readOnlyNote, stats } from "/lib.js";

const IMPACT = {
  "rentals.new": "Customers cannot create rentals; the catalog shows rentals as paused.",
  "billing.checkout": "Customers cannot start a checkout to pay for a rental.",
  "connect.grants": "Customers cannot get new connection grants to their Macs (open sessions go on).",
  "provisioning.worker": "The worker takes no new jobs: paid rentals wait in the queue.",
};

export async function page(ctx) {
  const r = await api("GET", "/api/v1/ops/feature-flags");
  const manage = can("ops.flags.manage");
  const off = r.flags.filter((f) => !f.enabled);

  const toggle = async (btn, f) => {
    const turnOff = f.enabled;
    const ok = await confirmDialog({
      title: turnOff ? `Turn off ${f.key}?` : `Turn on ${f.key}?`,
      confirm: turnOff ? "Turn off" : "Turn on", danger: turnOff,
      body: [h("p", {}, f.description + "."), turnOff
        ? alert("warn", "What customers notice", IMPACT[f.key] || "The feature stops for everyone until it is turned on again.")
        : h("p", {}, "The feature works again for everyone at once.")],
    });
    if (!ok) return;
    await act(btn, turnOff ? "Turning off…" : "Turning on…", () => api("PATCH", "/api/v1/ops/feature-flags/" + encodeURIComponent(f.key), { enabled: !f.enabled }),
      { ok: `${f.key} is ${turnOff ? "off" : "on"}.`, after: ctx.refresh });
  };

  const sw = (f) => {
    const b = h("button", { type: "button", class: "switch", role: "switch", "aria-checked": f.enabled ? "true" : "false", "aria-label": f.key, id: "flag-" + f.key.replace(/\W/g, "-"),
      disabled: !manage || null, title: manage ? (f.enabled ? "Turn off" : "Turn on") : "Read only" },
    h("span", { class: "knob", "aria-hidden": "true" }), h("span", { class: "sw-text" }, f.enabled ? "On" : "Off"));
    if (manage) b.addEventListener("click", () => toggle(b, f));
    return b;
  };

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Operations"], ["Feature flags"]]),
    pageHeader({ title: "Feature flags", desc: "Kill switches for the platform. They are on in normal operation; turn one off to stop a feature for everyone during an incident.",
      actions: button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }) }),
    manage ? null : readOnlyNote("feature flags"),
    off.length ? alert("warn", `${off.length} kill switch${off.length === 1 ? " is" : "es are"} off`, off.map((f) => f.key).join(", ") + ". Turn them on again when the incident is over.") : null,
    stats([["Flags", r.flags.length], ["On", r.flags.length - off.length, "ok"], ["Off", off.length, off.length ? "warn" : null]]),
    container({ title: "Flags", count: r.flags.length, flush: true },
      table([
        { label: "State", cell: sw, class: "st" },
        { label: "Flag", cell: (f) => h("div", {}, h("span", { class: "mono strong" }, f.key), h("div", { class: "muted small" }, f.description)) },
        { label: "When off", cell: (f) => h("span", { class: "small" }, IMPACT[f.key] || "–") },
        { label: "Changed", cell: (f) => when(f.updated_at, { rel: true }), class: "nowrap" },
        { label: "By", cell: (f) => f.updated_by || h("span", { class: "muted" }, "Default") },
      ], r.flags, { caption: "Feature flags", rowClass: (f) => f.enabled ? null : "warn-row", empty: { title: "No flags" } })));
}


