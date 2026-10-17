// Relay status page: what is public about this relay (never its key).
import { startGlass } from "/common/taro-glass.js";
startGlass(document.getElementById("stage"));
const $ = (id) => document.getElementById(id);
(async () => {
  try {
    const [info, health] = await Promise.all([fetch("/.well-known/taro/relay.json").then((r) => r.json()), fetch("/healthz").then((r) => r.json())]);
    $("product").textContent = info.product;
    document.title = `${info.product} relay`;
    $("addr").textContent = `${info.host}:${info.port}`;
    $("version").textContent = info.version;
    $("vendor").textContent = info.vendor;
    const up = health.status === "ok";
    $("state").className = "state " + (up ? "up" : "down");
    $("state-text").textContent = up ? "Online" : "Not answering";
  } catch {
    $("state").className = "state down";
    $("state-text").textContent = "Not answering";
  }
})();
