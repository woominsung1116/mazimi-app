-- Supabase's default explicit grants can survive REVOKE FROM PUBLIC.
-- Definer functions (including legacy dashboard-created admin helpers) must
-- never be executable by generated REST client roles in this API-only app.
DO $functions$
DECLARE
  fn record;
  client_role text;
BEGIN
  FOR fn IN
    SELECT p.oid::regprocedure AS signature
    FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='public' AND p.prosecdef
  LOOP
    EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',fn.signature);
    FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname=client_role) THEN
        EXECUTE format('REVOKE ALL ON FUNCTION %s FROM %I',fn.signature,client_role);
      END IF;
    END LOOP;
  END LOOP;
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname=client_role) THEN
      EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON FUNCTIONS FROM %I',client_role);
    END IF;
  END LOOP;
END $functions$;
ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON FUNCTIONS FROM PUBLIC;
