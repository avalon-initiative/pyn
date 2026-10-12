-- Content ids become git blob ids (decision #245). `content_v1` holds the string a revision's v1 hash covers: the
-- SHA-256 content hash for rows that predate blob ids, NULL for rows made since. A row whose `content_v1` still equals
-- `content` has not been re-keyed yet; the server does that once at startup (migrate_content_ids).
ALTER TABLE revisions ADD COLUMN content_v1 text;
UPDATE revisions SET content_v1 = content;
