-- Observed (worker) state is separated from published (admin) state.
-- `status` stays the reviewer's decision; the worker only writes the check
-- columns and may demote a verified row to 'unavailable' after three
-- consecutive HTTP 404/410. Additive except the valid_until clean-up below,
-- which only clears expiry stamps on rows that are no longer verified.
-- Rollback: drop the CHECK constraint and the four columns (do not run
-- automatically).
ALTER TABLE program_application_resources
    ADD COLUMN check_status TEXT
        CHECK (check_status IS NULL OR check_status IN ('ok','blocked','unreachable','changed','unsupported')),
    ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0
        CHECK (consecutive_failures BETWEEN 0 AND 100000),
    ADD COLUMN consecutive_gone INTEGER NOT NULL DEFAULT 0
        CHECK (consecutive_gone BETWEEN 0 AND 100000),
    ADD COLUMN last_failure_code TEXT
        CHECK (last_failure_code IS NULL OR length(last_failure_code) BETWEEN 1 AND 100);
COMMENT ON COLUMN program_application_resources.check_status IS
    'Last worker observation: ok (reachable, unchanged, or a login gate for login_required) | blocked (approval/robots/redirect/unsafe) | unreachable (network/HTTP) | changed (differs from the reviewed version; sticky until an admin review) | unsupported (format/size). NULL = never probed or reset by a review.';
COMMENT ON COLUMN program_application_resources.consecutive_failures IS
    'Consecutive failed probes; reset on success. Drives exponential backoff (1d,2d,4d,8d, then 14d cap).';
COMMENT ON COLUMN program_application_resources.consecutive_gone IS
    'Consecutive HTTP 404/410 probes; any other outcome resets it. Three demote a verified row to unavailable.';

-- One rule for expiry stamps: only a verified row carries valid_until.
-- verified_at is kept as "last verified at" history.
UPDATE program_application_resources SET valid_until = NULL
    WHERE status <> 'verified' AND valid_until IS NOT NULL;
ALTER TABLE program_application_resources
    ADD CONSTRAINT application_resources_valid_until_only_verified
        CHECK (status = 'verified' OR valid_until IS NULL);
