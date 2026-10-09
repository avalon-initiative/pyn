-- Organizations share the namespace of users (one `users` row per name); `kind` tells them apart.
ALTER TABLE users ADD COLUMN kind text NOT NULL DEFAULT 'user';

CREATE TABLE org_members (
    org     text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_id text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role    text NOT NULL,
    PRIMARY KEY (org, user_id)
);

CREATE INDEX org_members_user_idx ON org_members (user_id);
