ALTER TABLE service_credentials
    ADD COLUMN last_used_from        text,
    ADD COLUMN expires_at            timestamptz,
    ADD COLUMN allowed_sources       text[] NOT NULL DEFAULT '{}',
    ADD COLUMN rate_limit_per_minute integer CHECK (rate_limit_per_minute > 0);
