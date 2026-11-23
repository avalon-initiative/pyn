-- An administrator's invitation to the server itself, with an optional role in one repository or organization;
-- only a hash of the secret is kept. created_by and used_by are names without foreign keys, like org_invitations.invited_by,
-- so deleting an account never removes the record of what it did.
CREATE TABLE server_invitations (
    id          text        PRIMARY KEY,
    secret_hash text        NOT NULL,
    email       text,
    repo        text,
    repo_role   text,
    org         text,
    org_role    text,
    created_by  text        NOT NULL,
    created_at  timestamptz NOT NULL,
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    used_by     text,
    revoked_at  timestamptz,
    CHECK ((repo IS NULL) = (repo_role IS NULL)),
    CHECK ((org IS NULL) = (org_role IS NULL)),
    CHECK (repo IS NULL OR org IS NULL)
);

CREATE INDEX server_invitations_created_idx ON server_invitations (created_at DESC);

-- The password was set by an administrator and must be replaced at the person's next sign-in.
ALTER TABLE users ADD COLUMN password_change_required boolean NOT NULL DEFAULT false;
