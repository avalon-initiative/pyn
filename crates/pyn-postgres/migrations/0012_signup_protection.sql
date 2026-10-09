ALTER TABLE users
    ADD COLUMN email             text,
    ADD COLUMN email_verified_at timestamptz,
    ADD COLUMN signup            text    NOT NULL DEFAULT 'active',
    ADD COLUMN disabled_at       timestamptz,
    ADD COLUMN disabled_reason   text,
    ADD COLUMN is_admin          boolean NOT NULL DEFAULT false;

CREATE UNIQUE INDEX users_verified_email_idx ON users (email) WHERE email_verified_at IS NOT NULL;
CREATE INDEX users_email_idx ON users (email) WHERE email IS NOT NULL;

CREATE TABLE email_verifications (
    token_hash text        PRIMARY KEY,
    user_id    text        NOT NULL UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    email      text        NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE TABLE rate_limits (
    key       text        PRIMARY KEY,
    count     integer     NOT NULL,
    resets_at timestamptz NOT NULL
);
