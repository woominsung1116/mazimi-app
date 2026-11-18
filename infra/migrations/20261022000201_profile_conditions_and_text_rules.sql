-- Three optional profile answers and the rule_v1 text-rule extraction.
--
-- 1. user_profiles gains five nullable answers. NULL means "not answered"
--    and keeps every programme that asks about it at 확인 필요; the rule
--    engine reads NULL as Unknown, never Pass.
--    - homeless_household: the user's own household (세대) owns no home.
--    - marital_status: 'unmarried' | 'married'.
--    - newlywed: married within 7 years (asked only when married).
--    - children_count: number of children (0-20).
--    - income_median_band: 기준 중위소득 band the user chose, or 'unknown'
--      (모름). The band is never computed for the user.
--    Like the existing sensitive answers (has_disability,
--    is_low_income_household, income_bracket) these are plain columns of
--    user_profiles, which has FORCE ROW LEVEL SECURITY with own-row
--    policies (20260322000001) and no client-role grants (20260923000012).
--    The policies are table-wide, so they already cover the new columns.
--    Only uploaded documents are encrypted at the application layer.
--    The API never logs these values.
--
-- 2. program_rule_extractions: what the deterministic text rules (rule_v1)
--    read from each programme's eligibility text, one row per programme,
--    recomputed by the worker when extractor_version or input_hash changes.
--    Measurement only: nothing serves this table. It never feeds
--    eligibility_rules, ranking, labels, explanations or "조건 확인됨".
--
-- Additive only. Rollback (do not run automatically): revert the API and
-- worker readers first, then drop the five columns and the table.

ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS homeless_household BOOLEAN,
    ADD COLUMN IF NOT EXISTS marital_status     TEXT
        CHECK (marital_status IS NULL OR marital_status IN ('unmarried', 'married')),
    ADD COLUMN IF NOT EXISTS newlywed           BOOLEAN,
    ADD COLUMN IF NOT EXISTS children_count     INT
        CHECK (children_count IS NULL OR children_count BETWEEN 0 AND 20),
    ADD COLUMN IF NOT EXISTS income_median_band TEXT
        CHECK (income_median_band IS NULL OR income_median_band IN
               ('le50', 'le60', 'le100', 'le120', 'le150', 'le180', 'gt180', 'unknown'));

DO $newlywed$
BEGIN
  ALTER TABLE user_profiles ADD CONSTRAINT user_profiles_newlywed_married
    CHECK (newlywed IS NULL OR marital_status = 'married');
EXCEPTION WHEN duplicate_object THEN NULL;
END
$newlywed$;

CREATE TABLE IF NOT EXISTS program_rule_extractions (
    program_id        UUID        PRIMARY KEY REFERENCES programs(id) ON DELETE CASCADE,
    extractor         TEXT        NOT NULL CHECK (extractor ~ '^[a-z0-9_]{1,40}$'),
    extractor_version INTEGER     NOT NULL CHECK (extractor_version > 0),
    -- SHA-256 (hex) of the source fields the extractor reads, computed in SQL
    -- (mazimi_core::text_rules::input_hash_sql) so the list can compare it
    -- with the programme's current payload.
    input_hash        TEXT        NOT NULL CHECK (input_hash ~ '^[0-9a-f]{64}$'),
    has_text          BOOLEAN     NOT NULL,
    -- Every condition of the text is a fact of an enabled kind and no source
    -- code states a condition the rules do not compile.
    fully_structured  BOOLEAN     NOT NULL,
    -- fully_structured and every fact compiles to a profile condition.
    evaluable         BOOLEAN     NOT NULL,
    -- [{kind, value, role, field, quote, rule, evaluable}], quotes verbatim.
    facts             JSONB       NOT NULL CHECK (jsonb_typeof(facts) = 'array'),
    -- The rule_engine conditions the facts would compile to (never served).
    leaves            JSONB       NOT NULL CHECK (jsonb_typeof(leaves) = 'array'),
    -- Why the text is not fully structured (fixed codes, no source text).
    blockers          TEXT[]      NOT NULL DEFAULT '{}',
    computed_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (fully_structured OR NOT evaluable)
);
CREATE INDEX IF NOT EXISTS idx_program_rule_extractions_evaluable
    ON program_rule_extractions (program_id) WHERE evaluable;

DO $rls$
BEGIN
  ALTER TABLE program_rule_extractions ENABLE ROW LEVEL SECURITY;
  ALTER TABLE program_rule_extractions FORCE ROW LEVEL SECURITY;
  REVOKE ALL ON TABLE program_rule_extractions FROM PUBLIC;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE program_rule_extractions FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE program_rule_extractions FROM authenticated;
  END IF;
  DROP POLICY IF EXISTS program_rule_extractions_all_service ON program_rule_extractions;
  CREATE POLICY program_rule_extractions_all_service ON program_rule_extractions
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
END
$rls$;

COMMENT ON TABLE program_rule_extractions IS
    'rule_v1 eligibility facts per programme (verbatim quotes), recomputed on extractor_version or input_hash change. Measurement only; nothing serves it. Internal.';
