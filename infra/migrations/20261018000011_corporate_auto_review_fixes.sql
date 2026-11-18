-- Review fixes for the corporate automation (P0/P1) before any flag is on.
-- 20261018000001/02 are already applied with every flag off, so their
-- contents stay unchanged and every follow-up lives here. Additive only.
--
-- - Opt-out: the immediate effect is a reversible hold, retirement waits for
--   proof of domain control (the issued token served under /.well-known/);
--   the row keeps the matched source domains and what it changed so lifting
--   can restore it. compliance_status is never touched by an opt-out.
-- - Evidence keeps its own page hash; the hash path's verified hash is
--   never overwritten, so falling back to shadow does not hold everything.
-- - Shadow mode records its own neighbourhood baseline so the P1 gate can
--   measure window changes before live.
--
-- Rollback (do not run automatically; flags off first):
--   UPDATE program_source_holds SET hold_reason = 'access_revoked' WHERE hold_reason = 'publisher_optout';
--   ALTER TABLE program_source_holds DROP CONSTRAINT program_source_holds_hold_reason_check;
--   ALTER TABLE program_source_holds ADD CONSTRAINT program_source_holds_hold_reason_check CHECK (
--     hold_reason IN ('content_changed','source_unreachable','verification_overdue','access_revoked',
--                     'redirected','not_found','evidence_failed'));
--   ALTER TABLE program_source_monitors DROP COLUMN evidence_page_hash, DROP COLUMN evidence_verified_at;
--   ALTER TABLE corporate_offer_facts DROP COLUMN shadow_window_signature, DROP COLUMN shadow_baseline_at;
--   ALTER TABLE benefit_source_optouts DROP COLUMN domain_ids, DROP COLUMN previous_state,
--     DROP COLUMN verification_token_hash, DROP COLUMN verified_at, DROP COLUMN retired_at,
--     DROP COLUMN proof_checked_at;

ALTER TABLE benefit_source_optouts
    ADD COLUMN IF NOT EXISTS domain_ids UUID[] NOT NULL DEFAULT ARRAY[]::UUID[],
    ADD COLUMN IF NOT EXISTS previous_state JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS verification_token_hash TEXT,
    ADD COLUMN IF NOT EXISTS verified_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS retired_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS proof_checked_at TIMESTAMPTZ;
ALTER TABLE benefit_source_optouts DROP CONSTRAINT IF EXISTS benefit_source_optouts_token_hash_check;
ALTER TABLE benefit_source_optouts ADD CONSTRAINT benefit_source_optouts_token_hash_check
    CHECK (verification_token_hash IS NULL OR verification_token_hash ~ '^[0-9a-f]{64}$');
CREATE INDEX IF NOT EXISTS idx_benefit_source_optouts_open
    ON benefit_source_optouts (proof_checked_at NULLS FIRST, received_at) WHERE lifted_at IS NULL;

ALTER TABLE program_source_holds DROP CONSTRAINT IF EXISTS program_source_holds_hold_reason_check;
ALTER TABLE program_source_holds ADD CONSTRAINT program_source_holds_hold_reason_check CHECK (
    hold_reason IN (
        'content_changed', 'source_unreachable', 'verification_overdue',
        'access_revoked', 'redirected', 'not_found', 'evidence_failed',
        'publisher_optout'
    )
);

ALTER TABLE program_source_monitors
    ADD COLUMN IF NOT EXISTS evidence_page_hash TEXT,
    ADD COLUMN IF NOT EXISTS evidence_verified_at TIMESTAMPTZ;
ALTER TABLE program_source_monitors DROP CONSTRAINT IF EXISTS program_source_monitors_evidence_page_hash_check;
ALTER TABLE program_source_monitors ADD CONSTRAINT program_source_monitors_evidence_page_hash_check
    CHECK (evidence_page_hash IS NULL OR evidence_page_hash ~ '^[0-9a-f]{32}$');

ALTER TABLE corporate_offer_facts
    ADD COLUMN IF NOT EXISTS shadow_window_signature TEXT,
    ADD COLUMN IF NOT EXISTS shadow_baseline_at TIMESTAMPTZ;
ALTER TABLE corporate_offer_facts DROP CONSTRAINT IF EXISTS corporate_offer_facts_shadow_window_check;
ALTER TABLE corporate_offer_facts ADD CONSTRAINT corporate_offer_facts_shadow_window_check
    CHECK (shadow_window_signature IS NULL OR char_length(shadow_window_signature) <= 2000);
