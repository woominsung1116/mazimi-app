-- current_user inside SECURITY DEFINER is the function owner. Using it there
-- made the legacy RLS service check true even for a non-service caller.
ALTER FUNCTION is_service_role() SECURITY INVOKER;
ALTER FUNCTION is_service_role() SET search_path = pg_catalog, public;
ALTER FUNCTION app_user_id() SECURITY INVOKER;
ALTER FUNCTION app_user_id() SET search_path = pg_catalog, public;
ALTER FUNCTION promote_to_admin(UUID) SET search_path = pg_catalog, public;
ALTER FUNCTION run_corporate_benefit_maintenance(TIMESTAMPTZ) SET search_path = pg_catalog, public;
