ALTER TABLE repositories ADD COLUMN max_locks integer CHECK (max_locks IS NULL OR max_locks >= 1);
