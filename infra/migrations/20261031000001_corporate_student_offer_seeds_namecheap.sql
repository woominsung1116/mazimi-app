-- Official student-offer pages queued for the automated corporate pipeline
-- (2026-10-31, corp-expand). One list, read by every statement below.
--
-- Each page passed the whole intake chain in a local projection on
-- 2026-10-31 (policy, robots for MazimiBenefitBot, reader A, reader B,
-- scope, display; ending at `awaiting_stability`, the second sighting a
-- day later). Thirty other official student pages were projected and are
-- not listed here: reader A, the ending scan, a bot wall, robots or a
-- duplicate host refused them (see the corp-expand report).
--   nc.me   robots: `User-agent: *` disallows /admin/, /login/, /password/
--           only; `/` is 200 HTML: "Namecheap is giving university students
--           a free bundle to kickstart their online presence."
--
-- What this registers, as 20261028000201 does: a brand, a discovery target
-- with its official domain and seed URL, and a source domain row for
-- brand<->domain identity (crawl_allowed=false, compliance 'pending',
-- collection 'manual_review': no crawl approval). The official-seed
-- discoverer turns the seed into a `review` candidate; intake and the
-- evidence job decide the rest. No programme, monitor, fact, quote or
-- summary: nothing here is shown to users. Existing rows are never
-- overwritten.
--
-- The seed list is a TEMP table dropped at commit: it relies on the migration
-- running in one transaction (sqlx's default). Run by hand, wrap it in
-- BEGIN … COMMIT.
--
-- Rollback: DELETE the candidates whose canonical_url is a seed below and
-- that have no monitor, then these domain rows with no other candidate,
-- then the targets and brands with source_type 'coverage_seed' and the
-- target keys below. No user data.

CREATE TEMP TABLE corporate_seed_20261031 (
    target_key        TEXT PRIMARY KEY,
    display_name      TEXT NOT NULL,
    coverage_category TEXT NOT NULL,
    official_domain   TEXT NOT NULL,
    homepage_url      TEXT NOT NULL,
    seed_url          TEXT NOT NULL
) ON COMMIT DROP;

INSERT INTO corporate_seed_20261031 VALUES
    ('namecheap_students', 'Namecheap', '전자·소프트웨어', 'nc.me', 'https://nc.me/', 'https://nc.me/');

INSERT INTO brand_discovery_targets (target_key, display_name, coverage_category, discovery_priority)
SELECT target_key, display_name, coverage_category, 70 FROM corporate_seed_20261031
ON CONFLICT (target_key) DO NOTHING;

INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
SELECT s.display_name, lower(regexp_replace(s.display_name, '[^0-9A-Za-z가-힣]', '', 'g')),
       'coverage_seed', s.target_key, 70
FROM corporate_seed_20261031 s
WHERE NOT EXISTS (SELECT 1 FROM brands b
                  WHERE b.normalized_name = lower(regexp_replace(s.display_name, '[^0-9A-Za-z가-힣]', '', 'g')))
ON CONFLICT (source_type, source_id) DO NOTHING;

UPDATE brand_discovery_targets t
SET brand_id = (SELECT b.id FROM brands b
                WHERE b.normalized_name = lower(regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g'))
                ORDER BY b.discovery_priority DESC, b.created_at LIMIT 1),
    updated_at = now()
FROM corporate_seed_20261031 s
WHERE t.target_key = s.target_key AND t.brand_id IS NULL;

UPDATE brand_discovery_targets t
SET official_domain = s.official_domain,
    official_homepage_url = COALESCE(t.official_homepage_url, s.homepage_url),
    domain_status = 'verified',
    seed_urls = CASE WHEN s.seed_url = ANY (t.seed_urls) THEN t.seed_urls
                     ELSE t.seed_urls || s.seed_url END,
    policy_notes = COALESCE(t.policy_notes,
        'Student offer page projected through intake 2026-10-31: robots allows the path for MazimiBenefitBot, no bot wall. Queued as a review candidate only.'),
    updated_at = now()
FROM corporate_seed_20261031 s
WHERE t.target_key = s.target_key
  AND t.is_active
  AND t.policy_status <> 'blocked'
  AND t.domain_status IN ('unresolved', 'candidate')
  AND t.brand_id IS NOT NULL;

INSERT INTO benefit_source_domains (
    brand_id, organization_name, domain, source_role,
    crawl_allowed, verification_status, verification_reference, verified_at,
    compliance_status, collection_mode, policy_notes, is_active
)
SELECT t.brand_id, t.display_name, t.official_domain, 'brand_official',
       false, 'verified', t.official_homepage_url, now(),
       'pending', 'manual_review',
       'Official brand domain for a student-offer seed (2026-10-31). No crawl approval; the automated policy gates decide collection.',
       true
FROM brand_discovery_targets t
JOIN corporate_seed_20261031 s ON s.target_key = t.target_key
WHERE t.domain_status = 'verified' AND t.official_domain IS NOT NULL AND t.brand_id IS NOT NULL
ON CONFLICT (domain) DO NOTHING;

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT d.id, t.brand_id
FROM brand_discovery_targets t
JOIN corporate_seed_20261031 s ON s.target_key = t.target_key
JOIN benefit_source_domains d ON d.domain = t.official_domain
WHERE t.brand_id IS NOT NULL AND d.verification_status = 'verified' AND d.is_active
ON CONFLICT (domain_id, brand_id) DO NOTHING;
