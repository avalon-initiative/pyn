-- An organization's display name and website link, shown beside its handle; both optional (docs/concepts/access.md).
ALTER TABLE org_profiles ADD COLUMN display_name text, ADD COLUMN url text;
