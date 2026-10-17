// Relay status page: what is public about this relay (never its key).
const $ = (id) => document.getElementById(id);
const ICON = { ok: "i-ok", err: "i-err", pending: "i-pending" };

function setState(kind, text) {
  const s = document.createElement("span");
  s.className = "status " + kind;
  const i = document.createElement("span");
  i.className = "i " + ICON[kind];
  i.setAttribute("aria-hidden", "true");
  s.append(i, text);
  $("state").replaceChildren(s);
}

async function load() {
  setState("pending", "Checking…");
  try {
    const [info, health] = await Promise.all([
      fetch("/.well-known/taro/relay.json", { cache: "no-store" }).then((r) => r.json()),
      fetch("/healthz", { cache: "no-store" }).then((r) => r.json()),
    ]);
    $("product").textContent = info.product;
    $("crumb").textContent = info.product;
    document.title = `${info.product} relay status`;
    $("host").textContent = info.host;
    $("addr").textContent = `${info.host}:${info.port}`;
    $("version").textContent = info.version;
    $("vendor").textContent = info.vendor;
    if (info.support_url && /^https:\/\//.test(info.support_url)) { $("support").href = info.support_url; $("support").hidden = false; }
    const up = health.status === "ok";
    setState(up ? "ok" : "err", up ? "Operational" : "Not answering");
  } catch {
    setState("err", "Not answering");
  }
  $("checked").textContent = `Last checked ${new Date().toLocaleTimeString()}`;
}

$("refresh").addEventListener("click", load);
load();
