# Taro Enterprise console

The shared look of the Enterprise administration pages: the relay setup (Wizard 2), the relay
status page and the package builder (Wizard 1). A management console in the manner of the AWS
console and OpenStack Horizon:

- a dark top bar (provider logo and name, service name, context such as the host and a mode badge),
- side navigation, or wizard steps (`.wizard-steps`) with their state (done, warning, failed),
- breadcrumbs and a page header (step, title, description, actions),
- containers (`.container` with `.c-head` / `.c-body` / `.c-foot`), key-value pairs (`.kv`),
- tables (`.table`) with status indicators (`.status ok|warn|fail|pending|skip`) and badges,
- alerts in the page (`.alert ok|warn|err|info`) and a flash bar for notifications,
- form fields with label, description, hint and error, radio tiles, buttons, a confirmation dialog,
- the wizard footer (`.form-actions`: Previous, the step's actions, Next).

Files:

- `console.css`: the styles. Colours are CSS variables; every accent uses `--primary*`, which
  `/brand/tokens.css` (generated from the provider's colour in provider.json and loaded after this
  file) overrides. Light and dark follow the system.
- `console.js`: DOM helpers (`h`, `container`, `kv`, `status`, `alert`, `pageHeader`, `button`,
  `busy`, `flash`, `confirmDialog`, `download`). No framework, no inline styles or scripts: the
  pages' Content-Security-Policy allows their own origin only. Icons are CSS masks of `data:` SVGs.

These are shared core files: the relay ships them in `core/web/relay/common/` (updated by
enterprise-update.zip); the package builder embeds them. A provider's own changes belong in
`theme.css` of the provider folder, never here.
