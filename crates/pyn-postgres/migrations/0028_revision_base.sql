-- The revision each revision was based on (decision #299): the committing branch's pointer for the path. Revisions
-- made under the earlier single-line rule of an exclusive path were based on the revision before them.
ALTER TABLE revisions ADD COLUMN base_id bigint;
UPDATE revisions SET base_id = id - 1 WHERE mode = 'exclusive' AND id > 1;
