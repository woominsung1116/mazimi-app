-- A verified official domain can participate in licensed-search link
-- discovery without granting permission for direct page collection. Normalize
-- targets approved before this invariant was added.
UPDATE brand_discovery_targets AS target
SET policy_status = 'search_only',
    policy_notes = COALESCE(
        target.policy_notes,
        'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'
    ),
    updated_at = now()
WHERE target.domain_status = 'verified'
  AND target.policy_status = 'unreviewed'
  AND EXISTS (
      SELECT 1
      FROM benefit_source_domains AS source
      WHERE source.domain = target.official_domain
        AND source.is_active = true
        AND source.verification_status = 'verified'
        AND source.crawl_allowed = false
  );
