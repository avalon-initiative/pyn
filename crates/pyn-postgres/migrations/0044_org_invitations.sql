-- A pending offer of organization membership; at most one per organization and user. Expired rows are
-- removed (and logged) by the application when it next looks.
CREATE TABLE org_invitations (
    org        text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_id    text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role       text        NOT NULL,
    invited_by text        NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (org, user_id)
);

CREATE INDEX org_invitations_user_idx ON org_invitations (user_id);
