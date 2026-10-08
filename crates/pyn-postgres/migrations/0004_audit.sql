CREATE TABLE audit_events (
    id     bigserial   PRIMARY KEY,
    repo   text        NOT NULL,
    at     timestamptz NOT NULL,
    actor  text        NOT NULL,
    action text        NOT NULL,
    path   text,
    detail text        NOT NULL
);

CREATE INDEX audit_events_repo_idx ON audit_events (repo, id DESC);
