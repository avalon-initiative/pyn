-- Explicit revision hash encoding (decision #245): 1 = v1, 2 = v2. Rows with a stored structured author are v2.
ALTER TABLE revisions ADD COLUMN hash_version integer NOT NULL DEFAULT 1 CHECK (hash_version > 0);
UPDATE revisions SET hash_version = 2 WHERE author_name IS NOT NULL;
ALTER TABLE revisions ALTER COLUMN hash_version DROP DEFAULT;
