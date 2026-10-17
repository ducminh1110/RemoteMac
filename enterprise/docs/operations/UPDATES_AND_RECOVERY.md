# Updating and recovering a Taro Enterprise relay

## Routine updates

    sudo taro-relay update check
    sudo taro-relay update apply --latest --yes

`check` lists the newer Enterprise releases and the updates that apply to this server, in order
(an update applies to a range of installed versions; older servers get the chain in between).
`apply` restarts the relay (sessions reconnect by themselves), so it asks for `--yes`. To apply a
downloaded file: `sudo taro-relay update apply enterprise-update.zip --yes`.

An update changes only the shared core in `/opt/taro/core`. These stay as they are:
`/etc/taro/provider` (your branding, relay address, key), `/etc/taro/runtime/relay.toml`,
`/var/lib/taro` (Mac IDs, certificates, backups). A new setting a release needs is added to
`relay.toml` by a migration only if it is missing.

Automatic updates: in `sudo taro-relay setup` > Updates, choose **Check and tell me** (the hourly
timer records availability; `taro-relay status` shows it) or **Install in a maintenance window**.

## When an update is refused

| Message | Meaning | Do |
|---|---|---|
| `the signature is not by a trusted Taro release key` | the file is not Taro's | get it from the release page; never bypass this |
| `… its checksum does not match` | damaged or altered | download again |
| `… applies to version X; this server has Y` | too old or too new for this package | `update check` finds the chain; for a large jump, see Recovery |
| `core files were changed on this server …` | someone edited a core file | move the change into `/etc/taro/provider/theme.css` or provider.json, then apply; or `--overwrite-modified` (the edited file is kept in `/var/lib/taro/update/conflicts/<version>/`) |
| `… is outside what a relay update may change` | the package tries to touch something it may not | do not use it; report it |

Nothing changed on the server in any of these cases.

## When an update fails midway

The updater restores the previous core from its backup, restarts the relay and checks it is
healthy, and records the result (`taro-relay update status`). If even the restore fails, it
says so and the backup stays in `/var/lib/taro/update/backups/`. Then run:

    sudo taro-relay update rollback

If the machine stopped during an update (power loss), the next `taro-relay update apply` or
`rollback` rolls the
interrupted update back first.

To undo a successful update: `sudo taro-relay update rollback` (restores the last update's
backup, restarts, checks health).

## Recovery and major migrations

Use the relay package of the full bundle (`taro-enterprise-<v>-full.tar.gz`) or a release's
`taro-relay-<v>-linux-<arch>.tar.gz`, after building it into your provider package with Wizard
1 or reusing the generic package (your provider folder is already on the server):

    tar -xzf taro-relay-<v>-linux-<arch>.tar.gz && cd taro-relay-<v>-linux-<arch>
    sudo ./install.sh --recover

It saves the current core (and its record) to `/var/lib/taro/recovery/core-<time>/`, installs the
package's core, records its version, keeps `/etc/taro` and `/var/lib/taro`, and restarts the
relay. Add `--replace-provider` only to replace the provider folder with the package's (the
current one is saved first). `install.sh` without `--recover` refuses to run over an existing
installation, so a fresh install is never mistaken for an upgrade.

## Backups worth having

`/etc/taro` (provider folder, settings) and `/var/lib/taro/relay/ids.json` (the Mac IDs handed
out). The certificate can always be obtained again. The core comes from the release.
