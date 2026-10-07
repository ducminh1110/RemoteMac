# Security policy

MacBridge gives remote control of a Mac, so security reports matter a lot to us.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability reporting
(*Security → Report a vulnerability* on this repository) with the details and, if possible, a
way to reproduce it. We will acknowledge it as soon as we can and coordinate a fix and its
disclosure with you.

## Current security model

- The password never leaves the machines: both sides derive a token from the ID and the
  password, and only that is compared (by the relay, or by the Mac on the local network, which
  locks out further attempts for a minute after 5 wrong passwords).
- A relay with an admission key (`RM_RELAY_KEY`) refuses clients without it.
- **The session link is not yet end-to-end encrypted.** Control, input, clipboard and app
  window video travel in the clear (the Mac Desktop's GameStream control and input are
  encrypted, as in Moonlight). Someone who can watch the traffic could see the session and
  replay the token. Use trusted networks or your own relay until encryption lands; it is the
  top item on the roadmap.
