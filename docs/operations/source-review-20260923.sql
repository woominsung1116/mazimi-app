-- Run: psql -v crawler_contact_url="$CORPORATE_BOT_CONTACT_URL" -f source-review-20260923.sql
BEGIN;
WITH reviewed(domain,path) AS (VALUES
 ('www.pusan.ac.kr','/kor/CMS/Board/PopupBoard.do'),
 ('stat.pusan.ac.kr','/bbs/stat/502/968595/artclView.do'),
 ('onestop.pusan.ac.kr','/page'))
UPDATE benefit_source_domains d SET
 crawl_allowed=true,compliance_status='approved',approval_basis='public_facts_review',
 approval_reference='2026-09-23: owner-authorized bounded factual monitoring; docs/audits/official-source-policy-check-20260923.json; source+linked-policy inspection; not a blanket copyright license',
 terms_url=NULL,terms_checked_at=now(),terms_review_outcome='not_found',
 allowed_path_prefixes=ARRAY[r.path],allow_subdomains=false,min_request_interval_ms=30000,
 robots_url='https://'||r.domain||'/robots.txt',robots_status='allowed',robots_checked_at=now(),
 robots_content_hash='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
 robots_reviewed_path_prefixes=ARRAY[r.path],crawler_contact_url=:'crawler_contact_url',
 compliance_reviewed_by='owner-authorized-codex-review-20260923',compliance_reviewed_at=now(),
 approval_expires_at=now()+interval '90 days',collection_mode='direct_public',
 policy_notes='Only public factual benefit eligibility checks; no attachments, login, full-text redistribution. robots returned 404. Fetcher rechecks robots on every access. First-party permission is not claimed.',updated_at=now()
FROM reviewed r WHERE d.domain=r.domain AND d.verification_status='verified' AND d.is_active;
UPDATE benefit_source_candidates c SET next_check_at=now(),recheck_interval_seconds=86400
FROM benefit_source_domains d WHERE c.domain_id=d.id AND d.crawl_allowed AND c.status='approved';
UPDATE program_source_monitors m SET recheck_interval_seconds=86400 FROM benefit_source_candidates c,benefit_source_domains d
WHERE m.candidate_id=c.id AND c.domain_id=d.id AND d.crawl_allowed;
UPDATE benefit_source_domains SET crawl_allowed=false,compliance_status='rejected',terms_url='https://obsidian.md/terms',terms_review_outcome='restricted',terms_checked_at=now(),collection_mode='licensed_search',policy_notes='2026-09-23 official terms prohibit gathering/extracting/displaying site materials without express consent. Direct collection disabled; search metadata only.',updated_at=now() WHERE domain='obsidian.md';
COMMIT;
SELECT domain,crawl_allowed,collection_mode,approval_expires_at FROM benefit_source_domains WHERE crawl_allowed OR domain='obsidian.md';
