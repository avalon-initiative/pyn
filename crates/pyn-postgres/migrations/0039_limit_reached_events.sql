CREATE TABLE webhook_limit_events (
    owner       text        NOT NULL,
    limit_kind  text        NOT NULL,
    last_event  timestamptz NOT NULL,
    PRIMARY KEY (owner, limit_kind)
);
