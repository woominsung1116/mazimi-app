-- Curated official-domain baseline from the 2026-07-16 manual review.
--
-- Domain verification permits licensed-search link discovery only. It never
-- grants direct page access: every row is inserted with crawl_allowed=false
-- and compliance_status=pending. Existing rejected domains, blocked targets,
-- and operator-approved crawl settings are deliberately not overwritten.

WITH audited(
    target_key, official_domain, homepage_url, seed_urls,
    verification_reference, collection_mode, policy_notes
) AS (
    VALUES
      ('adobe', 'www.adobe.com', 'https://www.adobe.com/kr/', ARRAY[]::TEXT[], 'https://www.adobe.com/kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('apple', 'www.apple.com', 'https://www.apple.com/kr/', ARRAY[]::TEXT[], 'https://www.apple.com/kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('apple_music', 'www.apple.com', 'https://www.apple.com/kr/apple-music/', ARRAY[]::TEXT[], 'https://www.apple.com/kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('baemin', 'www.baemin.com', 'https://www.baemin.com/', ARRAY[]::TEXT[], 'https://www.baemin.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('cgv', 'cgv.co.kr', 'https://cgv.co.kr/', ARRAY['https://cgv.co.kr/evt/eventDetail?evntNo=1071']::TEXT[], 'https://cgv.co.kr/', 'licensed_search', 'Generic-bot disallow rule observed. Search/manual review only.'),
      ('coupang', 'www.coupang.com', 'https://www.coupang.com/', ARRAY[]::TEXT[], 'https://www.coupang.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('cu', 'cu.bgfretail.com', 'https://cu.bgfretail.com/', ARRAY[]::TEXT[], 'https://cu.bgfretail.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('daiso', 'www.daisomall.co.kr', 'https://www.daisomall.co.kr/', ARRAY[]::TEXT[], 'https://www.daisomall.co.kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('everland', 'www.everland.com', 'https://www.everland.com/', ARRAY[]::TEXT[], 'https://www.everland.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('github', 'github.com', 'https://github.com/', ARRAY[]::TEXT[], 'https://github.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('gs25', 'gs25.gsretail.com', 'https://gs25.gsretail.com/', ARRAY[]::TEXT[], 'https://gs25.gsretail.com/gscvs/ko/store-services/locations', 'manual_review', 'Official domain verified; direct collection remains disabled pending separate compliance approval.'),
      ('kakao_bank', 'www.kakaobank.com', 'https://www.kakaobank.com/', ARRAY[]::TEXT[], 'https://www.kakaobank.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('kb_bank', 'www.kbstar.com', 'https://www.kbstar.com/', ARRAY[]::TEXT[], 'https://www.kbstar.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('kbcard', 'card.kbcard.com', 'https://card.kbcard.com/', ARRAY[]::TEXT[], 'https://card.kbcard.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('korail', 'www.korail.com', 'https://www.korail.com/', ARRAY[]::TEXT[], 'https://www.korail.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('kt', 'www.kt.com', 'https://www.kt.com/', ARRAY[]::TEXT[], 'https://www.kt.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('lguplus', 'www.lguplus.com', 'https://www.lguplus.com/', ARRAY[]::TEXT[], 'https://www.lguplus.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('lottecinema', 'www.lottecinema.co.kr', 'https://www.lottecinema.co.kr/NLCHS', ARRAY['https://www.lottecinema.co.kr/NLCHS/Event']::TEXT[], 'https://www.lottecinema.co.kr/NLCHS', 'licensed_search', 'robots allows indexing but service terms restrict commercial activity; separate legal approval required.'),
      ('lotteworld', 'adventure.lotteworld.com', 'https://adventure.lotteworld.com/', ARRAY[]::TEXT[], 'https://adventure.lotteworld.com/price/member/prefer', 'manual_review', 'Official domain verified; direct collection remains disabled pending separate compliance approval.'),
      ('megabox', 'www.megabox.co.kr', 'https://www.megabox.co.kr/', ARRAY[]::TEXT[], 'https://www.megabox.co.kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('microsoft', 'www.microsoft.com', 'https://www.microsoft.com/', ARRAY[]::TEXT[], 'https://www.microsoft.com/ko-kr/download', 'manual_review', 'Official domain verified; direct collection remains disabled pending separate compliance approval.'),
      ('musinsa', 'www.musinsa.com', 'https://www.musinsa.com/', ARRAY[]::TEXT[], 'https://www.musinsa.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('notion', 'www.notion.com', 'https://www.notion.com/ko', ARRAY[]::TEXT[], 'https://www.notion.com/ko', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('oliveyoung', 'www.oliveyoung.co.kr', 'https://www.oliveyoung.co.kr/store/main/main.do', ARRAY['https://www.oliveyoung.co.kr/store/main/getEventList.do']::TEXT[], 'https://www.oliveyoung.co.kr/store/main/main.do', 'licensed_search', 'Bot challenge observed; terms restrict commercial reuse. Search/manual review only until permission.'),
      ('samsung_electronics', 'www.samsung.com', 'https://www.samsung.com/', ARRAY[]::TEXT[], 'https://www.samsung.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('shinhan_bank', 'bank.shinhan.com', 'https://bank.shinhan.com/', ARRAY[]::TEXT[], 'https://bank.shinhan.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('shinhancard', 'www.shinhancard.com', 'https://www.shinhancard.com/', ARRAY[]::TEXT[], 'https://www.shinhancard.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('skt', 'www.tworld.co.kr', 'https://www.tworld.co.kr/', ARRAY[]::TEXT[], 'https://www.tworld.co.kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('spotify', 'www.spotify.com', 'https://www.spotify.com/kr-ko/premium/', ARRAY[]::TEXT[], 'https://www.spotify.com/kr-ko/premium/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('srt', 'etk.srail.kr', 'https://etk.srail.kr/', ARRAY[]::TEXT[], 'https://etk.srail.kr/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'),
      ('starbucks', 'www.starbucks.co.kr', 'https://www.starbucks.co.kr/', ARRAY[]::TEXT[], 'https://www.starbucks.co.kr/', 'licensed_search', 'robots could not be reliably verified; search/manual review only.'),
      ('tving', 'www.tving.com', 'https://www.tving.com/', ARRAY[]::TEXT[], 'https://www.tving.com/', 'licensed_search', 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.')
)
UPDATE brand_discovery_targets AS target
SET official_domain = audited.official_domain,
    official_homepage_url = CASE
        WHEN target.domain_status = 'unresolved' THEN audited.homepage_url
        ELSE COALESCE(target.official_homepage_url, audited.homepage_url)
    END,
    domain_status = 'verified',
    preferred_collection_modes = CASE
        WHEN target.domain_status = 'unresolved'
        THEN ARRAY['licensed_search', 'manual_review']::TEXT[]
        ELSE target.preferred_collection_modes
    END,
    seed_urls = CASE
        WHEN cardinality(target.seed_urls) = 0 THEN audited.seed_urls
        ELSE target.seed_urls
    END,
    policy_status = 'search_only',
    policy_notes = COALESCE(target.policy_notes, audited.policy_notes),
    next_search_at = COALESCE(target.next_search_at, now()),
    last_error = NULL,
    updated_at = now()
FROM audited
WHERE target.target_key = audited.target_key
  AND target.policy_status <> 'blocked'
  AND (
      target.domain_status = 'unresolved'
      OR target.official_domain = audited.official_domain
  )
  AND NOT EXISTS (
      SELECT 1
      FROM benefit_source_domains AS rejected
      WHERE rejected.domain = audited.official_domain
        AND rejected.verification_status = 'rejected'
  );

UPDATE brands AS brand
SET homepage_url = COALESCE(brand.homepage_url, target.official_homepage_url),
    discovery_priority = GREATEST(brand.discovery_priority, target.discovery_priority),
    updated_at = now()
FROM brand_discovery_targets AS target
WHERE target.brand_id = brand.id
  AND target.domain_status = 'verified'
  AND target.official_homepage_url IS NOT NULL;

INSERT INTO benefit_source_domains (
    brand_id, organization_name, domain, source_role,
    crawl_allowed, verification_status, verification_reference, verified_at,
    compliance_status, collection_mode, policy_notes, is_active
)
SELECT DISTINCT ON (target.official_domain)
    target.brand_id,
    target.display_name,
    target.official_domain,
    'brand_official',
    false,
    'verified',
    target.official_homepage_url,
    now(),
    'pending',
    CASE
        WHEN target.target_key IN ('gs25', 'lotteworld', 'microsoft') THEN 'manual_review'
        ELSE 'licensed_search'
    END,
    target.policy_notes,
    true
FROM brand_discovery_targets AS target
WHERE target.domain_status = 'verified'
  AND target.official_domain IS NOT NULL
ORDER BY target.official_domain, target.discovery_priority DESC, target.created_at
ON CONFLICT (domain) DO UPDATE SET
    brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
    verification_status = 'verified',
    verification_reference = EXCLUDED.verification_reference,
    verified_at = now(),
    collection_mode = EXCLUDED.collection_mode,
    policy_notes = COALESCE(benefit_source_domains.policy_notes, EXCLUDED.policy_notes),
    updated_at = now()
WHERE benefit_source_domains.verification_status = 'pending'
  AND benefit_source_domains.crawl_allowed = false;

-- Retain the more specific official-page evidence used for the three domains
-- that were verified manually rather than through the licensed-search flow.
WITH manual_evidence(domain, verification_reference) AS (
    VALUES
      ('gs25.gsretail.com', 'https://gs25.gsretail.com/gscvs/ko/store-services/locations'),
      ('adventure.lotteworld.com', 'https://adventure.lotteworld.com/price/member/prefer'),
      ('www.microsoft.com', 'https://www.microsoft.com/ko-kr/download')
)
UPDATE benefit_source_domains AS source
SET verification_reference = evidence.verification_reference,
    collection_mode = 'manual_review',
    updated_at = now()
FROM manual_evidence AS evidence
WHERE source.domain = evidence.domain
  AND source.verification_status = 'verified'
  AND source.crawl_allowed = false
  AND source.compliance_status = 'pending';

-- Some official benefit pages live on a separately operated subdomain. They
-- are registered explicitly and remain manual-review-only.
WITH extra_source(
    organization_name, domain, verification_reference, brand_normalized_name
) AS (
    VALUES
      ('GitHub Education', 'education.github.com', 'https://education.github.com/pack', 'github'),
      ('Apple Support', 'support.apple.com', 'https://support.apple.com/ko-kr/106008', 'applemusic')
), selected AS (
    SELECT source.*, brand.id AS brand_id
    FROM extra_source AS source
    JOIN LATERAL (
        SELECT id
        FROM brands
        WHERE normalized_name = source.brand_normalized_name
          AND is_active = true
        ORDER BY discovery_priority DESC, created_at
        LIMIT 1
    ) AS brand ON true
)
INSERT INTO benefit_source_domains (
    brand_id, organization_name, domain, source_role,
    crawl_allowed, verification_status, verification_reference, verified_at,
    compliance_status, collection_mode, policy_notes, is_active
)
SELECT
    brand_id, organization_name, domain, 'brand_official',
    false, 'verified', verification_reference, now(),
    'pending', 'manual_review',
    'Official benefit subdomain verified; direct collection remains disabled pending separate compliance approval.',
    true
FROM selected
ON CONFLICT (domain) DO UPDATE SET
    brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
    verification_status = 'verified',
    verification_reference = EXCLUDED.verification_reference,
    verified_at = now(),
    updated_at = now()
WHERE benefit_source_domains.verification_status = 'pending'
  AND benefit_source_domains.crawl_allowed = false;

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT DISTINCT source.id, target.brand_id
FROM brand_discovery_targets AS target
JOIN benefit_source_domains AS source
  ON source.domain = target.official_domain
WHERE target.domain_status = 'verified'
  AND target.brand_id IS NOT NULL
  AND source.verification_status = 'verified'
  AND source.is_active = true
ON CONFLICT (domain_id, brand_id) DO NOTHING;

WITH extra_mapping(domain, brand_normalized_name) AS (
    VALUES
      ('education.github.com', 'github'),
      ('support.apple.com', 'applemusic')
), selected_brand AS (
    SELECT mapping.domain, brand.id AS brand_id
    FROM extra_mapping AS mapping
    JOIN LATERAL (
        SELECT id
        FROM brands
        WHERE normalized_name = mapping.brand_normalized_name
          AND is_active = true
        ORDER BY discovery_priority DESC, created_at
        LIMIT 1
    ) AS brand ON true
)
INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT source.id, selected_brand.brand_id
FROM selected_brand
JOIN benefit_source_domains AS source
  ON source.domain = selected_brand.domain
WHERE source.verification_status = 'verified'
  AND source.is_active = true
ON CONFLICT (domain_id, brand_id) DO NOTHING;
