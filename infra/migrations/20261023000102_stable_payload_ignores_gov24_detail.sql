SET LOCAL lock_timeout = '5s';
-- gov24 `serviceDetail` rows are stored in the 보조금24 raw payload under
-- `detail` (온라인신청사이트URL, 접수기관명, 구비서류). The normalizer does
-- not read them; only the link resolver does, from source_snapshots. Like
-- the view counters, `detail` is left out of the change hash
-- (`crates/worker/src/sources/mod.rs` `volatile_keys`) and of the stable
-- payload: adding or changing it must not re-normalize ~10,000 programmes,
-- re-enqueue them for AI extraction or invalidate admin-approved AI pins
-- (md5 of this function's output). Payloads without `detail` keep their
-- exact previous output, so existing pins stay valid.
--
-- CREATE OR REPLACE keeps the grants of 20261022000102. No index uses this
-- function. No new table, so no RLS change.
--
-- Rollback (do not run automatically): recreate the function from
-- 20261021000001_ai_queue_ignores_view_counters.sql.

CREATE OR REPLACE FUNCTION ingestion_stable_payload(source TEXT, payload JSONB) RETURNS JSONB
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    SET search_path = pg_catalog, public AS $$
    SELECT CASE WHEN jsonb_typeof(payload) = 'object' THEN
        payload - (CASE source
            WHEN 'gov_benefits' THEN ARRAY['조회수', 'detail']
            WHEN 'youth_center' THEN ARRAY['inqCnt']
            WHEN 'local_welfare' THEN ARRAY['inqNum']
            WHEN 'national_welfare' THEN ARRAY['inqNum']
            ELSE ARRAY[]::text[]
        END)
    ELSE payload END
$$;
