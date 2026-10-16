-- Tokens issued by a password sign-in. Created tokens always name a repository, so an unscoped one is a sign-in.
ALTER TABLE tokens ADD COLUMN sign_in boolean NOT NULL DEFAULT false;
UPDATE tokens SET sign_in = true WHERE cardinality(repos) = 0;
