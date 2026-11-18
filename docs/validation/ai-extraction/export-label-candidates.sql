-- Export 100 programs for human labeling of the AI extraction eval set.
-- Read-only. Public program text only (no user tables are read).
--
-- Stratified round-robin over (source_type, program_type): every stratum
-- contributes its first program before any contributes a second, in a
-- stable pseudo-random order (md5 of the id), so the set covers every
-- collector and category instead of the largest one.
--
-- Usage (JSON fixture template, fill `expected` by hand):
--   psql "$DATABASE_URL" -At -v limit=100 \
--     -f docs/validation/ai-extraction/export-label-candidates.sql \
--     > docs/validation/ai-extraction/label-candidates.json
-- Then print the exact text labels must quote:
--   cargo run -p mazimi-worker --bin extraction-eval -- \
--     --fixture docs/validation/ai-extraction/label-candidates.json --print-sources
-- The tracking-sheet CSV query at the bottom is commented out; run it
-- separately with \copy if the labelers want a spreadsheet.

\set ON_ERROR_STOP on
\if :{?limit}
\else
  \set limit 100
\endif

WITH eligible AS (
    SELECT p.*,
           row_number() OVER (
               PARTITION BY p.source_type, p.program_type
               ORDER BY md5(p.id::text)
           ) AS stratum_rank
    FROM programs p
    WHERE p.is_active
      AND p.program_type NOT IN ('corporate_benefit', 'financial_product')
      AND (p.summary IS NOT NULL OR p.raw_payload IS NOT NULL
           OR p.application_method IS NOT NULL OR p.submission_documents IS NOT NULL)
),
picked AS (
    SELECT * FROM eligible
    ORDER BY stratum_rank, source_type, program_type, md5(id::text)
    LIMIT :limit
)
SELECT jsonb_pretty(jsonb_build_object(
    'meta', jsonb_build_object(
        'labeler', 'UNLABELED - replace with the labeler name(s)',
        'created_at', to_char(now() AT TIME ZONE 'Asia/Seoul', 'YYYY-MM-DD'),
        'schema_version', 1,
        'human_verified', false,
        'note', 'Exported by export-label-candidates.sql. Set human_verified=true only after a second person checked every label.'
    ),
    'programs', jsonb_agg(jsonb_build_object(
        'id', 'program-' || id,
        'origin', source_type || '/' || program_type || ' ' || coalesce(source_id, id::text),
        'source', jsonb_strip_nulls(jsonb_build_object(
            'title', title,
            'summary', summary,
            'application_method', application_method,
            'submission_documents', submission_documents,
            'screening_method', screening_method,
            'raw_payload', raw_payload
        )),
        'expected', '[]'::jsonb
    ) ORDER BY source_type, program_type, stratum_rank)
))
FROM picked;

-- Tracking sheet (optional):
-- \copy (SELECT id, source_type, program_type, title, '' AS labeler, '' AS reviewer, '' AS status FROM programs WHERE id IN (...)) TO 'label-tracking.csv' CSV HEADER
