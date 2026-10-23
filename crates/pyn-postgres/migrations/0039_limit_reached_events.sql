-- No foreign key: throttle history survives deletion of the owner.
CREATE TABLE webhook_limit_events (
    owner       text        NOT NULL,
    limit_kind  text        NOT NULL,
    last_event_at timestamptz NOT NULL,
    PRIMARY KEY (owner, limit_kind)
);
