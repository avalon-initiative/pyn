-- Passkeys and security keys (#1105, decision #1127): WebAuthn credentials as second factors and sign-ins, and the
-- single-use challenges of their ceremonies.
CREATE TABLE account_passkeys (
    id            text        PRIMARY KEY,
    user_id       text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    name          text        NOT NULL,
    credential_id bytea       NOT NULL UNIQUE,
    public_key    bytea       NOT NULL,
    user_handle   bytea       NOT NULL,
    sign_count    bigint      NOT NULL,
    transports    text[]      NOT NULL,
    backup_eligible boolean   NOT NULL,
    backup_state  boolean     NOT NULL,
    created_at    timestamptz NOT NULL,
    last_used_at  timestamptz
);

CREATE INDEX account_passkeys_user_idx ON account_passkeys (user_id, created_at, id);

CREATE TABLE webauthn_challenges (
    id         text        PRIMARY KEY,
    user_id    text        REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    purpose    text        NOT NULL,
    challenge  bytea       NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX webauthn_challenges_user_idx ON webauthn_challenges (user_id, purpose);
