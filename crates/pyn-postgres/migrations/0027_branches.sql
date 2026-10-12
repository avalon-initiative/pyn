-- Branches (decision #233): a name pointing at a commit, one default branch per repository. The default branch of
-- existing repositories is created by the server at startup (migrate_branches), once history has been migrated.
CREATE TABLE branches (
    repo       text        NOT NULL,
    name       text        NOT NULL,
    head       text,
    is_default boolean     NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    created_by text        NOT NULL,
    PRIMARY KEY (repo, name)
);
CREATE UNIQUE INDEX branches_one_default ON branches (repo) WHERE is_default;

-- The revision of a path at a commit (decision #245 item 9), cached per commit as the paths whose revision differs
-- from the commit's first parent. A snapshot takes, per path, the nearest row along the first-parent chain.
CREATE TABLE commit_pointers (
    repo      text   NOT NULL,
    commit_id text   NOT NULL,
    path      text   NOT NULL,
    revision  bigint NOT NULL,
    PRIMARY KEY (repo, commit_id, path)
);

-- Every parent of a commit, in order; `parent` stays the first, for the first-parent chain.
ALTER TABLE commits ADD COLUMN parents text[] NOT NULL DEFAULT '{}';
UPDATE commits SET parents = ARRAY[parent] WHERE parent IS NOT NULL;
