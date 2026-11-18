-- Redesign resilience for corporate evidence (normalized anchors, element
-- relocation, proven value and URL changes). Additive only; every flag is
-- still off, so nothing reads these until the evidence job runs.
--
-- Rollback (do not run automatically; flags off first):
--   DROP TABLE IF EXISTS corporate_change_proposals;
--   ALTER TABLE corporate_offer_facts DROP COLUMN anchors, DROP COLUMN anchors_updated_at;
--   DELETE FROM corporate_evidence_checks WHERE outcome IN ('confirmed_fuzzy','relocated',
--     'update_proposed','update_applied','ambiguous','url_move_proposed','url_adopted');
--   ALTER TABLE corporate_evidence_checks DROP CONSTRAINT corporate_evidence_checks_outcome_check;
--   ALTER TABLE corporate_evidence_checks ADD CONSTRAINT corporate_evidence_checks_outcome_check CHECK (outcome IN (
--     'confirmed','quote_missing','window_changed','field_mismatch','no_evidence','adoption_expired',
--     'robots_disallowed','blocked','rate_limited','gone','retired_gone','redirected','fetch_failed',
--     'policy_blocked','opted_out','unsupported'));

-- Locators stored with a fact (element fingerprint with heading and label,
-- replaced quotes). Never displayed.
ALTER TABLE corporate_offer_facts
    ADD COLUMN IF NOT EXISTS anchors JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS anchors_updated_at TIMESTAMPTZ;
ALTER TABLE corporate_offer_facts DROP CONSTRAINT IF EXISTS corporate_offer_facts_anchors_check;
ALTER TABLE corporate_offer_facts ADD CONSTRAINT corporate_offer_facts_anchors_check
    CHECK (jsonb_typeof(anchors) = 'object' AND pg_column_size(anchors) <= 16384);

ALTER TABLE corporate_evidence_checks DROP CONSTRAINT IF EXISTS corporate_evidence_checks_outcome_check;
ALTER TABLE corporate_evidence_checks ADD CONSTRAINT corporate_evidence_checks_outcome_check CHECK (outcome IN (
    'confirmed', 'quote_missing', 'window_changed', 'field_mismatch',
    'no_evidence', 'adoption_expired', 'robots_disallowed', 'blocked',
    'rate_limited', 'gone', 'retired_gone', 'redirected', 'fetch_failed',
    'policy_blocked', 'opted_out', 'unsupported',
    'confirmed_fuzzy', 'relocated', 'update_proposed', 'update_applied', 'ambiguous',
    'url_move_proposed', 'url_adopted'));

-- A value or URL change seen on a live page. Applied only after two
-- fetches at least 24 h apart agree (and, for a value, both extractors
-- agreed each time); until then the old fact stays hidden.
CREATE TABLE IF NOT EXISTS corporate_change_proposals (
    id               UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    kind             TEXT        NOT NULL CHECK (kind IN ('value', 'url')),
    candidate_id     UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    program_id       UUID        REFERENCES programs(id) ON DELETE CASCADE,
    fact_id          UUID        REFERENCES corporate_offer_facts(id) ON DELETE CASCADE,
    change_key       TEXT        NOT NULL CHECK (change_key ~ '^[0-9a-f]{64}$'),
    old_value        TEXT        CHECK (old_value IS NULL OR char_length(old_value) <= 2000),
    new_value        TEXT        NOT NULL CHECK (char_length(new_value) BETWEEN 1 AND 2000),
    new_text         TEXT        CHECK (new_text IS NULL OR char_length(new_text) <= 300),
    new_min_age      INT,
    new_max_age      INT,
    extractors_agree BOOLEAN     NOT NULL DEFAULT false,
    sightings        INT         NOT NULL DEFAULT 1 CHECK (sightings >= 1),
    first_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    agreed_at        TIMESTAMPTZ,
    -- First sighting in live mode: only two live sightings >= 24 h apart
    -- apply a change, so agreement reached in shadow never applies at once.
    first_live_at    TIMESTAMPTZ,
    status           TEXT        NOT NULL DEFAULT 'pending'
                                 CHECK (status IN ('pending', 'applied', 'superseded', 'unappliable')),
    applied_at       TIMESTAMPTZ,
    details          JSONB       NOT NULL DEFAULT '{}'::jsonb,
    CHECK (kind <> 'value' OR fact_id IS NOT NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_corporate_change_proposals_pending
    ON corporate_change_proposals (candidate_id, change_key) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_corporate_change_proposals_fact
    ON corporate_change_proposals (fact_id) WHERE status = 'pending';

ALTER TABLE corporate_change_proposals ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_change_proposals FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_change_proposals FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_change_proposals FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_change_proposals FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_change_proposals_all_service ON corporate_change_proposals;
CREATE POLICY corporate_change_proposals_all_service ON corporate_change_proposals
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE corporate_change_proposals IS
    'Value and URL changes seen by the evidence job; applied only after two agreeing fetches >= 24 h apart. Internal, never displayed.';
