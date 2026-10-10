# MacBridge architecture

How the pieces fit together, for contributors and anyone who wants to check how MacBridge
works. User-facing instructions are in the [user guide](USER-GUIDE.md).

- [Components](#components)
- [Connection paths](#connection-paths)
- [Relay protocol](#relay-protocol)
- [End-to-end encryption](#end-to-end-encryption)
- [Session protocol](#session-protocol)
- [Video and input over UDP](#video-and-input-over-udp)
- [Window shapes, exact windows and the menu bar](#window-shapes-exact-windows-and-the-menu-bar)
- [Sound](#sound)
- [Opening files and adopting windows](#opening-files-and-adopting-windows)
- [Desktop Fusion](#desktop-fusion)
- [Mac Desktop over GameStream](#mac-desktop-over-gamestream)
- [The Windows app's own surfaces](#the-windows-apps-own-surfaces)
- [Testing](#testing)

## Components

| Component | Language | Where | Role |
|---|---|---|---|
| Mac app (`macbridge`) | Swift + a little Objective-C | `agent/macos/` | captures windows (ScreenCaptureKit), encodes H.264 (VideoToolbox), injects input, reads menus and windows (Accessibility), launches apps (LaunchServices), virtual displays |
| Windows app (`MacBridge.exe`) | Rust (Win32) | `crates/rm-viewer/` | one native window per Mac window, decoding (Media Foundation / openh264), presentation (D3D11, DirectComposition), input, clipboard, launcher |
| Relay (`rm-relay`) | Rust | `crates/rm-relay/` | pairs the two sides, forwards TCP and UDP, hands out Mac IDs |
| Protocol | Rust (mirrored in Swift) | `crates/rm-protocol/`, `agent/macos/Wire.swift` | framing, messages, UDP/FEC, sessions, encryption |
| GameStream | Rust + C | `crates/rm-gamestream/`, `crates/moonlight-sys/` | the Mac Desktop: a host ported from Sunshine, the Moonlight client core |
| Test tools | Rust | `crates/rm-client/`, `crates/rm-fakeagent/` | a command-line client for end-to-end tests; a scripted stand-in for the Mac |

## Connection paths

```
viewer                                                           Mac
  │  1. LAN: UDP broadcast "RMLAN?rm-<ID>" to port 7471  ──────────▶ │
  │  ◀────────────────────────────── "RMLAN!rm-<ID> <tcp port>"      │
  │  2. TCP to that port: join line ─────────────────────────────▶   │  READY
  │         (or, when nobody answers within 0.8 s, the same join line to the relay)
  │  3. end-to-end handshake ◀──────────────────────────────────▶    │
  │  4. session protocol, encrypted ◀───────────────────────────▶    │
```

The Mac waits on both paths at once: its LAN port and a wait at the relay. Whichever viewer
arrives first gets the session, and the other path is closed.

**Typed address.** When the user types the Mac's address (`host`, `host:port`, `[v6]:port`,
bare IPv6; 7471 by default), the viewer skips discovery and the relay: it resolves the name and
tries each address in turn with the same join line (step 2). The Mac's TCP listener is
dual-stack (IPv6 with IPv4-mapped addresses), on 7471 or `--port`. Steps 3 and 4 are the same,
so a typed address is as private as any other path, and nothing falls back to plain text.
`lan::parse_address` and `lan::connect_direct` in `crates/rm-relay`. After each session the Mac
starts over (a fresh process, same ID and password) and waits for the next viewer.

## Relay protocol

Each side opens TCP to the relay and sends one JSON line:

```json
{"session_id":"rm-123456789","role":"agent","token":"<relay token>","key":"<admission key>","owner":"<owner secret>"}
```

- `token` is `hex(SHA-256("remotemac/v2/relay:" + session))[..48]`. It pairs the two sides and
  says nothing about the password.
- `key` is the relay's admission key, if the relay has one (`RM_RELAY_KEY`).
- `owner`, from the Mac only, proves it owns an ID the relay handed out.

When both sides are there, the relay answers `READY` to each and forwards bytes both ways.
Otherwise it answers `ERR …`: `not admitted`, `no such session`, `locked`, `relay busy`,
`pair timeout`, or `this ID belongs to another Mac`.

**IDs.** A Mac asks for its ID with `{"claim_id":"<owner secret>","want":"<previous ID>"}` and
gets `ID 123456789`. An ID belongs to the first owner that got it, and the table is kept on
disk (`RM_RELAY_IDS`, or systemd's state directory).

**UDP.** The same port forwards datagrams between the two sides of a paired session, once each
has registered (`"RM" 1 …` with session, token and key). Types below 16 are the relay's own.

## End-to-end encryption

Implemented in `crates/rm-protocol/src/secure.rs` and `agent/macos/Secure.swift`, byte for byte
alike. Shared test vectors are checked on both sides.

**Inputs.** The session `S` (`rm-<ID>`) and the session secret `P`
(`hex(SHA-256("remotemac/v1:ID:PASSWORD"))[..48]`, which is never sent).

**Generator.** `G` is the first point whose x-coordinate is
`SHA-256("remotemac/v2/G" | u16 len | S | u16 len | P | counter)` for `counter = 0, 1, …`, taken
with even y. This is P-256 hash-to-curve by try-and-increment.

**Handshake** (CPace-style, x-coordinates only, as CryptoKit's ECDH gives):

```
Mac:     y_m random;  Ym = x(y_m·G)
Mac    → viewer   "RMK2" | Ym                       ("RMKL" | 32 zero bytes: locked)
viewer:  y_c random;  Yc = x(y_c·G);  K = x(y_c·lift(Ym))
         keys = HKDF-SHA256(salt = S, ikm = K, info = "remotemac/v2 keys" | Ym | Yc, 160 bytes)
              = kc | Mac→viewer stream | viewer→Mac stream | Mac→viewer UDP | viewer→Mac UDP
viewer → Mac      Yc | HMAC-SHA256(kc, "client" | Ym | Yc)
Mac:     K = x(y_m·lift(Yc)); checks the viewer's tag
Mac    → viewer   1 | HMAC-SHA256(kc, "agent" | Ym | Yc)       or   0 (wrong password)
```

`lift(x)` is the point with x-coordinate `x` and even y. Using x-coordinates only is sound
because `x(k·P) = x(k·(−P))`. Someone who does not know `P` (the relay included) can neither
compute `K` nor test password guesses offline against what they saw. An active attacker gets
one guess per connection. The Mac counts wrong passwords and locks for 60 s after five in a
row. The scalars are fresh for each session, so recorded sessions stay safe if the password
leaks later.

**Stream.** After the handshake, each direction is a sequence of records
`u32 BE length | ChaCha20-Poly1305(plaintext ≤ 64 KiB)`, with nonce `00000000 | u64 BE counter`.

**Datagrams.** Every datagram of type 16 and up except the hole punch (type 20) is sent as
`"RM" | type | u64 BE seq | ChaCha20-Poly1305(rest)`. The 3-byte header is the associated
data, and the nonce is `00000000 | seq`. The relay's own types (1–3) and the hole punch stay
readable: they carry no session data. The hole punch's secret is exchanged over the encrypted
stream.

## Session protocol

Inside the encrypted stream, frames are `u32 BE length | u8 channel | payload`. Channels are
input, control, window metadata, video, clipboard, files, telemetry and audio. Payloads are JSON
messages (`crates/rm-protocol/src/lib.rs`, `Message`), except video, which is binary:
window, timestamp, keyframe flag, size, and H.264 Annex-B data.

A session starts with `client_hello` / `server_hello` (version negotiation) and the Mac's
`capability_report` (Screen Recording, Accessibility, hardware encoding). Each hello also lists
the side's **features**; only those both list are used (`negotiate` in `rm-protocol`):

| Feature | Meaning |
|---|---|
| `control` | input, launching, window control |
| `video` | per-window H.264 |
| `audio` | the Mac's sound (`audio_control`, the audio channel and datagram) |
| `open_file` | `open_file` for uploaded documents |
| `fusion` | `dock_stream`, `set_wallpaper`, `restore_wallpaper` |
| `mask` | `window_mask` (pictures' shapes) |
| `exact` | `window_style`, `window_chrome` (windows as the Mac draws them) |
| `menubar` | `menu_bar_stream`, `menu_bar_status` (the Mac's menu bar) |

An older Mac simply lacks the newer names, and the viewer hides what needs them.

**Connection phases** (`crates/rm-viewer/src/lifecycle.rs`): Idle → Connecting →
Authenticating → Negotiating → EstablishingMedia → Connected, then Reconnecting (back through
the same steps) or Disconnecting → Disconnected; Error from any step. Only the transitions in
`Phase::allows` are taken; the connect window and the reconnect banner show the current one. The viewer then lists
and launches apps. The Mac reports windows (`window_created`, `window_moved`, …) with their
menus and streams each window as its own H.264 stream.

## Video and input over UDP

- **FEC.** Video frames are cut into 1200-byte shards with Reed-Solomon parity (10–50 %,
  adapted to the measured loss), so lost packets are rebuilt instead of resent.
- **Feedback.** The viewer reports what arrived every 200 ms. The Mac sends video over UDP
  only while these reports come in, so a blocked UDP path falls back to TCP by itself. The
  bitrate follows queueing delay and loss.
- **Direct path.** Both sides exchange their addresses (LAN, and the public one from STUN)
  over the encrypted stream and punch through to each other. The first address that answers
  becomes the path for video and input. LAN addresses win over public ones.
- **Input** goes over UDP as a reliable, ordered stream (sequence numbers, acks, resends) once
  a direct path exists, and over TCP otherwise.

## Window shapes, exact windows and the menu bar

- **Shapes** (`window_mask`, feature `mask`; `agent/macos/Shape.swift`, `crates/rm-protocol/src/mask.rs`):
  the video has no alpha, so when a stream starts the Mac takes one BGRA screenshot of the same
  thing (same filter, size and part) and sends its alpha as runs (u16 length, u8 alpha). The
  viewer draws the picture through it in the NV12 shader on a premultiplied swap chain (the
  video's edge pixels are already the window's colour mixed with black, i.e. premultiplied), so
  corners, menus and the Dock are the Mac's outline over whatever is behind them. A shape of
  another size than the picture is not used.
- **Exact windows** (`window_style`, `window_chrome`, feature `exact`): the Mac stops cutting the
  title bar and keeps its buttons in the picture, and describes the title bar through
  Accessibility: the band (down to a toolbar at the top), the three buttons, and the controls in
  the band. The viewer's frame is just the picture: the free band is HTCAPTION (Windows moves and
  snaps it), the buttons act here (close, minimise, full screen), the rest goes to the Mac.
- **The menu bar** (`menu_bar_stream`, `menu_bar_status`, feature `menubar`; `MenuStrip.swift`):
  the main display's top strip, streamed as window 0x7FFF0003 (no window covers it, so it is the
  bar as it is). Menus whose top is at the bar's bottom are popups of it, whoever owns them. On
  Windows the strip is a top-most window at the top of the monitor of the Mac window in front,
  shown only while a Mac window is in front.

## Sound

- **Capture** (`agent/macos/Audio.swift`): ScreenCaptureKit's audio of the session's apps (all
  apps while the Mac Desktop is open), never MacBridge's own (`excludesCurrentProcessAudio`).
  It starts on the viewer's `audio_control {enabled: true}` and answers `audio_status`
  (`playing`, `unavailable` with a reason). After one second of digital silence nothing is
  sent.
- **Packets** (`crates/rm-protocol/src/audio.rs`): PCM s16le, 48 kHz stereo, 5 ms (240
  frames). Payload: `seq u32 | pts_us u64 | format u8 (1 = PCM s16le) | channels u8 | frames
  u16 | samples`. Over UDP it is datagram type 26, sealed like video; without UDP it rides
  channel 7 of the encrypted stream.
- **Playback** (`crates/rm-viewer/src/audio.rs`): a jitter buffer starts with 40 ms and retargets
  between 30 and 150 ms from the measured jitter (RFC 3550); above target + 80 ms it trims; a
  lost packet is faded, an underrun waits for the cushion again; clock drift is corrected by
  dropping or repeating one frame in 480. WASAPI shared mode, event driven, on the default
  output device (followed when it changes); volume and mute ramp to avoid clicks.

## Opening files and adopting windows

- **`open_file {path, application_id}`** opens an uploaded document (`agent/macos/Open.swift`).
  The path must be in the uploads folder or the user's home (not `~/Library`, not a hidden
  folder), and the file must be a document: anything that runs code (`.app`, `.command`,
  scripts, packages, executables, by extension, UTType and the executable bit) is refused. It is
  opened with LaunchServices (`NSWorkspace`) in the named app or the default one; no shell is
  ever involved.
- **Adoption** (`Windows.swift`, `Apps.swift`): a new window of an app outside the session that
  appears within 5 s after the viewer's click or key press (on a window, not the desktop) is
  taken into the session (`app_launched`, then `window_created`), so a document opened in
  Finder or an app started from the Dock becomes a Windows window.

## Desktop Fusion

- **The Dock** is a window of its own (`dockWindowID` 0x7FFF0002, app `dock`). The Mac finds the
  Dock's rectangle through Accessibility (its `AXList`, plus room for magnification) and captures
  that region of the screen with a content filter that includes only the Dock's process and the
  owners of desktop-level windows, so app windows behind it never show. `dock_status` reports
  the rectangle, the screen edge, or why it is unavailable (an auto-hiding Dock is left alone).
  The Dock's menus are reported as popups whose parent is the Dock window.
- **Wallpaper**: the viewer uploads the PC's desktop picture (or sends its plain colour) and asks
  `set_wallpaper`. The Mac saves its own desktop pictures once to
  `~/Library/Application Support/RemoteMac/wallpaper-restore.json`, then sets the new one;
  `restore_wallpaper`, the session's end, `--stop` and the next start (when no other MacBridge
  runs) put the saved ones back and remove the file.
- **On Windows** (`ui.rs`, `fusion_*`): the Dock window is borderless, top-most, placed at the
  bottom of the monitor and slid in and out by the pointer; the wallpaper is checked every 4 s
  and sent again when it changes. If the Mac cannot stream its Dock, a Dock drawn by the viewer
  (`dock.rs`) with the session's apps stands in.

## Mac Desktop over GameStream

The Mac Desktop uses the GameStream protocol, like Sunshine and Moonlight. The Mac runs a host
ported from Sunshine (RTSP, ENet control and input, RTP video with FEC). The viewer runs
Moonlight's own client core (moonlight-common-c). Both talk through a tunnel: RTSP rides the
encrypted session stream, and the UDP flows ride the session's encrypted datagrams.

## The Windows app's own surfaces

MacBridge's own menus, search, loading window and banners are small layered windows drawn on
the CPU (`paint.rs`: premultiplied BGRA, SDF rounded shapes, blur, shadows), never over the
live video.

- **Liquid Glass** (`glass.rs`) is adapted from MobileLab (MIT, see NOTICE.md): the backdrop is
  captured before the surface is shown, blurred, refracted at the edges and lit. Three levels
  (glass, frosted, solid) from Settings; Windows' "Transparency effects" off, or
  `RM_REDUCE_TRANSPARENCY=1`, gives solid.
- **Motion** (`motion.rs`): duration tokens (press 80–160 ms, popover 120–200, menu 160–240,
  window 200–360, mode 250–420), Apple's curves and an analytic spring. Every animation can be
  retargeted mid-way from its current value and speed. Reduced motion (Windows' animation
  effects off, or Settings) shortens everything to at most 90 ms and turns springs into eased
  steps.
- **Keyboard** (`keymap.rs`): the three modes are a pure mapping from a Windows key and its
  modifiers to Mac keys, unit-tested; keys still down when a window loses the focus are
  released on the Mac.

## Testing

| What | Where | Runs on |
|---|---|---|
| unit tests of every Rust crate (protocol, encryption, relay, LAN discovery, FEC, decoder…) | `cargo test --workspace` | any OS |
| the Windows viewer against the scripted stand-in Mac, rendering checked, screenshots of the loading window, glass menu, MacBridge Search and reconnect banner | `.github/workflows/windows-viewer.yml` | Windows runners |
| the real Mac app end to end (encryption, LAN, typed address over IPv4 and IPv6, relay, video, input, menus, dialogs, files, opening documents and refusing programs, adoption, sound packets, the streamed Dock, wallpaper set and restored, fullscreen, Mac Desktop, the launcher script) | `scripts/e2e-macos.sh` | macOS runners |
| a recorded real Mac session replayed into the viewer | `.github/workflows/mac-to-windows.yml` | macOS + Windows runners |
