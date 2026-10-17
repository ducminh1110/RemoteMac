# GitHub bootstrap (Setup Wizard step 7)

Implemented in `enterprise/apps/setup-wizard/src/github.rs` (REST client and device flow),
`src/bootstrap.rs` (plan and execution), `src/sealed.rs` (secret encryption) and
`src/remember.rs` (optional DPAPI storage). Anything marked **Planned** does not exist yet.

## Authentication

### (a) OAuth device flow

Available when a client id is configured (`TARO_SETUP_GITHUB_CLIENT_ID`) or typed into the UI.
The app (OAuth App or GitHub App) must have device flow enabled.

1. `POST https://github.com/login/device/code` (`client_id`, `scope=repo workflow`; GitHub Apps
   ignore the scope and use their own permissions).
2. The UI shows the `user_code` and a link to `verification_uri`. The `device_code` stays in
   the wizard and is never shown.
3. A background task polls `POST https://github.com/login/oauth/access_token` with
   `grant_type=urn:ietf:params:oauth:grant-type:device_code`, waiting `interval` seconds
   between polls:
   - `authorization_pending`: keep polling;
   - `slow_down`: the interval grows by at least 5 s (or to the larger `interval` GitHub
     returns) for all later polls, per RFC 8628;
   - `expired_token` / `access_denied`: stop with a message;
   - network errors: retried up to 3 times.
4. The received token is validated with `GET https://api.github.com/user`.

**Cancel** stops the polling task. github.com receives the client id and device code only;
the access token is received from it and only ever sent to `api.github.com`.

### (b) Fine-grained personal access token

Pasted into a password field. Validated with `GET /user`; the `ghp_`/`github_pat_` prefix is
not trusted. For classic tokens the `X-OAuth-Scopes` header is shown.

Minimum fine-grained permissions (resource owner = the user or organisation that will own the
new repository; repository access: all repositories, or the template plus the new repository):

| Permission | Access | Used for |
|---|---|---|
| Administration | Read and write | generate the repository from the template; set workflow permissions |
| Contents | Read and write | commit `deploy/deployment.json` and branding files |
| Actions | Read and write | dispatch the release workflow, read runs and logs |
| Secrets | Read and write | read the Actions public key, write Actions secrets |
| Workflows | Read and write | the template contains workflow files |
| Metadata | Read-only | mandatory for fine-grained tokens |

Deleting the repository from the wizard additionally needs `delete_repo` (classic token /
OAuth) or Administration write; without it the wizard explains how to delete it on github.com.

### Token handling rules

- In memory only, in a `Secret` (no `Display`/`Serialize`, `[REDACTED]` debug output,
  zeroed on drop). **Sign out** drops it.
- Never logged (exact-value and pattern redaction in the diagnostic log), never written to
  disk unless "Remember on this PC" is ticked, never put in a URL or command line, never
  returned to the UI (the UI sees "Authenticated as `<login>`").
- Sent only to the GitHub API base (`https://api.github.com`). The client does not follow
  redirects; the job-log redirect is followed manually without the token.

### "Remember on this PC" (Windows only)

When ticked, the token is encrypted with DPAPI (`CryptProtectData`, current Windows user,
`CRYPTPROTECT_UI_FORBIDDEN`) and written to `%APPDATA%\TaroEnterpriseSetup\github-token.dpapi`.
On the next start the GitHub step offers **Use remembered token** (decrypt with
`CryptUnprotectData`, then `GET /user`) and **Forget it** (deletes the file). On macOS and
Linux the option is not offered.

## Choosing owner and repository

- Owner: the authenticated user, or an organisation from `GET /user/orgs`. For each org
  `GET /user/memberships/orgs/{org}` gives the role; admins can create repositories; for
  members `GET /orgs/{org}` → `members_can_create_private_repositories` decides; otherwise
  "permission unknown" is shown (GitHub decides on creation).
- Repository name: letters, digits, `-`, `_`, `.`, 1-100 characters.
- Template: default `ducminh1110/taro-enterprise-template` (override in the UI or with
  `TARO_SETUP_TEMPLATE`).

## Plan and confirmation

**Review plan** produces the exact ordered list of actions; nothing is sent to GitHub yet.
**Create repository** requires ticking the confirmation; the server rebuilds the plan and
refuses (`409`) if it differs from the one shown (for example after editing inputs).

Actions, executed in order, each with its own real status (`ok` / `warn` / `fail` /
`skipped`):

1. **template**: `GET /repos/{template_owner}/{template_repo}`; must exist, be readable and have
   `is_template: true`. Otherwise a clear error and nothing is created.
2. **create**: `POST /repos/{template_owner}/{template_repo}/generate` with
   `{owner, name, description, include_all_branches: false, private: true}`. If GitHub reports
   the new repository as public, the bootstrap stops before writing anything. A `422` (name
   taken) asks for another name. Re-running after a later failure re-uses the repository
   created in this session instead of creating another.
3. **workflow_perms**: `PUT /repos/{o}/{r}/actions/permissions/workflow` with
   `default_workflow_permissions: "read"`. A refusal (organisation policy, missing permission)
   is a warning with instructions, not a failure.
4. **Actions secrets**: `GET /repos/{o}/{r}/actions/secrets/public-key`, then for each secret a
   libsodium sealed box (`crypto_box_seal`: X25519 + XSalsa20-Poly1305, pure Rust
   `crypto_box` crate) to that key, base64-encoded, `PUT /repos/{o}/{r}/actions/secrets/{name}`
   with `{encrypted_value, key_id}`:
   - `TARO_SECRET_KEY`: 32 random bytes (hex) generated on this PC; never shown;
   - `STRIPE_SECRET_KEY`: the validated Stripe key (skipped if billing is disabled or the key
     was not validated);
   - `STRIPE_WEBHOOK_SECRET`: entered or captured when the wizard created the endpoint
     (skipped if absent).
5. **Files** via `PUT /repos/{o}/{r}/contents/{path}` (existing files are updated using their
   `sha`):
   - `deploy/deployment.json`: environment, profile, portal/ops URLs, provider adapter,
     bootstrap admin email, names, support contact, modules, branding values (colour, theme,
     density, image paths), domain strategy/name/target, billing mode, webhook path/events.
     No secrets and no image bytes.
   - `deploy/branding/logo.<ext>` and `deploy/branding/favicon.<ext>` if uploaded.

Calls that hit a freshly generated repository retry on `404`/`409` while GitHub finishes
creating it. The first failing action stops the run; later actions are marked
"not run because an earlier action failed".

## Failure, cleanup and rollback

If an action fails after the repository was created, the GitHub step shows:

- **Retry remaining actions** (fix the cause first; creation is not repeated);
- guidance to delete a Stripe webhook endpoint the wizard created;
- **Delete the repository I just created**: only for a repository created in this session,
  only after typing its full name; `DELETE /repos/{o}/{r}`. If GitHub refuses (`403`/`404`),
  the wizard explains that `delete_repo` (classic/OAuth) or Administration write is needed and
  how to delete it manually.

## Release (step 8)

Described in [SETUP_WIZARD.md](SETUP_WIZARD.md#8-build--release). The template must provide
`.github/workflows/enterprise-release.yml` with a `workflow_dispatch` trigger and a `version`
input that publishes a release tagged exactly `{version}` with at least one asset.

## Planned

- Creating the template repository `ducminh1110/taro-enterprise-template` itself.
- A GitHub App manifest flow (one-click app creation) instead of a pre-configured client id.
- Environment-scoped secrets (`/environments/{env}/secrets`) instead of repository secrets.
- Token persistence on macOS (Keychain) and Linux (Secret Service).
