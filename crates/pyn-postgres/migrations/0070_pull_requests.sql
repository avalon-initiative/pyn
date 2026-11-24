-- Pull requests and their events (issue #696). repo is a repository id without a foreign key, like branches.repo; the
-- rows go with the repository (delete_repo_with_events).
CREATE TABLE pull_requests (
    repo         text        NOT NULL,
    number       bigint      NOT NULL,
    title        text        NOT NULL,
    body         text        NOT NULL,
    state        text        NOT NULL CHECK (state IN ('open', 'merged', 'closed')),
    draft        boolean     NOT NULL,
    base         text        NOT NULL,
    head         text        NOT NULL,
    author       text        NOT NULL,
    created_at   timestamptz NOT NULL,
    updated_at   timestamptz NOT NULL,
    closed_at    timestamptz,
    merged_at    timestamptz,
    merged_by    text,
    merge_commit text,
    frozen_base  text,
    frozen_head  text,
    PRIMARY KEY (repo, number)
);

CREATE UNIQUE INDEX pull_requests_one_open ON pull_requests (repo, base, head) WHERE state = 'open';
CREATE INDEX pull_requests_state_idx ON pull_requests (repo, state, number DESC);

CREATE TABLE pull_events (
    id        bigserial   PRIMARY KEY,
    repo      text        NOT NULL,
    number    bigint      NOT NULL,
    kind      text        NOT NULL CHECK (kind IN ('comment', 'merged', 'closed', 'reopened')),
    actor     text        NOT NULL,
    at        timestamptz NOT NULL,
    body      text,
    commit_id text
);

CREATE INDEX pull_events_pull_idx ON pull_events (repo, number, id);
