# Changelog

Each version of Taro (called MacBridge until 1.2.0), newest first. Both sides should run the
same version; a newer Windows app works with an older Mac app without the features the Mac
lacks.

## 1.2.3

Taro updates itself, and Windows gets an installer. Update both sides (this once by hand: the
next updates come on their own).

### Added

- **Windows installer, `Taro-Setup.exe`**: Taro for you only (no administrator) in
  `%LOCALAPPDATA%\Programs\Taro`, with Taro in the Start menu, an optional desktop icon and an
  uninstaller. `Taro-windows.zip` stays, for running without installing.
- **Updates on Windows**: Taro looks for a newer release on GitHub a few seconds after it starts
  and every six hours, and a notice says so at the top of the screen (a click updates). The
  installer is downloaded, checked (its size and the SHA-256 GitHub lists for it), and run
  silently: Taro closes, updates and opens again, and the Mac's apps stay open for it.
- **Settings → About this Taro**: Taro's icon, its version and the Mac's (with a word when the
  Mac's is older), **Software Update** with **Check for Updates** / **Update Now** and where it is
  at, and **Check for updates automatically**.
- **`taro-update.sh` on the Mac** (also `./taro.sh --update`): fetches the newest
  `Taro-macos.tar.gz`, checks it (SHA-256, and that the program runs), and puts `taro`,
  `taro.sh`, `taro-update.sh` and `README.txt` in place. A Taro running in the background goes
  on with the new version by itself, same ID and password. `--check` only says whether there is
  one.

## 1.2.2

Closing a window works as on the Mac, and the Dock comes out with a shorter push. Update both
sides.

### Changed

- **The close button closes the window, not the app**, as the red button does on the Mac: the
  app stays open, with its dot in the Dock, and a click on it there (or in the launcher) brings
  a window back. To quit it, right-click it in the Dock and choose Quit, or use Quit in its own
  menu (⌘Q). Alt+F4 does the same as the close button. A window without a close button on the
  Mac gets ⌘W, as File > Close does. With no window left, the launcher comes back (Taro used
  to end there, quitting the apps).
- **The Dock comes out with a short nudge** against its edge (about half of the push it took
  before); with a pen or in a remote session, half a second at the edge. Passing by the edge
  still leaves it hidden.

## 1.2.1

A Mac app in its own window is as smooth as the same app on the Mac Desktop, and no app loses
the connection when it opens. Update both sides.

### Fixed

- **An app's window stuttered where the Mac Desktop did not.** Both are captured and encoded
  alike; only the sending differed. An app's frames left through a pacer at a few times the
  bitrate: a keyframe (one goes each time a window settles, to redraw it sharp) took tens of
  milliseconds to leave, the frames behind it were dropped as a backlog, the window started
  again from another keyframe, and the wait read as congestion and cut the bitrate, which
  slowed the pacer further. Frames now leave at once, as Sunshine (and the Mac Desktop's
  GameStream) sends them; only a frame that waited 0.15 s is a backlog, and the bitrate drops
  only when two reports in a row show congestion. It grows only once the link has carried at
  least half of it (a still window says nothing of the link).
- **Some apps lost the connection each time they were opened** (closing them brought it back).
  A window with translucent parts (a sidebar, a glass toolbar) had a shape of megabytes, and a
  menu bar with long entries (History, recent documents) could pass 1 MiB: Windows took nothing
  larger on its control channels and ended the session, and when it came back the app was
  still open. Windows now takes a message of up to 16 MiB on any channel and passes over one it
  does not understand instead of ending the session. The Mac sends shapes in 16 steps of
  transparency (only the outline when even that is too large), menu entries of at most 200
  characters, and a message past 1 MiB where an older Windows app takes it too.
- **The Mac Desktop sometimes stayed on the usual stream instead of GameStream.** Windows
  connects through the tunnel as soon as it learns of the desktop's window, and the Mac
  announced the window before its GameStream session was up (it was still setting up the
  sound): the first connection was dropped and timed out. The session now starts first.

## 1.2.0

**MacBridge is now Taro.** The first release of 1.2: everything of the three betas (sound,
typed addresses, drag and drop, Liquid Glass, the new launcher, connect window and Settings,
the title bar and glass menus of each Mac window), with a new name and icon, the Dock at the
side of the screen, full screen as on the Mac, and fixes from using beta 3. Update both sides:
the files are now `Taro-windows.zip` (`Taro.exe`), `Taro-macos.tar.gz` (`./taro.sh` or
`./taro`) and `taro-relay.tar.gz`. The Mac keeps its ID and the PC its settings.

### Changed

- **Taro**: the name everywhere, a taro (khoai môn) on violet for its icon, and Taro's violet
  as the accent of every surface (selected menu rows, switches, buttons), whatever Windows'
  accent colour is. On the Mac: `./taro`, `./taro.sh`, the log in `~/Library/Logs/Taro`,
  uploads in `~/Downloads/Taro Uploads`.
- **The Dock is back, at the left or right edge of the screen** (Settings → Dock; left by
  default), **as the Mac's own Dock**: the same items in the same order (apps kept and open,
  the lines between, folders, the Trash), the running dots, kept in step as it changes, and a
  **right-click shows the item's menu as the Mac does** (read from the Mac, chosen there). Its
  icons shrink to fit when there are many or the screen is short. Drawn by Taro in Liquid
  Glass, it comes out only when the pointer is **pushed against that edge** (the mouse keeps
  moving outward once the pointer is there): passing by the edge or resting there leaves it
  hidden. Icons grow under the pointer with their name beside them.
- **The Mac's own Dock is no longer streamed** (Desktop Fusion and its wallpaper sync are
  gone): its picture had a black background behind its glass. A wallpaper an older version
  changed on the Mac is still put back.
- **Full screen covers the taskbar; push the pointer against the bottom edge and the taskbar
  comes up**, as the Mac's Dock does; move away and the Mac app is in front again.

### Fixed

- **Closing an app with its red button and opening it again at once loaded for ever, then the
  Mac was lost.** The Mac took a quit app for alive (macOS keeps that state up to date only
  for apps with a run loop): the new launch was refused as "already running", and the old
  quit's clean-up then forgot the new copy (its window never came). Now it opens at once, also
  an app that is still quitting or still open without a window; a refused launch says why on
  the loading window.
- **A thin band of the desktop between the title bar and the app, and gaps beside a full-screen
  app.** A picture a few pixels off its window (its title row measured a moment late, a Mac
  window a few points short of the screen) now fills it; a picture of a really different shape
  has the title bar's colour (black in full screen) beside it, never the desktop seen through.
  The dark curve of the Mac window's corner under the title bar is gone too.
- **A window that stayed blank after resizing as it opened** (Xcode on macOS 15), and a
  capture that macOS stopped by itself, are captured again.
- **The Mac's menu bar and Dock left hidden** after Taro on the Mac stopped without a word
  while an app was full screen there: put back at its next start.

## 1.2.0-beta.3

The third beta: the title bar and menus drawn as the rest of MacBridge, smoother and lighter
animation and glass, and many fixes for what can go wrong while using it. Update both sides.

### Changed

- **The title bar of a Mac app's window is drawn as the rest of MacBridge**: the same text as
  the launcher and Settings (Inter, Apple's tracking, unhinted), the launcher's red, yellow and
  green buttons, light or dark as Windows is, and the menu title under the pointer on a small
  glass pill that fades in.
- **The app's menus open as glass menus** (as the navigation ball's), no longer as Windows'
  popup menus: their shortcuts on the right, rows that open another menu beside them (Format >
  Font), and the Mac's way of moving through them: press on a title and let go on an item, or
  click and click; the pointer moving along the titles, or Left and Right, goes from one menu to
  the next; Escape closes. The window stays the active one while a menu is open.
- **Smoother, lighter animation and Liquid Glass.** Animations run at 60 frames a second (16 ms
  timers fired every 31 ms at Windows' default timer resolution); the blur is about three times
  faster with the same pixels; a glass pane costs about a third less; shadows, the Settings
  window's glass and its open menu are made once instead of every frame; the launcher no longer
  softens the top of the grid when nothing is under the toolbar; nothing is drawn 60 times a
  second while only a caret or a spinner moves.
- **Pointer moves at most every 4 ms** (the latest one goes before any click): a 1000 Hz mouse
  sent the Mac 1000 messages a second.

### Fixed

- **Two title bars on a Mac window.** The Mac's own title row ("Untitled ⌄", the grey "being
  shared" capsule) showed under MacBridge's title bar in TextEdit, Finder, Safari: the window's
  title and its chevron counted as toolbar controls, rows taller than 40 points were kept, and a
  window measured before Accessibility was ready kept it for good. The row is measured from the
  close button and the title, up to 60 points, and again for 3 seconds when it could not be.
- **The Mac's capture streams no longer pile up.** A stream stopped while it was still starting
  (resizing a window, a menu opening and closing quickly) went on capturing and encoding for
  ever; restarts while a window is resized now wait for the size to settle.
- **A window no longer stays blank after resizing as it opened** (Xcode's welcome window on
  macOS 15): its new capture started while the old one was still starting, and ScreenCaptureKit
  gave it no picture at all. A new capture now starts once the old one is gone, and a window
  with no picture 2.5 s after its capture started is captured again (twice at most).
- **Clicks and keys never wait behind slow work on the Mac.** Opening an app (up to 20 s),
  Accessibility, displays, files and the wallpaper run on their own queue.
- **The Mac is never killed by a closed connection** (SIGPIPE), nor runs out of file handles
  after many sessions (sockets were inherited by each restart for the next viewer).
- **Closing the viewer quits the session's apps; a cut connection keeps them** (the viewer now
  says goodbye; before, a connection cut by the network or the relay quit them too).
- **Reconnecting no longer leaves the old UDP path running**: it held the old session's video
  and decoders and took the new session's place at the relay (video stuttered and fell back to
  TCP, worse with each reconnect).
- **A file upload no longer cuts the connection**: its chunks went ahead of the pings, so a slow
  upload (a dropped file, Desktop Fusion's wallpaper) made the viewer think the Mac was gone;
  the whole file was also held in memory.
- **Keyframes are asked for and sent at a sane rate**: one request per window until it comes,
  one keyframe per 0.3 s at most on the Mac (every lost frame asked for one).
- **AltGr typing** (@ { } [ ] \ | ~ € on German, French, Spanish… keyboards) types the character
  instead of sending Cmd+Option shortcuts to the Mac (Cmd+Option+8 toggled macOS Zoom).
- **Keys and mouse buttons are never left down on the Mac**: a key goes up there whatever the
  modifiers are by then, and buttons held when the pointer is taken away (Alt+Tab) go up.
- Pasted text types 20 characters per event (a page took many seconds); the pasteboard is never
  waited on; values from the viewer can no longer crash the Mac with a bad number.
- Settings opens again after the window it belonged to closed; clicking a running app in the
  launcher keeps it maximised; closing the launcher while an app is opening no longer quits;
  popups and dialogs are placed from where their window is now; F11 and the Ctrl+Alt+Shift
  toggles act once per press; Alt+letter no longer beeps; window menus are freed with their
  windows.

## 1.2.0-beta.2

The second beta, with what using 1.2.0-beta.1 on real machines showed. Update both sides, and
your own relay if you run one. As before, what the automated tests cannot check (a real Mac
left running for hours, asleep and awake again) is for you to try.

### Changed

- **The app's menus are back in each window's title bar** (Settings → **Mac windows**, the
  default again): MacBridge's title bar with the red, yellow and green buttons, the menus beside
  them and the title. The Mac's own title bars and its menu bar at the top of the screen are
  now the experimental choice ("As the Mac draws them"). A choice made in 1.2.0-beta.1 is not
  carried over.

### Fixed

- **The Mac no longer freezes under a moving pointer, and is never left unreachable.** Every
  pointer move asked the window tracker for its window and waited behind the tracker's work
  (which waits on busy apps, Finder above all): clicks, keys and resizing fell further and
  further behind, the connection backed up and was lost, and the Mac took long to be
  reachable again. Now:
  - the window of a click or a move is read without waiting;
  - of pointer moves waiting one after another only the last is posted (a move before or after
    a click or a key keeps its place);
  - the connection is only read on its thread, so it never backs up; the viewer's pings are
    answered at once;
  - a viewer gone silent is noticed by a thread doing nothing else;
  - the end of a session (the apps quitting, the wallpaper and the displays as they were)
    takes at most a few seconds: the apps quit all at once, one that does not is stopped, and
    the Mac waits for the next viewer after 8 seconds whatever happens;
  - in the background, the copy that left the terminal watches over the one doing the work and
    starts it again if it dies (`--stop` ends both).
- **The launcher's green button fills the screen with the launcher drawn at that size.** The
  window grew while its picture kept the old size (the rest black, with a stale copy of it).
- **The Dock of Desktop Fusion comes when the pointer reaches the taskbar** (or the bottom edge
  with the taskbar hidden), along the Dock and a little beyond it, instead of only with the
  pointer pressed against the bottom of the screen.
- **A Mac that wakes from sleep is reachable at once.** Its wait at the relay most likely died
  while it slept, and keepalive took up to about 40 seconds to notice; a viewer connecting in
  that time found the Mac "not online". The Mac now sees that it slept (the wall clock went on
  while its uptime did not) and waits at the relay again straight away.

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
- **Text as macOS draws it** on every surface MacBridge draws itself: Inter (variable: weight
  and optical size), shaped with HarfBuzz's rules (kerning, ligatures, marks), Apple's tracking
  for SF, unhinted at fractional positions; Windows' fonts for letters Inter lacks.
- **A new launcher**, after Apple's Screen Sharing on macOS 26: no Windows title bar (the
  toolbar is the title bar, with the red, yellow and green buttons), the Mac's name and how it is
  connected, Liquid Glass controls (Mac Desktop, Settings, search), a Finder-like icon grid
  (two-line names, the selection on accent, a dot under apps open here), the scroll edge effect
  under the toolbar, and springs: icons lift and give, the grid flows as the search narrows it,
  the apps come in one after another. Click opens, typing searches, arrow keys move.
- **A new Settings window**, after System Settings on macOS 26: a glass sidebar with the
  sections (Video, Sound, Keyboard & Pointer, Appearance, Desktop Fusion), each setting in
  rounded groups with menus that open as the Mac's do, switches whose knob springs across and a
  volume slider. A change applies at once; there is no Save.
- **A new connect window**, after Screen Sharing's sign-in: a Mac, a glass segmented control
  (**By ID** / **By Address**), rounded fields with their symbols and macOS's focus ring, the
  step under way with a spinner or the reason in red, glass Cancel / Connect. Its fields are
  MacBridge's own (caret, selection, words, paste, a secure password field).

- **Sound.** The Mac's sound plays on the PC. The Mac captures the session's apps (every app
  while the Mac Desktop is open, never MacBridge's own) with ScreenCaptureKit and sends 5 ms
  packets of 48 kHz stereo PCM, sealed like video, over UDP (or the encrypted stream without
  UDP). The PC plays them through WASAPI on the default output device, following it when it
  changes, with an adaptive jitter buffer (40 ms to start, 30–150 ms), loss concealment and
  drift correction. Settings: **Sound**, **Volume**; **Ctrl+Alt+Shift+M** mutes.
- **Connect by address.** The connect window's **By Address** (and `--direct host[:port]
  --password PASS`) connects straight to the Mac by IPv4, IPv6 or name, as Moonlight connects
  to Sunshine: only the address and the password, no ID and no relay, with the same handshake
  and encryption (the secret is the password alone). The Mac listens on IPv4 and IPv6 (`--port`, 7471 by default)
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

### Fixed

- **A Mac left running stays reachable.** After a while (the Mac asleep and awake again, a router
  forgetting an idle connection, the relay restarting) the Mac could stay "not online" for good:
  its wait at the relay had died without a word. Its connection to the relay now has TCP
  keepalive (a dead one is noticed within about 40 seconds), a wait that hears nothing for
  longer than the relay's pair timeout starts over, the Mac waits again at once when the relay's
  wait runs out, and App Nap no longer slows the background process. The viewer looks again for
  up to 8 seconds when the relay does not know the Mac yet (it waits again moments after each
  session), and the relay no longer pairs a viewer with a Mac whose wait has died (update your
  own relay with this release's `macbridge-relay.tar.gz`).
- **The window buttons of Mac windows are back.** While a window is captured, macOS draws a
  "being shared" capsule where its red, yellow and green buttons are (on the Mac's screen and in
  the capture). The Mac now draws the three buttons back in each picture, where the Mac lays
  them out, in colour for the active window and grey for the others, as macOS does.
- **A Mac window that does not change (a dialog) always shows a picture.** Its first frame could
  come (over UDP) before the viewer knew of the window and be dropped, and a keyframe asked for
  then only came with the window's next change. The viewer now asks for a keyframe as each window
  appears, and a keyframe asked for of a window that is not changing is sent at once (its last
  picture again).
- **The Mac's menu bar always shows a picture.** Its first frame could arrive (over UDP) before
  the viewer knew of the strip and be dropped, and a menu bar that does not change sends no
  other: every stream now sends its picture once more as a keyframe a moment after it starts,
  and the viewer asks for one when the menu bar is announced.

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
