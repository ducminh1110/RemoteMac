-- Signing in to the Taro app (a device authorization grant, as RFC 8628): the app asks for a
-- code, the user approves it in the portal, the app gets a token for its "My Macs" list. Only
-- hashes of the device code and the token are stored.

CREATE TABLE device_authorizations (
    id uuid PRIMARY KEY,
    device_code_hash bytea NOT NULL UNIQUE,
    user_code text NOT NULL UNIQUE,
    client text NOT NULL,
    ip text,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    last_poll_at timestamptz,
    approved_by uuid REFERENCES users (id),
    approved_at timestamptz,
    denied_at timestamptz,
    consumed_at timestamptz
);

CREATE TABLE device_tokens (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id),
    secret_hash bytea NOT NULL,
    client text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);

CREATE INDEX device_tokens_user ON device_tokens (user_id) WHERE revoked_at IS NULL;

INSERT INTO feature_flags (key, description, enabled) VALUES
    ('devices.sign_in', 'The Taro app can be signed in to (off: no new app sign-ins)', true)
ON CONFLICT (key) DO NOTHING;
