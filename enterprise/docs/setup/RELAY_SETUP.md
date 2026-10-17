# Wizard 2: setting up the relay on your Linux server

The relay package from the package builder (Wizard 1) contains the relay and your provider folder.
On the server:

    scp acme-cloud-relay-0.1.0-linux-x86_64.tar.gz you@relay.acme.example:
    ssh you@relay.acme.example
    tar -xzf acme-cloud-relay-0.1.0-linux-x86_64.tar.gz && cd taro-relay-0.1.0-linux-x86_64
    sudo ./install.sh

`install.sh` installs the core to `/opt/taro/core` and the provider folder to
`/etc/taro/provider`, records the version, then starts the setup. The setup prints:

    From your computer, open an SSH tunnel to this server and then the page:
        ssh -L 7480:127.0.0.1:7480 YOU@relay.acme.example
        http://127.0.0.1:7480/?t=<one-time token>

Run the `ssh -L` line on your computer and open the address. The page is reachable only through
the tunnel, from this server's loopback address, and only with the token. Requests from another
host name, origin or site are refused. Setup ends when you press **Finish** or stop it with
Ctrl+C.

## Requirements

- Linux with systemd (Ubuntu 22.04+, Debian 12+, RHEL 9+ and similar), x86_64 or aarch64 (use
  the package for your CPU), root access through sudo.
- The relay host name (from your provider package) has an A or AAAA record pointing at this
  server.
- Inbound: the relay port TCP and UDP (7470 by default), and 80/tcp and 443/tcp for the
  certificate and the status page. Outbound HTTPS: Let's Encrypt and GitHub (updates).

## The steps

1. **Provider:** what the package carries (product, relay address, colour, installed version).
   The address is fixed: your clients were built for it.
2. **This server:** OS, CPU, root, systemd, free disk space, and whether the ports are free.
   Ports held by a running Taro relay are fine.
3. **DNS & network:** the relay host's records compared with this server's public address, plus
   the CA and the update source.
4. **Certificate:** Let's Encrypt (HTTP-01 on port 80, renewed by the relay 30 days before
   expiry, swapped in without a restart), your own PEM files (checked: they match each other,
   they cover the host, they have not expired), or no HTTPS. On a later run, a valid certificate
   is kept.
5. **Firewall:** ufw, firewalld or iptables, with the rules the relay needs. **Open the ports**
   applies them after you confirm. Your hosting provider's firewall or security group cannot be
   seen from the server: open the same ports there.
6. **Updates:** manual, check and tell, or install inside a maintenance window (UTC).
7. **Review & apply:** creates the `taro-relay` account (if missing), sets owners and modes (the
   admission key in provider.json is readable by root and the relay only), writes
   `/etc/taro/runtime/relay.toml` (the previous one is kept as `relay.toml.<time>.bak`; your own
   comments and extra settings are kept), installs `taro-relay.service` and `taro-update.timer`,
   starts the relay, enables it at boot, and checks that it answers.
8. **Done:** the address, the status page, useful commands.

If a step fails, the page says which one and why, nothing after it is done, and **Download
diagnostics** gives the checks, the log and the last lines of the relay's journal (with the
admission key removed). Fix the cause and apply again: setup can be run any number of times.

## Afterwards

    taro-relay status                 # versions, provider, settings, available updates
    journalctl -u taro-relay -f       # the relay's log
    sudo taro-relay setup             # change settings (data, certificate and provider folder are kept)
    sudo taro-relay update check      # see docs/operations/UPDATES_AND_RECOVERY.md

The service runs `taro-relay run` as the `taro-relay` user, with only the capability to bind
ports 80 and 443, a read-only system, and write access to `/var/lib/taro/relay` and
`/var/lib/taro/tls` only. Without settings it exits with status 78 and systemd does not restart
it in a loop. The health endpoint `127.0.0.1:7479/healthz` is reachable from the server only.
