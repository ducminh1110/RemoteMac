# Security policy

MacBridge gives remote control of a Mac, so security reports matter a lot to us.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability reporting
(*Security → Report a vulnerability* on this repository). Include the details and, if
possible, a way to reproduce it. We will acknowledge it as soon as we can and coordinate a fix
and its disclosure with you.

## Security model

**Protected:**

- **The password.** It is never sent, not even as a hash. The two sides run a
  password-authenticated key exchange (CPace-style, on P-256). Someone who records the traffic,
  the relay operator included, gets nothing to test passwords against offline. An active
  attacker gets one guess per connection, and the Mac locks for a minute after five wrong
  passwords in a row.
- **The session.** Everything after the handshake is encrypted and authenticated with
  ChaCha20-Poly1305: the session stream, and every UDP datagram carrying session data (video,
  input, reports, the Mac Desktop's GameStream tunnel). Keys are fresh for every session
  (forward secrecy).
- **IDs on a relay.** A relay hands out each Mac ID once, and only the Mac holding that ID's
  owner secret may wait under it.
- **A relay with an admission key** (`RM_RELAY_KEY`) refuses clients that do not present it.

**Not protected:**

- **Metadata.** A relay, or anyone on the path, sees the session ID, IP addresses, and the size
  and timing of the traffic.
- **Weak passwords.** The lockout slows online guessing but cannot save a password that is
  trivially guessed. Use a long, random password.
- **The machines themselves.** Anyone who can use the PC with MacBridge connected has the Mac.
  The Mac app can see and control everything the logged-in user can.
- **Admission keys inside release binaries.** They keep casual users off a relay, but a
  determined person can extract them. They are not a secret against the public.

**Known limitations:**

- The protocol and its implementations have not been independently audited.
- Hash-to-curve uses try-and-increment. The number of tries depends on the password, which
  could in theory leak a little through timing to a local observer.
- UDP datagrams have no replay window. A replayed datagram is authentic and is handled like
  the original: video is shown again, and input is ignored because its sequence number was
  already seen.
- Release binaries are ad-hoc signed only (not notarized or signed with a developer
  certificate).

The design, step by step, is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#end-to-end-encryption).
