-- A stable id per team. The slug is only an address: it can be reused after a delete, the id never is.
-- Application code supplies the id on creation.
ALTER TABLE teams ADD COLUMN team_id text;

UPDATE teams SET team_id = 't' || substr(replace(gen_random_uuid()::text, '-', ''), 1, 24);

ALTER TABLE teams ALTER COLUMN team_id SET NOT NULL;
CREATE UNIQUE INDEX teams_team_id_idx ON teams (team_id);
