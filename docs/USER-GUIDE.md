# MacBridge user guide

This guide covers everyday use of MacBridge: installing it, connecting, the options of both
apps, keyboard shortcuts, logs, troubleshooting and removal. For running your own relay
server, see [deploy/RELAY-SETUP.md](../deploy/RELAY-SETUP.md).

- [1. Requirements](#1-requirements)
- [2. The Mac app](#2-the-mac-app)
- [3. The Windows app](#3-the-windows-app)
- [4. How the connection is made](#4-how-the-connection-is-made)
- [5. Working with Mac apps](#5-working-with-mac-apps)
- [6. Settings and shortcuts](#6-settings-and-shortcuts)
- [7. Logs](#7-logs)
- [8. Troubleshooting](#8-troubleshooting)
- [9. Updating and uninstalling](#9-updating-and-uninstalling)

## 1. Requirements

| | Minimum |
|---|---|
| Mac | macOS 14 (Sonoma) or later, Apple silicon or Intel, a logged-in user session |
| PC | Windows 10 or 11, 64-bit; a GPU with D3D11 is recommended (software rendering works) |
| Network | same network, or both able to reach a relay on TCP and UDP port 7470 |

Both sides must run the same MacBridge version: the Windows app tells you when the Mac runs an
older one.

## 2. The Mac app

### First start

```bash
tar -xzf MacBridge-macos.tar.gz && cd MacBridge
xattr -d com.apple.quarantine macbridge 2>/dev/null; chmod +x macbridge
./macbridge --password choose-a-password
```

The first time, macOS asks for two permissions for your terminal app (Terminal, iTerm, …):

1. **Screen Recording**, so that windows can be captured;
2. **Accessibility**, so that the mouse and keyboard can be driven.

Allow both in **System Settings → Privacy & Security**, then run the command again. MacBridge
lists what is still missing when it starts.

### What it shows

```
  MacBridge is ready — connect from Windows with:
    ID session to connect: 123 456 789
    Password: choose-a-password
    Reachable on this network directly, and from anywhere through the relay …
    Connections are end-to-end encrypted.

  Running in the background (pid 4242). Stop it with: ./macbridge --stop
```

Then it gives the prompt back and keeps running in the background. Closing the terminal window
does not stop it. Only one MacBridge runs in the background at a time.

### Options

| Option | Effect |
|---|---|
| `--password SECRET` | the password viewers must type (at least 4 characters; without it a random one is shown) |
| `--relay HOST:PORT` | be reachable from anywhere through this relay, which also gives this Mac its ID |
| `--id 123456789` | use this ID instead of one handed out by the relay |
| `--foreground` | stay in the terminal (Ctrl+C stops it) |
| `--logs-enabled` | write a log (see [Logs](#7-logs)) |
| `--stop` | stop the MacBridge running in the background |
| `--help` | list the options |

Environment variables: `RM_RELAY` (as `--relay`), `RM_RELAY_KEY` (the relay's admission key, if
it has one), `RM_NO_LAN=1` (do not answer on the local network), `RM_LOGS=1` (as
`--logs-enabled`).

### The ID

- With a relay (`--relay`, or the one built into release builds), the **relay hands out the
  ID**. It is unique on that relay, and only this Mac can use it there. The Mac remembers it
  per relay, so it stays the same across runs.
- Without any relay (a build from source started without `--relay`), the Mac makes up its own
  ID and is reachable **on its local network only**.

### Starting at login (optional)

To have MacBridge start whenever you log in, create
`~/Library/LaunchAgents/com.macbridge.agent.plist`, using your own path and password:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.macbridge.agent</string>
  <key>ProgramArguments</key><array>
    <string>/Users/you/MacBridge/macbridge</string>
    <string>--foreground</string>
    <string>--password</string><string>choose-a-password</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
```

Then run `launchctl load ~/Library/LaunchAgents/com.macbridge.agent.plist`. macOS asks for
Screen Recording and Accessibility for `macbridge` itself the first time. The plist holds the
password in plain text, so keep the file private (`chmod 600`).

## 3. The Windows app

Unzip `MacBridge-windows.zip` anywhere and run `MacBridge.exe`. No installation is needed.

The connect window asks for:

| Field | |
|---|---|
| **ID** | the 9 digits the Mac shows (spaces and dashes are fine) |
| **Password** | the Mac's password |
| **Relay server** | only needed for a Mac on another network. Release builds fill in their relay. Type `host:port` for another relay. It is remembered. |

Press **Connect**. The launcher opens with the Mac's apps, **Mac Desktop** first.

Command-line options (for shortcuts and scripts):

| Option | Effect |
|---|---|
| `--id 123456789 --password PASS` | connect without the connect window |
| `--relay HOST:PORT` | the relay for a Mac on another network |
| `--app ID` | open this Mac app right away (as in the launcher, e.g. `com.apple.safari`) |
| `--logs-enabled` | write a log |
| `--raw-ctrl` | send Ctrl as Control (by default Ctrl acts as ⌘ Command) |
| `--no-clipboard` | do not share the clipboard |
| `--renderer gdi` | draw without the GPU (for troubleshooting) |

## 4. How the connection is made

1. **On the same network** the PC looks for the Mac by its ID with a broadcast on UDP port
   7471 and connects straight to it. No relay or internet is needed.
2. **Otherwise** it goes through the relay. Both sides connect out to it, so neither needs an
   open port.
3. Either way, the two sides then run an **encrypted handshake** that proves the password
   without sending it. A wrong password is refused by the Mac. Five wrong ones in a row lock it
   for a minute.
4. Video moves to **UDP** (with error correction), and to a **direct path** between the two
   machines whenever the networks allow. Otherwise it stays on the relay, or falls back to TCP.

The stats overlay (Ctrl+Alt+Shift+S) shows which path is in use.

**If the connection drops** (Wi-Fi lost, a cable pulled, the network changing), both sides
notice within about 10 seconds. The Mac goes back to waiting for a viewer and keeps your apps
open. The Windows app connects again by itself for up to two minutes, then reopens the apps
you had open.

## 5. Working with Mac apps

- **Each Mac window is a Windows window**, with its own taskbar button. While connected, the
  Mac's apps also appear in the Start menu and Windows Search.
- **Menus**: the app's menu bar is under the title bar. Shortcuts are translated (Ctrl acts as
  ⌘ by default).
- **Fullscreen**: the green light or **F11**. The Mac app is sized exactly to your monitor.
- **Closing** an app's last window quits the app on the Mac. Closing one of several windows
  closes only that window.
- **Files**: in an app's Open dialog you can pick a file from this PC, and it is uploaded to
  the Mac (into `~/Downloads/RemoteMac Uploads`).
- **Clipboard**: text and images go both ways.
- **Mac Desktop** shows the whole Mac screen, in fullscreen. A small round **navigation
  ball** floats over it:
  - drag it anywhere; it settles at the nearest side;
  - click it for the menu: exit fullscreen, minimize, show this PC's pointer, settings,
    disconnect.

## 6. Settings and shortcuts

Open **Settings** from the launcher, the navigation ball, or **Ctrl+Alt+Shift+P**. It holds the
frame rate, bitrate, sharpness, the Mac screen size, pixel-for-pixel mode and the pointer.

| Shortcut | Action |
|---|---|
| F11 | fullscreen on / off |
| Ctrl+Alt+Shift+P | Settings |
| Ctrl+Alt+Shift+S | stream statistics overlay |
| Ctrl+Alt+Shift+C | show this PC's pointer over the picture |
| Alt+F4 | close the window |

## 7. Logs

Neither app writes a log unless asked to:

| Side | How | Where |
|---|---|---|
| Windows | `MacBridge.exe --logs-enabled` | `%APPDATA%\RemoteMac\viewer.log` (overwritten at each start) |
| Mac, background | `./macbridge --password … --logs-enabled` | `~/Library/Logs/MacBridge/macbridge.log` (appended) |
| Mac, foreground | `./macbridge --foreground --logs-enabled …` | the terminal |

Logs never contain the password or the keys. When you share one in a bug report, remove the ID
and any relay key.

## 8. Troubleshooting

| Message or symptom | What to do |
|---|---|
| "This Mac was not found on this network" | The Mac is on another network: enter a relay under **Relay server** and start the Mac with the same relay. |
| "This Mac is not online" | MacBridge is not running on the Mac, or it uses another relay. Check the ID. |
| "Wrong password." | Type the password the Mac shows. After five wrong ones, wait a minute. |
| "Too many wrong passwords" | Wait a minute, then try again. |
| "The Mac runs an older MacBridge" | Install the same version on both sides. |
| "The server refused this app (key mismatch)" | The relay requires a key: set `RM_RELAY_KEY` on both sides, or use builds made for that relay. |
| Black or frozen picture | Check that Screen Recording is allowed for the terminal app, then restart MacBridge on the Mac. |
| Clicks and keys do nothing | Allow Accessibility for the terminal app, then restart MacBridge on the Mac. |
| Choppy video; the stats show TCP | UDP is blocked between the machines or to the relay. Open UDP 7470 on the relay (and UDP/TCP 7471 on the Mac's firewall for the local network). |
| "MacBridge is already running in the background" | Stop it first with `./macbridge --stop`. |

## 9. Updating and uninstalling

**Update:** stop the Mac app (`./macbridge --stop`), replace `macbridge` and `MacBridge.exe`
with the new versions, and start again. The ID is kept.

**Uninstall on the Mac:**

```bash
./macbridge --stop
rm -rf ~/Library/Application\ Support/RemoteMac ~/Library/Logs/MacBridge
rm -f ~/Library/LaunchAgents/com.macbridge.agent.plist   # if you set up start at login
```

Then remove the terminal app from **Screen Recording** and **Accessibility** in System
Settings if you no longer need it there.

**Uninstall on Windows:** delete the folder with `MacBridge.exe` and `%APPDATA%\RemoteMac`.
The Start-menu entries for Mac apps are removed when MacBridge disconnects.
