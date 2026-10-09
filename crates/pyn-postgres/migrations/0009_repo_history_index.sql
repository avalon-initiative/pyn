CREATE INDEX revisions_repo_history ON revisions (repo, created_at DESC, (path COLLATE "C") DESC, id DESC);
