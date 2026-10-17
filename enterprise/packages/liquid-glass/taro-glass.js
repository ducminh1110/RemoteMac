// Taro's Liquid Glass for web pages: a wallpaper in the provider's colours behind glass panels.
//
//   <div class="stage" id="stage">
//     <canvas class="wallpaper"></canvas>
//     <header class="glass" data-glass="bar">…</header>
//     <main class="glass" data-glass="panel"><div class="scroll">…</div></main>
//   </div>
//   import { startGlass } from "/common/taro-glass.js"; startGlass(document.getElementById("stage"));
//
// The glass elements must be direct children of the stage. Without WebGL, or when the user asks
// for reduced transparency or motion, the CSS frosted glass of taro-glass.css stays.
import { LiquidGlass } from "./liquidglass.js";

const PRESETS = {
  // the window's toolbar: clear, strongly refractive, like Taro's toolbar
  bar: { blurAmount: 0.18, refraction: 0.75, chromAberration: 0.04, edgeHighlight: 0.18, specular: 0.25, fresnel: 0.9, cornerRadius: 22, zRadius: 22, shadowOpacity: 0.22, shadowSpread: 18, shadowOffsetY: 6 },
  // the sidebar and the content: frosted enough for text
  side: { blurAmount: 0.5, refraction: 0.55, chromAberration: 0.03, edgeHighlight: 0.14, specular: 0.15, fresnel: 0.8, cornerRadius: 22, zRadius: 26, shadowOpacity: 0.2, shadowSpread: 22, shadowOffsetY: 8 },
  panel: { blurAmount: 0.62, refraction: 0.45, chromAberration: 0.025, edgeHighlight: 0.12, specular: 0.12, fresnel: 0.75, cornerRadius: 24, zRadius: 30, shadowOpacity: 0.22, shadowSpread: 26, shadowOffsetY: 10 },
  card: { blurAmount: 0.55, refraction: 0.6, chromAberration: 0.03, edgeHighlight: 0.16, specular: 0.2, fresnel: 0.85, cornerRadius: 26, zRadius: 28, shadowOpacity: 0.25, shadowSpread: 24, shadowOffsetY: 10 },
};
// light glass brightens, dark glass darkens what is behind it
const MODE = {
  light: { brightness: 0.07, saturation: 0.3, tintStrength: 0 },
  dark: { brightness: -0.22, saturation: 0.15, tintStrength: 0 },
};

const dark = () => matchMedia("(prefers-color-scheme: dark)").matches;

function hexToHsl(hex) {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return [262, 0.75, 0.6];
  const v = parseInt(m[1], 16);
  const [r, g, b] = [(v >> 16) / 255, ((v >> 8) & 255) / 255, (v & 255) / 255];
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min, s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [h * 60, s, l];
}

const hsl = (h, s, l, a = 1) => `hsla(${((h % 360) + 360) % 360}, ${Math.round(s * 100)}%, ${Math.round(l * 100)}%, ${a})`;

/** The wallpaper: soft, large shapes in the primary colour and its neighbours (macOS-like). */
export function paintWallpaper(canvas) {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const w = Math.max(1, Math.round(canvas.clientWidth * dpr)), h = Math.max(1, Math.round(canvas.clientHeight * dpr));
  canvas.width = w;
  canvas.height = h;
  const c = canvas.getContext("2d");
  const primary = getComputedStyle(document.documentElement).getPropertyValue("--primary").trim() || "#7C4AE8";
  const [hue, sat] = hexToHsl(primary);
  const s = Math.max(0.45, Math.min(0.9, sat));
  const d = dark();
  const base = c.createLinearGradient(0, 0, w, h);
  base.addColorStop(0, d ? hsl(hue + 20, s * 0.5, 0.1) : hsl(hue - 25, s * 0.6, 0.86));
  base.addColorStop(1, d ? hsl(hue - 30, s * 0.55, 0.06) : hsl(hue + 35, s * 0.6, 0.76));
  c.fillStyle = base;
  c.fillRect(0, 0, w, h);
  const blob = (x, y, r, color) => {
    const g = c.createRadialGradient(x * w, y * h, 0, x * w, y * h, r * Math.max(w, h));
    g.addColorStop(0, color);
    g.addColorStop(1, color.replace(/[\d.]+\)$/, "0)"));
    c.fillStyle = g;
    c.fillRect(0, 0, w, h);
  };
  const L = d ? [0.42, 0.34, 0.5] : [0.5, 0.6, 0.66];
  blob(0.12, 0.18, 0.55, hsl(hue, s, L[0], d ? 0.75 : 0.85));
  blob(0.88, 0.12, 0.45, hsl(hue + 38, s * 0.9, L[1], d ? 0.6 : 0.7));
  blob(0.7, 0.92, 0.6, hsl(hue - 42, s * 0.85, L[2], d ? 0.55 : 0.75));
  blob(0.32, 0.78, 0.35, hsl(hue + 180, s * 0.35, d ? 0.3 : 0.9, d ? 0.35 : 0.55));
  // a light sweep, like the glossy band of macOS wallpapers
  const sweep = c.createLinearGradient(0, h * 0.25, w, h * 0.75);
  sweep.addColorStop(0, "rgba(255,255,255,0)");
  sweep.addColorStop(0.5, d ? "rgba(255,255,255,0.05)" : "rgba(255,255,255,0.28)");
  sweep.addColorStop(1, "rgba(255,255,255,0)");
  c.fillStyle = sweep;
  c.fillRect(0, 0, w, h);
}

function configure(el) {
  const p = PRESETS[el.dataset.glass] || PRESETS.panel;
  el.dataset.config = JSON.stringify({ ...p, ...MODE[dark() ? "dark" : "light"] });
}

function webglOk() {
  try {
    const c = document.createElement("canvas");
    return !!(c.getContext("webgl2") || c.getContext("webgl"));
  } catch {
    return false;
  }
}

/** Start the stage: wallpaper always, refracting glass when the browser and the user allow it. */
export async function startGlass(stage) {
  const wall = stage.querySelector(":scope > canvas.wallpaper");
  const glass = [...stage.querySelectorAll(":scope > .glass")];
  const repaint = () => wall && paintWallpaper(wall);
  repaint();
  const reduce = matchMedia("(prefers-reduced-transparency: reduce)").matches || matchMedia("(prefers-reduced-motion: reduce)").matches;
  let inst = null;
  if (!reduce && webglOk() && glass.length) {
    glass.forEach(configure);
    try {
      inst = await LiquidGlass.init({ root: stage, glassElements: glass });
      document.documentElement.classList.add("lg");
    } catch (e) {
      console.warn("Liquid Glass is off (CSS glass instead):", e);
      inst = null;
    }
  }
  let t = 0;
  addEventListener("resize", () => {
    clearTimeout(t);
    t = setTimeout(() => {
      repaint();
      inst && inst.markChanged(wall);
    }, 120);
  });
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    repaint();
    if (inst) {
      glass.forEach(configure);
      inst.markChanged();
    }
  });
  return inst;
}
