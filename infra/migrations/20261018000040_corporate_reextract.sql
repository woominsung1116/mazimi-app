-- Fresh extraction for sources the evidence job flagged
-- (`corporate_evidence_targets.reextract_requested_at`, 20261018000012).
-- The re-extraction job reads the flag, fetches the page through the same
-- policy engine as a new candidate and writes a proposal here. Nothing in
-- this migration or the job changes a programme, a monitor, a hold or a
-- fact: proposals are internal and wait for a reviewed publishing step.
-- Additive only; the job is off until CORPORATE_REEXTRACT_ENABLED is set.
--
-- Rollback (do not run automatically; flag off first):
--   DROP TABLE IF EXISTS corporate_reextract_proposals;
--   DROP TABLE IF EXISTS corporate_reextract_state;

-- Per-source progress of the consumer. The evidence job owns the request
-- flag; this table records which request was handled and the retry clock,
-- so a request is consumed once and a failing source backs off.
CREATE TABLE IF NOT EXISTS corporate_reextract_state (
    candidate_id         UUID        PRIMARY KEY REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    -- The `reextract_requested_at` value whose extraction completed.
    handled_request_at   TIMESTAMPTZ,
    attempts             INT         NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    consecutive_failures INT         NOT NULL DEFAULT 0 CHECK (consecutive_failures BETWEEN 0 AND 100),
    next_attempt_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_attempt_at      TIMESTAMPTZ,
    last_success_at      TIMESTAMPTZ,
    last_outcome         TEXT        CHECK (last_outcome IS NULL OR char_length(last_outcome) <= 60),
    last_detail          TEXT        CHECK (last_detail IS NULL OR char_length(last_detail) <= 300),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_corporate_reextract_state_next
    ON corporate_reextract_state (next_attempt_at);

-- One freshly extracted offer per source and content. Quotes are at most
-- 100 characters each (bootstrap::MAX_QUOTE_CHARS); no page body is kept.
CREATE TABLE IF NOT EXISTS corporate_reextract_proposals (
    id               UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    candidate_id     UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    program_ids      UUID[]      NOT NULL DEFAULT '{}',
    request_at       TIMESTAMPTZ NOT NULL,
    request_reason   TEXT        CHECK (request_reason IS NULL OR char_length(request_reason) <= 60),
    -- The verification mode of the run that proposed it (hide_only, like
    -- shadow, never publishes).
    mode             TEXT        NOT NULL CHECK (mode IN ('shadow', 'hide_only', 'live')),
    proposal_key     TEXT        NOT NULL CHECK (proposal_key ~ '^[0-9a-f]{64}$'),
    url              TEXT        NOT NULL CHECK (char_length(url) <= 2000),
    final_url        TEXT        CHECK (final_url IS NULL OR char_length(final_url) <= 2000),
    page_hash        TEXT        CHECK (page_hash IS NULL OR page_hash ~ '^[0-9a-f]{8,64}$'),
    offer            JSONB       NOT NULL CHECK (jsonb_typeof(offer) = 'object' AND pg_column_size(offer) <= 16384),
    gates            JSONB       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(gates) = 'object' AND pg_column_size(gates) <= 4096),
    status           TEXT        NOT NULL DEFAULT 'proposed'
                                 CHECK (status IN ('proposed', 'superseded', 'accepted', 'rejected')),
    sightings        INT         NOT NULL DEFAULT 1 CHECK (sightings >= 1),
    first_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at       TIMESTAMPTZ
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_corporate_reextract_proposals_open
    ON corporate_reextract_proposals (candidate_id, proposal_key) WHERE status = 'proposed';
CREATE INDEX IF NOT EXISTS idx_corporate_reextract_proposals_candidate
    ON corporate_reextract_proposals (candidate_id, last_seen_at DESC);

DO $rls$
DECLARE
  t TEXT;
BEGIN
  FOREACH t IN ARRAY ARRAY['corporate_reextract_state', 'corporate_reextract_proposals'] LOOP
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

COMMENT ON TABLE corporate_reextract_state IS
    'Progress of fresh extraction per flagged corporate source (handled request, retry clock). Internal.';
COMMENT ON TABLE corporate_reextract_proposals IS
    'Offers freshly re-extracted from a flagged source. Proposals only; never displayed or published by the job.';
