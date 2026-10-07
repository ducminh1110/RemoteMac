# Contributing to MacBridge

Thanks for helping! Bug reports, fixes and features are all welcome.

## Reporting a bug

Open an issue with:

- what you did, what you expected, what happened (a screenshot helps a lot);
- logs from both sides: start `MacBridge.exe --logs-enabled` (log in
  `%APPDATA%\RemoteMac\viewer.log`) and `./macbridge --foreground --logs-enabled ...` on the Mac
  (log in the terminal);
- Windows version and GPU, macOS version and Mac model, and whether the connection was on the
  same network or through a relay.

Remove IDs, passwords and relay keys from logs before posting them.

## Making a change

1. Fork, and branch from the default branch.
2. Keep the change focused; match the style of the surrounding code (short functions, comments
   that say *why*).
3. Before opening a pull request:

   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   # viewer changes: also check the Windows target
   cargo clippy -p rm-viewer --target x86_64-pc-windows-msvc --all-targets -- -D warnings
   # Mac host changes (on a Mac)
   ./scripts/build-agent-macos.sh && ./scripts/e2e-macos.sh
   ```

4. Describe what changed and how you tested it in the pull request. CI builds every part and
   runs end-to-end tests on real macOS and Windows runners.

## Where things are

See *Project layout* in the [README](README.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
The wire protocol and the encryption live in `crates/rm-protocol`; the Swift host mirrors them
in `agent/macos/Wire.swift` and `agent/macos/Secure.swift`, so keep the two in step (shared test
vectors, checked by the macOS end-to-end test, catch drift).

## License

By contributing you agree that your contribution is licensed under the GPL-3.0-or-later, the
project's license. Code taken from other projects must be GPL-3.0 compatible and listed in
[NOTICE.md](NOTICE.md).
