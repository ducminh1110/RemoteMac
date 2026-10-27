# Taro Enterprise: one server, real Macs, admin control

Contract between the parts built together for Taro Enterprise 0.2.0:

1. **The relay server is the whole product, one web site.** `taro-relay run` serves, on the
   relay's own HTTPS (443, with port 80 for certificates and the redirect), the Customer Portal
   at `https://<relay host>/`, the Provider Console at `https://<relay host>/console/` and the
   relay's status page at `https://<relay host>/relay/`. The relay port for the apps (7470)
   stays as it is. It also runs the API with its own PostgreSQL (bundled in the core, run by the relay, data in
   `/var/lib/taro/pg`). Updating a 0.1.x relay installs them: the first start after the update
   prepares the database and prints the setup code.
2. **First visit to the console creates the administrator** (email, name, password, and the
   setup code printed on the server: `sudo taro-relay portal setup-code`). Administrators sign in
   to the console with their password (no mail server needed).
3. **Macs enroll themselves.** Taro for Mac, Enterprise edition (`Taro.app`), asks for Screen
   Recording and Accessibility, then for an enrollment code made in the console (Macs > Add a
   Mac). Enrolled, it keeps a heartbeat with the server; the server chooses its session password
   and changes it whenever access must end (a rental ends, an administrator takes over).
4. **Administrators control Macs.** Console > Macs > Control opens the Mac in the Taro app. A Mac
   assigned to a customer is taken over: the customer's session ends and they cannot connect
   until the administrator presses Release.

## API (JSON; errors are `{"error": {"code", "message"}}` with the usual statuses)

### Console (paths under `/console`: `/console/api/v1/...`; session cookie + `X-CSRF-Token` on writes, as the other console calls)

The console page gets `<base href="/console/">` from the server: every URL it uses (scripts,
styles, API calls, its own routes) is relative to it, so it works at `/` on its own origin too.
Validation errors are 422 with a message to show.

| Call | Body | Answer |
|---|---|---|
| `GET /api/v1/setup/status` (no session) | | `{"needs_admin": bool, "mail": bool}` |
| `POST /api/v1/setup/admin` (no session) | `{"setup_code", "email", "name", "password"}` | 201, signed in (session cookie): `{"user_id"}`; then `GET /api/v1/me` as after any sign-in. 422 wrong code or a password under 12 characters, 409 an admin exists |
| `POST /api/v1/auth/password` (no session) | `{"email", "password"}` | 200, signed in: same as the code sign-in (`{"user_id"}` and the cookie). 422 wrong email or password (one message for both) |
| `POST /api/v1/auth/password/change` | `{"current", "new"}` | 200 |
| `GET /api/v1/ops/fleet` | | `{"macs": [Mac], "enroll": {"server", "relay"}}` |
| `POST /api/v1/ops/fleet/enrollment-codes` | `{"label"?}` | 201 `{"code": "ABCD-EFGH-JKLM", "expires_at"}` (one use, 24 hours) |
| `POST /api/v1/ops/fleet/{id}/control` | | 201 `{"taro_link", "expires_at", "took_over": bool}` |
| `POST /api/v1/ops/fleet/{id}/release` | | 200 `{"released": true}` |
| `DELETE /api/v1/ops/fleet/{id}` | | 204 (not while assigned: 409) |

`Mac`: `{"id", "display_name", "model", "chip", "macos", "agent_version", "online", "last_seen_at",
"taro_id", "state", "rental_id", "assigned_email", "organization", "control": {"by_email", "since"} | null}`.
`online`: a heartbeat in the last 60 seconds.

### For the Mac (the portal's paths, `https://<relay host>/api/v1/agent/...`; no session, no CSRF; `Authorization: Bearer <token>`)

| Call | Body | Answer |
|---|---|---|
| `POST /api/v1/agent/enroll` (no token) | `{"code", "name", "model", "chip", "cpu_cores", "memory_gb", "storage_gb", "macos", "version"}` | 201 `{"agent_id", "token", "heartbeat_seconds"}`. 422 the code is wrong, used or expired |
| `POST /api/v1/agent/heartbeat` | `{"taro_id", "applied_epoch", "version"}` | 200 `{"password", "epoch", "relay", "heartbeat_seconds"}`. 401: the Mac was removed |

The Mac runs its session with `password`. When `epoch` differs from `applied_epoch`, it restarts its
session with the new password (any viewer is disconnected) and reports the new epoch next time.
`relay` is `host:port` of the provider's relay. `taro_id` is the ID the relay gave the Mac.

### The customer side

`POST /api/v1/rentals/{id}/connect-grant` answers 409 *"An administrator is working on this Mac.
You can connect again when they hand it back."* while an administrator controls it.
