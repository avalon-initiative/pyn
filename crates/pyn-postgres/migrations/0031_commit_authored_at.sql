-- The author time, apart from the committer time in created_at (decision #300). Existing commits had one time for both.
ALTER TABLE commits ADD COLUMN authored_at timestamptz;
UPDATE commits SET authored_at = created_at;
ALTER TABLE commits ALTER COLUMN authored_at SET NOT NULL;
