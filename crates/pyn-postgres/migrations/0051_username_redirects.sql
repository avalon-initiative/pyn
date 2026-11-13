-- A rename updates users.id in place, so every key that names a user follows it.
DO $$
DECLARE c record;
BEGIN
    FOR c IN
        SELECT conrelid::regclass AS tbl, conname, pg_get_constraintdef(oid) AS def
          FROM pg_constraint
         WHERE contype = 'f' AND confupdtype = 'a'
           AND confrelid IN ('users'::regclass, 'org_members'::regclass)
    LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', c.tbl, c.conname);
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s ON UPDATE CASCADE', c.tbl, c.conname, c.def);
    END LOOP;
END $$;

-- The address `owner/name` as it was before its owner was renamed; a live repository there wins over it.
CREATE TABLE repo_redirects (
    owner      text        NOT NULL,
    name_lower text        NOT NULL,
    repo_id    text        NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (owner, name_lower)
);

CREATE INDEX repo_redirects_repo_idx ON repo_redirects (repo_id);

-- The names an account held and when, so history resolves an author to the account's current name.
CREATE TABLE account_renames (
    id         bigserial   PRIMARY KEY,
    account_id text        NOT NULL,
    old_name   text        NOT NULL,
    new_name   text        NOT NULL,
    held_from  timestamptz NOT NULL,
    at         timestamptz NOT NULL
);

CREATE INDEX account_renames_old_idx ON account_renames (old_name);
CREATE INDEX account_renames_account_idx ON account_renames (account_id, at DESC);
