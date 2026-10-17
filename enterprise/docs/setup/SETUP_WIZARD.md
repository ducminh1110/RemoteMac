# Taro Enterprise Setup (`TaroEnterpriseSetup.exe`): the package builder (Wizard 1)

Source: `enterprise/apps/setup-wizard/` (Rust package and binary `taro-enterprise-setup`; the
Windows release renames it `TaroEnterpriseSetup.exe`).

It builds a provider's own Taro: the standard Taro client for Windows and the Mac agent, plus
the Linux relay package, each with the provider's `provider/` folder added (name, colour, logo,
icon, relay, admission key, optional sign-in portal). It compiles nothing and changes no
binary: the programs are the signed, version-pinned official files of a Taro Enterprise
release.

## A Windows app

On Windows it is an app with its own window: the wizard's pages are shown in a WebView2 control
(the Microsoft Edge WebView2 Runtime, part of Windows 11 and current Windows 10). There is no
browser tab and no console window.

- The pages come from a server inside the program, on `127.0.0.1` and a random port, with a
  one-time 32-byte access token. The window loads only that origin. Links anywhere else (a
  release on GitHub, a support page) open in the default browser, and the page cannot open
  new windows.
- Closing the window ends the program. The packages already built stay in the output folder,
  and nothing else is saved.
- Without WebView2, a message says so, with the download address, and the wizard opens in the
  default browser instead. A second message box keeps it running; closing that box quits.
- `--browser` opens the default browser on purpose. `--no-browser` only prints the link. Both
  print to the terminal they were started from. `--port`, `--output`, `--help` and `--version`
  work as their names say.

On macOS and Linux (development) it opens the browser.

CI (`.github/workflows/enterprise.yml`, job `setup-wizard`, on `windows-latest`) builds the
exe and runs `enterprise/scripts/windows-setup-app.ps1`. That script checks four things:
- the "Taro Enterprise Setup" window appears;
- the page is drawn in it (it saves `setup-app-window.png`, kept as the
  `TaroEnterpriseSetup-window` artifact);
- closing the window ends the program;
- it ends within 15 seconds.

## The steps

The steps are shown in the shared console look of `enterprise/packages/console` (see its
README): wizard steps with their state in the side navigation, page headers, containers,
tables, status indicators, field descriptions and errors, and a Previous/Next footer.

1. **Preflight**: the OS, HTTPS reachability of the download hosts, free disk space for the
   output, and that the server listens on loopback only.
2. **Product identity**: provider id (`acme-cloud`), product name, short name (file names),
   vendor, and optional support address and email. rm-brand validates each field. An existing
   `provider.json` can be imported for a rebuild; this keeps the same admission key.
3. **Branding**:
   - Primary colour: 14 presets (Blue, Indigo, Purple, Violet, Teal, Cyan, Green, Emerald,
     Orange, Amber, Red, Rose, Pink, Slate) or a hex code (6 digits, with or without `#`, any
     case). A typed colour is kept exactly and never snapped to a preset.
   - **Customize…** opens a picker: a colour grid, a shade area (saturation and brightness,
     by mouse or arrow keys), hue, hex and the system picker, with a before/after view.
     **Apply** commits the colour; **Cancel** or Escape restores the previous one.
   - The live preview shows the client window and the relay console in light and dark mode.
     It uses rm-brand's derived tokens (hover, pressed, border, soft, dark-mode variants, text
     on the colour), and the contrast is reported against WCAG AA.
   - Also on this step: the theme, and logo/icon PNG uploads (type, size and dimensions are
     checked).
   - The colour is written as `branding.primary_color`, with the tokens derived from it.
4. **Relay endpoint**:
   - the relay's DNS name and port (TCP and UDP);
   - the client admission key: generated (32 characters) or an existing one entered for a
     rebuild. It is masked, with Reveal and Copy;
   - **Sign-in and machine management** (optional): the provider's Taro Enterprise portal,
     written as `account.portal`. With it, users sign in from the client and see the Macs they
     rented or were given. Without it, the client works as standard Taro does, with ID and
     password;
   - client updates: notify or off.
5. **Signing and release** (optional; for your own Enterprise releases):
   - Connect with a GitHub token of the repository that holds the release workflow. A
     fine-grained token needs Contents, Actions and Secrets (read and write). The token stays in
     memory and is sent only to `api.github.com`.
   - The wizard shows whether `TARO_ENTERPRISE_SIGNING_KEY` is set, the keys in
     `enterprise/keys/release-signing.pub`, the existing `enterprise-v…` releases, and the
     version of `enterprise/Cargo.toml`, which the workflow releases.
   - **Start** (after a confirmation listing every action) does all of it, free on GitHub
     Actions:
     1. makes an ed25519 signing key on this PC;
     2. stores its private half as the Actions secret, sealed with libsodium for the
        repository (only Actions can read it);
     3. commits its public half to `release-signing.pub`; next to the old keys when releases
        exist, since relays installed from them trust only the keys they were built with;
     4. backs the key up at `%LOCALAPPDATA%\Taro Enterprise\signing`, encrypted with DPAPI for
        this Windows account;
     5. starts `enterprise-release.yml` and follows its run to the end, linking to it.
   - An existing secret is never replaced silently. With the backup of this PC the same key is
     used again. Otherwise the secret is kept, unless **Make a new signing key** is ticked.
     taro-update refuses to sign with a key that is not in `release-signing.pub`, so a
     mismatch fails the run instead of publishing a release nobody trusts.
   - The private key is never shown, logged, exported in the summary, put in a package or sent
     unsealed.
6. **Components and versions**:
   - lists the `enterprise-v…` releases on GitHub;
   - verifies the chosen one's `taro-enterprise-components.json` signature with the release
     key compiled into the wizard;
   - releases are looked up in the repository of step 5 when you connected one; its keys are
     trusted in addition to the wizard's own;
   - shows the pinned Enterprise, Taro and relay versions;
   - lets you choose the platforms to build.
7. **Build**:
   1. Every download URL is checked against the download policy (https, allowed hosts, no
      redirect elsewhere).
   2. Each file is streamed with its size and sha256 enforced.
   3. The packages are assembled in `<name>.partial/` with `provider/`,
      `provider-manifest.json`, `SHA256SUMS` and `INSTALL.md`.
   4. Only a complete build is renamed to its final name.

   Cancel or a failure leaves no output.
8. **Validate and export**:
   - Every produced file is opened again and compared with what the build meant to write
     (26 checks for four platforms): the provider folder byte for byte, each archive's
     layout, the programs against the verified official files, the manifest and checksums.
   - Exports: a JSON or Markdown summary (the key masked), opening the output folder, and the
     diagnostic log.

## Security

- The guard on every request (`src/server.rs`):
  - `Host` must be `127.0.0.1:PORT`;
  - `Origin`, when present (and always for writes), must be the same origin;
  - `Sec-Fetch-Site` must be same-origin or none;
  - API calls carry the token in `X-Setup-Token` (constant-time compare);
  - responses get a strict CSP (scripts and styles from the page's own origin only),
    `no-referrer`, `DENY` framing, `no-store` and `nosniff`.
- Secrets (the admission key) are a `Secret` type. It cannot be serialized, is redacted in
  `Debug` and is zeroed on drop. The UI only ever sees the masked key, unless the operator
  presses Reveal or Copy. A response filter scrubs held secrets as a last line of defence, and
  the tests assert it never has to act.
- No server administration credential, TLS private key, database password or signing key is
  ever part of a package.

## Tests

`cargo test -p taro-enterprise-setup` (from `enterprise/`) runs the unit tests and the
integration tests against a local fake of the GitHub releases API and the asset hosts
(`tests/common/mod.rs`):

- `tests/build_flow.rs`: the whole flow through the API. Every expected file is produced, the
  provider folder is byte-identical everywhere, and the official programs are unchanged. It
  also checks the refusals: a tampered download, a bad signature, a host outside the policy,
  a redirect outside it, and cancel.
- `tests/inputs_api.rs`: rm-brand's validation per field, the colour rules, images, the key,
  and importing a provider.json.
- `tests/preservation.rs`: an Enterprise update applied over a relay installed from a built
  package leaves its `provider.json` byte-identical.
- `tests/signing_flow.rs`: step 5 against a fake GitHub:
  - the sealed secret opens (only with the repository's key) to a key whose public half was
    committed, next to the old key;
  - the workflow is started with the version and followed to its end;
  - the backup is the owner's only;
  - a second session reuses the key without replacing the secret;
  - an unknown existing secret is kept;
  - a failed run is reported with its link;
  - neither the key nor the token appears in any response, the log, the state, the summary,
    or any request body except sealed.
- `tests/guard.rs`: the request guard and the security headers.
- `tests/ui_demo.rs` (ignored) serves the real UI against the fake. `tests/ui/wizard1.mjs`
  drives it in headless Chromium and photographs every step:
  - presets, valid and invalid hex input;
  - Customize (grid, shade drag, Cancel restores, Apply commits);
  - the preview uses the server's tokens;
  - the build and validation;
  - no console errors, no CSP violations, no horizontal scroll at 390 px.
