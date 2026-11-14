-- Addresses an account holds beside its primary one (`users.email`). One holder per address, verified or not.
CREATE TABLE account_emails (
    user_id     text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    address     text        NOT NULL,
    verified_at timestamptz,
    created_at  timestamptz NOT NULL,
    PRIMARY KEY (user_id, address)
);

CREATE UNIQUE INDEX account_emails_address_key ON account_emails (address);

-- An account may wait on a link for several addresses at once.
ALTER TABLE email_verifications DROP CONSTRAINT email_verifications_user_id_key;
ALTER TABLE email_verifications ADD CONSTRAINT email_verifications_user_email_key UNIQUE (user_id, email);
