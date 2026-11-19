-- Second-factor requirements (decision #1097): the server's, an organization's, and the notices that tell an account
-- an administrator reset its factors.
ALTER TABLE server_settings
    ADD COLUMN two_factor_since timestamptz,
    ADD COLUMN two_factor_grace_days integer CHECK (two_factor_grace_days BETWEEN 0 AND 90);

CREATE TABLE org_two_factor (
    org        text        PRIMARY KEY REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    since      timestamptz NOT NULL,
    grace_days integer     NOT NULL CHECK (grace_days BETWEEN 0 AND 90)
);

CREATE TABLE account_notices (
    id         text        PRIMARY KEY,
    user_id    text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    kind       text        NOT NULL,
    detail     text        NOT NULL,
    created_at timestamptz NOT NULL
);

CREATE INDEX account_notices_user_idx ON account_notices (user_id, created_at, id);
