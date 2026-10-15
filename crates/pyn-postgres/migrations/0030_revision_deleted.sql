-- A revision that removes its path (hash_version 3): content and file_mode stay those of the version it removed.
ALTER TABLE revisions ADD COLUMN deleted boolean NOT NULL DEFAULT false;
