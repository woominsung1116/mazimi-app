-- Clients use the Axum API, not Supabase's generated public REST endpoints.
-- Table grants must not bypass the API's filtering or expose private rows.
DROP POLICY IF EXISTS user_documents_service ON user_documents;

DO $access$
DECLARE
  table_name text;
  client_role text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    '_sqlx_migrations','users','user_profiles','programs','current_programs',
    'user_bookmarks','user_program_states','user_program_state_history',
    'ingestion_runs','ingestion_items','source_snapshots','alert_subscriptions',
    'alert_deliveries','eligibility_rules','program_documents','notification_preferences',
    'push_tokens','subscription_plans','user_subscriptions','user_documents','expo_push_tickets',
    'brands','program_brands','brand_source_records','benefit_source_domains',
    'benefit_source_candidates','benefit_candidate_brands','benefit_source_domain_brands',
    'brand_discovery_targets','brand_source_suggestions','program_source_monitors',
    'benefit_source_checks','program_source_holds','benefit_candidate_enrichments',
    'program_data_quality_issues','operational_events','program_duplicate_candidates',
    'program_expiration_events','benefit_feeds','benefit_feed_batches','benefit_feed_items',
    'application_preparations','universities','worker_daily_jobs','application_program_snapshots',
    'oauth_handoffs'
  ] LOOP
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', table_name);
    FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname=client_role) THEN
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM %I', table_name, client_role);
      END IF;
    END LOOP;
  END LOOP;
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname=client_role) THEN
      EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON TABLES FROM %I',client_role);
      EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON SEQUENCES FROM %I',client_role);
    END IF;
  END LOOP;
END $access$;
ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON TABLES FROM PUBLIC;
