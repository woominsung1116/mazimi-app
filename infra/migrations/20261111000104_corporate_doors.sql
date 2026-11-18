-- Corporate doors (corporate-doors review slices 2-5): the routes the
-- server reads must-have brands through, and what each read found.
-- Service role only (RLS forced, client roles revoked). No page body is
-- stored anywhere here: counts, verdicts and accepted offers (a quote of at
-- most 100 characters each).
--
-- corporate_doors: one entry URL each.
--   - kind: `page` (a first-party page read under brand scope),
--     `listing` (a first-party list or newsroom whose same-host articles
--     become child `page` doors, dated), `hub_rows` (an issuer hub whose
--     list rows are read, review C1);
--   - grantor_brand_id: who pays or issues the benefit (the brand itself
--     for first-party doors, SK텔레콤/KB국민카드/PAYCO for hubs);
--   - flag: a door only read while its own switch is on (PAYCO);
--   - health and life cycle: state active → lost (two gone reads at least
--     an hour apart, robots disallow, moved off its host, a list that stops
--     reading three days running) → retired (replaced, or no replacement in
--     30 days: a counted outcome, review M6). No host is ever registered by
--     the job (review M4): a door lives on a domain a migration added.
--   - candidate_id: set once the door job runs (a first-party door's page
--     then goes through intake when its kind is live). Doors cause no
--     request while CORPORATE_DOORS_MODE is off.
-- corporate_door_reads: one row per read (operational log, 180-day
--   retention through the storage job's explicit allowlist).
-- corporate_door_offers: the offers a door states, keyed by (door, redeem
--   brand, offer key) (review H4: many offers per page); the offer key
--   excludes the period (H8). Shadow reads fill it; a live hub read
--   publishes from it.
--
-- Rollback: DROP TABLE corporate_door_offers, corporate_door_reads,
-- corporate_doors (programmes a live door published keep their rows; their
-- source_id names the door).

CREATE TABLE IF NOT EXISTS corporate_doors (
    id                   UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    domain_id            UUID        NOT NULL REFERENCES benefit_source_domains(id) ON DELETE CASCADE,
    entry_url            TEXT        NOT NULL UNIQUE
                                     CHECK (char_length(entry_url) <= 2000 AND entry_url ~ '^https://'),
    candidate_id         UUID        UNIQUE REFERENCES benefit_source_candidates(id) ON DELETE SET NULL,
    grantor_brand_id     UUID        NOT NULL REFERENCES brands(id) ON DELETE RESTRICT,
    kind                 TEXT        NOT NULL CHECK (kind IN ('page', 'listing', 'hub_rows')),
    parent_door_id       UUID        REFERENCES corporate_doors(id) ON DELETE CASCADE,
    flag                 TEXT        CHECK (flag IS NULL OR flag IN ('payco')),
    max_bytes            INT         NOT NULL DEFAULT 1048576
                                     CHECK (max_bytes BETWEEN 65536 AND 4194304),
    state                TEXT        NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'lost', 'retired')),
    next_read_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_attempt_at      TIMESTAMPTZ,
    last_success_at      TIMESTAMPTZ,
    lost_since           TIMESTAMPTZ,
    first_gone_at        TIMESTAMPTZ,
    unrecognised_days    INT         NOT NULL DEFAULT 0 CHECK (unrecognised_days BETWEEN 0 AND 1000),
    consecutive_failures INT         NOT NULL DEFAULT 0 CHECK (consecutive_failures BETWEEN 0 AND 1000),
    last_outcome         TEXT        CHECK (last_outcome IS NULL OR char_length(last_outcome) <= 60),
    last_matched_brands  UUID[]      NOT NULL DEFAULT '{}' CHECK (cardinality(last_matched_brands) <= 400),
    replaced_by          UUID        REFERENCES corporate_doors(id) ON DELETE SET NULL,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (max_bytes <= 1048576 OR kind = 'hub_rows'),
    CHECK (parent_door_id IS NULL OR kind = 'page')
);
CREATE INDEX IF NOT EXISTS idx_corporate_doors_due ON corporate_doors (state, next_read_at);
CREATE INDEX IF NOT EXISTS idx_corporate_doors_grantor ON corporate_doors (grantor_brand_id);
CREATE INDEX IF NOT EXISTS idx_corporate_doors_parent ON corporate_doors (parent_door_id)
    WHERE parent_door_id IS NOT NULL;

ALTER TABLE corporate_doors ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_doors FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_doors FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_doors FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_doors FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_doors_all_service ON corporate_doors;
CREATE POLICY corporate_doors_all_service ON corporate_doors
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE corporate_doors IS
    'Entry URLs the door job reads (first-party pages and lists, issuer hub lists), with health and life cycle. Internal.';

CREATE TABLE IF NOT EXISTS corporate_door_reads (
    id                    UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    door_id               UUID        NOT NULL REFERENCES corporate_doors(id) ON DELETE CASCADE,
    read_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    mode                  TEXT        NOT NULL CHECK (mode IN ('shadow', 'live')),
    verdict               TEXT        NOT NULL CHECK (char_length(verdict) BETWEEN 1 AND 60),
    rows_seen             INT         NOT NULL DEFAULT 0 CHECK (rows_seen >= 0),
    rows_matched          INT         NOT NULL DEFAULT 0 CHECK (rows_matched >= 0),
    rows_accepted         INT         NOT NULL DEFAULT 0 CHECK (rows_accepted >= 0),
    tiered_rows           INT         NOT NULL DEFAULT 0 CHECK (tiered_rows >= 0),
    by_reason             JSONB       NOT NULL DEFAULT '{}'
                                      CHECK (jsonb_typeof(by_reason) = 'object' AND pg_column_size(by_reason) <= 4096),
    matched_brands        UUID[]      NOT NULL DEFAULT '{}' CHECK (cardinality(matched_brands) <= 400),
    identical_to_previous BOOLEAN,
    requests              INT         NOT NULL DEFAULT 0 CHECK (requests >= 0)
);
CREATE INDEX IF NOT EXISTS idx_corporate_door_reads_door ON corporate_door_reads (door_id, read_at DESC);
CREATE INDEX IF NOT EXISTS idx_corporate_door_reads_at ON corporate_door_reads (read_at);

ALTER TABLE corporate_door_reads ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_door_reads FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_door_reads FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_door_reads FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_door_reads FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_door_reads_all_service ON corporate_door_reads;
CREATE POLICY corporate_door_reads_all_service ON corporate_door_reads
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE corporate_door_reads IS
    'One row per door read: verdict and counts only, never page text. Operational log, 180-day retention.';

CREATE TABLE IF NOT EXISTS corporate_door_offers (
    id               UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    door_id          UUID        NOT NULL REFERENCES corporate_doors(id) ON DELETE CASCADE,
    redeem_brand_id  UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    offer_key        TEXT        NOT NULL CHECK (offer_key ~ '^[0-9a-f]{64}$'),
    issuer_brand_id  UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    value_kind       TEXT        NOT NULL CHECK (value_kind IN ('discount_percent', 'krw')),
    value            TEXT        NOT NULL CHECK (value ~ '^[0-9]+(\.[0-9]+)?$'),
    upper_bound      BOOLEAN     NOT NULL DEFAULT false,
    tiers            TEXT        NOT NULL DEFAULT 'untiered'
                                 CHECK (tiers IN ('untiered', 'every_tier_same', 'every_tier_minimum')),
    quote            TEXT        NOT NULL CHECK (char_length(quote) BETWEEN 1 AND 100),
    restriction      TEXT        CHECK (restriction IS NULL OR char_length(restriction) <= 100),
    starts_on        DATE,
    ends_on          DATE,
    first_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    sightings        INT         NOT NULL DEFAULT 1 CHECK (sightings >= 1),
    rereads          INT         NOT NULL DEFAULT 0 CHECK (rereads >= 0),
    rereads_identical INT        NOT NULL DEFAULT 0 CHECK (rereads_identical >= 0 AND rereads_identical <= rereads),
    status           TEXT        NOT NULL DEFAULT 'observed' CHECK (status IN ('observed', 'published', 'gone')),
    program_id       UUID        REFERENCES programs(id) ON DELETE SET NULL,
    missing_since    TIMESTAMPTZ,
    UNIQUE (door_id, redeem_brand_id, offer_key)
);
CREATE INDEX IF NOT EXISTS idx_corporate_door_offers_identity
    ON corporate_door_offers (issuer_brand_id, redeem_brand_id, value_kind);
CREATE INDEX IF NOT EXISTS idx_corporate_door_offers_program ON corporate_door_offers (program_id)
    WHERE program_id IS NOT NULL;

ALTER TABLE corporate_door_offers ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_door_offers FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_door_offers FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_door_offers FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_door_offers FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_door_offers_all_service ON corporate_door_offers;
CREATE POLICY corporate_door_offers_all_service ON corporate_door_offers
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE corporate_door_offers IS
    'Offers doors state, keyed by (door, redeem brand, offer key; the key excludes the period). Shadow ledger and live link. Internal.';

-- A hub programme is verified by the door job alone: its monitor's mode is
-- `door`, which the evidence job (evidence_auto) and the hash fetcher
-- (crawl-approved domains only) never read.
ALTER TABLE program_source_monitors
    DROP CONSTRAINT IF EXISTS program_source_monitors_verification_mode_check;
ALTER TABLE program_source_monitors
    ADD CONSTRAINT program_source_monitors_verification_mode_check
    CHECK (verification_mode IN ('human', 'evidence_auto', 'door'));

-- One shown corporate benefit per (issuer, redeem brand, value kind)
-- (code review H-3): a hub row and the brand's own page stating the same
-- offer claim the same row here in the transaction that publishes, and the
-- second loses. The door job and intake both write it; hiding frees it.
CREATE TABLE IF NOT EXISTS corporate_benefit_identities (
    issuer_brand_id  UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    redeem_brand_id  UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    value_kind       TEXT        NOT NULL CHECK (value_kind IN ('discount_percent', 'krw')),
    program_id       UUID        NOT NULL UNIQUE REFERENCES programs(id) ON DELETE CASCADE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (issuer_brand_id, redeem_brand_id, value_kind)
);
ALTER TABLE corporate_benefit_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_benefit_identities FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_benefit_identities FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_benefit_identities FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_benefit_identities FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_benefit_identities_all_service ON corporate_benefit_identities;
CREATE POLICY corporate_benefit_identities_all_service ON corporate_benefit_identities
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
