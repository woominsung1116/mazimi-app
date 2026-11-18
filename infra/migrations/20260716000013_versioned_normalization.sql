-- Make normalization reproducible and independently replayable from retained
-- source snapshots. Version 0 means "produced before version tracking" and is
-- intentionally eligible for a one-time re-normalization at version 1.

ALTER TABLE source_snapshots
    ADD COLUMN normalizer_version INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN normalized_at TIMESTAMPTZ;

ALTER TABLE programs
    ADD COLUMN normalizer_version INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN normalized_at TIMESTAMPTZ;

ALTER TABLE ingestion_runs
    ADD COLUMN normalizer_version INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'ingestion',
    ADD COLUMN total_renormalized INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN total_failed INTEGER NOT NULL DEFAULT 0;

ALTER TABLE source_snapshots
    ADD CONSTRAINT source_snapshots_normalizer_version_check
    CHECK (normalizer_version >= 0);

ALTER TABLE programs
    ADD CONSTRAINT programs_normalizer_version_check
    CHECK (normalizer_version >= 0);

ALTER TABLE ingestion_runs
    ADD CONSTRAINT ingestion_runs_normalizer_version_check
    CHECK (normalizer_version >= 0),
    ADD CONSTRAINT ingestion_runs_total_renormalized_check
    CHECK (total_renormalized >= 0),
    ADD CONSTRAINT ingestion_runs_total_failed_check
    CHECK (total_failed >= 0),
    ADD CONSTRAINT ingestion_runs_run_kind_check
    CHECK (run_kind IN ('ingestion', 'renormalization'));

CREATE INDEX idx_source_snapshots_stale_normalizer
    ON source_snapshots (source_name, normalizer_version, fetched_at);

CREATE INDEX idx_programs_normalizer_version
    ON programs (source_type, normalizer_version)
    WHERE source_id IS NOT NULL;

COMMENT ON COLUMN source_snapshots.normalizer_version IS
    'Normalizer version last applied successfully to this retained raw payload; 0 means pending/legacy.';
COMMENT ON COLUMN programs.normalizer_version IS
    'Source-specific normalizer version that produced normalized_payload.';
COMMENT ON COLUMN ingestion_runs.run_kind IS
    'ingestion fetches upstream; renormalization replays retained source_snapshots without network access.';
