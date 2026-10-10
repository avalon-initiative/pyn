CREATE TABLE external_identities (
    id              text        PRIMARY KEY,
    user_id         text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    issuer          text        NOT NULL,
    subject         text        NOT NULL,
    email           text,
    created_at      timestamptz NOT NULL,
    last_sign_in_at timestamptz,
    UNIQUE (issuer, subject)
);

CREATE INDEX external_identities_user_idx ON external_identities (user_id, created_at, id);

CREATE TABLE oidc_flows (
    state_hash    text        PRIMARY KEY,
    binding_hash  text        NOT NULL,
    nonce         text        NOT NULL,
    code_verifier text        NOT NULL,
    link_user     text        REFERENCES users (id) ON DELETE CASCADE,
    return_to     text,
    expires_at    timestamptz NOT NULL
);

CREATE INDEX oidc_flows_expiry_idx ON oidc_flows (expires_at);
