// Plans: the catalog customers rent from. Plans are never edited in place (a rental keeps the
// price it was made with): a change is a new plan, usually with the old one retired.
import { h, icon, container, pageHeader, button, confirmDialog, alert, flash } from "../_console/console.js";
import { api, can, st, money, minorUnits, table, toolbar, searchBox, selectBox, act, field, readOnlyNote, stats, checkbox } from "../lib.js";

function billingText(p) {
  return p.billing === "monthly" ? "Monthly" : `One-time · ${p.duration_hours} h`;
}

export async function list(ctx) {
  const show = ctx.query.get("show") || "";
  const q = (ctx.query.get("q") || "").toLowerCase();
  const r = await api("GET", "api/v1/ops/plans");
  const manage = can("ops.config.manage");
  let rows = r.plans;
  if (show === "active") rows = rows.filter((p) => p.active);
  if (show === "retired") rows = rows.filter((p) => !p.active);
  if (q) rows = rows.filter((p) => [p.name, p.slug, p.chip, p.region, p.description].join(" ").toLowerCase().includes(q));

  const toggle = async (btn, p) => {
    const retire = p.active;
    if (!await confirmDialog({ title: retire ? `Retire ${p.name}?` : `Offer ${p.name} again?`, confirm: retire ? "Retire plan" : "Reactivate plan", danger: retire,
      body: h("p", {}, retire ? "New rentals can no longer choose it. Rentals made with it keep their price and go on." : "Customers can rent it again from the catalog.") })) return;
    await act(btn, retire ? "Retiring…" : "Reactivating…", () => api("PATCH", "api/v1/ops/plans/" + p.id, { active: !retire }), { ok: retire ? `${p.name} is retired.` : `${p.name} is offered again.`, after: ctx.refresh });
  };

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Catalog"], ["Plans"]]),
    pageHeader({ title: "Plans", desc: "What customers can rent. A plan is not changed in place: to change a price or specs, create a new version and retire the old plan.",
      actions: [button("Refresh", { icon: "refresh", onclick: () => ctx.refresh() }), manage ? h("a", { class: "btn primary", href: "plans/new", id: "new-plan" }, icon("plus"), "Create plan") : null] }),
    manage ? null : readOnlyNote("plans"),
    stats([["Plans", r.plans.length], ["Offered", r.plans.filter((p) => p.active).length, "ok"], ["Retired", r.plans.filter((p) => !p.active).length]]),
    container({ title: "Plans", count: rows.length, flush: true },
      toolbar(
        searchBox({ label: "Search plans", value: ctx.query.get("q") || "", id: "plan-search", placeholder: "Name, slug, chip or region", oninput: (v) => { ctx.setQuery({ q: v }); ctx.refresh(); } }),
        selectBox({ label: "Show", value: show, id: "plan-show", options: [["", "All plans"], ["active", "Offered"], ["retired", "Retired"]], onchange: (v) => { ctx.setQuery({ show: v }); ctx.refresh(); } })),
      table([
        { label: "Plan", cell: (p) => h("div", {}, h("strong", {}, p.name), h("div", { class: "mono muted small" }, p.slug), p.description ? h("div", { class: "muted small clip", title: p.description }, p.description) : null) },
        { label: "Specs", cell: (p) => h("span", { class: "nowrap" }, h("strong", {}, p.chip), h("span", { class: "muted" }, ` · ${p.cpu_cores} cores · ${p.memory_gb} GB · ${p.storage_gb} GB`)) },
        { label: "Region", cell: (p) => p.region },
        { label: "Billing", cell: (p) => billingText(p), class: "nowrap" },
        { label: "Price", cell: (p) => h("strong", {}, money(p.price_minor, p.currency)), class: "num nowrap" },
        { label: "Status", cell: (p) => p.active ? st("active", "Offered") : st("disabled", "Retired") },
        manage ? { label: "Actions", class: "actions", cell: (p) => [
          h("a", { class: "btn sm", href: "plans/new?from=" + p.id, title: "Create a changed copy of this plan" }, "New version"),
          button(p.active ? "Retire" : "Reactivate", { kind: p.active ? "sm danger" : "sm", onclick: (e) => toggle(e.currentTarget, p) }),
        ] } : null,
      ].filter(Boolean), rows, { caption: "Plans", rowClass: (p) => p.active ? null : "retired",
        empty: { title: "No plans match", text: "Clear a filter, or create a plan." } })));
}

const slugify = (s) => s.toLowerCase().normalize("NFKD").replace(/[^\w\s-]/g, "").replace(/[\s_]+/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "").slice(0, 60);

export async function create(ctx) {
  const fromId = ctx.query.get("from");
  const [plans, fleet] = await Promise.all([
    api("GET", "api/v1/ops/plans"),
    can("ops.inventory.read") ? api("GET", "api/v1/ops/mac-resources").catch(() => null) : null,
  ]);
  const from = fromId ? plans.plans.find((p) => p.id === fromId) : null;
  const caps = fleet && fleet.capabilities;
  const chips = [...new Set([...(caps ? caps.chips : []), ...plans.plans.map((p) => p.chip)])];
  const regions = [...new Set([...(caps ? caps.regions : []), ...plans.plans.map((p) => p.region)])];
  const taken = new Set(plans.plans.map((p) => p.slug));
  let slugTouched = !!from;
  const nextSlug = (s) => { let x = s, n = 2; while (taken.has(x)) x = `${s}-v${n++}`; return x; };

  const fName = field({ label: "Name", name: "name", value: from ? from.name : "", required: true, maxlength: 80, placeholder: "e.g. Mac mini M4 · Monthly",
    oninput: () => { if (!slugTouched) fSlug.control.value = nextSlug(slugify(fName.control.value)); } });
  const fSlug = field({ label: "Slug", name: "slug", value: from ? nextSlug(from.slug) : "", required: true, mono: true, maxlength: 60,
    desc: "The plan's permanent key: lowercase letters, digits and -.", oninput: () => { slugTouched = true; } });
  const fDesc = field({ label: "Description", name: "description", type: "textarea", rows: 2, opt: true, value: from ? from.description : "", maxlength: 300 });
  const dl = (id, vals) => h("datalist", { id }, vals.map((v) => h("option", { value: v })));
  const fChip = field({ label: "Chip", name: "chip", value: from ? from.chip : (chips[0] || "M4"), required: true, attrs: { list: "chip-list" }, desc: "Rentals get a free Mac with this chip in the region." });
  const fCpu = field({ label: "CPU cores", name: "cpu_cores", type: "number", value: from ? from.cpu_cores : 10, min: 1, step: 1, required: true, inputmode: "numeric" });
  const fMem = field({ label: "Memory (GB)", name: "memory_gb", type: "number", value: from ? from.memory_gb : 24, min: 1, step: 1, required: true, inputmode: "numeric" });
  const fSto = field({ label: "Storage (GB)", name: "storage_gb", type: "number", value: from ? from.storage_gb : 512, min: 1, step: 1, required: true, inputmode: "numeric" });
  const fRegion = field({ label: "Region", name: "region", value: from ? from.region : (regions[0] || "eu-west"), required: true, attrs: { list: "region-list" } });
  let billing = from ? from.billing : "monthly";
  const fDur = field({ label: "Duration (hours)", name: "duration_hours", type: "number", value: from && from.duration_hours ? from.duration_hours : 24, min: 1, step: 1, inputmode: "numeric", desc: "How long a one-time rental lasts." });
  const durWrap = h("div", { hidden: billing !== "one_time" }, fDur);
  const tiles = h("div", { class: "tiles", role: "radiogroup", "aria-label": "Billing" },
    [["monthly", "Monthly", "A subscription, billed every month until cancelled"], ["one_time", "One-time", "Paid once, for a fixed number of hours"]].map(([v, t, d]) =>
      h("label", { class: "tile" }, h("input", { type: "radio", name: "billing", value: v, checked: billing === v || null, onchange: () => { billing = v; durWrap.hidden = v !== "one_time"; } }),
        h("span", { class: "t" }, t), h("span", { class: "d" }, d))));
  const cur0 = from ? from.currency : "usd";
  const fPrice = field({ label: "Price", name: "price", value: from ? (from.price_minor / 10 ** minorUnits(cur0)).toFixed(minorUnits(cur0)) : "", required: true, inputmode: "decimal", placeholder: "99.00",
    desc: "Per month for monthly plans; for the whole period for one-time plans." });
  const fCur = field({ label: "Currency", name: "currency", value: cur0.toUpperCase(), required: true, maxlength: 3, mono: true, placeholder: "USD", desc: "ISO code, e.g. USD or EUR." });
  const retire = from && from.active ? checkbox({ label: `Retire “${from.name}” when this plan is created`, desc: "Rentals made with it keep their price.", checked: true }) : null;
  const errBox = h("div", { role: "alert" });

  const submit = button(from ? "Create new version" : "Create plan", { kind: "primary", type: "submit", id: "plan-submit" });
  const form = h("form", { novalidate: true, id: "plan-form", onsubmit: async (e) => {
    e.preventDefault();
    errBox.replaceChildren();
    const all = [fName, fSlug, fChip, fCpu, fMem, fSto, fRegion, fDur, fPrice, fCur];
    all.forEach((f) => f.setError(null));
    const err = (f, m) => { f.setError(m); bad = bad || f; };
    let bad = null;
    const name = fName.control.value.trim(), slug = fSlug.control.value.trim();
    if (!name) err(fName, "Give the plan a name.");
    if (!/^[a-z0-9-]{2,}$/.test(slug)) err(fSlug, "2 or more of a-z, 0-9 and -.");
    else if (taken.has(slug)) err(fSlug, "Another plan has this slug.");
    if (!fChip.control.value.trim()) err(fChip, "Required.");
    if (!fRegion.control.value.trim()) err(fRegion, "Required.");
    const int = (f) => { const v = Number(f.control.value); if (!Number.isInteger(v) || v < 1) { err(f, "A whole number, 1 or more."); return null; } return v; };
    const cpu = int(fCpu), mem = int(fMem), sto = int(fSto);
    const dur = billing === "one_time" ? int(fDur) : null;
    const cur = fCur.control.value.trim().toLowerCase();
    if (!/^[a-z]{3}$/.test(cur)) err(fCur, "Three letters, e.g. USD.");
    const priceTxt = fPrice.control.value.trim().replace(",", ".");
    const digits = minorUnits(cur);
    const re = digits ? /^\d+(\.\d{1,2})?$/ : /^\d+$/;
    if (!re.test(priceTxt)) err(fPrice, digits ? "A price such as 99 or 99.50." : "A whole number.");
    if (bad) { bad.control.focus(); errBox.replaceChildren(alert("err", null, "Check the highlighted fields.")); return; }
    const price_minor = Math.round(Number(priceTxt) * 10 ** digits);
    const body = { slug, name, description: fDesc.control.value.trim(), chip: fChip.control.value.trim(), cpu_cores: cpu, memory_gb: mem, storage_gb: sto,
      region: fRegion.control.value.trim(), billing, duration_hours: billing === "one_time" ? dur : null, price_minor, currency: cur };
    await act(submit, "Creating…", async () => {
      try {
        await api("POST", "api/v1/ops/plans", body);
        if (retire && retire.control.checked) await api("PATCH", "api/v1/ops/plans/" + from.id, { active: false });
      } catch (x) { errBox.replaceChildren(alert("err", "The plan was not created", x.message)); throw x; }
      flash("ok", retire && retire.control.checked ? `${name} created and ${from.name} retired.` : `${name} created.`);
      ctx.navigate("/plans");
    });
  } },
  errBox,
  container({ title: "Plan" }, h("div", { class: "form-grid" }, fName, fSlug), fDesc),
  container({ title: "Mac" }, h("div", { class: "form-grid four" }, fChip, fCpu, fMem, fSto), h("div", { class: "form-grid" }, fRegion), dl("chip-list", chips), dl("region-list", regions)),
  container({ title: "Billing and price" }, tiles, durWrap, h("div", { class: "form-grid" }, fPrice, fCur), retire),
  h("div", { class: "form-actions" }, h("a", { class: "btn link", href: "plans" }, "Cancel"), submit));

  return h("div", {},
    ctx.crumbs([["Taro Operations", "/"], ["Plans", "/plans"], [from ? "New version" : "Create plan"]]),
    pageHeader({ title: from ? `New version of ${from.name}` : "Create plan", desc: from ? "The plan is copied with a new slug. Change what you need; existing rentals keep their price." : "A plan is what customers choose in the catalog: a Mac's specs, a region and a price." }),
    form);
}
