-- Open resumable (tus) LFS uploads: what each stored so far, so a restart or a second server process continues them.
-- `busy_until` leases the session to one append at a time; the part and created lists are object ids in upload order.
CREATE TABLE tus_sessions (
    repo        text        NOT NULL,
    oid         text        NOT NULL,
    owner       text        NOT NULL,
    size        bigint      NOT NULL CHECK (size >= 0),
    received    bigint      NOT NULL DEFAULT 0 CHECK (received >= 0),
    part_ids    text[]      NOT NULL DEFAULT '{}',
    part_sizes  bigint[]    NOT NULL DEFAULT '{}',
    created     text[]      NOT NULL DEFAULT '{}',
    expires_at  timestamptz NOT NULL,
    busy_until  timestamptz,
    PRIMARY KEY (repo, oid)
);
CREATE INDEX tus_sessions_owner ON tus_sessions (owner);
CREATE INDEX tus_sessions_expires ON tus_sessions (expires_at);
