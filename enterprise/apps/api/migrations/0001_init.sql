-- Taro Enterprise: the initial schema. Times are UTC (timestamptz), money is integer minor
-- units with an ISO currency code, secrets are never stored here (only references to them).

-- ------------------------------------------------------------------ identity

CREATE TABLE users (
    id              uuid PRIMARY KEY,
    email           text NOT NULL,
    display_name    text NOT NULL DEFAULT '',
    email_verified  boolean NOT NULL DEFAULT false,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    disabled_at     timestamptz,
    CONSTRAINT users_email_lower CHECK (email = lower(email))
);
CREATE UNIQUE INDEX users_email ON users (email);

-- one row per way a user signs in: Google (subject = the `sub` claim) or email (subject = the address)
CREATE TABLE auth_identities (
    id          uuid PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider    text NOT NULL CHECK (provider IN ('google', 'email')),
    subject     text NOT NULL,
    email       text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    UNIQUE (provider, subject)
);
CREATE INDEX auth_identities_user ON auth_identities (user_id);

-- passwordless email sign-in: a link (token) and a 6-digit code for the same request, one use
CREATE TABLE email_login_tokens (
    id          uuid PRIMARY KEY,
    email       text NOT NULL,
    audience    text NOT NULL CHECK (audience IN ('customer', 'ops')),
    token_hash  bytea NOT NULL UNIQUE,
    code_hash   bytea NOT NULL,
    attempts    integer NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    ip          text
);
CREATE INDEX email_login_tokens_email ON email_login_tokens (email, created_at DESC);

-- OpenID Connect: what a sign-in started with (state, nonce, PKCE verifier), one use
CREATE TABLE oidc_states (
    state          text PRIMARY KEY,
    nonce          text NOT NULL,
    pkce_verifier  text NOT NULL,
    audience       text NOT NULL CHECK (audience IN ('customer', 'ops')),
    return_to      text NOT NULL DEFAULT '/',
    created_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL
);

-- sessions: the cookie carries a random token, only its SHA-256 is kept
CREATE TABLE sessions (
    id            uuid PRIMARY KEY,
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    audience      text NOT NULL CHECK (audience IN ('customer', 'ops')),
    token_hash    bytea NOT NULL UNIQUE,
    csrf_token    text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_seen_at  timestamptz NOT NULL DEFAULT now(),
    authenticated_at timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    revoked_at    timestamptz,
    ip            text,
    user_agent    text
);
CREATE INDEX sessions_user ON sessions (user_id) WHERE revoked_at IS NULL;

-- ------------------------------------------------------------------ organizations

CREATE TABLE organizations (
    id          uuid PRIMARY KEY,
    name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    slug        text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,62}$'),
    created_by  uuid REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    suspended_at timestamptz
);

CREATE TABLE memberships (
    organization_id  uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    user_id          uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role             text NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'billing_manager')),
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, user_id)
);
CREATE INDEX memberships_user ON memberships (user_id);

CREATE TABLE invitations (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    email            text NOT NULL CHECK (email = lower(email)),
    role             text NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'billing_manager')),
    token_hash       bytea NOT NULL UNIQUE,
    invited_by       uuid NOT NULL REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    accepted_at      timestamptz,
    accepted_by      uuid REFERENCES users (id),
    revoked_at       timestamptz
);
-- one open invitation per address and organization
CREATE UNIQUE INDEX invitations_open ON invitations (organization_id, email) WHERE accepted_at IS NULL AND revoked_at IS NULL;

-- Taro Operations: platform roles, never inherited from customer roles
CREATE TABLE operator_grants (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        text NOT NULL CHECK (role IN ('platform_admin', 'operations_operator', 'support_operator', 'billing_operator', 'security_auditor')),
    granted_by  uuid REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, role)
);

-- the role and permission catalog as the code enforces it (src/authz.rs is authoritative; these
-- rows are kept in step by the server at start, for reporting and audits)
CREATE TABLE roles (
    key    text PRIMARY KEY,
    scope  text NOT NULL CHECK (scope IN ('organization', 'platform')),
    description text NOT NULL
);
CREATE TABLE permissions (
    key          text PRIMARY KEY,
    description  text NOT NULL
);
CREATE TABLE role_permissions (
    role_key        text NOT NULL REFERENCES roles (key) ON DELETE CASCADE,
    permission_key  text NOT NULL REFERENCES permissions (key) ON DELETE CASCADE,
    PRIMARY KEY (role_key, permission_key)
);

-- ------------------------------------------------------------------ catalog and inventory

CREATE TABLE mac_plans (
    id              uuid PRIMARY KEY,
    slug            text NOT NULL UNIQUE,
    name            text NOT NULL,
    description     text NOT NULL DEFAULT '',
    chip            text NOT NULL,
    cpu_cores       integer NOT NULL CHECK (cpu_cores > 0),
    memory_gb       integer NOT NULL CHECK (memory_gb > 0),
    storage_gb      integer NOT NULL CHECK (storage_gb > 0),
    region          text NOT NULL,
    -- one_time: a fixed duration paid once; monthly: a Stripe subscription
    billing         text NOT NULL CHECK (billing IN ('one_time', 'monthly')),
    duration_hours  integer CHECK (duration_hours IS NULL OR duration_hours > 0),
    price_minor     bigint NOT NULL CHECK (price_minor >= 0),
    currency        text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    active          boolean NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT plan_duration CHECK ((billing = 'one_time') = (duration_hours IS NOT NULL))
);

CREATE TABLE provider_integrations (
    id              uuid PRIMARY KEY,
    kind            text NOT NULL,
    name            text NOT NULL UNIQUE,
    -- non-secret settings only; credentials live in the secret store, named by secret_ref
    config          jsonb NOT NULL DEFAULT '{}',
    secret_ref      text,
    enabled         boolean NOT NULL DEFAULT true,
    health          text NOT NULL DEFAULT 'unknown' CHECK (health IN ('unknown', 'healthy', 'degraded', 'down')),
    last_health_at  timestamptz,
    last_sync_at    timestamptz,
    last_error      text,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE mac_resources (
    id              uuid PRIMARY KEY,
    provider_id     uuid NOT NULL REFERENCES provider_integrations (id),
    external_id     text NOT NULL,
    display_name    text NOT NULL,
    model           text NOT NULL,
    chip            text NOT NULL,
    cpu_cores       integer NOT NULL,
    memory_gb       integer NOT NULL,
    storage_gb      integer NOT NULL,
    region          text NOT NULL,
    state           text NOT NULL CHECK (state IN ('discovered', 'available', 'reserved', 'assigned', 'maintenance', 'unavailable', 'releasing', 'released', 'error')),
    -- what the provider last reported (the database state above is Taro's view of it)
    provider_state  text NOT NULL DEFAULT 'unknown',
    -- held out of new rentals by an operator (an incident, maintenance)
    hold_reason     text,
    taro_id         text,
    connect_secret_ref text,
    last_synced_at  timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider_id, external_id)
);
CREATE INDEX mac_resources_state ON mac_resources (state, region);

-- ------------------------------------------------------------------ rentals

CREATE TABLE rentals (
    id                  uuid PRIMARY KEY,
    organization_id     uuid NOT NULL REFERENCES organizations (id),
    plan_id             uuid NOT NULL REFERENCES mac_plans (id),
    resource_id         uuid REFERENCES mac_resources (id),
    created_by          uuid NOT NULL REFERENCES users (id),
    status              text NOT NULL CHECK (status IN ('draft', 'pending_payment', 'paid', 'provisioning', 'ready', 'suspended', 'cancel_pending', 'expired', 'failed', 'cancelled')),
    billing             text NOT NULL CHECK (billing IN ('one_time', 'monthly')),
    amount_minor        bigint NOT NULL CHECK (amount_minor >= 0),
    currency            text NOT NULL,
    duration_hours      integer,
    starts_at           timestamptz,
    ends_at             timestamptz,
    idempotency_key     text NOT NULL,
    stripe_checkout_session_id text UNIQUE,
    stripe_subscription_id     text UNIQUE,
    stripe_payment_intent_id   text UNIQUE,
    billing_state       text NOT NULL DEFAULT 'none' CHECK (billing_state IN ('none', 'checkout_open', 'paid', 'past_due', 'refunded', 'canceled', 'expired')),
    status_reason       text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, idempotency_key)
);
CREATE INDEX rentals_org ON rentals (organization_id, created_at DESC);
CREATE INDEX rentals_status ON rentals (status);
-- one resource is assigned to one live rental at a time
CREATE UNIQUE INDEX rentals_live_resource ON rentals (resource_id) WHERE resource_id IS NOT NULL AND status IN ('provisioning', 'ready', 'suspended', 'cancel_pending');

CREATE TABLE rental_events (
    id          bigserial PRIMARY KEY,
    rental_id   uuid NOT NULL REFERENCES rentals (id) ON DELETE CASCADE,
    from_status text,
    to_status   text NOT NULL,
    reason      text NOT NULL,
    actor       text NOT NULL,
    at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX rental_events_rental ON rental_events (rental_id, id);

CREATE TABLE rental_assignments (
    id           uuid PRIMARY KEY,
    rental_id    uuid NOT NULL REFERENCES rentals (id) ON DELETE CASCADE,
    user_id      uuid NOT NULL REFERENCES users (id),
    assigned_by  uuid NOT NULL REFERENCES users (id),
    assigned_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at   timestamptz,
    revoked_by   uuid REFERENCES users (id)
);
CREATE UNIQUE INDEX rental_assignments_live ON rental_assignments (rental_id, user_id) WHERE revoked_at IS NULL;

CREATE TABLE provisioning_jobs (
    id              uuid PRIMARY KEY,
    rental_id       uuid NOT NULL REFERENCES rentals (id),
    provider_id     uuid REFERENCES provider_integrations (id),
    operation       text NOT NULL CHECK (operation IN ('provision', 'release')),
    state           text NOT NULL CHECK (state IN ('queued', 'running', 'waiting_provider', 'succeeded', 'retryable_failed', 'permanent_failed', 'cancelled')),
    attempts        integer NOT NULL DEFAULT 0,
    max_attempts    integer NOT NULL DEFAULT 6,
    run_after       timestamptz NOT NULL DEFAULT now(),
    locked_until    timestamptz,
    external_ref    text,
    last_error      text,
    correlation_id  uuid NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    started_at      timestamptz,
    completed_at    timestamptz,
    -- one job per rental and operation: retries reuse it, never a second resource
    UNIQUE (rental_id, operation)
);
CREATE INDEX provisioning_jobs_due ON provisioning_jobs (run_after) WHERE state IN ('queued', 'retryable_failed', 'waiting_provider');

-- ------------------------------------------------------------------ connectivity

CREATE TABLE connection_grants (
    id           uuid PRIMARY KEY,
    rental_id    uuid NOT NULL REFERENCES rentals (id),
    resource_id  uuid NOT NULL REFERENCES mac_resources (id),
    user_id      uuid NOT NULL REFERENCES users (id),
    secret_hash  bytea NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    redeemed_at  timestamptz,
    revoked_at   timestamptz
);

CREATE TABLE remote_sessions (
    id           uuid PRIMARY KEY,
    grant_id     uuid NOT NULL REFERENCES connection_grants (id),
    rental_id    uuid NOT NULL REFERENCES rentals (id),
    user_id      uuid NOT NULL REFERENCES users (id),
    started_at   timestamptz NOT NULL DEFAULT now(),
    client       text
);

CREATE TABLE relay_nodes (
    id             uuid PRIMARY KEY,
    name           text NOT NULL UNIQUE,
    address        text NOT NULL,
    region         text NOT NULL,
    reachable      boolean,
    latency_ms     integer,
    last_check_at  timestamptz,
    last_error     text,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- ------------------------------------------------------------------ billing

CREATE TABLE billing_accounts (
    organization_id     uuid PRIMARY KEY REFERENCES organizations (id),
    stripe_customer_id  text NOT NULL UNIQUE,
    livemode            boolean NOT NULL,
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE subscriptions (
    id                    text PRIMARY KEY,
    organization_id       uuid NOT NULL REFERENCES organizations (id),
    rental_id             uuid REFERENCES rentals (id),
    status                text NOT NULL,
    current_period_end    timestamptz,
    cancel_at_period_end  boolean NOT NULL DEFAULT false,
    -- the Stripe event time this row reflects: older events never overwrite it
    stripe_updated        bigint NOT NULL DEFAULT 0,
    updated_at            timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE invoices (
    id                  text PRIMARY KEY,
    organization_id     uuid NOT NULL REFERENCES organizations (id),
    rental_id           uuid REFERENCES rentals (id),
    number              text,
    status              text NOT NULL,
    amount_due          bigint NOT NULL,
    amount_paid         bigint NOT NULL,
    currency            text NOT NULL,
    hosted_invoice_url  text,
    period_end          timestamptz,
    stripe_updated      bigint NOT NULL DEFAULT 0,
    created_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX invoices_org ON invoices (organization_id, created_at DESC);

-- the durable webhook inbox: verified events are stored first, processed after (idempotent)
CREATE TABLE stripe_webhook_events (
    id            text PRIMARY KEY,
    type          text NOT NULL,
    livemode      boolean NOT NULL,
    created       bigint NOT NULL,
    payload       jsonb NOT NULL,
    state         text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'processed', 'ignored', 'failed', 'dead')),
    attempts      integer NOT NULL DEFAULT 0,
    last_error    text,
    received_at   timestamptz NOT NULL DEFAULT now(),
    processed_at  timestamptz,
    next_attempt_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX stripe_webhook_events_pending ON stripe_webhook_events (next_attempt_at) WHERE state IN ('pending', 'failed');

-- ------------------------------------------------------------------ domains, branding, flags

CREATE TABLE domain_assignments (
    id                  uuid PRIMARY KEY,
    organization_id     uuid REFERENCES organizations (id),
    hostname            text NOT NULL UNIQUE CHECK (hostname = lower(hostname)),
    kind                text NOT NULL CHECK (kind IN ('managed', 'custom')),
    verification_token  text NOT NULL,
    state               text NOT NULL CHECK (state IN ('pending_verification', 'verified', 'provisioning_tls', 'active', 'error', 'disabled')),
    verified_at         timestamptz,
    tls_checked_at      timestamptz,
    tls_expires_at      timestamptz,
    last_check_at       timestamptz,
    last_error          text,
    created_by          uuid REFERENCES users (id),
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE assets (
    id               uuid PRIMARY KEY,
    organization_id  uuid REFERENCES organizations (id),
    kind             text NOT NULL CHECK (kind IN ('logo', 'favicon')),
    content_type     text NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg', 'image/webp')),
    bytes            bytea NOT NULL,
    sha256           text NOT NULL,
    width            integer NOT NULL,
    height           integer NOT NULL,
    created_by       uuid REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now()
);

-- organization_id NULL: the platform's own (default) branding
CREATE TABLE branding_configs (
    id               uuid PRIMARY KEY,
    organization_id  uuid REFERENCES organizations (id),
    version          integer NOT NULL,
    config           jsonb NOT NULL,
    published_at     timestamptz,
    created_by       uuid REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX branding_configs_version ON branding_configs (coalesce(organization_id, '00000000-0000-0000-0000-000000000000'::uuid), version);

-- feature flags: rollout and kill switches (operators)
CREATE TABLE feature_flags (
    key          text PRIMARY KEY CHECK (key ~ '^[a-z0-9_.]{2,64}$'),
    description  text NOT NULL,
    enabled      boolean NOT NULL DEFAULT false,
    updated_by   uuid REFERENCES users (id),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- entitlements: what an organization may use (from its plan, or granted by an operator)
CREATE TABLE entitlements (
    organization_id  uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    feature          text NOT NULL,
    source           text NOT NULL CHECK (source IN ('default', 'plan', 'manual')),
    granted_by       uuid REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, feature)
);

-- ------------------------------------------------------------------ operations

CREATE TABLE notifications (
    id               uuid PRIMARY KEY,
    user_id          uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    organization_id  uuid REFERENCES organizations (id),
    kind             text NOT NULL,
    title            text NOT NULL,
    body             text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    read_at          timestamptz
);
CREATE INDEX notifications_user ON notifications (user_id, created_at DESC);

CREATE TABLE incidents (
    id            uuid PRIMARY KEY,
    severity      text NOT NULL CHECK (severity IN ('sev1', 'sev2', 'sev3', 'sev4')),
    component     text NOT NULL,
    title         text NOT NULL,
    impact        text NOT NULL DEFAULT '',
    status        text NOT NULL CHECK (status IN ('open', 'mitigated', 'resolved')),
    dedupe_key    text,
    first_seen    timestamptz NOT NULL DEFAULT now(),
    last_seen     timestamptz NOT NULL DEFAULT now(),
    occurrences   integer NOT NULL DEFAULT 1,
    owner_id      uuid REFERENCES users (id),
    mitigation    text NOT NULL DEFAULT '',
    resolution    text NOT NULL DEFAULT '',
    linked        jsonb NOT NULL DEFAULT '{}',
    resolved_at   timestamptz
);
CREATE UNIQUE INDEX incidents_open_dedupe ON incidents (dedupe_key) WHERE status <> 'resolved' AND dedupe_key IS NOT NULL;

-- the audit trail: append-only (updates and deletes are refused by the trigger below)
CREATE TABLE audit_events (
    id               bigserial PRIMARY KEY,
    at               timestamptz NOT NULL DEFAULT now(),
    actor_kind       text NOT NULL CHECK (actor_kind IN ('user', 'operator', 'system', 'stripe', 'provider')),
    actor_user_id    uuid,
    organization_id  uuid,
    action           text NOT NULL,
    resource_type    text NOT NULL,
    resource_id      text,
    result           text NOT NULL CHECK (result IN ('success', 'denied', 'failure')),
    ip               text,
    request_id       text,
    details          jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_events_org ON audit_events (organization_id, id DESC);
CREATE INDEX audit_events_actor ON audit_events (actor_user_id, id DESC);

CREATE FUNCTION audit_events_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER audit_events_no_update BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_append_only();

-- development only: mail that would have been sent (TARO_ENV=development without SMTP)
CREATE TABLE dev_outbox (
    id          uuid PRIMARY KEY,
    to_email    text NOT NULL,
    subject     text NOT NULL,
    body        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
