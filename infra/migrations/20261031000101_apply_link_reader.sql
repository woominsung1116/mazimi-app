-- Automatically derived application links
-- (crates/worker/src/application_resources/apply_entry).
--
-- program_application_resources.reader: which structural reader derived an
-- application row (gov24 | bokjiro | youthcenter | generic). NULL is every
-- other row (seeded discovery, admin entries): the existing probe keeps
-- those, the apply-link job keeps its own. A human review leaves `reader`
-- in place but replaces reviewed_by, and the job never overrides a human
-- decision.
--
-- program_apply_link_scans: one row per programme with the job's latest
-- outcome (verified, pending, offline, not_found, ambiguous, unreachable,
-- rejected), the reason code, a hash of the inputs it read (so an unchanged
-- programme is not re-derived) and when to scan it again. Worker-written
-- only; RLS is on and forced with only the service-role policy.
--
-- Additive only. Rollback (do not run automatically): unset
-- APPLY_LINKS_ENABLED, then DROP TABLE program_apply_link_scans and
-- ALTER TABLE program_application_resources DROP COLUMN reader.

ALTER TABLE program_application_resources
    ADD COLUMN IF NOT EXISTS reader TEXT
        CHECK (reader IS NULL OR reader IN ('gov24','bokjiro','youthcenter','generic'));
COMMENT ON COLUMN program_application_resources.reader IS
    'Structural reader that derived this application row (worker apply_entry); NULL for seeded or admin rows.';
CREATE INDEX IF NOT EXISTS application_resources_reader_due
    ON program_application_resources (next_check_at) WHERE reader IS NOT NULL AND status <> 'rejected';

CREATE TABLE IF NOT EXISTS program_apply_link_scans (
    program_id   UUID        PRIMARY KEY REFERENCES programs(id) ON DELETE CASCADE,
    outcome      TEXT        NOT NULL CHECK (outcome IN
                     ('verified','pending','offline','not_found','ambiguous','unreachable','rejected')),
    reader       TEXT        CHECK (reader IS NULL OR reader IN ('gov24','bokjiro','youthcenter','generic')),
    reason       TEXT        CHECK (reason IS NULL OR char_length(reason) BETWEEN 1 AND 80),
    input_hash   TEXT        NOT NULL CHECK (input_hash ~ '^[0-9a-f]{64}$'),
    resource_id  UUID        REFERENCES program_application_resources(id) ON DELETE SET NULL,
    -- Scans in a row that could not read the page (robots, refusals,
    -- network): the next scan backs off 1, 2, 4, then 7 days.
    consecutive_unreachable INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_unreachable BETWEEN 0 AND 100000),
    scanned_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_scan_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_program_apply_link_scans_due ON program_apply_link_scans (next_scan_at);
CREATE INDEX IF NOT EXISTS idx_program_apply_link_scans_outcome ON program_apply_link_scans (outcome);

DO $rls$
DECLARE
  client_role text;
BEGIN
  ALTER TABLE public.program_apply_link_scans ENABLE ROW LEVEL SECURITY;
  ALTER TABLE public.program_apply_link_scans FORCE ROW LEVEL SECURITY;
  REVOKE ALL ON TABLE public.program_apply_link_scans FROM PUBLIC;
  FOREACH client_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON TABLE public.program_apply_link_scans FROM %I', client_role);
    END IF;
  END LOOP;
  DROP POLICY IF EXISTS program_apply_link_scans_all_service ON public.program_apply_link_scans;
  CREATE POLICY program_apply_link_scans_all_service ON public.program_apply_link_scans
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
END
$rls$;

COMMENT ON TABLE program_apply_link_scans IS
    'Latest apply-link derivation outcome per programme (worker apply_entry). Worker-written; never read by clients.';
