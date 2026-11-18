-- Naver Search API residue (corporate-doors plan §1.3, review slice 0).
--
-- The Naver Search client was removed on 2026-11-06 (9ea2d537), but what it
-- wrote is still stored. The API terms (revised 2026-10-07, 2.3) forbid
-- copying, storing or caching its data "and its derivatives" beyond the
-- 21-day history allowance (2.4), and feeding it to AI. So:
--   1. result titles and the query that produced them are cleared on every
--      search-API candidate and suggestion;
--   2. a search-API candidate still waiting for its first read (status
--      queued/error, never checked, no content hash, no programme, never
--      watched by any monitor) is rejected: its URL is a derivative until
--      our own robots-checked fetch establishes it;
--   3. everything else is kept: a URL our bot fetched, one a person
--      approved or is reviewing (approved, review, change_review,
--      verification_due, unreachable_review), one tied to a programme or a
--      monitor;
--   4. pending search-API suggestions are marked rejected (URLs kept).
-- Touches exactly: benefit_source_candidates.page_title, discovery_query,
-- status, last_error, next_check_at, updated_at; brand_source_suggestions
-- .page_title, status, review_note, reviewed_at, reviewed_by, updated_at.
-- No user data, programme, monitor or fact is touched.
-- Idempotent: running it again changes nothing.
--
-- Rollback: none, on purpose. The cleared titles must not come back.

UPDATE benefit_source_candidates
SET page_title = NULL, discovery_query = NULL, updated_at = now()
WHERE discovered_via = 'search_api'
  AND (page_title IS NOT NULL OR discovery_query IS NOT NULL);

UPDATE brand_source_suggestions
SET page_title = NULL, updated_at = now()
WHERE discovered_via IN ('naver_search', 'search_api')
  AND page_title IS NOT NULL;

UPDATE benefit_source_candidates c
SET status = 'rejected', last_error = 'search_provenance_withdrawn', next_check_at = NULL,
    updated_at = now()
WHERE c.discovered_via = 'search_api'
  -- Only a candidate still waiting for its first read: never one a person
  -- approved or is reviewing, one tied to a programme, or one any monitor
  -- (retired included) ever watched.
  AND c.status IN ('queued', 'error')
  AND c.program_id IS NULL
  AND c.last_checked_at IS NULL
  AND c.content_hash IS NULL
  AND NOT EXISTS (SELECT 1 FROM program_source_monitors m WHERE m.candidate_id = c.id);

-- Pending search-API suggestions are withdrawn from the review queue
-- (security review M8). Their URLs stay stored: deleting rows of an
-- operational table is the owner's decision, not this migration's.
UPDATE brand_source_suggestions
SET status = 'rejected', review_note = 'search_provenance_withdrawn', reviewed_at = now(),
    reviewed_by = 'migration:20261111000101', updated_at = now()
WHERE discovered_via IN ('naver_search', 'search_api') AND status = 'pending';
