-- Optional profile text and the avatar record of an account; the avatar bytes live in the object store.
CREATE TABLE account_profiles (
    user_id        text        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    display_name   text,
    bio            text,
    location       text,
    company        text,
    links          text[]      NOT NULL DEFAULT '{}',
    avatar_content text,
    avatar_type    text,
    avatar_size    bigint,
    updated_at     timestamptz NOT NULL
);
