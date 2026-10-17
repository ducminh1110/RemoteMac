# Taro Enterprise Setup Wizard (`TaroEnterpriseSetup.exe`)

Source: `enterprise/apps/setup-wizard/` (Rust package and binary `taro-enterprise-setup`; the
Windows release renames the binary to `TaroEnterpriseSetup.exe`).

This page describes what is implemented today. Anything marked **Planned** does not exist yet.

## Running it

```sh
cargo run --manifest-path enterprise/apps/setup-wizard/Cargo.toml --release
# options: --no-browser, --port <PORT>, --help, --version
```

The wizard:

1. binds `127.0.0.1` on a random free port (or `--port`), never another interface;
2. generates a one-time 32-byte random access token;
3. prints `http://127.0.0.1:PORT/?t=TOKEN` and opens it in the default browser
   (Windows: `ShellExecuteW`; macOS: `open`; Linux: `xdg-open`);
4. serves an embedded single-page UI (HTML/CSS/vanilla JS, embedded with `include_str!`,
   no npm build).

Quit with **Quit wizard** in the UI or Ctrl+C in the console window. Everything the wizard
holds (tokens, keys, uploaded images, the log) lives in memory and is gone when it exits,
except the optional DPAPI-protected GitHub token described in
[GITHUB_BOOTSTRAP.md](GITHUB_BOOTSTRAP.md).

Environment variables:

| Variable | Purpose |
|---|---|
| `TARO_SETUP_GITHUB_CLIENT_ID` | OAuth/GitHub App client id for the device flow (can also be typed in the UI) |
| `TARO_SETUP_TEMPLATE` | Template repository, default `ducminh1110/taro-enterprise-template` |
| `TARO_SETUP_MANAGED_ZONE` | DNS zone for Taro-managed subdomains; without it that option shows "Needs config" |

Release builds always talk to `https://api.github.com`, `https://github.com` (device flow
only) and `https://api.stripe.com`; base URLs can only be changed by code (the tests do this).

## Local server security

Every request passes a guard (`src/server.rs`):

- `Host` must be exactly `127.0.0.1:PORT` (DNS-rebinding guard); otherwise `421`.
- `Origin`, if present, must be `http://127.0.0.1:PORT`; for non-GET API calls it is required.
- `Sec-Fetch-Site`, if present, must be `same-origin` or `none`.
- API calls (`/api/*`) need the access token in `X-Setup-Token` (constant-time compare);
  the page `/` needs it as `?t=`. Static assets carry no data and need only the Host check.
- Responses carry `Content-Security-Policy` (`script-src 'self'`, no inline script,
  `frame-ancestors 'none'`), `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`,
  `Cache-Control: no-store`, `nosniff`, COOP/CORP same-origin.
- Request bodies are limited to 64 KiB (2 MiB for image uploads).
- A response middleware scrubs any secret the wizard holds from every API response body as a
  safety net; the tests assert it never has to act.

The UI builds all DOM with `createElement`/`textContent` (no `innerHTML` with data).

## Secrets

- Held in a `Secret` type (`src/secret.rs`): no `Display`, no `Serialize`, `Debug` prints
  `[REDACTED]`, the buffer is zeroed on drop. State structs mark secret fields
  `#[serde(skip)]`; the compiler rejects a `Serialize` derive that forgets one.
- Never logged, never written to disk (except the opt-in DPAPI token), never put in a URL or a
  command line, never echoed to the UI. The UI only learns facts ("authenticated as octo",
  "key valid for acct_...", "webhook secret stored").
- Each credential is sent only to its own service: the GitHub token only to the GitHub API
  base, the Stripe key only to the Stripe API base. The API client does not follow redirects,
  so a credential cannot be forwarded by a redirect; job-log redirects are followed manually
  without credentials.
- Best effort: request bodies that carried a secret are freed by the HTTP stack without
  zeroing; the copies the wizard makes are zeroed.

## Diagnostic log

An in-memory log (max 2000 entries, `src/redact.rs`), downloadable from the UI
(**Diagnostic log**). Every entry is scrubbed twice: exact matches of every secret the wizard
has seen, and pattern redaction of anything token-shaped (`ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`,
`github_pat_`, `sk_live_`, `sk_test_`, `rk_live_`, `rk_test_`, `whsec_`, `Bearer ...`,
`Authorization: ...`, `access_token=`/`device_code=`/`client_secret=` fields).

## The steps

Each step shows only real results. A step whose dependency is missing shows **Needs config**
or **Skipped**, never success.

### 1. Preflight

- OS name/version/build: `RtlGetVersion` on Windows (warns below build 17763), `uname` elsewhere.
- HTTPS reachability of `https://api.github.com/` and `https://api.stripe.com/v1/` with a
  10 s timeout. Any HTTP response over verified TLS counts (Stripe answers 401/404 without a
  key). Failures get hints (proxy/TLS interception, timeout, DNS). A Stripe failure is a
  warning because Stripe is only needed for billing.
- Free disk space on the temp-dir volume (`GetDiskFreeSpaceExW` / `statvfs`); warns below 200 MB.
- The listener address is loopback.

### 2. Deployment mode

Environment (development / staging / production), profile (only
"Single server with Docker Compose"; other profiles **Planned**), public portal URL, ops
console URL, provider adapter (only `mock` exists and is shown as such; real adapters
**Planned**), bootstrap admin email. URLs must be absolute http(s) without credentials, query
or fragment; `https://` is required for staging and production. Per-field errors are shown.

### 3. Product identity

Deployment name, owner/operator name, support contact (email or `https://` URL), modules
(`my_macs`, `rentals`, `billing`, `team`; at least one). Disabling `billing` makes step 6
"Skipped" and the Stripe secrets are not set.

### 4. Branding

- Logo and favicon upload: type decided by magic bytes only; PNG, JPEG, WebP accepted; SVG,
  any text/markup, CSS, JS, GIF rejected; max 1 MB; dimensions parsed from the headers
  (PNG IHDR, JPEG SOFn, WebP VP8/VP8L/VP8X), 16x16 to 1024x1024.
- Primary colour `#RRGGBB`; WCAG contrast against white computed; below 4.5:1 a warning says
  it must not be used for text (the step is shown as "warning", not failed).
- Theme light/dark, density comfortable/compact, live preview card.

### 5. Domain & TLS

- Strategy: Taro-managed subdomain (needs `TARO_SETUP_MANAGED_ZONE`; the managed DNS API that
  would create the records automatically is **Planned**) or customer-owned domain.
- Server target: IPv4 (A), IPv6 (AAAA) or host name (CNAME). Loopback is refused.
- Records shown: the A/AAAA/CNAME and `TXT _taro-verify.<domain>` = `taro-verify=<random 16-byte hex>`.
- **Verify** resolves DNS for real with `hickory-resolver` (system resolver config, Cloudflare
  fallback; a fresh resolver per check): the TXT value must match; the A/AAAA must contain the
  IP; a CNAME must point at the host (a flattened CNAME whose addresses equal the target's is
  accepted). Only then `GET https://<domain>/healthz` must return 200 over a trusted
  certificate. TLS itself is done on the server by Caddy (on-demand TLS); the wizard only
  verifies. The step is complete only when both DNS and HTTPS pass.

### 6. Billing (Stripe)

- Secret key: `sk_test_`/`rk_test_` accepted; `sk_live_`/`rk_live_` only when "Use live mode"
  is ticked **and** `LIVE` is typed; publishable keys and malformed keys rejected locally
  (they are never sent to Stripe). Validated with `GET /v1/account`.
- Webhook signing secret: must be `whsec_` + at least 16 characters.
- Webhook endpoint (`<portal>/webhooks/stripe`): **Check existing endpoint** lists
  `GET /v1/webhook_endpoints` and reports missing events; **Create endpoint** (explicit
  confirmation) calls `POST /v1/webhook_endpoints` with the events
  `checkout.session.completed, checkout.session.expired, invoice.paid, invoice.payment_failed,
  customer.subscription.updated, customer.subscription.deleted, charge.refunded`, and never
  creates a duplicate. The signing secret Stripe returns on creation is captured in memory.
- Smoke test (test mode only, explicit confirmation): creates a Checkout Session for 1 x
  100 minor units (`price_data`, USD), shows the returned id and URL, then expires it with
  `POST /v1/checkout/sessions/{id}/expire`. Passes only if the id is `cs_test_...`, a URL is
  returned, `livemode` is false, and the session is expired.

### 7. GitHub bootstrap

See [GITHUB_BOOTSTRAP.md](GITHUB_BOOTSTRAP.md).

### 8. Build & release

- Requires a completed bootstrap. If a release for the version already exists with assets, it
  is shown and nothing is dispatched.
- Snapshots existing run ids, then `POST /repos/{o}/{r}/actions/workflows/enterprise-release.yml/dispatches`
  with `ref` = default branch and `inputs.version` (default `0.1.0`).
- Finds the new run (not in the snapshot, `created_at` no older than 2 minutes before
  dispatch), polls it every 5 s (job and step status), fetches the tail (60 lines) of each
  completed job's log and redacts it.
- Shows the release only after `GET /repos/{o}/{r}/releases/tags/{version}` returns a
  non-draft release with at least one asset (waits up to 3 minutes after the run succeeds).
- **Retry** re-dispatches; refused while a run is being followed.
- Release asset download/installation on the server: **Planned**.

### 9. Verification

Against the saved portal URL: `GET /healthz` (200), `GET /readyz` (200 = database migrated),
`GET /api/v1/auth/providers` (must list at least one login method; accepts a list or
`{ "providers": [...] }`), Stripe key re-check (`GET /v1/account`, or "skipped" without a
key), domain TLS (`GET https://<domain>/healthz`, or "skipped" without a domain).

Exports: **Download JSON** / **Download Markdown** deployment summary (settings, step
statuses, repository, release, check results; no secrets).

## UI

Left step list with honest status icons (and screen-reader text), main panel per step,
Back/Next. Light mode by default with a dark-mode toggle. Translucent ("glass") surfaces only
for the top bar and step list (with solid fallbacks when `backdrop-filter` is unsupported or
`prefers-reduced-transparency` is set); forms and results on solid high-contrast cards.
Keyboard accessible (native buttons/inputs, visible focus ring, skip link, `aria-live`
announcements, `aria-invalid`/`aria-describedby` on fields), `prefers-reduced-motion`
respected, responsive down to phone width.

## Tests

`cargo test --manifest-path enterprise/apps/setup-wizard/Cargo.toml` runs unit tests plus
integration tests against a local axum fake of GitHub, Stripe and the portal
(`tests/common/mod.rs`):

- `tests/flow.rs`: all nine steps end to end; sealed secrets decrypt to the right values;
  committed files contain no secrets; release shown only after it has an asset; log tail
  redacted; no credential in any response, the diagnostic log or the summary; each credential
  sent only to its own service; log download fetched without credentials; non-template
  template fails before any repository is created; bad tokens are rejected and not kept.
- `tests/guard.rs`: Host/Origin/token/Sec-Fetch-Site rejections, security headers.
- `tests/validation_api.rs`: SVG/CSS/JS/oversize uploads, colours, live-key rules (a rejected
  live key never leaves the PC), webhook secret format, deployment/identity/domain validation.
- `tests/device_flow.rs`: `authorization_pending`, `slow_down` (interval grows by 5 / to
  GitHub's value), success, expiry, denial, cancel, and the API path.
- `tests/ui_demo.rs` (ignored): serves the real UI against the fake for manual clicking:
  `cargo test --manifest-path enterprise/apps/setup-wizard/Cargo.toml --test ui_demo -- --ignored --nocapture`.

## Planned

- Code signing, version resource and icon for `TaroEnterpriseSetup.exe`; release workflow for it.
- Deployment profiles other than single-server Docker Compose; real provider adapters.
- Taro-managed DNS API (automatic records for managed subdomains).
- Installing the release on the server (the wizard verifies a deployed portal but does not deploy).
- Resuming a wizard session after it is closed (state is in memory only).
