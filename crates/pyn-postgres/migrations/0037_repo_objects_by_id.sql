-- Rolling back an upload asks whether any repository recorded the object.
CREATE INDEX repo_objects_id ON repo_objects (id);
