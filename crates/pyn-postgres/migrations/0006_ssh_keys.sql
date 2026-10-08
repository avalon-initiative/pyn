CREATE TABLE ssh_keys (
    id           text        PRIMARY KEY,
    user_id      text        NOT NULL REFERENCES users (id),
    title        text        NOT NULL,
    algorithm    text        NOT NULL,
    public_key   text        NOT NULL,
    fingerprint  text        NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL,
    last_used_at timestamptz
);

CREATE INDEX ssh_keys_user_idx ON ssh_keys (user_id, created_at DESC);
