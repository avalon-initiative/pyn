-- What a commit's snapshot holds, kept per commit so a summary is a row read (docs/concepts/browsing.md#summary):
-- live paths, how many of them their revision records as exclusive, and the newest revision time along the first-parent
-- chain. NULL means not counted yet; the server counts such rows at startup, and a read counts them on the fly.
ALTER TABLE commits ADD COLUMN file_count bigint, ADD COLUMN exclusive_count bigint, ADD COLUMN newest_at timestamptz;
CREATE INDEX commits_uncounted ON commits (repo, seq) WHERE file_count IS NULL;

-- Backfill, oldest commit first so a commit starts from its parent's counts: parent + what the commit's pointers add or
-- remove. Keep the statement identical to COUNT_COMMIT in crates/pyn-postgres/src/counts.rs.
DO $backfill$
DECLARE
    c record;
BEGIN
    FOR c IN SELECT repo, id FROM commits ORDER BY repo, seq LOOP
        EXECUTE $count$
            WITH RECURSIVE self AS (
                SELECT parent FROM commits WHERE repo = $1 AND id = $2
            ), chain(id, parent, depth) AS (
                SELECT k.id, k.parent, 0 FROM commits k WHERE k.repo = $1 AND k.id = (SELECT parent FROM self)
                UNION ALL
                SELECT k.id, k.parent, chain.depth + 1 FROM chain JOIN commits k ON k.repo = $1 AND k.id = chain.parent
            ), mine AS (
                SELECT r.path, r.deleted, COALESCE(r.mode = 'exclusive', false) AS exclusive, r.created_at
                FROM commit_pointers p JOIN revisions r ON r.repo = p.repo AND r.path = p.path AND r.id = p.revision
                WHERE p.repo = $1 AND p.commit_id = $2
            ), prior AS (
                SELECT DISTINCT ON (p.path) p.path, r.deleted, COALESCE(r.mode = 'exclusive', false) AS exclusive
                FROM chain
                JOIN commit_pointers p ON p.repo = $1 AND p.commit_id = chain.id
                JOIN revisions r ON r.repo = p.repo AND r.path = p.path AND r.id = p.revision
                WHERE p.path IN (SELECT path FROM mine)
                ORDER BY p.path, chain.depth
            ), delta AS (
                SELECT COALESCE(SUM((NOT m.deleted)::int - (b.path IS NOT NULL AND NOT b.deleted)::int), 0) AS f,
                       COALESCE(SUM((NOT m.deleted AND m.exclusive)::int
                                    - (b.path IS NOT NULL AND NOT b.deleted AND b.exclusive)::int), 0) AS x,
                       MAX(m.created_at) AS t
                FROM mine m LEFT JOIN prior b ON b.path = m.path
            ), fresh AS (
                SELECT CASE WHEN s.parent IS NULL THEN 0 ELSE pc.file_count END + d.f AS f,
                       CASE WHEN s.parent IS NULL THEN 0 ELSE pc.exclusive_count END + d.x AS x,
                       GREATEST(pc.newest_at, d.t) AS t
                FROM self s CROSS JOIN delta d LEFT JOIN commits pc ON pc.repo = $1 AND pc.id = s.parent
            )
            UPDATE commits SET file_count = fresh.f, exclusive_count = fresh.x, newest_at = fresh.t
            FROM fresh WHERE commits.repo = $1 AND commits.id = $2
        $count$ USING c.repo, c.id;
    END LOOP;
END
$backfill$;
