-- Times an event names (docs/concepts/access.md), as parallel arrays sorted by name; empty for every older event.
-- An event with times hashes under the pyn-audit-v3 tag, one without as before (docs/concepts/integrity.md).
ALTER TABLE audit_events
    ADD COLUMN time_names  text[]        NOT NULL DEFAULT '{}',
    ADD COLUMN time_values timestamptz[] NOT NULL DEFAULT '{}';
