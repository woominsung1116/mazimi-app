-- Official student-offer pages queued for the automated corporate pipeline.
--
-- Probed 2026-10-26 with the honest MazimiBenefitBot user agent, one robots
-- request and one page request per host, no redirects followed:
--   www.ableton.com     robots allows /en/shop/education/, 200 HTML, no challenge
--   lucid.co            robots allows /lucidchart/use-cases/education, 200 HTML
--   gemini.google       robots allows /students/, 200 HTML (ko locale)
--   www.wolframalpha.com robots allows /pro-for-students, 200 HTML, no challenge marker
--   www.overleaf.com    robots allows /user/subscription/plans, 200 HTML, no challenge marker
--   master.dev          robots allows /welcome/github-student-developers/, 200 HTML. Frontend
--                       Masters rebranded: frontendmasters.com and its robots.txt now 301 to
--                       master.dev, so the old candidate can never be read (robots redirects
--                       are not followed off the host). This adds the new host as the same
--                       brand's domain and queues the new page; the existing programme and its
--                       monitor are left as they are.
--
-- What this registers, and what it does not:
-- - A brand discovery target with its official domain and one seed URL. The
--   official-seed discoverer turns each seed into a `review` candidate linked
--   to the brand; the census then rates it (robots per path, ToS signal,
--   lane) and the bootstrap/re-extraction gates read it.
-- - A source domain row: brand<->domain identity only (`verification_status`
--   'verified' = this host is the brand's own site). crawl_allowed=false,
--   compliance 'pending', collection 'manual_review': no crawl approval.
-- - No programme, monitor, fact, quote or summary. Nothing here is shown to
--   users; an offer becomes visible only through the evidence pipeline.
-- Existing rows are never overwritten: rejected/blocked domains and targets
-- keep their state, and a verified target keeps its own domain.
--
-- Rollback: DELETE the candidates whose canonical_url is in the seed list and
-- that have no monitor, then the domain rows below with no other candidate,
-- then reset the six targets' official_domain/seed_urls. No user data.

WITH seed(target_key, display_name, coverage_category, official_domain, homepage_url, seed_url) AS (
    VALUES
      ('ableton', 'Ableton', '전자·소프트웨어', 'www.ableton.com', 'https://www.ableton.com/en/', 'https://www.ableton.com/en/shop/education/'),
      ('coverage_lucidchart', 'Lucidchart', '전자·소프트웨어', 'lucid.co', 'https://lucid.co/', 'https://lucid.co/lucidchart/use-cases/education'),
      ('gemini_students', 'Google Gemini', '전자·소프트웨어', 'gemini.google', 'https://gemini.google/', 'https://gemini.google/students/'),
      ('wolfram', 'Wolfram', '교육·자기계발', 'www.wolframalpha.com', 'https://www.wolframalpha.com/', 'https://www.wolframalpha.com/pro-for-students'),
      ('coverage_overleaf', 'Overleaf', '전자·소프트웨어', 'www.overleaf.com', 'https://www.overleaf.com/', 'https://www.overleaf.com/user/subscription/plans'),
      ('coverage_frontendmasters', 'Frontend Masters', '교육·자기계발', 'master.dev', 'https://master.dev/', 'https://master.dev/welcome/github-student-developers/')
)
INSERT INTO brand_discovery_targets (target_key, display_name, coverage_category, discovery_priority)
SELECT target_key, display_name, coverage_category, 70 FROM seed
ON CONFLICT (target_key) DO NOTHING;

INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
SELECT t.display_name, lower(regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g')), 'coverage_seed', t.target_key, 70
FROM brand_discovery_targets t
WHERE t.target_key IN ('ableton', 'gemini_students') AND t.brand_id IS NULL
  AND NOT EXISTS (SELECT 1 FROM brands b
                  WHERE b.normalized_name = lower(regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g')))
ON CONFLICT (source_type, source_id) DO NOTHING;

UPDATE brand_discovery_targets t
SET brand_id = (SELECT b.id FROM brands b
                WHERE b.normalized_name = lower(regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g'))
                ORDER BY b.discovery_priority DESC, b.created_at LIMIT 1),
    updated_at = now()
WHERE t.target_key IN ('ableton', 'coverage_lucidchart', 'gemini_students', 'wolfram', 'coverage_overleaf', 'coverage_frontendmasters')
  AND t.brand_id IS NULL;

WITH seed(target_key, official_domain, homepage_url, seed_url) AS (
    VALUES
      ('ableton', 'www.ableton.com', 'https://www.ableton.com/en/', 'https://www.ableton.com/en/shop/education/'),
      ('coverage_lucidchart', 'lucid.co', 'https://lucid.co/', 'https://lucid.co/lucidchart/use-cases/education'),
      ('gemini_students', 'gemini.google', 'https://gemini.google/', 'https://gemini.google/students/'),
      ('wolfram', 'www.wolframalpha.com', 'https://www.wolframalpha.com/', 'https://www.wolframalpha.com/pro-for-students'),
      ('coverage_overleaf', 'www.overleaf.com', 'https://www.overleaf.com/', 'https://www.overleaf.com/user/subscription/plans'),
      ('coverage_frontendmasters', 'master.dev', 'https://master.dev/', 'https://master.dev/welcome/github-student-developers/')
)
UPDATE brand_discovery_targets t
SET official_domain = seed.official_domain,
    official_homepage_url = COALESCE(t.official_homepage_url, seed.homepage_url),
    domain_status = 'verified',
    seed_urls = CASE WHEN seed.seed_url = ANY (t.seed_urls) THEN t.seed_urls
                     ELSE t.seed_urls || seed.seed_url END,
    policy_notes = COALESCE(t.policy_notes,
        'Student offer page probed 2026-10-26: robots allows the path for MazimiBenefitBot, no bot wall. Queued as a review candidate only.'),
    updated_at = now()
FROM seed
WHERE t.target_key = seed.target_key
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
       'Official brand domain for a student-offer seed (2026-10-26). No crawl approval; the automated policy gates decide collection.',
       true
FROM brand_discovery_targets t
WHERE t.target_key IN ('ableton', 'coverage_lucidchart', 'gemini_students', 'wolfram', 'coverage_overleaf', 'coverage_frontendmasters')
  AND t.domain_status = 'verified' AND t.official_domain IS NOT NULL AND t.brand_id IS NOT NULL
ON CONFLICT (domain) DO NOTHING;

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT d.id, t.brand_id
FROM brand_discovery_targets t
JOIN benefit_source_domains d ON d.domain = t.official_domain
WHERE t.target_key IN ('ableton', 'coverage_lucidchart', 'gemini_students', 'wolfram', 'coverage_overleaf', 'coverage_frontendmasters')
  AND t.brand_id IS NOT NULL AND d.verification_status = 'verified' AND d.is_active
ON CONFLICT (domain_id, brand_id) DO NOTHING;
