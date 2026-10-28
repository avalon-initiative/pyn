-- A stable id per user and organization. The name is only an address: it can be recycled after a delete, the id never is.
-- Application code supplies the id on creation; the default covers rows created without one.
ALTER TABLE users ADD COLUMN account_id text;

UPDATE users
   SET account_id = CASE kind WHEN 'org' THEN 'o' ELSE 'u' END
                    || substr(replace(gen_random_uuid()::text, '-', ''), 1, 24);

ALTER TABLE users ALTER COLUMN account_id SET NOT NULL;
ALTER TABLE users ALTER COLUMN account_id
    SET DEFAULT 'u' || substr(replace(gen_random_uuid()::text, '-', ''), 1, 24);
CREATE UNIQUE INDEX users_account_id_idx ON users (account_id);
