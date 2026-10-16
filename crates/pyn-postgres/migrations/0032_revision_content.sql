-- Reading a blob by id asks whether a revision of the repository has it.
CREATE INDEX revisions_content ON revisions (repo, content);
