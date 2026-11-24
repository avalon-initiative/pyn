-- A user's mark on a repository (decision #1282). repo is a repository id without a foreign key, like memberships.repo;
-- the mark goes with the account (also on a rename) and with the repository (delete_repo_access).
CREATE TABLE repo_stars (
    repo       text        NOT NULL,
    user_id    text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (repo, user_id)
);

CREATE INDEX repo_stars_user_idx ON repo_stars (user_id, created_at DESC, repo);

ALTER TABLE account_profiles ADD COLUMN show_starred boolean NOT NULL DEFAULT true;
