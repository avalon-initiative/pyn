CREATE TABLE sessions (
    id_hash    text        PRIMARY KEY,
    user_id    text        NOT NULL REFERENCES users (id),
    csrf_token text        NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX sessions_expiry_idx ON sessions (expires_at);
