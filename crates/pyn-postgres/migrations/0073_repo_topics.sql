-- A repository's topics, lowercase labels in the order given (docs/concepts/repositories.md).
ALTER TABLE repositories ADD COLUMN topics text[] NOT NULL DEFAULT '{}';
