-- One server for everything (Taro Enterprise 0.2): the first administrator is made on the first
-- visit to the console and signs in with a password; Macs enroll themselves (Taro for Mac,
-- Enterprise edition); administrators can take a Mac over.

-- an Argon2id hash; console operators only (customers sign in by email or Google)
ALTER TABLE users ADD COLUMN password_hash text;

-- one-use codes an administrator makes to add a Mac (only their hashes are kept)
CREATE TABLE mac_enrollment_codes (
    id          uuid PRIMARY KEY,
    code_hash   bytea NOT NULL UNIQUE,
    label       text,
    created_by  uuid REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    agent_id    uuid
);

-- Macs that enrolled: their facts, their credential (hash), and the session password's epoch.
-- The password itself is derived from the server's secret and the epoch, never stored; a new
-- epoch (a rental ends, an administrator takes over) ends every session made with the old one.
CREATE TABLE mac_agents (
    id             uuid PRIMARY KEY,
    secret_hash    bytea NOT NULL,
    display_name   text NOT NULL,
    model          text NOT NULL,
    chip           text NOT NULL,
    cpu_cores      integer NOT NULL,
    memory_gb      integer NOT NULL,
    storage_gb     integer NOT NULL,
    macos          text NOT NULL DEFAULT '',
    version        text NOT NULL DEFAULT '',
    taro_id        text,
    epoch          integer NOT NULL DEFAULT 1,
    applied_epoch  integer NOT NULL DEFAULT 0,
    last_seen_at   timestamptz,
    enrolled_at    timestamptz NOT NULL DEFAULT now(),
    removed_at     timestamptz
);

-- an administrator working on the Mac: its customer cannot connect meanwhile
ALTER TABLE mac_resources ADD COLUMN control_by uuid REFERENCES users (id);
ALTER TABLE mac_resources ADD COLUMN control_since timestamptz;

-- an administrator's grant and session belong to no rental
ALTER TABLE connection_grants ALTER COLUMN rental_id DROP NOT NULL;
ALTER TABLE remote_sessions ALTER COLUMN rental_id DROP NOT NULL;
