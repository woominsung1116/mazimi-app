-- The schema is recreated or upgraded by api-migrate before this file runs.
-- Replace every application table in public while retaining SQLx's migration
-- ledger, which belongs to the currently deployed application version.
DO $restore$
DECLARE
    managed_tables TEXT;
BEGIN
    SELECT string_agg(format('%I.%I', schemaname, tablename), ', ' ORDER BY tablename)
      INTO managed_tables
      FROM pg_tables
     WHERE schemaname = 'public'
       AND tablename <> '_sqlx_migrations';

    IF managed_tables IS NOT NULL THEN
        EXECUTE 'TRUNCATE TABLE ' || managed_tables || ' RESTART IDENTITY CASCADE';
    END IF;
END
$restore$;
