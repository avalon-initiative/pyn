CREATE TABLE repositories (
    id          text        PRIMARY KEY,
    owner       text        NOT NULL,
    name        text        NOT NULL,
    visibility  text        NOT NULL,
    lease_hours integer     NOT NULL,
    created_at  timestamptz NOT NULL,
    UNIQUE (owner, name)
);

-- The single-repository server kept everything under the id 'default'; register it as owner/default so its
-- data stays reachable. The owner is its oldest admin, else its oldest member, else 'default'.
INSERT INTO repositories (id, owner, name, visibility, lease_hours, created_at)
SELECT 'default',
       COALESCE(
           (SELECT m.user_id FROM memberships m JOIN users u ON u.id = m.user_id
             WHERE m.repo = 'default' AND m.role = 'admin' ORDER BY u.created_at, u.id LIMIT 1),
           (SELECT m.user_id FROM memberships m JOIN users u ON u.id = m.user_id
             WHERE m.repo = 'default' ORDER BY u.created_at, u.id LIMIT 1),
           'default'),
       'default', 'private', 8, now()
WHERE EXISTS (SELECT 1 FROM revisions WHERE repo = 'default')
   OR EXISTS (SELECT 1 FROM locks WHERE repo = 'default')
   OR EXISTS (SELECT 1 FROM memberships WHERE repo = 'default')
   OR EXISTS (SELECT 1 FROM role_permissions WHERE repo = 'default')
   OR EXISTS (SELECT 1 FROM invites WHERE repo = 'default')
   OR EXISTS (SELECT 1 FROM audit_events WHERE repo = 'default');
