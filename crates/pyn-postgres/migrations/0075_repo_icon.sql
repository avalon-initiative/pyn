-- A repository's icon record, stored like an account avatar; the bytes live in the object store (docs/concepts/repositories.md).
ALTER TABLE repositories ADD COLUMN icon_content text, ADD COLUMN icon_type text, ADD COLUMN icon_size bigint;
