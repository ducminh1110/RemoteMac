# Taro user guide

This guide covers everyday use of Taro: installing it, connecting, the options of both
apps, keyboard shortcuts, logs, troubleshooting and removal. For running your own relay
server, see [deploy/RELAY-SETUP.md](../deploy/RELAY-SETUP.md).

- [1. Requirements](#1-requirements)
- [2. The Mac app](#2-the-mac-app)
- [3. The Windows app](#3-the-windows-app)
- [4. How the connection is made](#4-how-the-connection-is-made)
- [5. Working with Mac apps](#5-working-with-mac-apps)
- [6. Sound](#6-sound)
- [7. The Dock and full screen](#7-the-dock-and-full-screen)
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

Both sides must run the same Taro version: the Windows app tells you when the Mac runs an
older one.

## 2. The Mac app

### First start

```bash
tar -xzf Taro-macos.tar.gz && cd Taro
xattr -d com.apple.quarantine taro 2>/dev/null; chmod +x taro
./taro.sh --password choose-a-password
```

`taro.sh` is the launcher. Before it starts `taro` it checks that the program is next
to it and runnable, that the Mac runs macOS 14 or later, that Gatekeeper's quarantine mark is
gone, and which permissions are still missing, and says how to fix each. It then runs
`taro` with the same options (`./taro` on its own works as well).
`./taro.sh --check` only checks: it exits with 0 when everything is in place.

The first time, macOS asks for two permissions for your terminal app (Terminal, iTerm, …):

1. **Screen Recording**, so that windows can be captured (and the Mac's sound, see
   [Sound](#6-sound));
2. **Accessibility**, so that the mouse and keyboard can be driven.

Allow both in **System Settings → Privacy & Security**, quit the terminal app completely and
open it again (macOS applies the change at its next start), then run the command again.
Taro never changes these settings itself.

### What it shows

```
  Taro is ready — connect from Windows with:
    ID session to connect: 123 456 789
    Password: choose-a-password
    Reachable on this network directly, and from anywhere through the relay …
    Connections are end-to-end encrypted.

  Running in the background (pid 4242). Stop it with: ./taro --stop
```

Then it gives the prompt back and keeps running in the background. Closing the terminal window
does not stop it. Only one Taro runs in the background at a time.

### Options

| Option | Effect |
|---|---|
| `--password SECRET` | the password viewers must type (at least 4 characters; without it a random one is shown) |
| `--relay HOST:PORT` | be reachable from anywhere through this relay, which also gives this Mac its ID |
| `--id 123456789` | use this ID instead of one handed out by the relay |
| `--port PORT` | the TCP port a PC types this Mac's address with (7471 by default) |
| `--foreground` | stay in the terminal (Ctrl+C stops it) |
| `--logs-enabled` | write a log (see [Logs](#9-logs)) |
| `--stop` | stop the Taro running in the background |
| `--check-permissions` | list the permissions macOS gives Taro here (exit 3 when one is missing) |
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

To have Taro start whenever you log in, create
`~/Library/LaunchAgents/com.taro.agent.plist`, using your own path and password:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.taro.agent</string>
  <key>ProgramArguments</key><array>
    <string>/Users/you/Taro/taro</string>
    <string>--foreground</string>
    <string>--password</string><string>choose-a-password</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
```

Then run `launchctl load ~/Library/LaunchAgents/com.taro.agent.plist`. macOS asks for
Screen Recording and Accessibility for `taro` itself the first time. The plist holds the
password in plain text, so keep the file private (`chmod 600`).

## 3. The Windows app

Run `Taro-Setup.exe`. It installs Taro for you only (no administrator, nothing to choose)
into `%LOCALAPPDATA%\Programs\Taro`, with **Taro** in the Start menu and an uninstaller in
Windows' **Installed apps**. The installer is not signed: Windows SmartScreen may ask once
(**More info → Run anyway**). Taro updates itself from then on (see
[Updating](#11-updating-and-uninstalling)). `Taro-windows.zip` has the same `Taro.exe` to run
without installing.

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
files, …) and only what both have is used. A Mac with an older Taro
still works, without the newer features.

## 5. Working with Mac apps

- **Each Mac window is a Windows window**, with its own taskbar button. While connected, the
  Mac's apps also appear in the Start menu and Windows Search.
- **The title bar** (the default) is Taro's, with the red, yellow and green buttons, the
  app's menus beside them and the window's title. Drag it to move the window; resize it by its
  edges. Red closes it, yellow minimises it to the taskbar, green makes it full screen.
- **As the Mac draws it** (Settings → **Mac windows**, experimental): the window comes whole,
  with its own title bar and buttons, and nothing of Taro's around it; the Mac's menu bar
  is then at the top of the screen while a Mac window is in front. (The Mac's setting
  "Automatically hide and show the menu bar" must be off for it.)
- **Opening an app** shows a loading window like the Mac's own: the app's icon and name
  with a spinner, over a blur of the app's main colour. It turns into the app's window when
  that appears.
- **Taro Search** (**Ctrl+Alt+Space**, or **Open an App…** in the navigation ball's
  menu): type part of a Mac app's name, then Enter to open it, or to switch to it when it is
  already open. Arrow keys move, Escape closes.
- **Menus**: in the window's title bar, beside its buttons (as the Mac draws them: in the Mac's
  menu bar at the top of the screen). They open as glass menus and work as the Mac's: press on a
  title and let go on an item, or click, then click; move along the titles (or press Left and
  Right) to go from one menu to the next; a row with a chevron opens another menu beside it.
  Shortcuts are translated (Ctrl acts as ⌘ by default).
- **Fullscreen**: the green light or **F11**. The Mac app is sized exactly to your monitor.
- **Closing** a window (its red light, the close button or Alt+F4) closes that window, as the
  red button does on the Mac: the app stays open, its dot stays in the Dock. Click it in the
  Dock (or the launcher) to get a window again; to **quit** it, right-click it in the Dock and
  choose **Quit** (or use Quit in its own menu, ⌘Q). With no window left, Taro's launcher comes
  back; closing it ends Taro.
- **Files**: in an app's Open dialog you can pick a file from this PC, and it is uploaded to
  the Mac (into `~/Downloads/Taro Uploads`).
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
  - click it for the menu: exit fullscreen, minimize, open an app (Taro Search), show
    this PC's pointer, sound on or off, settings, disconnect.

## 6. Sound

The Mac's sound plays on this PC (on by default; **Sound** and **Volume** in Settings,
**Ctrl+Alt+Shift+M** to mute and unmute).

- Only the apps you opened from Windows are heard; with **Mac Desktop** open, every app is.
  Taro's own sound is never sent back.
- It plays on Windows' default output device and follows it when you switch (headphones
  plugged in, a Bluetooth headset).
- It is sent as plain PCM (48 kHz stereo), encrypted like everything else, in small packets
  with a short buffer of about 40 ms that grows by itself on an uneven network. A lost
  packet fades out instead of clicking. When the Mac is silent nothing is sent.
- macOS gives sound to the same **Screen Recording** permission as the picture.

## 7. The Dock and full screen

**The Dock** is the Mac's own Dock, as a column of glass at the **left edge** of the screen
(Settings → **Dock**: at the left edge, at the right edge, or off): the same items in the same
order as on the Mac (the apps kept in it and the open ones, the lines between them, Downloads
and other folders, the Trash), a dot beside each app that runs on the Mac. It follows the Mac's
Dock as it changes. Its icons are as large as the Mac's by default and smaller when there are
many of them or the screen is short.

- It stays out of the way until you **push the pointer against that edge**: a short nudge on
  towards the edge after the pointer has reached it. Passing by the edge, or leaving the
  pointer there, does nothing. (With a pen or in a remote session, where there is no such
  push, leaving the pointer at the edge for half a second brings it out.)
- The icon under the pointer grows, with its name beside it. A click opens the app (with its
  loading window), or brings its window to the front (back from the taskbar if it was
  minimised); a folder or the Trash opens as a Finder window; Launchpad or Apps opens Taro's
  launcher.
- **A right-click shows the item's menu as the Mac's Dock shows it** (the app's windows,
  Options, Show in Finder, Quit, Empty Trash…): the menu is read from the Mac and what you
  choose is done there.
- It slides away once the pointer has left it.
- It is drawn by Taro on this PC (no picture of the Mac's Dock is streamed) and changes
  nothing of Windows. A Mac running an older Taro gets a Dock of the Mac Desktop, the apps open
  here and Open an App.

**Full screen** (the green button, or F11) gives a Mac app the whole monitor, over the
taskbar, at the monitor's size on the Mac (Taro makes the Mac a display of this size):

- Move the pointer to the top edge: the window's title bar and menus slide in.
- **Push the pointer against the bottom edge**: the Windows taskbar comes up, as the Mac's
  Dock does; move away from it and the Mac app is in front again.
- When a Mac app cannot be quite the monitor's size, the bands beside it are black; a window
  a few pixels off fills its window instead.

## 8. Settings and shortcuts

Open **Settings** from the launcher (the gear in its toolbar), the navigation ball, or
**Ctrl+Alt+Shift+P**. It is laid out as the Mac's System Settings: the sections on the left
(**Video**, **Sound**, **Keyboard & Pointer**, **Appearance**, **Dock**, **About this Taro**;
Up and Down move between them), their settings on the right. A change applies at once; there is no Save.
**Video** holds the frame rate, bitrate, sharpness, the Mac screen size (pixel for pixel is its
last step), the Mac Desktop scale, the decoder and frame pacing; and:

| Setting | |
|---|---|
| **Sound**, **Volume** | the Mac's sound on this PC (see [Sound](#6-sound)) |
| **Use this PC's pointer** | this PC's pointer over the picture instead of the Mac's (Ctrl+Alt+Shift+C) |
| **Keyboard** | **Windows**: Ctrl acts as ⌘ Command (the default). **Mac**: keys as on a Mac keyboard (Ctrl is Control, the Windows key is ⌘). **Fusion**: as Windows, plus Windows' text keys: Home/End go to the start/end of the line, Ctrl+Home/End to the start/end of the document, Ctrl+arrows move by word, Ctrl+Backspace/Delete delete a word, Ctrl+Y redoes. |
| **Glass** | how Taro's own menus and panels are drawn: Liquid Glass, frosted (blur only), or solid (least GPU). Windows' "transparency effects" setting off also turns the glass solid. |
| **Animations** | as Windows is set (Settings → Accessibility → Visual effects → Animation effects), reduced, or full. Reduced keeps fades but drops movement and springs. |
| **Mac windows** | with the app's menus in the title bar, or as the Mac draws them (experimental: its menu bar at the top); from the next connection |
| **Dock** | at the left edge (the default), at the right edge, or off; see [The Dock](#7-the-dock-and-full-screen) |
| **About this Taro** | Taro's version and the Mac's; **Software Update** (Check for Updates, Update Now) and **Check for updates automatically**; see [Updating](#11-updating-and-uninstalling) |

Keys still held down when a window loses the focus are let go on the Mac, so none stays
stuck.

| Shortcut | Action |
|---|---|
| F11 | fullscreen on / off |
| Ctrl+Alt+Space | Taro Search |
| Ctrl+Alt+Shift+P | Settings |
| Ctrl+Alt+Shift+M | the Mac's sound off / on |
| Ctrl+Alt+Shift+S | stream statistics overlay |
| Ctrl+Alt+Shift+C | show this PC's pointer over the picture |
| Alt+F4 | close the window |

## 9. Logs

Neither app writes a log unless asked to:

| Side | How | Where |
|---|---|---|
| Windows | `Taro.exe --logs-enabled` | `%APPDATA%\RemoteMac\viewer.log` (overwritten at each start) |
| Mac, background | `./taro --password … --logs-enabled` | `~/Library/Logs/Taro/taro.log` (appended) |
| Mac, foreground | `./taro --foreground --logs-enabled …` | the terminal |

Logs never contain the password or the keys. When you share one in a bug report, remove the ID
and any relay key.

## 10. Troubleshooting

| Message or symptom | What to do |
|---|---|
| "This Mac was not found on this network" | The Mac is on another network: enter a relay under **Relay server** and start the Mac with the same relay. |
| "This Mac is not online" | Taro is not running on the Mac, or it uses another relay. Check the ID. |
| "Wrong password." | Type the password the Mac shows. After five wrong ones, wait a minute. |
| "Too many wrong passwords" | Wait a minute, then try again. |
| "The Mac runs an older Taro" | Install the same version on both sides. |
| "The server refused this app (key mismatch)" | The relay requires a key: set `RM_RELAY_KEY` on both sides, or use builds made for that relay. |
| Black or frozen picture | Check that Screen Recording is allowed for the terminal app, then restart Taro on the Mac. |
| Clicks and keys do nothing | Allow Accessibility for the terminal app, then restart Taro on the Mac. |
| Choppy video; the stats show TCP | UDP is blocked between the machines or to the relay. Open UDP 7470 on the relay (and UDP/TCP 7471 on the Mac's firewall for the local network). |
| "Taro is already running in the background" | Stop it first with `./taro --stop`. |
| No connection to a typed address | Check the address the Mac prints, that the Mac's firewall lets TCP and UDP 7471 in (or the `--port` used), and that a VPN or router passes them. |
| No sound | Check **Sound** in Settings and the Windows volume mixer; the Mac needs Screen Recording; only apps opened from Windows are heard unless Mac Desktop is open. |
| A dropped file is not opened | Files that run code are refused. Files must be regular documents. |
| The Mac's desktop picture did not come back | Start Taro again on the Mac (it puts it back at start), or run `./taro --stop`. |
| `taro.sh` says a permission is missing | Allow the terminal app it names under Privacy & Security, quit that app completely, open it again. |

## 11. Updating and uninstalling

**Update on Windows:** Taro looks for a newer release on its own, a few seconds after it
starts and then every six hours (Settings → About this Taro → **Check for updates
automatically**). When there is one, a notice comes at the top of the screen: a click updates.
**Settings → About this Taro** shows this Taro's version and the Mac's, **Check for Updates**,
and then **Update Now**. The installer is downloaded from Taro's releases on GitHub and checked
(its size and the SHA-256 GitHub lists for it); Taro then closes, the installer updates it
silently and opens it again. The Mac's apps stay open, and Taro connects back to them. A copy
run from `Taro-windows.zip` is updated into the installed place.

**Update on the Mac:** in Taro's folder,

```bash
./taro-update.sh            # the newest release, when there is a newer one
./taro-update.sh --check    # only say whether there is one
```

It fetches `Taro-macos.tar.gz` from Taro's releases on GitHub, checks it (the SHA-256 GitHub
lists for it, and that the program runs on this Mac), and puts `taro`, `taro.sh`,
`taro-update.sh` and `README.txt` in place of the ones in the folder (`./taro.sh --update`
does the same). A Taro running in the background goes on with the new version by itself,
with the same ID and password; a connected PC reconnects on its own. The ID, the password and
the permissions are kept. Update both sides: Settings → About this Taro on Windows says when
the Mac runs an older Taro.

**Uninstall on the Mac:**

```bash
./taro --stop
rm -rf ~/Library/Application\ Support/RemoteMac ~/Library/Logs/Taro
rm -f ~/Library/LaunchAgents/com.taro.agent.plist   # if you set up start at login
```

Then remove the terminal app from **Screen Recording** and **Accessibility** in System
Settings if you no longer need it there.

**Uninstall on Windows:** Windows Settings → Apps → **Installed apps** → Taro → Uninstall
(a copy from the zip: delete its folder). Settings and the remembered ID are in
`%APPDATA%\RemoteMac`; delete it too to remove everything. The Start-menu entries for Mac apps
are removed when Taro disconnects.
