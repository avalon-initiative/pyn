-- A repository's About block: a one-line description and a website link, both optional (docs/concepts/repositories.md).
ALTER TABLE repositories ADD COLUMN description text, ADD COLUMN website text;
