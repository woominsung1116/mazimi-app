SET LOCAL lock_timeout = '5s';
-- 복지로 중앙 (`national_welfare`) detail rows (`NationalWelfaredetailedV001`
-- `applmetList`: the institution of each 복지 process stage) are stored in
-- the raw payload under `detail`, as 보조금24's serviceDetail is. Like that
-- key and the view counters, `detail` is left out of the change hash
-- (`crates/worker/src/sources/mod.rs` `volatile_keys`) and of the stable
-- payload, so adding or changing it never re-normalizes a programme,
-- re-enqueues it for AI extraction or invalidates an admin-approved AI pin.
-- Payloads without `detail` keep their exact previous output.
--
-- CREATE OR REPLACE keeps the function's grants. No index uses it. No new
-- table, so no RLS change.
--
-- Rollback (do not run automatically): recreate the function from
-- 20261023000102_stable_payload_ignores_gov24_detail.sql.

CREATE OR REPLACE FUNCTION ingestion_stable_payload(source TEXT, payload JSONB) RETURNS JSONB
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    SET search_path = pg_catalog, public AS $$
    SELECT CASE WHEN jsonb_typeof(payload) = 'object' THEN
        payload - (CASE source
            WHEN 'gov_benefits' THEN ARRAY['조회수', 'detail']
            WHEN 'youth_center' THEN ARRAY['inqCnt']
            WHEN 'local_welfare' THEN ARRAY['inqNum']
            WHEN 'national_welfare' THEN ARRAY['inqNum', 'detail']
            ELSE ARRAY[]::text[]
        END)
    ELSE payload END
$$;
