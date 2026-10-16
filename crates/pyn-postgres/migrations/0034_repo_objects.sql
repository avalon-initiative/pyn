-- The uploaded objects each repository may reference. `assembled` marks a chunk that an assembled object holds,
-- which stops counting as an unreferenced upload. Existing revisions backfill the content they name.
CREATE TABLE repo_objects (
    repo      text    NOT NULL,
    id        text    NOT NULL,
    size      bigint  NOT NULL,
    assembled boolean NOT NULL DEFAULT false,
    PRIMARY KEY (repo, id)
);

INSERT INTO repo_objects (repo, id, size)
SELECT DISTINCT ON (repo, content) repo, content, size FROM revisions
ON CONFLICT DO NOTHING;
