-- Official student-offer pages queued for the automated corporate pipeline
-- (2026-10-31, corp-expand + corp-precision). One list, read by every
-- statement below.
--
-- Each page passed the whole intake chain after the precision fixes
-- (audience bound to the value, someone else's offer, term and trial,
-- staleness, region) on 2026-10-31, with robots allowing MazimiBenefitBot,
-- and each accept was checked by hand against the page (24/24 correct
-- across the measured sets). Already-catalogued programmes and two
-- borderline accepts (New Relic: also a free tier for everyone; Keeper:
-- cookie text in its notes) are not seeded. Paperpile, Dashlane and Sentry
-- reuse their existing unresolved targets (their category stays as
-- catalogued: 전자·소프트웨어; the browse chip comes from the categorizer).
--
-- What this registers, as 20261031000001 does: brand, discovery target with
-- its official domain and seed URL, and a source domain row for
-- brand<->domain identity (crawl_allowed=false, compliance 'pending',
-- collection 'manual_review'). No programme, monitor, fact, quote or
-- summary: nothing here is shown to users. Existing rows are never
-- overwritten.
--
-- The seed list is a TEMP table dropped at commit: it relies on the migration
-- running in one transaction (sqlx's default). Run by hand, wrap it in
-- BEGIN … COMMIT.
--
-- Rollback: this migration inserts brands, discovery targets, source
-- domains and their brand links, and fills unresolved targets' domain and
-- seed URL. It creates no candidate: the official-seed discoverer does,
-- later. To undo: DELETE the source domain rows below that have no
-- candidate, then the targets and brands with source_type 'coverage_seed'
-- and the target keys below, and reset Paperpile/Dashlane/Sentry's
-- official_domain, seed_urls and domain_status to 'unresolved'. Candidates
-- the discoverer created afterwards are deleted separately, only those
-- with no monitor. No user data.

CREATE TEMP TABLE corporate_seed_20261101 (
    target_key        TEXT PRIMARY KEY,
    display_name      TEXT NOT NULL,
    coverage_category TEXT NOT NULL,
    official_domain   TEXT NOT NULL,
    homepage_url      TEXT NOT NULL,
    seed_url          TEXT NOT NULL
) ON COMMIT DROP;

INSERT INTO corporate_seed_20261101 VALUES
    ('coverage_configcat', 'ConfigCat', '전자·소프트웨어', 'configcat.com', 'https://configcat.com/', 'https://configcat.com/student/'),
    ('coverage_deepscan', 'DeepScan', '전자·소프트웨어', 'deepscan.io', 'https://deepscan.io/', 'https://deepscan.io/github-student-pack'),
    ('coverage_imgbot', 'Imgbot', '전자·소프트웨어', 'imgbot.net', 'https://imgbot.net/', 'https://imgbot.net/github-students'),
    ('coverage_popsql', 'PopSQL', '전자·소프트웨어', 'popsql.com', 'https://popsql.com/', 'https://popsql.com/github-students'),
    ('coverage_paperpile', 'Paperpile', '전자·소프트웨어', 'paperpile.com', 'https://paperpile.com/', 'https://paperpile.com/pricing/'),
    ('coverage_sentry', 'Sentry', '전자·소프트웨어', 'sentry.io', 'https://sentry.io/', 'https://sentry.io/for/education/'),
    ('coverage_todiagram', 'ToDiagram', '전자·소프트웨어', 'todiagram.com', 'https://todiagram.com/', 'https://todiagram.com/education'),
    ('coverage_workingcopy', 'Working Copy', '전자·소프트웨어', 'workingcopy.app', 'https://workingcopy.app/', 'https://workingcopy.app/education/'),
    ('coverage_captureone', 'Capture One', '전자·소프트웨어', 'www.captureone.com', 'https://www.captureone.com/', 'https://www.captureone.com/en/education'),
    ('coverage_camber', 'Camber', '전자·소프트웨어', 'www.cambercloud.com', 'https://www.cambercloud.com/', 'https://www.cambercloud.com/github-student-pack'),
    ('coverage_dashlane', 'Dashlane', '전자·소프트웨어', 'www.dashlane.com', 'https://www.dashlane.com/', 'https://www.dashlane.com/github-students'),
    ('coverage_honeybadger', 'Honeybadger', '전자·소프트웨어', 'www.honeybadger.io', 'https://www.honeybadger.io/', 'https://www.honeybadger.io/github-students/'),
    ('coverage_nativeinstruments', 'Native Instruments', '전자·소프트웨어', 'www.native-instruments.com', 'https://www.native-instruments.com/', 'https://www.native-instruments.com/en/company/education-program/'),
    ('coverage_reasonstudios', 'Reason Studios', '전자·소프트웨어', 'www.reasonstudios.com', 'https://www.reasonstudios.com/', 'https://www.reasonstudios.com/school'),
    ('coverage_zeplin', 'Zeplin', '전자·소프트웨어', 'zeplin.io', 'https://zeplin.io/', 'https://zeplin.io/students/');

INSERT INTO brand_discovery_targets (target_key, display_name, coverage_category, discovery_priority)
SELECT target_key, display_name, coverage_category, 70 FROM corporate_seed_20261101
ON CONFLICT (target_key) DO NOTHING;

INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
SELECT s.display_name, lower(regexp_replace(s.display_name, '[^0-9A-Za-z가-힣]', '', 'g')),
       'coverage_seed', s.target_key, 70
FROM corporate_seed_20261101 s
WHERE NOT EXISTS (SELECT 1 FROM brands b
                  WHERE b.normalized_name = lower(regexp_replace(s.display_name, '[^0-9A-Za-z가-힣]', '', 'g')))
ON CONFLICT (source_type, source_id) DO NOTHING;

UPDATE brand_discovery_targets t
SET brand_id = (SELECT b.id FROM brands b
                WHERE b.normalized_name = lower(regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g'))
                ORDER BY b.discovery_priority DESC, b.created_at LIMIT 1),
    updated_at = now()
FROM corporate_seed_20261101 s
WHERE t.target_key = s.target_key AND t.brand_id IS NULL;

UPDATE brand_discovery_targets t
SET official_domain = s.official_domain,
    official_homepage_url = COALESCE(t.official_homepage_url, s.homepage_url),
    domain_status = 'verified',
    seed_urls = CASE WHEN s.seed_url = ANY (t.seed_urls) THEN t.seed_urls
                     ELSE t.seed_urls || s.seed_url END,
    policy_notes = COALESCE(t.policy_notes,
        'Student offer page passed the precision-fixed intake chain 2026-10-31 (every accept checked by hand): robots allows the path for MazimiBenefitBot, no bot wall. Queued as a review candidate only.'),
    updated_at = now()
FROM corporate_seed_20261101 s
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
JOIN corporate_seed_20261101 s ON s.target_key = t.target_key
WHERE t.domain_status = 'verified' AND t.official_domain IS NOT NULL AND t.brand_id IS NOT NULL
ON CONFLICT (domain) DO NOTHING;

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT d.id, t.brand_id
FROM brand_discovery_targets t
JOIN corporate_seed_20261101 s ON s.target_key = t.target_key
JOIN benefit_source_domains d ON d.domain = t.official_domain
WHERE t.brand_id IS NOT NULL AND d.verification_status = 'verified' AND d.is_active
ON CONFLICT (domain_id, brand_id) DO NOTHING;
