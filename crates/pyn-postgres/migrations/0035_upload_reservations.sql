-- Holds on a repository's unreferenced-upload budget while an upload streams in; expired holds are ignored.
CREATE TABLE upload_reservations (
    id         bigserial   PRIMARY KEY,
    repo       text        NOT NULL,
    bytes      bigint      NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX upload_reservations_repo ON upload_reservations (repo);
