-- P2 link discovery: when each allowed brand domain was last explored for
-- offer pages (crates/worker/src/corporate_auto/intake.rs `discover`).
-- Additive; service role only (RLS forced). Pages found become ordinary
-- `review` candidates on the existing intake path; nothing here is shown.
--
-- Rollback: DROP TABLE IF EXISTS corporate_discovery_state;

CREATE TABLE IF NOT EXISTS corporate_discovery_state (
    domain_id     UUID        PRIMARY KEY REFERENCES benefit_source_domains(id) ON DELETE CASCADE,
    last_run_at   TIMESTAMPTZ,
    next_run_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    pages_fetched INT         NOT NULL DEFAULT 0 CHECK (pages_fetched >= 0),
    found         INT         NOT NULL DEFAULT 0 CHECK (found >= 0),
    last_outcome  TEXT        CHECK (last_outcome IS NULL OR char_length(last_outcome) <= 60),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_corporate_discovery_state_next ON corporate_discovery_state (next_run_at);

ALTER TABLE corporate_discovery_state ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_discovery_state FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_discovery_state FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_discovery_state FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_discovery_state FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_discovery_state_all_service ON corporate_discovery_state;
CREATE POLICY corporate_discovery_state_all_service ON corporate_discovery_state
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE corporate_discovery_state IS
    'P2 link discovery schedule per allowed brand domain (pages fetched, offer pages found). Internal.';
