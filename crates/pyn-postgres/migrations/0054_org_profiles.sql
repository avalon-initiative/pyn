-- An organization's description and avatar record; the avatar bytes live in the object store.
CREATE TABLE org_profiles (
    org_id         text        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    description    text,
    avatar_content text,
    avatar_type    text,
    avatar_size    bigint,
    updated_at     timestamptz NOT NULL
);
