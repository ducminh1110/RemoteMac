# Taro Enterprise distribution: packages, setup and updates

Taro Enterprise lets a provider ship its own branded Taro: a Windows client, a Mac client and a
relay server under the provider's name, colour and relay address. Three separate pieces of
software do this, and they are never merged into one:

| | Runs on | Does | Code |
|---|---|---|---|
| **Wizard 1**: package builder | the provider's Windows PC | builds the provider's distribution | `apps/setup-wizard` (`TaroEnterpriseSetup.exe`) |
| **Wizard 2**: relay first-run setup | the provider's Linux server | configures the installed relay | `apps/relay` (`taro-relay setup`) |
| **Release and partial update system** | GitHub Actions; the server | publishes releases and `enterprise-update.zip`; applies it | `.github/workflows/enterprise-release.yml`, `crates/taro-update`, `taro-relay update` |

The standard Taro edition keeps its own full-replacement update (`Taro-Setup.exe` from the
latest Taro release). Enterprise releases are tagged `enterprise-v<version>`, published as
pre-releases with `--latest=false`, and the release workflow checks that Taro's "latest" release
is still a Taro one, so standard clients never see them.

## The three kinds of files

Every file of an installation belongs to exactly one of these kinds. The build, the packages and
the updater enforce the kinds. They are not only a convention.

| Kind | Examples | Where | Who may change it |
|---|---|---|---|
| **A. Shared core** | `taro-relay`, the setup and status pages, the Taro clients' binaries | server: `/opt/taro/core/`; clients: the executable | Taro releases only (`enterprise-update.zip` on servers, Taro releases on clients) |
| **B. Provider customization** | `provider.json` (product, primary colour and derived tokens, relay host/port, admission key, client update policy), `logo.png`, `icon.png`, `theme.css` | server: `/etc/taro/provider/`; clients: `provider/` next to the executable | the provider (a new package from Wizard 1) |
| **C. Persistent runtime state** | `relay.toml` (this server's settings), TLS certificates and keys, the relay's Mac IDs, update backups and history, logs | `/etc/taro/runtime/`, `/var/lib/taro/`, the journal | Wizard 2 and the running relay |

How this is enforced:

- **The updater's path policy is built in** (`crates/taro-update/src/policy.rs`). It does not
  come from the package. A relay update may write only below `core/bin/`, `core/web/` and
  `core/share/` of the installation root. Everything else is refused before anything is
  changed: `/etc/taro`, `/var/lib/taro`, `INSTALLED.json`, absolute paths, `..`, symbolic links
  (in the zip or on the server) and entries a package should not contain. The packer
  (`taro-update pack`) applies the same policy, so a release cannot even be built with a file
  outside it.
- **Core pages hold no branding.** The pages of the relay (`core/web/relay/setup`, `status`) use
  CSS variables only. `/brand/tokens.css` is generated from `provider.json`'s primary colour,
  `/brand/logo.png` and `/brand/icon.png` are the provider's files, and `/brand/theme.css` is the
  provider's own optional stylesheet, loaded last. This is the supported customization interface
  (`apps/relay/src/web.rs`). Replacing every core page therefore cannot reset a provider's look.
  A test applies an update that changes the shared CSS and checks that the served tokens, theme
  and logo are unchanged (`apps/relay/tests/wizard2.rs`).
- **Clients are the official binaries plus a provider folder.** Nothing is patched into the
  binaries: the Windows viewer and the Mac agent read `provider/provider.json` at start
  (`crates/rm-brand`). A client update replaces the executable only, so the folder stays.
- **Hand-edited core files are not overwritten silently.** `INSTALLED.json` records the sha256 of
  every installed core file. If a file was changed on the server, the update stops and names
  it. The fix is to move the change to `/etc/taro/provider/theme.css` (or provider.json). Running
  again with `--overwrite-modified` keeps the changed file in
  `/var/lib/taro/update/conflicts/<version>/` and then updates it.

## provider.json: one file for every component

`crates/rm-brand` defines it (`taro.provider/v1`): `provider_id`, `product` (name, short name,
vendor, support), `branding` (`primary_color` as `#RRGGBB`, the derived `tokens`, theme, logo,
icon), `relay` (host, port, admission key) and `updates.clients` (`notify` or `off`). Wizard 1
writes it once. The same bytes go into the Windows zip, the Mac tarball and the relay package,
and Wizard 1 checks that they are identical. The hover, active, border, soft, on-primary and
dark-mode colours are derived from the primary colour by `Tokens::derive`, which also checks
contrast. Components never hard-code them, and the validation refuses tokens that do not match
the colour.

What it never contains: server administrator credentials, TLS keys, database passwords or
signing keys. The relay admission key is in it because every client of the provider sends that
key, just as standard Taro builds carry Taro's own key.

## Wizard 1: the provider's distribution

See [../setup/SETUP_WIZARD.md](../setup/SETUP_WIZARD.md). In short:

1. It verifies the signed `taro-enterprise-components.json` of the chosen Enterprise release
   against the release key built into the exe.
2. It downloads the exact files listed there: `Taro-windows.zip` and `Taro-macos.tar.gz` of the
   pinned Taro release, and `taro-relay-<v>-linux-<arch>.tar.gz`. It checks each one's sha256.
3. It assembles `windows/`, `macos/` and `linux/` packages, each with the same provider folder,
   plus `provider-manifest.json`, `SHA256SUMS` and `INSTALL.md`.

The Mac package is the official ad-hoc-signed CLI with the provider folder next to it. It is not
re-signed or notarized under the provider's Apple Developer ID, because no such credentials or
infrastructure are available here.

## Wizard 2: the relay's first run

See [../setup/RELAY_SETUP.md](../setup/RELAY_SETUP.md). `install.sh` installs the core and the
provider folder and starts `taro-relay setup`. The setup listens on 127.0.0.1 behind a one-time
token, reached through an SSH tunnel. It checks the server, DNS and network, obtains a
certificate, opens the firewall, writes `relay.toml`, creates the `taro-relay` service account
and the systemd units, and starts and health-checks the relay. Launched again later, it starts
from the saved settings and keeps the data, the certificate and the provider folder.

Wizard 1 gives the intended relay host and port. Wizard 2 checks that the host points at this
server and configures what belongs to this machine: certificates, service and paths. It does
not let the host or port differ from the provider's, because the clients were built for them.

## Releases

`enterprise-release.yml` (dispatched with a version equal to `enterprise/Cargo.toml`'s):

| Artifact | Built from |
|---|---|
| `taro-relay-<v>-linux-{x86_64,aarch64}.tar.gz` | static (musl) `taro-relay`, `scripts/package-relay.sh`; tested by `scripts/test-relay-package.sh` |
| `enterprise-update.zip` | the packages' `core/`, compared with the previous release's `enterprise-core-files.json`; `scripts/build-enterprise-update.sh` |
| `enterprise-core-files.json` | this release's core (the next update's base) |
| `taro-enterprise-components.json` + `.sig` | the pinned Taro clients (URL, sha256 from GitHub) and the relay packages |
| `TaroEnterpriseSetup.exe` | `apps/setup-wizard`, built on Windows |
| control plane bundle and image | `apps/api` (optional) |
| `taro-enterprise-<v>-full.tar.gz` | all of the above, for fresh installs and recovery |
| `SHA256SUMS` | every asset |

`enterprise-update.zip` contains `manifest.json` (signed: `manifest.json.sig`),
`checksums.sha256`, `payload/{any,x86_64,aarch64}/core/…` holding only the core files that are
new or changed since the base release, and `migrations/` when a release needs one. The manifest
names the product and component, the version, the installed versions it applies to
(`min_from_version`..`max_from_version`), the permitted paths, each file's operation, sha256,
size, mode and architecture, the removals, the migrations and the services to restart.

Signing: ed25519. The private key exists only as the `TARO_ENTERPRISE_SIGNING_KEY` secret of
the release workflow. The public key is `enterprise/keys/release-signing.pub`, built into
`taro-relay` and `TaroEnterpriseSetup.exe`. A checksum shows a file is intact; the signature
shows it is Taro's. To rotate the key, add the new public key to that file and ship one release
signed by the old key, then switch the secret.

## Applying an update (`taro-relay update`)

`crates/taro-update/src/apply.rs`, in order:

1. Recover first: if a journal says an earlier update was interrupted, roll that one back.
2. Open the package: only the allowed entry names, no links, bounded sizes; the manifest's
   signature by a trusted key; every path against the built-in policy; `checksums.sha256`.
3. Compatibility: the same product and component, and an installed version inside the update's
   range. Otherwise it is refused, naming the updates in between, or the full bundle for a jump
   too large.
4. Conflicts with hand-edited core files (see above).
5. Staging: the files are extracted next to the core (same file system), and each is checked
   against its sha256 and size.
6. Backup of every file that will change, of `INSTALLED.json`, and of the runtime files a
   migration touches. A journal is written.
7. Swap: one atomic rename per file, then the removals, the migrations (they only add missing
   settings, never replace the operator's) and the new `INSTALLED.json`.
8. Restart only the services the manifest names, and check health: the health endpoint must
   answer with the new version, and the relay port must refuse joins without the key.
9. If any step fails: restore the backup, restart, check that the old version is healthy, and
   record the result. It is never marked successful before the health check passes, and a
   failure never falls back to a full-directory replacement.

Discovery and policy: `taro-relay update check` lists the newer `enterprise-v*` releases and the
chain of updates that applies to this server. `relay.toml`'s `[updates] mode` selects `manual`,
`notify` (the hourly `taro-update.timer` records what is available) or `auto` (it also installs,
only inside `window`, e.g. `Sun 03:00-05:00`, UTC). `taro-relay update rollback` restores the
last update's backup. `taro-relay update status` shows the history
(`/var/lib/taro/update/history.jsonl`).

## Separate components, separate versions

- **Linux relay:** Enterprise version (`taro-relay version`, `INSTALLED.json`), updated by
  `enterprise-update.zip`.
- **Windows and Mac clients:** the Taro version pinned in the components manifest. They update in
  place from Taro releases when `provider.json` says `notify`, replacing only the executable. With
  `off`, the provider ships new packages. An Enterprise update never touches clients.
- **Control plane (`apps/api`), if deployed:** updated by its container image tag or server
  bundle. It is not part of `enterprise-update.zip`.

Clients and relay speak the same relay protocol. A new Taro client release is offered to
providers' clients only as Taro releases are published; a provider who needs a hold sets
`updates.clients` to `off`.

## Fresh install, routine upgrade, recovery

| | How |
|---|---|
| Fresh installation | Wizard 1's packages; on the server, `sudo ./install.sh` (refuses to run over an existing installation), then Wizard 2 |
| Routine upgrade | `sudo taro-relay update apply --latest --yes` (or a downloaded `enterprise-update.zip`) |
| Recovery or major migration | `sudo ./install.sh --recover` from the full bundle's relay package: the damaged core is saved to `/var/lib/taro/recovery/`, the core is reinstalled, `/etc/taro` and `/var/lib/taro` are kept (`--replace-provider` only if asked). See [../operations/UPDATES_AND_RECOVERY.md](../operations/UPDATES_AND_RECOVERY.md). |
