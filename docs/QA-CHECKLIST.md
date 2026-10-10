# QA checklist

What to check by hand on real machines before a release. The automated tests (see
[ARCHITECTURE.md](ARCHITECTURE.md#testing)) cover the protocol, the Mac app end to end on a CI
Mac and the Windows app against a scripted Mac, but not real speakers, real monitors or a real
user's Dock. Write the result next to each item (✓, ✗ with a note, or "n/a").

Setup: a Mac with macOS 14 or later and a logged-in user; a Windows 10 or 11 PC with speakers
or headphones; both on the same network unless the item says otherwise. Note both versions
(`./macbridge --version`, the first line of `MacBridge.exe --logs-enabled`'s log).

## 1. Installing and starting

- [ ] On a Mac that never ran MacBridge, `./macbridge.sh --check` lists Screen Recording and
      Accessibility as missing, names the terminal app, and exits with 3.
- [ ] A copy downloaded with a browser (quarantined): `./macbridge.sh` says so and shows the
      `xattr` command; after it, it starts.
- [ ] After allowing both permissions and reopening the terminal, `./macbridge.sh --check`
      exits with 0 and `./macbridge.sh --password …` shows the ID, the password and the Mac's
      addresses, then goes to the background.
- [ ] `./macbridge.sh --stop` stops it.

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
- [ ] **Ctrl+Alt+Space** opens MacBridge Search; typing filters; Enter opens the app, or
      switches to it when it is already open; Escape closes; arrow keys move.
- [ ] In the Mac Desktop, double-click a document in Finder: its app's window also appears as
      a Windows window.

## 3b. Exact windows, the menu bar, shapes

- [ ] By default (menus in the title bar): Finder, Safari, TextEdit windows have MacBridge's
      title bar with the app's menus; clicks and typing in them work at once, also with the
      pointer moving fast; resizing by the edges and the green button work.
- [ ] The title bar's text matches the launcher's (same font and spacing), light and dark; the
      menu title under the pointer gets a pill. Menus open as glass menus; TextEdit's Format >
      Font opens beside it; moving along the titles switches menus; Left / Right / Escape work;
      the window's buttons stay coloured while a menu is open.
- [ ] The launcher's green button fills the screen and the launcher is drawn at that size
      (nothing black, no second copy of it); again restores it.
- [ ] Leave the Mac unused for an hour, and asleep for a while: the PC connects again at once.
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

## 8. Desktop Fusion (experimental)

- [ ] Turn on **Desktop Fusion**: the Mac's desktop picture becomes the PC's within a few
      seconds; the Dock slides in at the bottom of the PC's screen when the pointer comes down
      to the taskbar under it (no need to press against the bottom of the screen), and away
      when it leaves.
- [ ] Clicking an app in the Dock opens it as a Windows window; a right-click menu of a Dock
      icon shows over the Dock.
- [ ] Change the PC's wallpaper while connected: the Mac follows within 4–5 s.
- [ ] Turn Desktop Fusion off, disconnect, and `./macbridge --stop`: each time the Mac's own
      desktop picture is back.
- [ ] Kill MacBridge on the Mac (`kill -9`) while Fusion is on, then start it again: the Mac's
      own desktop picture is back.
- [ ] With automatic hiding of the Dock on the Mac: the setting is left as it is and the
      viewer's own Dock stands in.
- [ ] The Windows taskbar and Explorer are unchanged throughout.

## 9. Stopping and uninstalling

- [ ] Disconnect from the ball's menu: the Mac's Start-menu entries disappear on the PC.
- [ ] Follow the user guide's uninstall steps on both sides; nothing of MacBridge is left
      (`~/Library/Application Support/RemoteMac`, `%APPDATA%\RemoteMac`).
