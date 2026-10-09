CREATE TABLE service_credentials (
    id           text        PRIMARY KEY,
    name         text        NOT NULL UNIQUE,
    secret_hash  text        NOT NULL,
    scopes       text[]      NOT NULL,
    created_by   text        NOT NULL,
    created_at   timestamptz NOT NULL,
    revoked_at   timestamptz,
    last_used_at timestamptz
);
