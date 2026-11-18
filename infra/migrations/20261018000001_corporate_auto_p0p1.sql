-- Corporate benefits without humans in the loop, phases P0 and P1
-- (design: corporate-autonomous.md §5.1, §6).
--
-- P0 census: a read-only policy dry run over every source domain, candidate
-- and monitor. Its results live only in corporate_census_* and never change
-- crawl permissions or publication.
-- P1 evidence re-verification: a published corporate fact stays public while
-- its verbatim evidence quote is still on the live first-party page and the
-- deterministic parser reads the same value. Monitors opt in through
-- verification_mode = 'evidence_auto'; every other monitor keeps the
-- existing hash-and-hold behaviour ('human').
--
-- Everything here is additive. No existing row changes meaning: the new
-- monitor column defaults to 'human', the new domain switches are only read
-- by the new code paths, and every new code path is behind an environment
-- flag that defaults to off.
--
-- Rollback (do not run automatically; stop the worker flags first):
--   DROP TABLE IF EXISTS corporate_evidence_checks, corporate_evidence_targets,
--       corporate_offer_facts, corporate_census_entries, corporate_census_runs,
--       corporate_pipeline_daily, benefit_source_optouts;
--   UPDATE program_source_holds SET hold_reason = 'content_changed'
--       WHERE hold_reason = 'evidence_failed';
--   ALTER TABLE program_source_holds DROP CONSTRAINT program_source_holds_hold_reason_check;
--   ALTER TABLE program_source_holds ADD CONSTRAINT program_source_holds_hold_reason_check
--     CHECK (hold_reason IN ('content_changed','source_unreachable','verification_overdue',
--                            'access_revoked','redirected','not_found'));
--   ALTER TABLE program_source_monitors DROP COLUMN verification_mode, DROP COLUMN evidence_set_hash;
--   ALTER TABLE benefit_source_domains DROP COLUMN gray_generic_tos,
--       DROP COLUMN gray_robots_missing, DROP COLUMN gray_crawl_delay_min,
--       DROP COLUMN max_pages_per_day;

-- ── Per-domain GRAY switches (design §2.2). The matching global flag must
-- also be on; a global flag that is off overrides every domain switch.
ALTER TABLE benefit_source_domains
    ADD COLUMN IF NOT EXISTS gray_generic_tos BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS gray_robots_missing BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS gray_crawl_delay_min BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS max_pages_per_day INT NOT NULL DEFAULT 200;
ALTER TABLE benefit_source_domains DROP CONSTRAINT IF EXISTS benefit_source_domains_max_pages_check;
ALTER TABLE benefit_source_domains ADD CONSTRAINT benefit_source_domains_max_pages_check
    CHECK (max_pages_per_day BETWEEN 1 AND 200);

-- ── Monitor verification mode.
ALTER TABLE program_source_monitors
    ADD COLUMN IF NOT EXISTS verification_mode TEXT NOT NULL DEFAULT 'human',
    ADD COLUMN IF NOT EXISTS evidence_set_hash TEXT;
ALTER TABLE program_source_monitors DROP CONSTRAINT IF EXISTS program_source_monitors_verification_mode_check;
ALTER TABLE program_source_monitors ADD CONSTRAINT program_source_monitors_verification_mode_check
    CHECK (verification_mode IN ('human', 'evidence_auto'));
ALTER TABLE program_source_monitors DROP CONSTRAINT IF EXISTS program_source_monitors_evidence_set_hash_check;
ALTER TABLE program_source_monitors ADD CONSTRAINT program_source_monitors_evidence_set_hash_check
    CHECK (evidence_set_hash IS NULL OR evidence_set_hash ~ '^[0-9a-f]{64}$');

-- An evidence failure is its own hold reason so health metrics can tell it
-- from a hash change.
ALTER TABLE program_source_holds DROP CONSTRAINT IF EXISTS program_source_holds_hold_reason_check;
ALTER TABLE program_source_holds ADD CONSTRAINT program_source_holds_hold_reason_check CHECK (
    hold_reason IN (
        'content_changed', 'source_unreachable', 'verification_overdue',
        'access_revoked', 'redirected', 'not_found', 'evidence_failed'
    )
);

-- ── Takedown / opt-out (design §2.4). The contact is stored only as a hash.
CREATE TABLE IF NOT EXISTS benefit_source_optouts (
    id            UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    domain        TEXT        CHECK (domain IS NULL OR domain ~ '^[a-z0-9.-]{1,253}$'),
    url_prefix    TEXT        CHECK (url_prefix IS NULL OR (url_prefix ~ '^https://' AND char_length(url_prefix) <= 2000)),
    scope         TEXT        NOT NULL CHECK (scope IN ('collection', 'display', 'both')),
    channel       TEXT        NOT NULL CHECK (channel IN ('web_form', 'http_410', 'robots_change', 'contact_reply')),
    contact_hash  TEXT        CHECK (contact_hash IS NULL OR contact_hash ~ '^[0-9a-f]{64}$'),
    received_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    effective_at  TIMESTAMPTZ,
    programs_withdrawn INT    NOT NULL DEFAULT 0 CHECK (programs_withdrawn >= 0),
    lifted_at     TIMESTAMPTZ,
    lift_proof    TEXT        CHECK (lift_proof IS NULL OR char_length(lift_proof) <= 500),
    CHECK (domain IS NOT NULL OR url_prefix IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS idx_benefit_source_optouts_active
    ON benefit_source_optouts (domain) WHERE lifted_at IS NULL;

-- ── P0 census.
CREATE TABLE IF NOT EXISTS corporate_census_runs (
    id           UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at  TIMESTAMPTZ,
    status       TEXT        NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'completed', 'failed')),
    settings     JSONB       NOT NULL DEFAULT '{}'::jsonb,
    summary      JSONB       NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX IF NOT EXISTS idx_corporate_census_runs_completed
    ON corporate_census_runs (finished_at DESC) WHERE status = 'completed';

CREATE TABLE IF NOT EXISTS corporate_census_entries (
    id                      UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    run_id                  UUID        NOT NULL REFERENCES corporate_census_runs(id) ON DELETE CASCADE,
    subject_kind            TEXT        NOT NULL CHECK (subject_kind IN ('domain', 'candidate', 'monitor')),
    domain_id               UUID        REFERENCES benefit_source_domains(id) ON DELETE CASCADE,
    candidate_id            UUID        REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    program_id              UUID        REFERENCES programs(id) ON DELETE CASCADE,
    url                     TEXT        CHECK (url IS NULL OR char_length(url) <= 2000),
    host                    TEXT        CHECK (host IS NULL OR char_length(host) <= 253),
    lane                    TEXT        NOT NULL CHECK (lane IN ('direct', 'render', 'secondary', 'snippet', 'feed', 'impossible')),
    reason                  TEXT        NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 100),
    probed                  BOOLEAN     NOT NULL DEFAULT false,
    robots_state            TEXT        NOT NULL CHECK (char_length(robots_state) BETWEEN 1 AND 40),
    path_allowed            BOOLEAN,
    search_engine_allowlist BOOLEAN     NOT NULL DEFAULT false,
    crawl_delay_ms          INT         CHECK (crawl_delay_ms IS NULL OR crawl_delay_ms >= 0),
    tos_signal              TEXT        CHECK (tos_signal IS NULL OR tos_signal IN (
                                'none', 'login_only', 'generic_automated', 'specific_collection',
                                'unfetchable', 'not_found', 'unknown')),
    http_status             INT         CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599),
    text_chars              INT         CHECK (text_chars IS NULL OR text_chars >= 0),
    -- Shadow extraction and audit comparison. Short (<=100 char) quotes only;
    -- never page text. Cleared on opt-out.
    shadow                  JSONB,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_corporate_census_entries_run ON corporate_census_entries (run_id, subject_kind);
CREATE INDEX IF NOT EXISTS idx_corporate_census_entries_candidate ON corporate_census_entries (candidate_id, created_at DESC);

-- ── P1 evidence facts. One row per verbatim evidence fragment. Full page
-- text is never stored, only the quote (<=300 chars, internal, never shown),
-- the parser reading of the quote and a value signature of the neighbourhood
-- around the quote recorded at the first live confirmation.
CREATE TABLE IF NOT EXISTS corporate_offer_facts (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    program_id          UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    fact_type           TEXT        NOT NULL CHECK (fact_type IN ('presence', 'age')),
    value               JSONB       NOT NULL DEFAULT '{}'::jsonb,
    quote               TEXT        NOT NULL CHECK (char_length(quote) BETWEEN 2 AND 300),
    quote_key           TEXT        NOT NULL CHECK (quote_key ~ '^[0-9a-f]{64}$'),
    channel             TEXT        NOT NULL DEFAULT 'page' CHECK (channel IN ('page', 'render', 'snippet', 'app_store', 'feed')),
    evidence_source     TEXT        NOT NULL CHECK (char_length(evidence_source) BETWEEN 1 AND 100),
    pass_b              JSONB       NOT NULL DEFAULT '{}'::jsonb,
    status              TEXT        NOT NULL DEFAULT 'published' CHECK (status IN (
                            'observed', 'stable', 'published', 'contradicted', 'hidden', 'superseded', 'retired')),
    window_signature    TEXT        CHECK (window_signature IS NULL OR char_length(window_signature) <= 2000),
    adopt_before        TIMESTAMPTZ,
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_confirmed_at   TIMESTAMPTZ,
    last_checked_at     TIMESTAMPTZ,
    last_outcome        TEXT        CHECK (last_outcome IS NULL OR char_length(last_outcome) <= 60),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (program_id, quote_key)
);
CREATE INDEX IF NOT EXISTS idx_corporate_offer_facts_program ON corporate_offer_facts (program_id) WHERE status <> 'retired';

-- Evidence-job schedule per source URL. Kept apart from
-- benefit_source_candidates so a shadow run never moves the human schedule.
CREATE TABLE IF NOT EXISTS corporate_evidence_targets (
    candidate_id         UUID        PRIMARY KEY REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    next_check_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_checked_at      TIMESTAMPTZ,
    last_success_at      TIMESTAMPTZ,
    last_outcome         TEXT        CHECK (last_outcome IS NULL OR char_length(last_outcome) <= 60),
    consecutive_failures INT         NOT NULL DEFAULT 0 CHECK (consecutive_failures BETWEEN 0 AND 100),
    first_gone_at        TIMESTAMPTZ,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_corporate_evidence_targets_due ON corporate_evidence_targets (next_check_at);

-- Every evidence decision, shadow or live. This is what the P1 gate reads.
CREATE TABLE IF NOT EXISTS corporate_evidence_checks (
    id            UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    checked_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    candidate_id  UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    program_id    UUID        REFERENCES programs(id) ON DELETE CASCADE,
    mode          TEXT        NOT NULL CHECK (mode IN ('shadow', 'live')),
    outcome       TEXT        NOT NULL CHECK (outcome IN (
                      'confirmed', 'quote_missing', 'window_changed', 'field_mismatch',
                      'no_evidence', 'adoption_expired', 'robots_disallowed', 'blocked',
                      'rate_limited', 'gone', 'retired_gone', 'redirected', 'fetch_failed',
                      'policy_blocked', 'opted_out', 'unsupported')),
    applied       BOOLEAN     NOT NULL DEFAULT false,
    http_status   INT         CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599),
    details       JSONB       NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX IF NOT EXISTS idx_corporate_evidence_checks_time ON corporate_evidence_checks (checked_at DESC);
CREATE INDEX IF NOT EXISTS idx_corporate_evidence_checks_program ON corporate_evidence_checks (program_id, checked_at DESC);

-- ── Self-reported health: one row per KST day.
CREATE TABLE IF NOT EXISTS corporate_pipeline_daily (
    day         DATE        PRIMARY KEY,
    metrics     JSONB       NOT NULL DEFAULT '{}'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Access control: service role only on every new table.
DO $rls$
DECLARE t TEXT;
BEGIN
  FOREACH t IN ARRAY ARRAY[
      'benefit_source_optouts', 'corporate_census_runs', 'corporate_census_entries',
      'corporate_offer_facts', 'corporate_evidence_targets', 'corporate_evidence_checks',
      'corporate_pipeline_daily'
  ] LOOP
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
    EXECUTE format(
      'CREATE POLICY %I ON %I FOR ALL USING (is_service_role()) WITH CHECK (is_service_role())',
      t || '_all_service', t);
  END LOOP;
END
$rls$;

COMMENT ON TABLE corporate_offer_facts IS
    'Verbatim evidence for published corporate facts (P1). Quotes are internal, <=300 chars, never displayed; full page text is never stored.';
COMMENT ON TABLE corporate_evidence_checks IS
    'Every evidence re-verification decision (shadow or live). Read by the P1 gate and the daily health row.';
COMMENT ON TABLE corporate_census_entries IS
    'P0 read-only policy census: robots per path for MazimiBenefitBot, ToS signal, lane. Never grants crawl permission.';
