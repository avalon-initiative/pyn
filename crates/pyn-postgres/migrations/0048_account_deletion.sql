-- A deletion scheduled for an account (decision #976): the account stays disabled until purge_at, then is purged.
CREATE TABLE account_deletions (
    user_id      text        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    requested_by text        NOT NULL,
    by_owner     boolean     NOT NULL,
    reason       text,
    scheduled_at timestamptz NOT NULL,
    purge_at     timestamptz NOT NULL
);

CREATE INDEX account_deletions_due_idx ON account_deletions (purge_at);

-- A purged account's name, lowercase. reserved_until is null once the name is known to appear in history or the
-- audit log, which keeps it reserved for good.
CREATE TABLE name_reservations (
    name           text        PRIMARY KEY,
    reserved_at    timestamptz NOT NULL,
    reserved_until timestamptz
);

CREATE INDEX name_reservations_until_idx ON name_reservations (reserved_until)
    WHERE reserved_until IS NOT NULL;

CREATE INDEX users_disabled_at_idx ON users (disabled_at) WHERE disabled_at IS NOT NULL;
