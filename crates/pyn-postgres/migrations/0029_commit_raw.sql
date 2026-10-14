-- The exact git commit bytes (decision #245): the id is their hash. Existing rows are re-encoded from their fields the
-- way the server derived each id.
ALTER TABLE commits ADD COLUMN raw bytea;
UPDATE commits c SET raw = convert_to(
    'tree ' || c.tree || E'\n'
    || coalesce((SELECT string_agg('parent ' || p || E'\n', '' ORDER BY ord) FROM unnest(c.parents) WITH ORDINALITY AS t(p, ord)), '')
    || 'author ' || c.author_name || ' <' || c.author_email || '> ' || floor(extract(epoch FROM c.created_at))::bigint
        || ' ' || CASE WHEN c.author_offset < 0 THEN '-' ELSE '+' END
        || lpad((abs(c.author_offset) / 60)::text, 2, '0') || lpad((abs(c.author_offset) % 60)::text, 2, '0') || E'\n'
    || 'committer ' || c.committer_name || ' <' || c.committer_email || '> ' || floor(extract(epoch FROM c.created_at))::bigint
        || ' ' || CASE WHEN c.committer_offset < 0 THEN '-' ELSE '+' END
        || lpad((abs(c.committer_offset) / 60)::text, 2, '0') || lpad((abs(c.committer_offset) % 60)::text, 2, '0') || E'\n'
    || coalesce('gpgsig ' || replace(c.signature, E'\n', E'\n ') || E'\n', '')
    || E'\n'
    || c.message || CASE WHEN right(c.message, 1) = E'\n' THEN '' ELSE E'\n' END,
    'UTF8');
ALTER TABLE commits ALTER COLUMN raw SET NOT NULL;
