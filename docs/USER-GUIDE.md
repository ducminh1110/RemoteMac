# MacBridge user guide

This guide covers everyday use of MacBridge: installing it, connecting, the options of both
apps, keyboard shortcuts, logs, troubleshooting and removal. For running your own relay
server, see [deploy/RELAY-SETUP.md](../deploy/RELAY-SETUP.md).

- [1. Requirements](#1-requirements)
- [2. The Mac app](#2-the-mac-app)
- [3. The Windows app](#3-the-windows-app)
- [4. How the connection is made](#4-how-the-connection-is-made)
- [5. Working with Mac apps](#5-working-with-mac-apps)
- [6. Sound](#6-sound)
- [7. Desktop Fusion (experimental)](#7-desktop-fusion-experimental)
- [8. Settings and shortcuts](#8-settings-and-shortcuts)
- [9. Logs](#9-logs)
- [10. Troubleshooting](#10-troubleshooting)
- [11. Updating and uninstalling](#11-updating-and-uninstalling)

## 1. Requirements

| | Minimum |
|---|---|
| Mac | macOS 14 (Sonoma) or later, Apple silicon or Intel, a logged-in user session |
| PC | Windows 10 or 11, 64-bit; a GPU with D3D11 is recommended (software rendering works) |
| Network | same network; or the PC able to reach the Mac's address (TCP and UDP 7471); or both able to reach a relay on TCP and UDP port 7470 |

Both sides must run the same MacBridge version: the Windows app tells you when the Mac runs an
older one.

## 2. The Mac app

### First start

```bash
tar -xzf MacBridge-macos.tar.gz && cd MacBridge
xattr -d com.apple.quarantine macbridge 2>/dev/null; chmod +x macbridge
./macbridge.sh --password choose-a-password
```

`macbridge.sh` is the launcher. Before it starts `macbridge` it checks that the program is next
to it and runnable, that the Mac runs macOS 14 or later, that Gatekeeper's quarantine mark is
gone, and which permissions are still missing, and says how to fix each. It then runs
`macbridge` with the same options (`./macbridge` on its own works as well).
`./macbridge.sh --check` only checks: it exits with 0 when everything is in place.

The first time, macOS asks for two permissions for your terminal app (Terminal, iTerm, …):

1. **Screen Recording**, so that windows can be captured (and the Mac's sound, see
   [Sound](#6-sound));
2. **Accessibility**, so that the mouse and keyboard can be driven.

Allow both in **System Settings → Privacy & Security**, quit the terminal app completely and
open it again (macOS applies the change at its next start), then run the command again.
MacBridge never changes these settings itself.

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
| `--port PORT` | the TCP port a PC types this Mac's address with (7471 by default) |
| `--foreground` | stay in the terminal (Ctrl+C stops it) |
| `--logs-enabled` | write a log (see [Logs](#9-logs)) |
| `--stop` | stop the MacBridge running in the background |
| `--check-permissions` | list the permissions macOS gives MacBridge here (exit 3 when one is missing) |
| `--version` | print the version |
| `--help` | list the options |

Environment variables: `RM_RELAY` (as `--relay`), `RM_RELAY_KEY` (the relay's admission key, if
it has one), `RM_NO_LAN=1` (do not answer on the local network), `RM_PORT` (as `--port`),
`RM_LOGS=1` (as `--logs-enabled`).

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

The connect window has two ways in, chosen at the top:

| **By ID** | |
|---|---|
| **Mac ID** | the 9 digits the Mac shows (spaces and dashes are fine). The PC looks for the Mac on this network, then through the relay. |
| **Password** | the Mac's password |
| **Relay server** | only needed for a Mac on another network. Release builds fill in their relay. Type `host:port` for another relay. It is remembered. |

| **By Address** | |
|---|---|
| **IP address or name** | the Mac's address: `192.168.1.20`, `mac.example.com`, `fe80::1`, `[2001:db8::5]:7471` (7471 is the default port). The Mac prints its addresses when it starts. |
| **Password** | the Mac's password. No ID: the PC goes straight to the Mac, as Moonlight goes to Sunshine, and never uses a relay. |

Press **Connect**. While it connects, the window shows the step it is at (finding the Mac,
checking the password, agreeing on features, starting the picture). The launcher then opens
with the Mac's apps, **Mac Desktop** first: click one to open it, type to search, use the arrow
keys and Enter. The capsule at the top says which Mac it is and how it is connected; a dot under
an app means it is open on this PC.

Command-line options (for shortcuts and scripts):

| Option | Effect |
|---|---|
| `--id 123456789 --password PASS` | connect without the connect window |
| `--relay HOST:PORT` | the relay for a Mac on another network |
| `--direct HOST[:PORT] --password PASS` | connect straight to the Mac's address (IPv4, IPv6 or a name): no ID, no relay |
| `--app ID` | open this Mac app right away (as in the launcher, e.g. `com.apple.safari`) |
| `--logs-enabled` | write a log |
| `--raw-ctrl` | send Ctrl as Control (by default Ctrl acts as ⌘ Command; see **Keyboard** in Settings) |
| `--no-clipboard` | do not share the clipboard |
| `--renderer gdi` | draw without the GPU (for troubleshooting) |

## 4. How the connection is made

1. **On the same network** the PC looks for the Mac by its ID with a broadcast on UDP port
   7471 and connects straight to it. No relay or internet is needed.
2. **Otherwise** it goes through the relay. Both sides connect out to it, so neither needs an
   open port.
   - **With an address typed in**, it connects straight to that address instead (TCP 7471, or
     the port given), with the password alone (no ID), and never uses the relay. This is for a Mac reachable over a VPN, a
     routed network or a forwarded port. The connection is encrypted the same way; there is
     no unencrypted fallback.
3. Either way, the two sides then run an **encrypted handshake** that proves the password
   without sending it. A wrong password is refused by the Mac. Five wrong ones in a row lock it
   for a minute.
4. Video moves to **UDP** (with error correction), and to a **direct path** between the two
   machines whenever the networks allow. Otherwise it stays on the relay, or falls back to TCP.

The stats overlay (Ctrl+Alt+Shift+S) shows which path is in use.

**If the connection drops** (Wi-Fi lost, a cable pulled, the network changing), both sides
notice within about 10 seconds. The Mac goes back to waiting for a viewer and keeps your apps
open. A banner at the top of the screen says the connection was lost and shows each step of
the new attempt; the Windows app connects again by itself for up to two minutes, then reopens
the apps you had open and the banner says "Connected again". Clicking the banner hides it
(connecting goes on).

**Features** are agreed when connecting: each side lists what it can do (sound, opening
files, Desktop Fusion, …) and only what both have is used. A Mac with an older MacBridge
still works, without the newer features.

## 5. Working with Mac apps

- **Each Mac window is a Windows window**, with its own taskbar button. While connected, the
  Mac's apps also appear in the Start menu and Windows Search.
- **The title bar** (the default) is MacBridge's, with the red, yellow and green buttons, the
  app's menus beside them and the window's title. Drag it to move the window; resize it by its
  edges. Red closes it, yellow minimises it to the taskbar, green makes it full screen.
- **As the Mac draws it** (Settings → **Mac windows**, experimental): the window comes whole,
  with its own title bar and buttons, and nothing of MacBridge's around it; the Mac's menu bar
  is then at the top of the screen while a Mac window is in front. (The Mac's setting
  "Automatically hide and show the menu bar" must be off for it.)
- **Opening an app** shows a loading window like the Mac's own: the app's icon and name
  with a spinner, over a blur of the app's main colour. It turns into the app's window when
  that appears.
- **MacBridge Search** (**Ctrl+Alt+Space**, or **Open an App…** in the navigation ball's
  menu): type part of a Mac app's name, then Enter to open it, or to switch to it when it is
  already open. Arrow keys move, Escape closes.
- **Menus**: in the window's title bar, beside its buttons (as the Mac draws them: in the Mac's
  menu bar at the top of the screen). Shortcuts are translated (Ctrl acts as ⌘ by default).
- **Fullscreen**: the green light or **F11**. The Mac app is sized exactly to your monitor.
- **Closing** an app's last window quits the app on the Mac. Closing one of several windows
  closes only that window.
- **Files**: in an app's Open dialog you can pick a file from this PC, and it is uploaded to
  the Mac (into `~/Downloads/RemoteMac Uploads`).
- **Drag and drop**: drop files from Explorer onto a Mac app's window to open them in that
  app, or onto the launcher or Mac Desktop to open them in the Mac's default app for them.
  They are uploaded first. Files that run code (`.app`, `.command`, `.sh`, `.pkg`, scripts
  and other executables) are refused on both sides.
- **Apps opened on the Mac**: an app opened from the Mac Desktop (a document in Finder, a
  link, the Dock) shortly after your click or key press becomes its own Windows window, the
  same as one opened from the launcher.
- **Clipboard**: text and images go both ways.
- **Mac Desktop** shows the whole Mac screen, in fullscreen. A small round **navigation
  ball** floats over it:
  - drag it anywhere; it settles at the nearest side;
  - click it for the menu: exit fullscreen, minimize, open an app (MacBridge Search), show
    this PC's pointer, sound on or off, settings, disconnect.

## 6. Sound

The Mac's sound plays on this PC (on by default; **Sound** and **Volume** in Settings,
**Ctrl+Alt+Shift+M** to mute and unmute).

- Only the apps you opened from Windows are heard; with **Mac Desktop** open, every app is.
  MacBridge's own sound is never sent back.
- It plays on Windows' default output device and follows it when you switch (headphones
  plugged in, a Bluetooth headset).
- It is sent as plain PCM (48 kHz stereo), encrypted like everything else, in small packets
  with a short buffer of about 40 ms that grows by itself on an uneven network. A lost
  packet fades out instead of clicking. When the Mac is silent nothing is sent.
- macOS gives sound to the same **Screen Recording** permission as the picture.

## 7. Desktop Fusion (experimental)

**Desktop Fusion** (Settings, off by default) puts the Mac's own Dock on this PC's desktop:

- While it is on, the Mac's desktop picture is set to the same picture as the PC's, and the
  Mac's real Dock is streamed as a window of its own along the bottom of the PC's screen. It
  slides in when the pointer comes down to the taskbar (or to the bottom edge, with the taskbar
  hidden) under it and away when the pointer leaves, so it looks like a Dock over the PC's own
  wallpaper.
- Clicking an icon in it works as on the Mac: apps it opens become Windows windows, and its
  menus show over it.
- Only the Dock and the desktop picture are captured for it; app windows behind it never
  show.
- The Mac's own desktop picture is saved first and put back when Desktop Fusion is turned
  off, when you disconnect, on `./macbridge --stop`, and at the next start of MacBridge if it
  ever stopped without doing so.
- If the Mac's Dock hides itself (automatic hiding), MacBridge leaves that setting alone and
  shows a Dock of the apps opened from Windows instead.
- The Windows taskbar and Explorer are never changed or replaced.

It is experimental: Dock folders (stacks) and some right-click menus may not show yet, and
a dynamic or video desktop picture may come back as a still picture.

## 8. Settings and shortcuts

Open **Settings** from the launcher (the gear in its toolbar), the navigation ball, or
**Ctrl+Alt+Shift+P**. It is laid out as the Mac's System Settings: the sections on the left
(**Video**, **Sound**, **Keyboard & Pointer**, **Appearance**, **Desktop Fusion**; Up and Down
move between them), their settings on the right. A change applies at once; there is no Save.
**Video** holds the frame rate, bitrate, sharpness, the Mac screen size (pixel for pixel is its
last step), the Mac Desktop scale, the decoder and frame pacing; and:

| Setting | |
|---|---|
| **Sound**, **Volume** | the Mac's sound on this PC (see [Sound](#6-sound)) |
| **Use this PC's pointer** | this PC's pointer over the picture instead of the Mac's (Ctrl+Alt+Shift+C) |
| **Keyboard** | **Windows**: Ctrl acts as ⌘ Command (the default). **Mac**: keys as on a Mac keyboard (Ctrl is Control, the Windows key is ⌘). **Fusion**: as Windows, plus Windows' text keys: Home/End go to the start/end of the line, Ctrl+Home/End to the start/end of the document, Ctrl+arrows move by word, Ctrl+Backspace/Delete delete a word, Ctrl+Y redoes. |
| **Glass** | how MacBridge's own menus and panels are drawn: Liquid Glass, frosted (blur only), or solid (least GPU). Windows' "transparency effects" setting off also turns the glass solid. |
| **Animations** | as Windows is set (Settings → Accessibility → Visual effects → Animation effects), reduced, or full. Reduced keeps fades but drops movement and springs. |
| **Mac windows** | with the app's menus in the title bar, or as the Mac draws them (experimental: its menu bar at the top); from the next connection |
| **Desktop Fusion** | see [Desktop Fusion](#7-desktop-fusion-experimental) |

Keys still held down when a window loses the focus are let go on the Mac, so none stays
stuck.

| Shortcut | Action |
|---|---|
| F11 | fullscreen on / off |
| Ctrl+Alt+Space | MacBridge Search |
| Ctrl+Alt+Shift+P | Settings |
| Ctrl+Alt+Shift+M | the Mac's sound off / on |
| Ctrl+Alt+Shift+S | stream statistics overlay |
| Ctrl+Alt+Shift+C | show this PC's pointer over the picture |
| Alt+F4 | close the window |

## 9. Logs

Neither app writes a log unless asked to:

| Side | How | Where |
|---|---|---|
| Windows | `MacBridge.exe --logs-enabled` | `%APPDATA%\RemoteMac\viewer.log` (overwritten at each start) |
| Mac, background | `./macbridge --password … --logs-enabled` | `~/Library/Logs/MacBridge/macbridge.log` (appended) |
| Mac, foreground | `./macbridge --foreground --logs-enabled …` | the terminal |

Logs never contain the password or the keys. When you share one in a bug report, remove the ID
and any relay key.

## 10. Troubleshooting

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
| No connection to a typed address | Check the address the Mac prints, that the Mac's firewall lets TCP and UDP 7471 in (or the `--port` used), and that a VPN or router passes them. |
| No sound | Check **Sound** in Settings and the Windows volume mixer; the Mac needs Screen Recording; only apps opened from Windows are heard unless Mac Desktop is open. |
| A dropped file is not opened | Files that run code are refused. Files must be regular documents. |
| The Mac's desktop picture did not come back | Start MacBridge again on the Mac (it puts it back at start), or run `./macbridge --stop`. |
| `macbridge.sh` says a permission is missing | Allow the terminal app it names under Privacy & Security, quit that app completely, open it again. |

## 11. Updating and uninstalling

**Update:** stop the Mac app (`./macbridge --stop`), replace `macbridge`, `macbridge.sh` and
`MacBridge.exe` with the new versions, and start again. The ID is kept.

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
