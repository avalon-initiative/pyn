-- Repository names keep their case but are unique per owner ignoring it.
ALTER TABLE repositories DROP CONSTRAINT repositories_owner_name_key;
CREATE UNIQUE INDEX repositories_owner_lower_name_key ON repositories (owner, lower(name));
