-- P2 intake: the automatic path from a corporate candidate to a programme
-- (crates/worker/src/corporate_auto/intake.rs, .reports/corporate-p2-design.md).
--
-- Additive only. Four internal tables, service role only (RLS forced):
-- - corporate_intake_state: per-candidate retry clock;
-- - corporate_intake_offers: an offer as read (quotes <= 100 chars and the
--   verbatim display that would be shown; never a page body), its sightings
--   for the 24 h stability rule, and the programme it became;
-- - corporate_intake_decisions: one audit row per automatic decision;
-- - corporate_offer_identities: one programme per (brand, offer key), the
--   dedup rule by construction.
-- The job runs in shadow unless CORPORATE_INTAKE_MODE=live (and the evidence
-- job is live); shadow never writes a programme.
--
-- Rollback (flag off first; programmes it created stay, retire them by
-- source_id 'intake:%' if needed):
--   DROP TABLE IF EXISTS corporate_intake_decisions, corporate_offer_identities,
--                        corporate_intake_offers, corporate_intake_state;

CREATE TABLE IF NOT EXISTS corporate_intake_state (
    candidate_id         UUID        PRIMARY KEY REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    attempts             INT         NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    consecutive_failures INT         NOT NULL DEFAULT 0 CHECK (consecutive_failures BETWEEN 0 AND 100),
    next_attempt_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_attempt_at      TIMESTAMPTZ,
    last_outcome         TEXT        CHECK (last_outcome IS NULL OR char_length(last_outcome) <= 60),
    last_detail          TEXT        CHECK (last_detail IS NULL OR char_length(last_detail) <= 300),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_corporate_intake_state_next ON corporate_intake_state (next_attempt_at);

CREATE TABLE IF NOT EXISTS corporate_intake_offers (
    id             UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    candidate_id   UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    kind           TEXT        NOT NULL CHECK (kind IN ('new_offer', 'human_source')),
    offer_key      TEXT        NOT NULL CHECK (offer_key ~ '^[0-9a-f]{64}$'),
    brand_key      TEXT        CHECK (brand_key IS NULL OR char_length(brand_key) <= 200),
    url_key        TEXT        NOT NULL CHECK (char_length(url_key) <= 2000),
    page_url       TEXT        NOT NULL CHECK (char_length(page_url) <= 2000),
    offer          JSONB       NOT NULL CHECK (jsonb_typeof(offer) = 'object' AND pg_column_size(offer) <= 16384),
    display        JSONB       NOT NULL CHECK (jsonb_typeof(display) = 'object' AND pg_column_size(display) <= 8192),
    status         TEXT        NOT NULL DEFAULT 'observed'
                               CHECK (status IN ('observed', 'published', 'superseded')),
    sightings      INT         NOT NULL DEFAULT 1 CHECK (sightings >= 1),
    first_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    program_id     UUID        REFERENCES programs(id) ON DELETE SET NULL,
    decided_at     TIMESTAMPTZ
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_corporate_intake_offers_observed
    ON corporate_intake_offers (candidate_id, offer_key) WHERE status = 'observed';
-- One published offer per page URL, across candidates.
CREATE UNIQUE INDEX IF NOT EXISTS uq_corporate_intake_offers_published_url
    ON corporate_intake_offers (url_key) WHERE status = 'published';
CREATE INDEX IF NOT EXISTS idx_corporate_intake_offers_candidate
    ON corporate_intake_offers (candidate_id, last_seen_at DESC);

CREATE TABLE IF NOT EXISTS corporate_offer_identities (
    brand_key   TEXT        NOT NULL CHECK (char_length(brand_key) BETWEEN 1 AND 200),
    offer_key   TEXT        NOT NULL CHECK (offer_key ~ '^[0-9a-f]{64}$'),
    program_id  UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (brand_key, offer_key)
);

CREATE TABLE IF NOT EXISTS corporate_intake_decisions (
    id            UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    decided_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    mode          TEXT        NOT NULL CHECK (mode IN ('shadow', 'live')),
    candidate_id  UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    outcome       TEXT        NOT NULL CHECK (char_length(outcome) BETWEEN 1 AND 80),
    gate          TEXT        NOT NULL CHECK (char_length(gate) BETWEEN 1 AND 40),
    detail        TEXT        CHECK (detail IS NULL OR char_length(detail) <= 300),
    offer_id      UUID        REFERENCES corporate_intake_offers(id) ON DELETE SET NULL,
    program_id    UUID        REFERENCES programs(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_corporate_intake_decisions_candidate
    ON corporate_intake_decisions (candidate_id, decided_at DESC);
CREATE INDEX IF NOT EXISTS idx_corporate_intake_decisions_recent
    ON corporate_intake_decisions (decided_at DESC);

DO $rls$
DECLARE
  t TEXT;
BEGIN
  FOREACH t IN ARRAY ARRAY['corporate_intake_state', 'corporate_intake_offers',
                           'corporate_offer_identities', 'corporate_intake_decisions'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON TABLE %I FROM PUBLIC', t);
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
      EXECUTE format('REVOKE ALL ON TABLE %I FROM anon', t);
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
      EXECUTE format('REVOKE ALL ON TABLE %I FROM authenticated', t);
    END IF;
    EXECUTE format('DROP POLICY IF EXISTS %I ON %I', t || '_all_service', t);
    EXECUTE format('CREATE POLICY %I ON %I FOR ALL USING (is_service_role()) WITH CHECK (is_service_role())',
                   t || '_all_service', t);
  END LOOP;
END
$rls$;

COMMENT ON TABLE corporate_intake_offers IS
    'P2 intake observations: offers read from live official pages (quotes <= 100 chars, verbatim display), stability sightings, the programme created. No page body.';
COMMENT ON TABLE corporate_intake_decisions IS
    'P2 intake audit: one row per automatic decision (mode, outcome, failing gate, detail).';
COMMENT ON TABLE corporate_offer_identities IS
    'P2 dedup by construction: at most one programme per brand and normalised offer key.';
