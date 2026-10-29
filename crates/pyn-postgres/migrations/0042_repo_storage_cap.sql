-- A repository's own storage cap in bytes; null means none.
ALTER TABLE repositories ADD COLUMN max_storage_bytes bigint CHECK (max_storage_bytes >= 0);
