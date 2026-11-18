-- Lock lists need locks read: a token that holds contents at any level and no locks grant keeps listing locks.
UPDATE tokens
SET grants = array_append(grants, 'locks:read')
WHERE NOT sign_in
  AND EXISTS (SELECT 1 FROM unnest(grants) g WHERE g LIKE 'contents:%')
  AND NOT EXISTS (SELECT 1 FROM unnest(grants) g WHERE g LIKE 'locks:%');
