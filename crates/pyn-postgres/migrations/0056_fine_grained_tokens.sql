-- Fine-grained tokens: a resource owner, a repository access mode and per-resource grants ("resource:level")
-- replace the permission list and the empty-means-all repository list.
ALTER TABLE tokens
    ADD COLUMN description text,
    ADD COLUMN owner text,
    ADD COLUMN repo_access text,
    ADD COLUMN grants text[];

UPDATE tokens SET
    owner = user_id,
    repo_access = CASE WHEN sign_in OR cardinality(repos) = 0 THEN 'all' ELSE 'selected' END,
    grants = CASE WHEN sign_in THEN ARRAY[
            'metadata:read', 'contents:write', 'locks:admin', 'policy:write',
            'members:write', 'administration:write', 'audit:read']
        ELSE array_remove(ARRAY[
            'metadata:read',
            CASE WHEN permissions && ARRAY['checkin', 'restore'] THEN 'contents:write'
                 WHEN 'read' = ANY (permissions) THEN 'contents:read' END,
            CASE WHEN 'force_unlock' = ANY (permissions) THEN 'locks:admin'
                 WHEN 'lock' = ANY (permissions) THEN 'locks:write' END,
            CASE WHEN 'edit_policy' = ANY (permissions) THEN 'policy:write' END,
            CASE WHEN 'manage_users' = ANY (permissions) THEN 'members:write' END,
            CASE WHEN 'manage_roles' = ANY (permissions) THEN 'administration:write' END,
            CASE WHEN 'view_audit' = ANY (permissions) THEN 'audit:read' END
        ], NULL) END;

ALTER TABLE tokens
    ALTER COLUMN owner SET NOT NULL,
    ALTER COLUMN repo_access SET NOT NULL,
    ALTER COLUMN grants SET NOT NULL,
    DROP COLUMN permissions;
