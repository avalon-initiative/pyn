-- Limits are opt-in: no row means the owner follows the server default, and a NULL column is unlimited.
CREATE TABLE owner_limits (
    owner             text   PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    max_repositories  bigint CHECK (max_repositories IS NULL OR max_repositories >= 0),
    max_members       bigint CHECK (max_members IS NULL OR max_members >= 0),
    max_storage_bytes bigint CHECK (max_storage_bytes IS NULL OR max_storage_bytes >= 0)
);

-- Bytes of the revision's content, recorded at check-in; revisions made before this migration count as 0.
ALTER TABLE revisions ADD COLUMN size bigint NOT NULL DEFAULT 0 CHECK (size >= 0);
