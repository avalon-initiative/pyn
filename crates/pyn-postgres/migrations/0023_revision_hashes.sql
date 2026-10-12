-- Canonical hashes (docs/concepts/integrity.md). Rows that predate them are hashed once by the server at startup.
ALTER TABLE revisions ADD COLUMN hash text, ADD COLUMN previous_hash text;
ALTER TABLE audit_events ADD COLUMN hash text, ADD COLUMN previous_hash text;
