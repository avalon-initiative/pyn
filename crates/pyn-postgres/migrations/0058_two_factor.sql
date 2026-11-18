-- Second factors (decision #1097): authenticator-app factors, single-use recovery codes, the growing delay on failed
-- attempts, and the fresh checks that open sensitive actions on a credential.
CREATE TABLE account_factors (
    id           text        PRIMARY KEY,
    user_id      text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    kind         text        NOT NULL,
    label        text        NOT NULL,
    secret       bytea       NOT NULL,
    encrypted    boolean     NOT NULL,
    key_id       text,
    created_at   timestamptz NOT NULL,
    confirmed_at timestamptz,
    last_step    bigint
);

CREATE INDEX account_factors_user_idx ON account_factors (user_id, created_at, id);
CREATE UNIQUE INDEX account_factors_one_pending ON account_factors (user_id) WHERE confirmed_at IS NULL;

CREATE TABLE recovery_codes (
    user_id    text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    code_hash  text        NOT NULL,
    created_at timestamptz NOT NULL,
    used_at    timestamptz,
    PRIMARY KEY (user_id, code_hash)
);

CREATE TABLE factor_attempts (
    user_id      text        PRIMARY KEY REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    attempts     integer     NOT NULL,
    locked_until timestamptz,
    last_at      timestamptz
);

CREATE TABLE fresh_checks (
    credential  text        PRIMARY KEY,
    user_id     text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    fresh_until timestamptz NOT NULL
);

CREATE INDEX fresh_checks_user_idx ON fresh_checks (user_id);
