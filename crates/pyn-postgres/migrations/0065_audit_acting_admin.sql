-- The administrator who acted as `actor` in an access session (docs/concepts/access.md); null for every other event.
-- An event with one hashes under the pyn-audit-v2 tag, one without under v1 as before (docs/concepts/integrity.md).
ALTER TABLE audit_events ADD COLUMN acting_admin text;
