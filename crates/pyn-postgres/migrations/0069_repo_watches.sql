-- A user's watch level on a repository and the events queued for them (decision #1282). repo is a repository id
-- without a foreign key, like memberships.repo; both tables go with the account (also on a rename) and the level and
-- queue go with the repository (delete_repo_access). A queued row goes with its event when the outbox is purged.
CREATE TABLE repo_watches (
    repo       text        NOT NULL,
    user_id    text        NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    level      text        NOT NULL CHECK (level IN ('all', 'locks', 'ignore')),
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (repo, user_id)
);

CREATE INDEX repo_watches_user_idx ON repo_watches (user_id, updated_at DESC, repo);

CREATE TABLE watch_notifications (
    id       bigserial PRIMARY KEY,
    user_id  text   NOT NULL REFERENCES users (id) ON UPDATE CASCADE ON DELETE CASCADE,
    repo     text   NOT NULL,
    event_id bigint NOT NULL REFERENCES webhook_events (id) ON DELETE CASCADE,
    UNIQUE (user_id, event_id)
);

CREATE INDEX watch_notifications_repo_idx ON watch_notifications (repo);
