CREATE TABLE server_settings (
    singleton      boolean     PRIMARY KEY DEFAULT true CHECK (singleton),
    initialised_at timestamptz NOT NULL,
    server_name    text,
    public_url     text,
    registration   text
);

-- A server that already has accounts was set up by the earlier bootstrap settings.
INSERT INTO server_settings (initialised_at) SELECT now() WHERE EXISTS (SELECT 1 FROM users);
