-- Security M1 / code M4: the verified-destination domain binding must not
-- trust an official_url that an admin wrote. Admin create/update records the
-- value it wrote here; while programs.official_url still equals it, the
-- binding requires an approved source domain. Ingestion replacing
-- official_url makes the two differ and restores the source binding.
-- Additive; no backfill (no admin edit history exists to reconstruct).
-- Rollback: ALTER TABLE programs DROP COLUMN admin_official_url.
ALTER TABLE programs ADD COLUMN IF NOT EXISTS admin_official_url TEXT
    CHECK (admin_official_url IS NULL OR length(admin_official_url) BETWEEN 1 AND 2048);
COMMENT ON COLUMN programs.admin_official_url IS
    'official_url value last written through the admin API; never written by ingestion. Equal to official_url => the URL is admin-set and does not bind application destinations.';
