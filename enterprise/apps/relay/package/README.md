# Taro relay (Taro Enterprise)

This package installs the relay server of a Taro Enterprise provider on Linux (systemd).

    tar -xzf <package>.tar.gz && cd <package>
    sudo ./install.sh

`install.sh` copies the shared core to `/opt/taro/core`, the provider folder (`provider/`, written
by the Taro Enterprise package builder) to `/etc/taro/provider`, and starts the first-run setup
(`taro-relay setup`, Wizard 2). The setup page listens on 127.0.0.1 only; open it from your
computer through an SSH tunnel, as the setup prints.

| What | Where | Changed by |
|---|---|---|
| Shared core (program, pages) | `/opt/taro/core` | `enterprise-update.zip` |
| Provider customization | `/etc/taro/provider` | you (a new package from the package builder) |
| This server's settings | `/etc/taro/runtime/relay.toml` | `sudo taro-relay setup` |
| Data, certificates, update backups | `/var/lib/taro` | the relay |

Upgrades: `sudo taro-relay update check`, then `sudo taro-relay update apply --latest --yes`
(signed, checked, backed up, rolled back on failure). Recovery: `sudo ./install.sh --recover`.
