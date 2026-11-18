-- Re-verification checkpoint for priority franchise brands.
-- Evidence is recorded from first-party pages only; this migration never
-- enables automated crawling or republishes search-result text.

WITH evidence(source_id, official_url, brand_key) AS (
    VALUES
      ('https://www.starbucks.co.kr/msr/msreward/level_benefit.do', 'https://www.starbucks.co.kr/msr/msreward/level_benefit.do', 'starbucks'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1000&cat_cd=&coopco_cd=7010&mcht_no=1000', 'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1000&cat_cd=&coopco_cd=7010&mcht_no=1000', 'cgv'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3000&coopco_cd=7030&mcht_no=3000', 'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3000&coopco_cd=7030&mcht_no=3000', 'oliveyoung')
)
UPDATE programs AS p
SET raw_payload = COALESCE(p.raw_payload, '{}'::jsonb) ||
    jsonb_build_object('curation', jsonb_build_object(
      'status', 'verified', 'reviewed_at', '2026-07-21',
      'review_method', 'manual_official_page',
      'official_url', e.official_url,
      'crawl_allowed', false)),
    official_url = e.official_url,
    last_synced_at = now(), updated_at = now()
FROM evidence e
WHERE p.source_type = 'corporate_official' AND p.source_id = e.source_id;

-- Keep the three domains explicitly search/manual-review only.
UPDATE benefit_source_domains
SET crawl_allowed = false,
    collection_mode = 'manual_review',
    compliance_status = COALESCE(compliance_status, 'pending'),
    policy_notes = COALESCE(policy_notes, 'Official source; manual verification only. Automated collection disabled.'),
    updated_at = now()
WHERE domain IN ('www.starbucks.co.kr', 'cgv.co.kr', 'www.oliveyoung.co.kr');

