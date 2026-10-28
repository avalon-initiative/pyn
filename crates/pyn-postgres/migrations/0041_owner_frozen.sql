-- Set while an owner (user or organization) is frozen: every write under it is refused, reads continue.
ALTER TABLE users ADD COLUMN frozen_at timestamptz;
ALTER TABLE users ADD COLUMN frozen_reason text;
