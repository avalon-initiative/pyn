CREATE TABLE locks (
    repo        text        NOT NULL,
    path        text        NOT NULL,
    owner       text        NOT NULL,
    acquired_at timestamptz NOT NULL,
    expires_at  timestamptz NOT NULL,
    PRIMARY KEY (repo, path)
);

CREATE TABLE revisions (
    repo       text        NOT NULL,
    path       text        NOT NULL,
    id         bigint      NOT NULL,
    content    text        NOT NULL,
    author     text        NOT NULL,
    message    text        NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (repo, path, id)
);
