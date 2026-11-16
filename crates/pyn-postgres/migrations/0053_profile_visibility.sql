-- Which profile fields other people may see; every field is visible until its owner hides it.
ALTER TABLE account_profiles
    ADD COLUMN show_display_name boolean NOT NULL DEFAULT true,
    ADD COLUMN show_bio          boolean NOT NULL DEFAULT true,
    ADD COLUMN show_location     boolean NOT NULL DEFAULT true,
    ADD COLUMN show_company      boolean NOT NULL DEFAULT true,
    ADD COLUMN show_links        boolean NOT NULL DEFAULT true,
    ADD COLUMN show_avatar       boolean NOT NULL DEFAULT true;
