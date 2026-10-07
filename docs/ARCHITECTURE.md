# MacBridge architecture

How the pieces fit together, for contributors and anyone who wants to check how MacBridge
works. User-facing instructions are in the [user guide](USER-GUIDE.md).

- [Components](#components)
- [Connection paths](#connection-paths)
- [Relay protocol](#relay-protocol)
- [End-to-end encryption](#end-to-end-encryption)
- [Session protocol](#session-protocol)
- [Video and input over UDP](#video-and-input-over-udp)
- [Mac Desktop over GameStream](#mac-desktop-over-gamestream)
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
arrives first gets the session, and the other path is closed. After each session the Mac
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
input, control, window metadata, video, clipboard, files and telemetry. Payloads are JSON
messages (`crates/rm-protocol/src/lib.rs`, `Message`), except video, which is binary:
window, timestamp, keyframe flag, size, and H.264 Annex-B data.

A session starts with `client_hello` / `server_hello` (version negotiation) and the Mac's
`capability_report` (Screen Recording, Accessibility, hardware encoding). The viewer then lists
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

## Mac Desktop over GameStream

The Mac Desktop uses the GameStream protocol, like Sunshine and Moonlight. The Mac runs a host
ported from Sunshine (RTSP, ENet control and input, RTP video with FEC). The viewer runs
Moonlight's own client core (moonlight-common-c). Both talk through a tunnel: RTSP rides the
encrypted session stream, and the UDP flows ride the session's encrypted datagrams.

## Testing

| What | Where | Runs on |
|---|---|---|
| unit tests of every Rust crate (protocol, encryption, relay, LAN discovery, FEC, decoder…) | `cargo test --workspace` | any OS |
| the Windows viewer against the scripted stand-in Mac, rendering checked | `.github/workflows/windows-viewer.yml` | Windows runners |
| the real Mac app end to end (encryption, LAN, relay, video, input, menus, dialogs, files, fullscreen, Mac Desktop) | `scripts/e2e-macos.sh` | macOS runners |
| a recorded real Mac session replayed into the viewer | `.github/workflows/mac-to-windows.yml` | macOS + Windows runners |
