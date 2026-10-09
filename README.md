<div align="center">

# MacBridge

**Your Mac's apps, as real windows on your Windows PC.**

Open Xcode, Safari or Notes on a Mac and use each one as its own native Windows window,
with its own taskbar button, Start-menu entry, menus and clipboard. Or open the whole Mac
Desktop, streamed the way Moonlight streams a game.

[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
![Windows 10/11](https://img.shields.io/badge/viewer-Windows%2010%2F11-0078D6)
![macOS 14+](https://img.shields.io/badge/host-macOS%2014%2B-000000)
![Rust + Swift](https://img.shields.io/badge/built%20with-Rust%20%2B%20Swift-orange)

</div>

---

## Contents

- [Features](#features)
- [How it works](#how-it-works)
- [Quick start (release builds)](#quick-start-release-builds)
- [Connecting: same network or through a relay](#connecting-same-network-or-through-a-relay)
- [Running your own relay](#running-your-own-relay)
- [Building from source](#building-from-source)
- [Using MacBridge](#using-macbridge)
- [Security](#security)
- [Documentation](#documentation)
- [Project layout](#project-layout)
- [Contributing](#contributing)
- [License and credits](#license-and-credits)

## Features

- **One app, one window.** Every Mac window becomes a native Windows window with Mac-style
  traffic lights, its menu bar, resizing, minimize and fullscreen. Popups, menus and sheets
  appear where the Mac shows them.
- **Mac Desktop mode.** The whole Mac screen in fullscreen, carried by Moonlight's own client
  core (moonlight-common-c) and a host ported from Sunshine. A floating **navigation ball**
  gives you the controls without covering the Mac's menu bar.
- **Sharp picture.** The Mac lays out its screen at your monitor's size and density (Retina on
  HiDPI). Hardware H.264 on the Mac, D3D11 decode and NV12 shader presentation on Windows. An
  optional pixel-for-pixel mode is available too.
- **Low latency.** Video goes over UDP with Reed-Solomon FEC and adaptive bitrate. A direct
  peer-to-peer path is used whenever one exists (LAN, or NAT hole punching), and TCP is the
  fallback.
- **Connect by ID and password**, like TeamViewer or AnyDesk. On the same network the PC finds
  the Mac by its ID and connects to it directly, with no server involved. From anywhere else
  the connection goes through a small relay that you can host yourself.
- **Clipboard sync** of text and images in both directions, with keyboard translation
  (Ctrl ⇄ ⌘) and Unicode text input.
- **Launch any app** in `/Applications`, from the launcher or **MacBridge Search**
  (Ctrl+Alt+Space), with a loading window in the app's own colours. Apps already running are
  adopted, and so are apps opened on the Mac after your click (a document in Finder, the Dock).
- **Sound.** The Mac's sound plays on the PC: low-latency PCM over the same encrypted
  transport, with a jitter buffer that adapts to the network. *(new in 1.2)*
- **Drag and drop to open.** Drop files from Explorer onto a Mac app to open them there.
  Programs and scripts are refused. *(new in 1.2)*
- **Connect by address** too: type the Mac's IPv4 or IPv6 address or name (over a VPN, for
  example), with the same encryption and no relay. *(new in 1.2)*
- **Liquid Glass UI** for MacBridge's own menus, search and banners, with reduced-motion and
  reduced-transparency fallbacks, and three **keyboard modes** (Windows, Mac, Fusion).
  *(new in 1.2)*
- **Desktop Fusion** *(experimental)*: the Mac's real Dock streamed onto the PC's desktop, with
  the Mac's wallpaper set to the PC's while it is on (and always put back).
- **End-to-end encrypted.** A password-authenticated key exchange, then ChaCha20-Poly1305 on
  everything. A relay only carries ciphertext and never learns anything about the password.
- **Quiet by default.** The Mac app shows its ID and password, then runs in the background.
  Neither side writes logs unless you ask for them with `--logs-enabled`.

## How it works

```
                 same network: found by ID (UDP broadcast), then a direct TCP connection
   ┌──────────────────┐ ─────────────────────────────────────────────────▶ ┌──────────────────┐
   │ Windows PC       │                                                    │ Mac              │
   │ MacBridge.exe    │      elsewhere:  ┌──────────────────────┐          │ macbridge        │
   │ (Rust, Win32,    │ ───── TCP ─────▶ │ relay  (rm-relay)    │ ◀─ TCP ─ │ (Swift agent,    │
   │  D3D11)          │ ◀──── UDP ─────▶ │ pairs by ID, forwards│ ◀─ UDP ─▶│  ScreenCaptureKit│
   └──────────────────┘                  └──────────────────────┘          │  VideoToolbox)   │
            ▲                                                              └──────────────────┘
            └──────────── direct UDP video path (P2P), when the networks allow ────────┘
```

- **Agent (Mac):** a terminal-launched executable. It captures each window with
  ScreenCaptureKit, encodes it with VideoToolbox, injects input through Accessibility, and
  reports windows, menus and apps.
- **Viewer (Windows):** one native top-level window per Mac window, drawn with D3D11 or
  DirectComposition. It sends input back and mirrors the clipboard.
- **Relay:** pairs a viewer with a Mac by session ID and forwards bytes it cannot read. It
  also hands out unique Mac IDs. Both sides only connect *out*, so the Mac and the PC open no
  ports to the internet.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the protocol, the encryption and the
connection paths in detail.

## Quick start (release builds)

Download the [latest release](../../releases/latest):

| File | Runs on | What it is |
|---|---|---|
| `MacBridge-windows.zip` | Windows 10/11 x64 | `MacBridge.exe`, the viewer |
| `MacBridge-macos.tar.gz` | macOS 14+, Apple silicon and Intel | `macbridge`, the Mac host |
| `macbridge-relay.tar.gz` | Linux x86_64 / ARM64 | relay server and its one-command installer |

The official release builds come with a public relay preset, so they work out of the box over
the internet. Builds from source have no relay set (see below).

**1. On the Mac**

```bash
tar -xzf MacBridge-macos.tar.gz && cd MacBridge
xattr -d com.apple.quarantine macbridge 2>/dev/null; chmod +x macbridge
./macbridge.sh --password choose-a-password
```

`macbridge.sh` checks the macOS version, Gatekeeper's quarantine mark and the two permissions
first, and says how to fix what is missing (`./macbridge.sh --check` only checks). It then
starts `macbridge` with the same options.

```
  MacBridge is ready — connect from Windows with:
    ID session to connect: 123 456 789
    Password: choose-a-password
    Reachable on this network directly, and from anywhere through the relay …
    Connections are end-to-end encrypted.

  Running in the background (pid 4242). Stop it with: ./macbridge --stop
```

It returns you to the prompt and keeps running in the background, even after the terminal
window is closed. Stop it with `./macbridge --stop`. Use `--foreground` to keep it in the
terminal instead.

The first time, macOS asks to allow your terminal app under **System Settings → Privacy &
Security → Screen Recording** and **Accessibility**. Allow both, then run the command again.
The ID stays the same on that Mac.

**2. On Windows:** run `MacBridge.exe`, type the ID and the password, and press **Connect**
(or choose **Type its address** and type the Mac's address). Pick an app from the launcher, or
choose **Mac Desktop**.

The [user guide](docs/USER-GUIDE.md) covers everything else: options, settings, shortcuts,
logs, troubleshooting and uninstalling.

## Connecting: same network or through a relay

MacBridge picks the path for you:

1. **Same network (LAN).** The viewer broadcasts "who has ID 123456789?" on UDP port
   **7471**. The Mac with that ID answers with its TCP port, and the viewer connects straight
   to it. The Mac checks the password proof itself. No relay or internet connection is
   needed.
2. **Anywhere else.** If no Mac answers on the local network, the viewer goes through the
   relay shown in the **Relay server** field of the connect window (remembered for next
   time). The Mac waits on both paths at once, and the first viewer to arrive gets the
   session.
3. **By address.** With **Type its address** (or `--direct host[:port]`), the viewer connects
   straight to the Mac's TCP port 7471 (or the Mac's `--port`), over IPv4 or IPv6, and never
   uses a relay. Use it over a VPN or a forwarded port. The handshake and encryption are the
   same; there is no unencrypted fallback.

**Where the Mac's ID comes from**

| Mac started with | ID | Reachable |
|---|---|---|
| `--relay host:port` | handed out by **that relay** | on this network, and from anywhere through that relay |
| no `--relay`, build with a preset relay (official releases) | handed out by the **preset relay** | on this network, and from anywhere |
| no `--relay`, build without a preset (from source) | made up by the Mac itself | **on this network only** |

A relay gives each Mac its own ID, which never collides with another Mac's, and lets only that
Mac (proved by a secret kept in `~/Library/Application Support/RemoteMac/owner`) wait under it.
The Mac remembers the ID per relay, so it stays the same across runs. `--id 123456789` sets an
ID by hand.

| Build | Relay preset | Reaching a Mac on another network |
|---|---|---|
| Official release | yes (public relay) | works out of the box |
| From source | **none** | type `host:port` of a relay in the viewer and start the Mac with `--relay host:port` |

```bash
# Mac: reachable on this network, and from anywhere through your relay
./macbridge --password PASS --relay relay.example.com:7470
```

Either side can also take the relay from the `RM_RELAY` environment variable.
`RM_NO_LAN=1` turns off local discovery.

> On the LAN, allow UDP 7471 and TCP 7471 on the Mac if its firewall is on. Video then
> takes a direct UDP path between the two machines.

## Running your own relay

The relay is a single static binary (`rm-relay`). It needs **TCP 7470** (session) and
**UDP 7470** (video) open. On Ubuntu or Debian, from a release:

```bash
tar -xzf macbridge-relay.tar.gz && cd remotemac-relay
sudo ./remotemac-relay-setup.sh --name relay.example.com
```

The script installs the relay as a systemd service, opens the local firewall, generates an
**admission key** (so strangers cannot use your relay), checks it, and prints what to open in
your cloud provider's firewall.

Clients present the key through `RM_RELAY_KEY`, at run time or baked into a build:

```bash
RM_RELAY_KEY=... ./macbridge --relay relay.example.com:7470 --password PASS
```

Other platforms: `cargo build --release -p rm-relay`, then run
`RM_RELAY_KEY=... rm-relay 0.0.0.0:7470`.

The full guide, including troubleshooting, is in [deploy/RELAY-SETUP.md](deploy/RELAY-SETUP.md).

## Building from source

Requirements:

- [Rust](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`)
- **Windows viewer:** Windows 10/11 with the MSVC build tools
- **Mac host:** macOS 14+ with the Xcode command-line tools (Swift 5.9+)
- **Relay:** any OS that Rust supports

```bash
# Windows viewer -> target/release/remote-mac-viewer.exe
cargo build --release -p rm-viewer

# Mac host -> out/remote-agent-mac (+ out/rm-testapp)
./scripts/build-agent-macos.sh

# Relay -> target/release/rm-relay
cargo build --release -p rm-relay

# Tests (portable crates run on any OS)
cargo test --workspace
```

**Presetting a relay in your own builds.** The source contains no relay address. To ship
builds that connect over the internet without typing a relay, set these at build time:

| Variable | Applies to | Effect |
|---|---|---|
| `RM_DEFAULT_RELAY=host:port` | `cargo build -p rm-viewer` / `rm-client` | default relay of the viewer |
| `RM_BUILD_DEFAULT_RELAY=host:port` | `scripts/build-agent-macos.sh` | default relay of the Mac host |
| `RM_RELAY_KEY` / `RM_BUILD_RELAY_KEY` | viewer / Mac host | relay admission key built in |

The **Release builds** workflow (`.github/workflows/release.yml`) does exactly this, using
the repository variable `RM_DEFAULT_RELAY` and the secret `RM_RELAY_KEY`. Pushing a version tag
(`1.0.0` or `v1.0.0`), or running the workflow by hand with a version, runs the tests and
publishes a GitHub release with all three packages.

## Using MacBridge

| Action | How |
|---|---|
| Fullscreen a Mac app | green traffic light, or **F11** |
| Mac Desktop controls | the **navigation ball**: drag it anywhere, click for the menu (exit fullscreen, minimize, open an app, pointer, sound, settings, disconnect) |
| Open or switch to a Mac app by name (MacBridge Search) | **Ctrl+Alt+Space** |
| Open a file on the Mac | drop it from Explorer onto a Mac app's window |
| Settings (frame rate, bitrate, sharpness, screen size, pixel-for-pixel, sound, keyboard, glass, animations, Desktop Fusion) | **Ctrl+Alt+Shift+P** |
| The Mac's sound off / on | **Ctrl+Alt+Shift+M** |
| Show this PC's pointer over the picture | **Ctrl+Alt+Shift+C** |
| Stream statistics overlay | **Ctrl+Alt+Shift+S** |
| Copy / paste | Ctrl+C / Ctrl+V. The clipboard syncs both ways, including images |
| Close an app | close its last window (quits the app on the Mac) |

**Logs** are off by default. Start either side with `--logs-enabled` to get one:

| Side | Log |
|---|---|
| Windows | `MacBridge.exe --logs-enabled` → `%APPDATA%\RemoteMac\viewer.log` |
| Mac, in the background | `./macbridge --password … --logs-enabled` → `~/Library/Logs/MacBridge/macbridge.log` |
| Mac, `--foreground` | written to the terminal (stderr) |

## Security

Everything between the viewer and the Mac is **end-to-end encrypted**, whether it goes through a
relay or straight over the local network:

- **Password-authenticated key exchange.** After connecting, the two sides run a CPace-style
  PAKE on P-256. The password is never sent, not even as a hash. A relay, or anyone watching
  the network, sees nothing it could test passwords against offline. Each wrong guess costs a
  connection, and the Mac locks out for a minute after five in a row. Keys are fresh for every
  session (forward secrecy).
- **Encrypted transport.** The session stream and every UDP datagram of the session (video,
  sound, input, reports, the Mac Desktop's GameStream tunnel) are protected with
  ChaCha20-Poly1305.
- **Scoped file opening.** The viewer can only ask the Mac to open documents in the uploads
  folder or the user's home, through LaunchServices; programs, scripts and packages are
  refused and no shell is ever run. The Mac's wallpaper is changed only during Desktop Fusion
  and always restored.
  Tampered data is rejected.
- **What a relay sees:** the session ID, IP addresses, and the size and timing of the traffic.
  Nothing else. A relay with `RM_RELAY_KEY` set also refuses clients without the key.
- **Limits.** The protocol has not had an independent audit. The release binaries are not
  notarized/signed by a developer account. The Mac app can see and control everything the
  logged-in user can, so choose a strong password and stop it (`--stop`) when you do not need
  it.

Details are in [SECURITY.md](SECURITY.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
Found a vulnerability? Please report it privately as described in SECURITY.md.

## Documentation

| Guide | For |
|---|---|
| [User guide](docs/USER-GUIDE.md) | installing, connecting, options, shortcuts, logs, troubleshooting |
| [Relay setup](deploy/RELAY-SETUP.md) | running your own relay server |
| [Architecture](docs/ARCHITECTURE.md) | how it works: connection paths, protocol, encryption, sound, Desktop Fusion |
| [Changelog](CHANGELOG.md) | what changed in each version |
| [QA checklist](docs/QA-CHECKLIST.md) | what to check by hand before a release |
| [Security policy](SECURITY.md) | the security model and reporting vulnerabilities |
| [Contributing](CONTRIBUTING.md) | building, testing and sending changes |

## Project layout

| Path | Contents |
|---|---|
| `agent/macos/` | the Mac host (Swift): capture, encode, input, windows, apps, LAN listener, encryption, background mode |
| `crates/rm-viewer/` | the Windows viewer (Rust, Win32, D3D11, DirectComposition, Media Foundation) |
| `crates/rm-relay/` | the relay server, and LAN discovery (`lan.rs`) |
| `crates/rm-protocol/` | wire protocol, framing, sessions, UDP/FEC, end-to-end encryption (`secure.rs`) |
| `crates/rm-gamestream/`, `crates/moonlight-sys/` | GameStream host (ported from Sunshine) and the Moonlight client core |
| `crates/rm-client/` | command-line client used by end-to-end tests |
| `crates/rm-decode/`, `rm-core/`, `rm-agent/`, `rm-fakeagent/` | decoder, shared logic, test agents |
| `scripts/`, `.github/workflows/` | builds, end-to-end tests on real macOS runners, releases |
| `deploy/` | relay installer and guide |
| `docs/USER-GUIDE.md`, `docs/ARCHITECTURE.md` | user guide; architecture, protocol and encryption |
| `docs/SPEC.md` | the original design notes (historical, in Vietnamese) |

## Contributing

Issues and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License and credits

MacBridge is free software under the **GNU General Public License v3.0 or later**
([LICENSE](LICENSE)).

It stands on the shoulders of [Moonlight](https://moonlight-stream.org) and
[Sunshine](https://github.com/LizardByte/Sunshine): moonlight-common-c is vendored, and parts
of Sunshine and moonlight-qt are ported. See [NOTICE.md](NOTICE.md) for every project used and
its license.

MacBridge is not affiliated with Apple, Microsoft, Moonlight or LizardByte. macOS and Mac are
trademarks of Apple Inc.
