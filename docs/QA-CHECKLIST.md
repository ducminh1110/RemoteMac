# QA checklist

What to check by hand on real machines before a release. The automated tests (see
[ARCHITECTURE.md](ARCHITECTURE.md#testing)) cover the protocol, the Mac app end to end on a CI
Mac and the Windows app against a scripted Mac, but not real speakers, real monitors or a real
user's Dock. Write the result next to each item (✓, ✗ with a note, or "n/a").

Setup: a Mac with macOS 14 or later and a logged-in user; a Windows 10 or 11 PC with speakers
or headphones; both on the same network unless the item says otherwise. Note both versions
(`./taro --version`, the first line of `Taro.exe --logs-enabled`'s log).

## 1. Installing and starting

- [ ] On a Mac that never ran Taro, `./taro.sh --check` lists Screen Recording and
      Accessibility as missing, names the terminal app, and exits with 3.
- [ ] A copy downloaded with a browser (quarantined): `./taro.sh` says so and shows the
      `xattr` command; after it, it starts.
- [ ] After allowing both permissions and reopening the terminal, `./taro.sh --check`
      exits with 0 and `./taro.sh --password …` shows the ID, the password and the Mac's
      addresses, then goes to the background.
- [ ] `./taro.sh --stop` stops it.

## 2. Connecting

- [ ] By ID on the same network: connects without a relay (the stats overlay shows LAN).
- [ ] By ID through the relay from another network.
- [ ] **By Address** (only the address and the password, no ID) with the Mac's IPv4 address; with an IPv6 address (`[addr]` or bare);
      with a name; with a wrong port (a clear message, no hang).
- [ ] A wrong password is refused, and five in a row lock for a minute.
- [ ] The connect window shows the steps while connecting, and the spinner stops on an error.
- [ ] Pull the network cable or turn Wi-Fi off for 15 s: the banner appears at the top, shows
      the steps, then "Connected again", and the apps come back. Clicking the banner hides it.

## 3. Apps and windows

- [ ] Opening an app from the launcher shows the loading window (icon, name, spinner, blur of
      the app's colour), which turns into the app's window. Try Safari, Notes, Xcode, a
      third-party app.
- [ ] An app that fails to open: the loading window says so and closes.
- [ ] **Ctrl+Alt+Space** opens Taro Search; typing filters; Enter opens the app, or
      switches to it when it is already open; Escape closes; arrow keys move.
- [ ] In the Mac Desktop, double-click a document in Finder: its app's window also appears as
      a Windows window.

## 3b. Exact windows, the menu bar, shapes

- [ ] By default (menus in the title bar): Finder, Safari, TextEdit windows have Taro's
      title bar with the app's menus; clicks and typing in them work at once, also with the
      pointer moving fast; resizing by the edges and the green button work.
- [ ] The title bar's text matches the launcher's (same font and spacing), light and dark; the
      menu title under the pointer gets a pill. Menus open as glass menus; TextEdit's Format >
      Font opens beside it; moving along the titles switches menus; Left / Right / Escape work;
      the window's buttons stay coloured while a menu is open.
- [ ] The launcher's green button fills the screen and the launcher is drawn at that size
      (nothing black, no second copy of it); again restores it.
- [ ] Leave the Mac unused for an hour, and asleep for a while: the PC connects again at once.
- [ ] On a German or French keyboard layout, AltGr+Q / AltGr+7 type @ and { in TextEdit (no
      macOS Zoom, no Dock hiding).
- [ ] Drag-select text, Alt+Tab away while holding the button, come back: moving the pointer no
      longer selects. Ctrl+Z with Ctrl released first does not leave Z held.
- [ ] Cut the network for 15 s, then restore it: the viewer reconnects, the apps are still open
      on the Mac, video comes over UDP again (not TCP). Close the viewer: the apps quit.
- [ ] Drop a 50 MB file on a Mac window on a slow link: the session stays up while it uploads.
- [ ] TextEdit, Finder and Safari windows show only Taro's title bar (no "Untitled ⌄" row
      or grey capsule under it).
- [ ] With **Mac windows** = as the Mac draws them: Safari, TextEdit, Finder windows show their own
      title bar and buttons; dragging the empty title bar moves the window here (snap to the
      screen edges works); toolbar buttons and the address field work; red closes, yellow
      minimises, green goes full screen.
- [ ] The Mac's menu bar is at the top while a Mac window is in front and goes with a Windows app
      in front; its menus open under it; the Apple menu and the clock work.
- [ ] Corners: on a coloured wallpaper, no black at the corners of windows, menus and popovers
      (macOS 15 and 26).
- [ ] The launcher: light and dark, search, arrow keys, the dot under open apps.

## 4. Files

- [ ] Drop a `.txt` and a `.pdf` from Explorer onto TextEdit / Preview: they open there.
- [ ] Drop a `.png` onto the launcher: it opens in the Mac's default app.
- [ ] Drop a `.command`, a `.sh`, a `.app` folder, a `.pkg`: refused, nothing opens on the Mac.

## 5. Sound

- [ ] Play a video in Safari opened from Windows: sound plays on the PC, in sync with the
      picture (within about a tenth of a second).
- [ ] Sound of an app not opened from Windows is not heard (unless the Mac Desktop is open).
- [ ] **Ctrl+Alt+Shift+M** mutes and unmutes without a click; the **Volume** slider ramps.
- [ ] Switch the Windows output device (plug in headphones): sound follows within 2 s.
- [ ] On Wi-Fi with other traffic: no crackling over a few minutes; the stats overlay shows
      the audio buffer staying within 30–150 ms.
- [ ] Turning **Sound** off in Settings stops the stream (the stats show no audio packets).

## 6. Keyboard

- [ ] **Windows** mode: Ctrl+C / Ctrl+V / Ctrl+Z in TextEdit act as ⌘C / ⌘V / ⌘Z.
- [ ] **Mac** mode: Ctrl+C in Terminal sends Control-C; Win+C copies.
- [ ] **Fusion** mode: Home/End, Ctrl+Home/End, Ctrl+arrows, Ctrl+Backspace, Ctrl+Y work as in
      a Windows text field.
- [ ] Hold Ctrl in a Mac window and Alt+Tab away: on return, no key is stuck on the Mac.

## 7. Look and motion

- [ ] The ball's menu, Search, the banner and the loading window look right in light and dark
      mode, at 100 %, 150 % and 200 % scale, and on a second monitor with another scale.
- [ ] **Glass** set to Frosted and to Off; Windows' Transparency effects off: surfaces turn solid.
- [ ] Windows' Animation effects off (or **Animations** = Reduced): no sliding, only short fades.
- [ ] Opening and closing a menu quickly several times never leaves it half-drawn.

## 8. The Dock and full screen

- [ ] Move the pointer quickly to the left edge and away, or leave it resting there: the Dock
      stays hidden. Push the mouse on against the edge: the Dock slides out, centred on that
      edge, with the Mac Dock's items in its order (lines, folders, the Trash), dots on the
      running apps.
- [ ] Right-click an app, a folder and the Trash: the menu is the Mac Dock's (Options, Show
      in Finder, Quit, Empty Trash…); choose Quit, Keep in Dock / Remove from Dock: done on the
      Mac, and the Dock here follows within a few seconds.
- [ ] Many apps open (or a short screen): the icons shrink so the whole Dock fits.
- [ ] Hover an icon: it grows, its name beside it; click it: the app's window comes to the
      front (back from the taskbar when minimised); Open an App opens Taro Search.
- [ ] Move away: the Dock slides away within half a second. Settings → Dock → At the right
      edge: it comes out at the right edge instead; Off: never.
- [ ] Two monitors side by side: the edge between them never brings the Dock out.
- [ ] Green button on a Mac app: it fills the monitor over the taskbar, no band beside it on
      a Mac that makes a display of this size. Push against the bottom edge: the taskbar comes
      up; move up: the Mac app is in front again, the taskbar hidden.
- [ ] Kill Taro on the Mac (`kill -9`) while an app is full screen there, start it again:
      the Mac's menu bar and Dock are as before.
- [ ] Open TextEdit, Notes, Finder: no thin band of the desktop between the title bar and the
      app, no dark curve at the top corners.
- [ ] Close an app with its red button and open it again at once from the launcher or the
      Dock: it opens (no endless loading), the connection stays.

## 9. Stopping and uninstalling

- [ ] Disconnect from the ball's menu: the Mac's Start-menu entries disappear on the PC.
- [ ] Follow the user guide's uninstall steps on both sides; nothing of Taro is left
      (`~/Library/Application Support/RemoteMac`, `%APPDATA%\RemoteMac`).
