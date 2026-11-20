-- Administrator access sessions (decision #1024): an administrator acts as one account for at most an hour.
-- Account ids, not names, so a rename does not lose a session; no foreign key, so the record outlives an account.
CREATE TABLE access_sessions (
    id          text        PRIMARY KEY,
    secret_hash text        NOT NULL,
    admin_id    text        NOT NULL,
    admin       text        NOT NULL,
    account_id  text        NOT NULL,
    username    text        NOT NULL,
    reason      text        NOT NULL,
    notes       text,
    started_at  timestamptz NOT NULL,
    expires_at  timestamptz NOT NULL,
    ended_at    timestamptz,
    ended_how   text        CHECK (ended_how IN ('ended', 'expired'))
);

CREATE INDEX access_sessions_account_idx ON access_sessions (account_id, started_at DESC);
CREATE INDEX access_sessions_open_idx ON access_sessions (expires_at) WHERE ended_at IS NULL;

-- One row per method, route and repository a session called, with a count; bounded by the service.
CREATE TABLE access_session_reach (
    seq        bigserial   NOT NULL,
    session_id text        NOT NULL REFERENCES access_sessions (id) ON DELETE CASCADE,
    method     text        NOT NULL,
    route      text        NOT NULL,
    target     text        NOT NULL,
    refused    boolean     NOT NULL,
    count      bigint      NOT NULL,
    first_at   timestamptz NOT NULL,
    last_at    timestamptz NOT NULL,
    PRIMARY KEY (session_id, method, route, target, refused)
);
