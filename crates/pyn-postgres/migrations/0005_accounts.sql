ALTER TABLE users ADD COLUMN password_hash text;

CREATE TABLE invites (
    id          text        PRIMARY KEY,
    secret_hash text        NOT NULL,
    repo        text        NOT NULL,
    role        text        NOT NULL,
    created_by  text        NOT NULL REFERENCES users (id),
    created_at  timestamptz NOT NULL,
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    used_by     text,
    revoked_at  timestamptz
);

CREATE INDEX invites_repo_idx ON invites (repo, created_at DESC);
