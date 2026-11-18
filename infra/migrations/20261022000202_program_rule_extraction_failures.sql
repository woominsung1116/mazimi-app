-- Write failures of the rule_v1 text-rule refresh, per programme.
--
-- The refresh reads pending programmes in a bounded run (1,000 rows or
-- 60 s). A row whose write keeps failing would otherwise stay first in line
-- and, with enough of them, hold back every other pending row. The worker
-- counts failures here for the programme's current input hash and extractor
-- version, orders pending rows by that count, and parks a row after five
-- failures until its input hash or the extractor version changes. A
-- successful write deletes the row.
--
-- Measurement bookkeeping only: nothing serves it, and it holds no source
-- text (ids, a hash, counts and a time). Idempotent; additive only.

CREATE TABLE IF NOT EXISTS program_rule_extraction_failures (
    program_id        UUID        PRIMARY KEY REFERENCES programs(id) ON DELETE CASCADE,
    extractor_version INTEGER     NOT NULL CHECK (extractor_version > 0),
    input_hash        TEXT        NOT NULL CHECK (input_hash ~ '^[0-9a-f]{64}$'),
    failures          INTEGER     NOT NULL CHECK (failures > 0),
    last_failed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

DO $rls$
BEGIN
  ALTER TABLE program_rule_extraction_failures ENABLE ROW LEVEL SECURITY;
  ALTER TABLE program_rule_extraction_failures FORCE ROW LEVEL SECURITY;
  REVOKE ALL ON TABLE program_rule_extraction_failures FROM PUBLIC;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE program_rule_extraction_failures FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE program_rule_extraction_failures FROM authenticated;
  END IF;
  DROP POLICY IF EXISTS program_rule_extraction_failures_all_service
    ON program_rule_extraction_failures;
  CREATE POLICY program_rule_extraction_failures_all_service ON program_rule_extraction_failures
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
END
$rls$;

COMMENT ON TABLE program_rule_extraction_failures IS
    'rule_v1 refresh write failures per programme (current input hash and extractor version). Orders and parks pending rows; deleted on a successful write. Internal.';
