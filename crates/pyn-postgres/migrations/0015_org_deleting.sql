-- Set on an organization while a delete is in flight; repository creation under it is refused.
ALTER TABLE users ADD COLUMN deleting_since timestamptz;
