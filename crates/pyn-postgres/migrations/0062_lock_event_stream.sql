ALTER TABLE webhook_events ADD COLUMN repo_id text;

CREATE INDEX webhook_events_repo_idx ON webhook_events (repo_id, id) WHERE repo_id IS NOT NULL;
CREATE INDEX webhook_events_lock_idx ON webhook_events (id) WHERE event_type LIKE 'lock.%';
CREATE INDEX locks_expiry_idx ON locks (expires_at);
