SET LOCAL lock_timeout = '5s';

-- AI and reviewer pins (`program_ai_field_overrides.raw_payload_md5`) are
-- now written as md5(ingestion_stable_payload(source_type, raw_payload)::text):
-- the payload without its view counter, so a counter-only change does not
-- drop an approved value. The worker still accepts a key of the full
-- payload (md5(raw_payload::text)) written before this change. Production
-- had 0 pins on 2026-10-22, so nothing is rewritten. Comment only; no data
-- or schema change.
--
-- Rollback (do not run automatically): restore the comment of
-- 20261010000001_ai_extraction.sql.

COMMENT ON TABLE program_ai_field_overrides IS
    'Admin-approved AI values for ingestion-managed program columns; valid only while md5(ingestion_stable_payload(source_type, raw_payload)::text) matches (legacy pins: md5(raw_payload::text)).';
