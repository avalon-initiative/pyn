ALTER TABLE revisions ADD COLUMN mode TEXT CHECK (mode IN ('shared', 'exclusive'));
