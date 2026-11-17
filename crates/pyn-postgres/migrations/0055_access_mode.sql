-- Existing servers stay private: nothing is readable signed out until an administrator says otherwise.
ALTER TABLE server_settings ADD COLUMN access_mode text NOT NULL DEFAULT 'private' CHECK (access_mode IN ('private', 'public'));
