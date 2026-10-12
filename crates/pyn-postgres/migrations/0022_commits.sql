-- A commit is one atomic write (a checkin or a changeset), numbered per repository; revisions record theirs.
ALTER TABLE revisions ADD COLUMN commit_no bigint;

-- Existing revisions are grouped by changeset (a lone revision is its own group) and numbered in time order.
UPDATE revisions r SET commit_no = g.n
FROM (SELECT repo, path, id,
             dense_rank() OVER (PARTITION BY repo ORDER BY created_at,
                                COALESCE(changeset, path || '@' || lpad(id::text, 20, '0'))) AS n
      FROM revisions) g
WHERE r.repo = g.repo AND r.path = g.path AND r.id = g.id;

ALTER TABLE revisions ALTER COLUMN commit_no SET NOT NULL;

CREATE TABLE repo_commits (
    repo text   PRIMARY KEY,
    last bigint NOT NULL
);
INSERT INTO repo_commits (repo, last) SELECT repo, max(commit_no) FROM revisions GROUP BY repo;

CREATE INDEX revisions_commit ON revisions (repo, commit_no);
