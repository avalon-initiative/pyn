-- An organization's repository-creation policy: a base setting for members and allow or deny rules by team, user
-- or organization role. Deleting the organization removes both; the store drops a team's or a leaving member's rules
-- in the same transaction as the removal.
CREATE TABLE org_repo_policy (
    org             text PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    member_creation text NOT NULL CHECK (member_creation IN ('none', 'private', 'both'))
);

CREATE TABLE org_repo_creation_rules (
    org     text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind    text NOT NULL CHECK (kind IN ('team', 'user', 'role')),
    subject text NOT NULL,
    effect  text NOT NULL CHECK (effect IN ('allow', 'deny')),
    scope   text NOT NULL CHECK (scope IN ('public', 'private', 'both')),
    PRIMARY KEY (org, kind, subject, effect)
);
