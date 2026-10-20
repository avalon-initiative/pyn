CREATE TABLE webhook_subscriptions (
    id          bigserial   PRIMARY KEY,
    name        text        NOT NULL,
    url         text        NOT NULL,
    event_types text[]      NOT NULL DEFAULT '{}',
    enabled     boolean     NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL,
    updated_at  timestamptz NOT NULL,
    created_by  text        NOT NULL
);

CREATE TABLE webhook_secrets (
    id              bigserial   PRIMARY KEY,
    subscription_id bigint      NOT NULL REFERENCES webhook_subscriptions (id) ON DELETE CASCADE,
    secret          bytea       NOT NULL,
    encrypted       boolean     NOT NULL DEFAULT false,
    key_id          text,
    created_at      timestamptz NOT NULL,
    retired_at      timestamptz
);

CREATE INDEX webhook_secrets_active_idx ON webhook_secrets (subscription_id) WHERE retired_at IS NULL;

CREATE TABLE webhook_events (
    id         bigserial   PRIMARY KEY,
    event_type text        NOT NULL,
    payload    jsonb       NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX webhook_events_created_idx ON webhook_events (created_at);

CREATE TABLE webhook_deliveries (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id  bigint      NOT NULL REFERENCES webhook_subscriptions (id) ON DELETE CASCADE,
    event_id         bigint      NOT NULL REFERENCES webhook_events (id) ON DELETE CASCADE,
    attempt_count    integer     NOT NULL DEFAULT 0,
    next_attempt_at  timestamptz NOT NULL,
    claimed_until    timestamptz,
    status           text        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'failed')),
    last_status_code integer,
    last_error       text,
    created_at       timestamptz NOT NULL,
    delivered_at     timestamptz,
    failed_at        timestamptz,
    UNIQUE (subscription_id, event_id)
);

CREATE INDEX webhook_deliveries_due_idx ON webhook_deliveries (next_attempt_at) WHERE status = 'pending';
CREATE INDEX webhook_deliveries_subscription_idx ON webhook_deliveries (subscription_id, event_id DESC);
