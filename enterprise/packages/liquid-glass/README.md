# Liquid Glass for Taro Enterprise web pages

- `liquidglass.js`: [@ybouane/liquidglass](https://github.com/ybouane/liquidglass) 1.0.3, vendored as
  built (no CDN: the pages' Content-Security-Policy allows scripts from their own origin only).
  WebGL refraction, blur, chromatic aberration, Fresnel and specular light on HTML elements.
- `taro-glass.js`: Taro's layer over it: paints the wallpaper from the provider's colour tokens,
  applies Taro's glass presets (bar, sidebar, panel, card) for light and dark mode, and falls back
  to CSS frosted glass where WebGL is missing or the user prefers reduced transparency/motion.
- `taro-glass.css`: the shared look (typography, the glass window layout, controls), using only the
  colour variables of `/brand/tokens.css`, so a provider's colour carries through and updates of
  these files never reset it.

These are shared core files: the relay ships them in `core/web/relay/common/` (updated by
enterprise-update.zip); the package builder embeds them.

Updating the library: build it from a pinned commit as above, keep the header comment and
LICENSE.md, and check the pages in a browser (the button mode injects a `<style>` element, which
the CSP refuses: Taro does not use it).
