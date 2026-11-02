-- The policy a repository starts with, in force until its branch has a policy commit; null keeps the server fallback.
ALTER TABLE repositories ADD COLUMN initial_policy text;
