-- Evidence outcome `domain_withdrawn`: our own domain policy withdrew the
-- source (rejected, blocked, restricted, or no corporate benefit left on the
-- domain). It used to share `policy_blocked` with the census refusal, which
-- now means only that (corporate_auto::reverify gives each label one class).
--
-- Rollback:
--   DELETE FROM corporate_evidence_checks WHERE outcome = 'domain_withdrawn';
--   then re-run the constraint below without 'domain_withdrawn'.
ALTER TABLE corporate_evidence_checks DROP CONSTRAINT IF EXISTS corporate_evidence_checks_outcome_check;
ALTER TABLE corporate_evidence_checks ADD CONSTRAINT corporate_evidence_checks_outcome_check CHECK (outcome IN (
    'confirmed', 'quote_missing', 'window_changed', 'field_mismatch',
    'no_evidence', 'adoption_expired', 'robots_disallowed', 'blocked',
    'rate_limited', 'gone', 'retired_gone', 'redirected', 'fetch_failed',
    'policy_blocked', 'opted_out', 'unsupported',
    'confirmed_fuzzy', 'relocated', 'update_proposed', 'update_applied', 'ambiguous',
    'url_move_proposed', 'url_adopted', 'domain_withdrawn'));
