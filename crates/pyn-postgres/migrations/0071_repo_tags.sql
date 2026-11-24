-- Tags: immutable names pointing at a commit (#1424). repo is a repository id without a foreign key, like branches.repo;
-- a tag goes with its repository (delete_repo_with_events). The primary key keeps a name from being taken twice.
CREATE TABLE repo_tags (
    repo       text        NOT NULL,
    name       text        NOT NULL,
    commit_id  text        NOT NULL,
    message    text,
    created_at timestamptz NOT NULL,
    created_by text        NOT NULL,
    PRIMARY KEY (repo, name)
);
