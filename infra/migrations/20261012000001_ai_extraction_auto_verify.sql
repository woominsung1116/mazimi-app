-- A-09 auto-verify: a candidate fact that a deterministic rule parser reads
-- identically from the same evidence quote may be applied without a human,
-- through the same transactional apply path as an admin approval, with
-- provenance (rule, parser version, run) and a one-shot revert. Humans
-- review only the exceptions and a random audit sample.
--
-- Additive only. Rollback (do not run automatically): set
-- EXTRACTION_AUTO_APPLY_ENABLED=false, revert every auto-applied run through
-- POST /api/v1/admin/ai-extraction/revert, revert the API/worker consumers,
-- then
--   DROP TRIGGER programs_enqueue_ai_extraction_insert ON programs;
--   DROP TRIGGER programs_enqueue_ai_extraction_update ON programs;
--   DROP FUNCTION enqueue_ai_extraction();
--   DROP TABLE program_ai_extraction_queue;
--   ALTER TABLE program_ai_extraction_candidates DROP COLUMN auto_check, ...;
-- and restore the two CHECK constraints below to their previous lists.
--
-- The queue trigger runs as the invoking role (SECURITY INVOKER) and the
-- queue has FORCE RLS with the service-only policy: only the service role
-- may write `programs`, or the insert fails and aborts the write. Keep
-- programs writes service-only (20260322000001_rls_policies.sql).

-- ── Candidates: cross-check result, provenance, audit, revert ────────────
ALTER TABLE program_ai_extraction_candidates
    -- The rule parser's independent reading of the evidence quote:
    -- {verdict agree|disagree|no_parse, parser_version, rule, parsed, reason}
    -- plus `hold` when an agreeing fact was not auto-applied.
    ADD COLUMN auto_check JSONB CHECK (auto_check IS NULL OR jsonb_typeof(auto_check) = 'object'),
    ADD COLUMN applied_by TEXT CHECK (applied_by IS NULL OR applied_by IN ('reviewer', 'auto_verified')),
    ADD COLUMN auto_rule TEXT CHECK (auto_rule IS NULL OR char_length(auto_rule) BETWEEN 1 AND 100),
    ADD COLUMN auto_parser_version TEXT CHECK (auto_parser_version IS NULL OR char_length(auto_parser_version) BETWEEN 1 AND 100),
    ADD COLUMN auto_run_id UUID REFERENCES program_ai_extraction_runs(id) ON DELETE SET NULL,
    ADD COLUMN audit_sample BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN audit_reviewed_at TIMESTAMPTZ,
    ADD COLUMN audit_reviewer_id UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN audit_note TEXT CHECK (audit_note IS NULL OR char_length(audit_note) BETWEEN 1 AND 1000),
    ADD COLUMN reverted_at TIMESTAMPTZ,
    ADD COLUMN reverted_by UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN revert_note TEXT CHECK (revert_note IS NULL OR char_length(revert_note) BETWEEN 1 AND 1000),
    ADD COLUMN revert_report JSONB CHECK (revert_report IS NULL OR jsonb_typeof(revert_report) = 'object');

UPDATE program_ai_extraction_candidates SET applied_by = 'reviewer' WHERE status = 'approved';

ALTER TABLE program_ai_extraction_candidates
    DROP CONSTRAINT program_ai_extraction_candidates_status_check,
    ADD CONSTRAINT program_ai_extraction_candidates_status_check CHECK (status IN (
        'pending', 'flagged', 'invalid', 'approved', 'rejected', 'superseded', 'reverted'
    )),
    -- An automatic application has no human reviewer but always a rule and
    -- a parser version. (NULL applied_by on an approved row = reviewer.)
    ADD CONSTRAINT program_ai_extraction_candidates_auto_provenance_check CHECK (
        applied_by IS DISTINCT FROM 'auto_verified'
        OR (reviewer_id IS NULL AND auto_rule IS NOT NULL AND auto_parser_version IS NOT NULL)
    ),
    ADD CONSTRAINT program_ai_extraction_candidates_revert_check CHECK (
        status <> 'reverted' OR (reverted_at IS NOT NULL AND revert_note IS NOT NULL)
    ),
    ADD CONSTRAINT program_ai_extraction_candidates_audit_check CHECK (
        audit_reviewed_at IS NULL OR (audit_sample AND audit_note IS NOT NULL)
    );

-- Revert by run or parser version, and the audit queue.
CREATE INDEX idx_program_ai_extraction_candidates_auto_run
    ON program_ai_extraction_candidates (auto_run_id)
    WHERE applied_by = 'auto_verified';
CREATE INDEX idx_program_ai_extraction_candidates_audit
    ON program_ai_extraction_candidates (reviewed_at)
    WHERE applied_by = 'auto_verified' AND audit_sample AND audit_reviewed_at IS NULL;

-- ── Runs: auto-apply telemetry ────────────────────────────────────────────
ALTER TABLE program_ai_extraction_runs
    ADD COLUMN trigger TEXT NOT NULL DEFAULT 'nightly' CHECK (trigger IN ('nightly', 'queue', 'manual')),
    ADD COLUMN auto_apply_enabled BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN auto_verified INTEGER NOT NULL DEFAULT 0 CHECK (auto_verified >= 0),
    ADD COLUMN auto_applied INTEGER NOT NULL DEFAULT 0 CHECK (auto_applied >= 0),
    ADD COLUMN auto_held INTEGER NOT NULL DEFAULT 0 CHECK (auto_held >= 0),
    ADD COLUMN auto_cap_skips INTEGER NOT NULL DEFAULT 0 CHECK (auto_cap_skips >= 0),
    ADD COLUMN audit_sampled INTEGER NOT NULL DEFAULT 0 CHECK (audit_sampled >= 0),
    ADD COLUMN queue_drained INTEGER NOT NULL DEFAULT 0 CHECK (queue_drained >= 0);

-- ── Documents written by auto-apply ──────────────────────────────────────
ALTER TABLE program_documents
    DROP CONSTRAINT program_documents_source_check,
    ADD CONSTRAINT program_documents_source_check
        CHECK (source IS NULL OR source IN ('ai_extraction_approved', 'ai_extraction_auto'));

-- ── Ingestion trigger: changed program text is queued for extraction ─────
-- One row per program. The worker computes the real evidence-source hash and
-- skips an unchanged source through the response cache, so a column change
-- that does not alter the masked text costs no model call.
CREATE TABLE program_ai_extraction_queue (
    program_id   UUID        PRIMARY KEY REFERENCES programs(id) ON DELETE CASCADE,
    reason       TEXT        NOT NULL CHECK (reason IN ('created', 'source_changed')),
    enqueued_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_program_ai_extraction_queue_enqueued
    ON program_ai_extraction_queue (enqueued_at);

CREATE FUNCTION enqueue_ai_extraction() RETURNS TRIGGER
    LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    IF NEW.is_active AND NEW.program_type NOT IN ('corporate_benefit', 'financial_product') THEN
        INSERT INTO program_ai_extraction_queue (program_id, reason)
        VALUES (NEW.id, CASE WHEN TG_OP = 'INSERT' THEN 'created' ELSE 'source_changed' END)
        ON CONFLICT (program_id) DO UPDATE
            SET enqueued_at = now(), reason = EXCLUDED.reason;
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER programs_enqueue_ai_extraction_insert
    AFTER INSERT ON programs
    FOR EACH ROW EXECUTE FUNCTION enqueue_ai_extraction();

-- Only the columns the evidence source is built from
-- (`job::PROGRAM_TEXT_COLUMNS`); a sync that rewrites identical values
-- enqueues nothing.
CREATE TRIGGER programs_enqueue_ai_extraction_update
    AFTER UPDATE OF title, summary, application_method, submission_documents,
        screening_method, raw_payload, is_active ON programs
    FOR EACH ROW
    WHEN (OLD.title IS DISTINCT FROM NEW.title
       OR OLD.summary IS DISTINCT FROM NEW.summary
       OR OLD.application_method IS DISTINCT FROM NEW.application_method
       OR OLD.submission_documents IS DISTINCT FROM NEW.submission_documents
       OR OLD.screening_method IS DISTINCT FROM NEW.screening_method
       OR OLD.raw_payload IS DISTINCT FROM NEW.raw_payload
       OR (NEW.is_active AND NOT OLD.is_active))
    EXECUTE FUNCTION enqueue_ai_extraction();

ALTER TABLE program_ai_extraction_queue ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_ai_extraction_queue FORCE ROW LEVEL SECURITY;
CREATE POLICY program_ai_extraction_queue_service ON program_ai_extraction_queue
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

DO $access$
DECLARE
  client_role text;
BEGIN
  EXECUTE 'REVOKE ALL ON TABLE public.program_ai_extraction_queue FROM PUBLIC';
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON TABLE public.program_ai_extraction_queue FROM %I', client_role);
    END IF;
  END LOOP;
END $access$;


COMMENT ON TABLE program_ai_extraction_queue IS
    'Programs whose source text changed since the last extraction; drained with priority by the AI extraction worker.';
COMMENT ON COLUMN program_ai_extraction_candidates.applied_by IS
    'reviewer = admin approval; auto_verified = rule parser read the same value from the same quote (auto_rule, auto_parser_version, auto_run_id).';
