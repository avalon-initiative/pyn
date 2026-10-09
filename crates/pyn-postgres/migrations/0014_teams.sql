-- Teams are flat groups of an organization's members. Leaving the organization removes the team rows through
-- the composite foreign key, and deleting a team removes its members and repository roles.
CREATE TABLE teams (
    org         text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    slug        text        NOT NULL,
    name        text        NOT NULL,
    description text        NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL,
    PRIMARY KEY (org, slug)
);

CREATE TABLE team_members (
    org     text NOT NULL,
    team    text NOT NULL,
    user_id text NOT NULL,
    PRIMARY KEY (org, team, user_id),
    FOREIGN KEY (org, team) REFERENCES teams (org, slug) ON DELETE CASCADE,
    FOREIGN KEY (org, user_id) REFERENCES org_members (org, user_id) ON DELETE CASCADE
);

CREATE INDEX team_members_user_idx ON team_members (user_id);

CREATE TABLE team_repo_roles (
    repo text NOT NULL,
    org  text NOT NULL,
    team text NOT NULL,
    role text NOT NULL,
    PRIMARY KEY (repo, org, team),
    FOREIGN KEY (org, team) REFERENCES teams (org, slug) ON DELETE CASCADE
);

CREATE INDEX team_repo_roles_team_idx ON team_repo_roles (org, team);
