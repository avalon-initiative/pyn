-- Objects uploaded through the Git LFS API, per repository: the sha256 oid and length the client named, and the
-- object store id (a git blob id) holding the bytes.
CREATE TABLE lfs_objects (
    repo       text        NOT NULL,
    oid        text        NOT NULL,
    size       bigint      NOT NULL CHECK (size >= 0),
    object     text        NOT NULL,
    created_at timestamptz NOT NULL,
    created_by text        NOT NULL,
    PRIMARY KEY (repo, oid)
);
