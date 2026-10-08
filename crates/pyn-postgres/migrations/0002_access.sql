CREATE TABLE users (
    id         text        PRIMARY KEY,
    created_at timestamptz NOT NULL
);

CREATE TABLE memberships (
    repo    text NOT NULL,
    user_id text NOT NULL REFERENCES users (id),
    role    text NOT NULL,
    PRIMARY KEY (repo, user_id)
);

CREATE TABLE role_permissions (
    repo        text   NOT NULL,
    role        text   NOT NULL,
    permissions text[] NOT NULL,
    PRIMARY KEY (repo, role)
);

CREATE TABLE tokens (
    id           text        PRIMARY KEY,
    user_id      text        NOT NULL REFERENCES users (id),
    name         text        NOT NULL,
    secret_hash  text        NOT NULL,
    permissions  text[]      NOT NULL,
    repos        text[]      NOT NULL,
    created_at   timestamptz NOT NULL,
    expires_at   timestamptz,
    revoked_at   timestamptz,
    last_used_at timestamptz
);

CREATE INDEX tokens_user_idx ON tokens (user_id, created_at DESC);
