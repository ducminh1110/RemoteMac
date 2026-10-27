// Relay nodes: the relays customers' connections go through. Add, delete, and check them
// (a TCP connect from the API: reachability and connect time are all the telemetry there is).
import { h, container, pageHeader, button, confirmDialog, alert, flash } from "../_console/console.js";
import { api, can, st, when, table, act, field, formDialog, readOnlyNote, stats } from "../lib.js";

export async function page(ctx) {
  const r = await api("GET", "api/v1/ops/relay-nodes");
  const manage = can("ops.relays.manage");
  const reachable = r.relays.filter((x) => x.reachable === true).length;
  const down = r.relays.filter((x) => x.reachable === false).length;

  const add = async () => {
    const name = field({ label: "Name", name: "name", required: true, placeholder: "e.g. relay-eu-1", maxlength: 80 });
    const address = field({ label: "Address", name: "address", required: true, mono: true, placeholder: "relay.example.com:443", desc: "host:port that clients connect to." });
    const region = field({ label: "Region", name: "region", required: true, placeholder: "e.g. eu-west", maxlength: 40 });
    const ok = await formDialog({ title: "Add a relay node", confirm: "Add relay", body: [name, address, region], submit: async () => {
      let bad = false;
      for (const f of [name, address, region]) { const v = f.control.value.trim(); f.setError(v ? null : "Required."); if (!v) bad = true; }
      const a = address.control.value.trim();
      if (a && !/^.+:\d{1,5}$/.test(a)) { address.setError("Write it as host:port."); bad = true; }
      if (bad) throw new Error("Fill in the name, the address (host:port) and the region.");
      await api("POST", "api/v1/ops/relay-nodes", { name: name.control.value.trim(), address: a, region: region.control.value.trim() });
    } });
    if (ok) { flash("ok", "Relay node added. Check it to see whether it can be reached."); ctx.refresh(); }
  };
  const del = async (btn, x) => {
    if (!await confirmDialog({ title: `Delete ${x.name}?`, confirm: "Delete relay", danger: true,
      body: [h("p", {}, "The relay node is removed from Taro. Clients that use ", h("code", {}, x.address), " are not changed by this."), h("p", { class: "muted" }, "This cannot be undone.")] })) return;
    await act(btn, "Deleting…", () => api("DELETE", "api/v1/ops/relay-nodes/" + x.id), { ok: `${x.name} deleted.`, after: ctx.refresh });
  };
  const check = (e) => act(e.currentTarget, "Checking…", () => api("POST", "api/v1/ops/relay-nodes/check"), { ok: "Relay nodes checked.", after: ctx.refresh });

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Infrastructure"], ["Relay nodes"]]),
    pageHeader({ title: "Relay nodes", desc: "Relays that carry customers' remote sessions. A check connects to each over TCP from the API; an unreachable relay opens an incident.",
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }),
        manage ? button("Check health", { icon: "sync", id: "check-relays", onclick: check, disabled: !r.relays.length }) : null,
        manage ? button("Add relay", { kind: "primary", icon: "plus", id: "add-relay", onclick: add }) : null] }),
    manage ? null : readOnlyNote("relay nodes"),
    stats([["Relay nodes", r.relays.length], ["Reachable", reachable, "ok"], ["Unreachable", down, down ? "bad" : null], ["Not checked", r.relays.length - reachable - down]]),
    container({ title: "Relay nodes", count: r.relays.length, flush: true, foot: "Telemetry: " + r.telemetry + "." },
      table([
        { label: "Name", cell: (x) => h("strong", {}, x.name) },
        { label: "Address", cell: (x) => h("span", { class: "mono" }, x.address) },
        { label: "Region", cell: (x) => x.region },
        { label: "Health", cell: (x) => x.reachable == null ? st("unknown", "Not checked") : x.reachable ? st("healthy", "Reachable") : st("down", "Unreachable") },
        { label: "Connect time", cell: (x) => x.connect_ms != null ? x.connect_ms + " ms" : h("span", { class: "muted" }, "–"), class: "num nowrap" },
        { label: "Checked", cell: (x) => when(x.last_check_at, { rel: true }), class: "nowrap" },
        { label: "Last error", cell: (x) => x.last_error ? h("span", { class: "err-text" }, x.last_error) : h("span", { class: "muted" }, "–") },
        manage ? { label: "Actions", class: "actions", cell: (x) => button("Delete", { kind: "sm danger", onclick: (e) => del(e.currentTarget, x) }) } : null,
      ].filter(Boolean), r.relays, { caption: "Relay nodes", rowClass: (x) => x.reachable === false ? "fail" : null,
        empty: { title: "No relay nodes", text: "Add the relays your customers connect through.", action: manage ? button("Add relay", { kind: "primary", onclick: add }) : null } })),
    down ? alert("warn", "Some relays cannot be reached", "Customers whose connections go through them cannot connect. See Incidents for details.") : null);
}
