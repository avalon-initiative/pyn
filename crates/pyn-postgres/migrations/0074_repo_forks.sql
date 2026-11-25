-- Forks: the source a repository was copied from, cleared when the source is deleted, and whether a private repository may
-- be forked by those who can read it (docs/concepts/repositories.md).
ALTER TABLE repositories
    ADD COLUMN fork_of text REFERENCES repositories (id) ON DELETE SET NULL,
    ADD COLUMN allow_private_forks boolean NOT NULL DEFAULT false;
CREATE INDEX repositories_fork_of ON repositories (fork_of) WHERE fork_of IS NOT NULL;
