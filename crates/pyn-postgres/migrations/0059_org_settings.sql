-- An organization's repository defaults, one row per organization that set any; a NULL column is unset.
CREATE TABLE org_settings (
    org                 text PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    exclusive_read_only boolean
);
