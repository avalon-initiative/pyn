-- Commits become git commits on a single mainline (decision #245). `seq` is the internal order of the mainline and the
-- old per-repository commit number; the commit id is the git commit id. Rows that predate this are migrated once by the
-- server at startup (migrate_history): `revisions.commit_id` stays NULL until then.
CREATE TABLE commits (
    repo             text        NOT NULL,
    seq              bigint      NOT NULL,
    id               text        NOT NULL,
    parent           text,
    tree             text        NOT NULL,
    author_name      text        NOT NULL,
    author_email     text        NOT NULL,
    author_offset    integer     NOT NULL,
    committer_name   text        NOT NULL,
    committer_email  text        NOT NULL,
    committer_offset integer     NOT NULL,
    created_at       timestamptz NOT NULL,
    message          text        NOT NULL,
    signature        text,
    changeset        text,
    PRIMARY KEY (repo, seq),
    UNIQUE (repo, id)
);

-- Git tree objects, keyed by their id; bodies are git's tree encoding.
CREATE TABLE trees (
    repo text  NOT NULL,
    id   text  NOT NULL,
    body bytea NOT NULL,
    PRIMARY KEY (repo, id)
);

-- Revision encoding v2: a row with author_name set is hashed as v2, one without keeps its v1 hash.
ALTER TABLE revisions
    ADD COLUMN commit_id     text,
    ADD COLUMN file_mode     text    NOT NULL DEFAULT '100644' CHECK (file_mode IN ('100644', '100755', '120000')),
    ADD COLUMN author_name   text,
    ADD COLUMN author_email  text,
    ADD COLUMN author_offset integer;

CREATE INDEX revisions_commit_id ON revisions (repo, commit_id);
