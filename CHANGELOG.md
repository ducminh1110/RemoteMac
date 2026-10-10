# Changelog

Each version of MacBridge, newest first. Both sides should run the same version; a newer
Windows app works with an older Mac app without the features the Mac lacks.

## 1.2.0-beta.1

A beta: everything below passes the automated tests (unit tests on Linux and Windows, the real
Mac app end to end on macOS runners, the Windows app against a scripted Mac). What could only
be checked by hand on real machines is listed under **Not yet verified**.

### Added

- **Mac windows as the Mac draws them** (Settings → **Mac windows**, the default with a Mac that
  can): each window is streamed whole, its own title bar, toolbar and red, yellow and green
  buttons included, with no frame of MacBridge's around it. The free part of its title bar moves
  it here (snapping and double-click maximise as on Windows), its edges resize it, red closes,
  yellow minimises it here, green makes it full screen; toolbar items, tabs and fields go to the
  Mac. "With MacBridge's title bar and menus" keeps the previous frame.
- **The Mac's own menu bar** at the top of the screen while a Mac window is in front: streamed
  as it is (the front app's menus, the Apple menu, status items, the clock); menus open from it
  under it; the window in front keeps its title bar below it.
- **Window shapes**: rounded corners, menus and the Dock have the Mac's exact outline, with
  nothing black at the corners (the Mac sends each picture's alpha once; the viewer draws through
  it).
- **A new launcher**, after MobileLab's look: a tinted window, a status capsule (which Mac,
  connected how), a floating panel with a large title, a search field, the app grid (hover and
  press animations, a dot under apps open here) and a status strip. Click opens, typing searches,
  arrow keys move.

- **Sound.** The Mac's sound plays on the PC. The Mac captures the session's apps (every app
  while the Mac Desktop is open, never MacBridge's own) with ScreenCaptureKit and sends 5 ms
  packets of 48 kHz stereo PCM, sealed like video, over UDP (or the encrypted stream without
  UDP). The PC plays them through WASAPI on the default output device, following it when it
  changes, with an adaptive jitter buffer (40 ms to start, 30–150 ms), loss concealment and
  drift correction. Settings: **Sound**, **Volume**; **Ctrl+Alt+Shift+M** mutes.
- **Connect by address.** The connect window's **Type its address** (and `--direct
  host[:port]`) connects straight to the Mac by IPv4, IPv6 or name, with the same handshake
  and encryption and no relay. The Mac listens on IPv4 and IPv6 (`--port`, 7471 by default)
  and prints its addresses.
- **Drag and drop to open.** Files dropped onto a Mac app's window are uploaded and opened in
  that app; onto the launcher or Mac Desktop, in the Mac's default app.
- **Apps opened on the Mac become windows.** An app that opens a window shortly after your
  click or key press (a document in Finder, a link, the Dock) is taken into the session.
- **MacBridge Search** (**Ctrl+Alt+Space**, or **Open an App…**): type part of an app's name to
  open it or switch to it.
- **Loading window** when an app opens: the app's icon and name with a spinner, over a blur of
  the app's main colour, turning into the app's window.
- **Liquid Glass** for MacBridge's own menus, search, banners and loading window (adapted from
  MobileLab, MIT): glass, frosted or solid (Settings → **Glass**); Windows' "Transparency
  effects" off gives solid.
- **Motion system**: duration tokens, Apple's curves, springs, animations that can be
  interrupted mid-way; **Animations** setting (as Windows, reduced, full).
- **Navigation ball menu** in glass, with keyboard navigation, shortcuts and check marks.
- **Reconnect banner**: when the connection drops, a banner shows each step of the new attempt,
  then "Connected again".
- **Connection phases** shown in the connect window (finding the Mac, checking the password,
  agreeing on features, starting the picture).
- **Keyboard modes** (Settings → **Keyboard**): Windows (Ctrl acts as ⌘), Mac (keys as on a Mac
  keyboard) and Fusion (Windows' text keys: Home/End, Ctrl+arrows, Ctrl+Backspace, Ctrl+Y).
  Keys held when a window loses the focus are let go on the Mac.
- **Desktop Fusion** *(experimental, off by default)*: the Mac's real Dock is streamed onto the
  bottom of the PC's screen, captured with only the Dock and the desktop picture, while the
  Mac's desktop picture is set to the PC's. Its clicks and menus work as on the Mac. The Mac's
  own desktop picture is saved first and restored when Fusion is turned off, at disconnect, on
  `--stop`, and at the next start after a crash. A Dock that hides itself is left alone; a Dock
  drawn by the viewer stands in.
- **`macbridge.sh` launcher**: checks the program, macOS 14+, Gatekeeper's quarantine and the
  permissions (`macbridge --check-permissions`), says how to fix what is missing, then starts
  `macbridge` (`--check` only checks). It never changes privacy settings.
- **Feature negotiation**: both sides list their features in the hello; only shared ones are
  used, so mixed versions keep working.

### Security

- `open_file` is scoped: documents in the uploads folder or the user's home only (not
  `~/Library`, not hidden folders); anything that runs code is refused on both sides (by
  extension, UTType and the executable bit); files are opened through LaunchServices, never a
  shell.
- Sound datagrams are sealed with ChaCha20-Poly1305 like every other session datagram.
- A typed address never falls back to an unencrypted connection.
- The wallpaper is only changed on the viewer's request during Desktop Fusion, only to a
  picture uploaded by the viewer or a plain colour, and always restored.

### Changed

- The release tarball for macOS contains `macbridge.sh`, and its README starts MacBridge with it.
- The navigation ball's menu has **Open an App…** and **Sound**.
- The launcher opens an app with a single click (it was a double-click).

### Known limitations

- Desktop Fusion: Dock folders (stacks) and some right-click menus of the Dock are not shown
  yet; a dynamic or video desktop picture may be restored as a still picture; the Mac's
  automatic hiding of the Dock is not supported (the viewer's own Dock stands in).
- Sound is uncompressed PCM (about 1.5 Mbit/s while something plays): fine on a LAN or a good
  link, heavy on a slow one. A compressed encoding (Opus) is not done yet.
- The Windows taskbar and Explorer are never replaced (by design).

### Not yet verified

- Sound actually heard through speakers: CI machines have no audio devices. The packets'
  content, timing and the jitter buffer are tested; playback through WASAPI is not.
- The glass surfaces, loading window and banner on real monitors at every scale; CI takes
  screenshots of them on a Windows runner.
- Desktop Fusion with a real user's Dock (many items, magnification, a Dock on the left or
  right), and wallpaper restore with multiple monitors and Spaces.
- The launcher's permission advice on a fresh Mac (CI runners have the permissions granted).
- Exact windows with many toolbar layouts (the title bar's clickable items come from
  Accessibility; an app that does not describe them could be moved where it should be clicked),
  and the menu bar strip with several monitors.

## 1.0.2

Previous release: per-window streaming, Mac Desktop over GameStream, LAN discovery, relay with
IDs, end-to-end encryption, clipboard, background mode.
