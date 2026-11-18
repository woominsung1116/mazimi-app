-- The "신청" deep-link resolver (crates/worker/src/link_resolver).
--
-- program_link_candidates: every link a programme's source names (URL
-- fields in preference order, URLs in its method/deadline text, the portal
-- detail page) and application links found on its notice page, with the
-- worker's checks. program_link_selections: which candidate the programme
-- shows (selected_id) and its best non-application link (notice_id); the
-- API reads only these. link_check_hosts: robots.txt per origin (24 h
-- cache). link_resolver_metrics: daily self-metrics per source.
--
-- All four are written by the worker only. RLS is on and forced, with only
-- the service-role policy: no anon or authenticated access.
--
-- Additive only. Rollback (do not run automatically): unset
-- LINK_RESOLVER_ENABLED, deploy the previous API (it reads official_url),
-- then DROP TABLE link_resolver_metrics, link_check_hosts,
-- program_link_selections, program_link_candidates.

CREATE TABLE IF NOT EXISTS program_link_candidates (
    id                 UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id         UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    field              TEXT        NOT NULL CHECK (char_length(field) BETWEEN 1 AND 64),
    field_rank         SMALLINT    NOT NULL CHECK (field_rank BETWEEN 0 AND 1000),
    url                TEXT        NOT NULL CHECK (url ~ '^https://' AND char_length(url) <= 2048),
    host               TEXT        NOT NULL CHECK (char_length(host) BETWEEN 1 AND 253),
    origin             TEXT        NOT NULL CHECK (origin IN ('agency','public_sector','allowlist','form_host','rejected')),
    free_text          BOOLEAN     NOT NULL DEFAULT false,
    -- The notice page a one-hop application link was found on.
    source_url         TEXT        CHECK (source_url IS NULL OR (source_url ~ '^https://' AND char_length(source_url) <= 2048)),
    -- How the candidate is shown (NULL: not shown); rank a=0 .. d=3.
    kind               TEXT        CHECK (kind IN ('application','notice','information','portal')),
    rank               SMALLINT    CHECK (rank BETWEEN 0 AND 3),
    sub                TEXT        CHECK (sub IS NULL OR char_length(sub) BETWEEN 1 AND 80),
    access             TEXT        NOT NULL DEFAULT 'direct' CHECK (access IN ('direct','login_required')),
    -- Worker observation. The page facts are the last passing check's.
    check_status       TEXT        NOT NULL DEFAULT 'unchecked'
                                   CHECK (check_status IN ('unchecked','skipped','ok','blocked','soft_fail','hard_fail','broken')),
    final_url          TEXT        CHECK (final_url IS NULL OR char_length(final_url) <= 2048),
    http_status        SMALLINT    CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599),
    page               JSONB       CHECK (page IS NULL OR (jsonb_typeof(page) = 'object' AND pg_column_size(page) <= 65536)),
    content_hash       TEXT        CHECK (content_hash IS NULL OR content_hash ~ '^[0-9a-f]{64}$'),
    last_failure_code  TEXT        CHECK (last_failure_code IS NULL OR char_length(last_failure_code) BETWEEN 1 AND 80),
    checked_at         TIMESTAMPTZ,
    last_pass_at       TIMESTAMPTZ,
    next_check_at      TIMESTAMPTZ,
    fail_count         INTEGER     NOT NULL DEFAULT 0 CHECK (fail_count >= 0),
    first_fail_at      TIMESTAMPTZ,
    soft_fail_count    INTEGER     NOT NULL DEFAULT 0 CHECK (soft_fail_count >= 0),
    first_soft_fail_at TIMESTAMPTZ,
    restore_passes     INTEGER     NOT NULL DEFAULT 0 CHECK (restore_passes >= 0),
    -- Never set: soft failures never demote a link (kept for the worker's state type).
    demoted            BOOLEAN     NOT NULL DEFAULT false,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (program_id, field, url),
    CHECK ((kind IS NULL) = (rank IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_program_link_candidates_due
    ON program_link_candidates (next_check_at) WHERE next_check_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_program_link_candidates_url ON program_link_candidates (url);
CREATE INDEX IF NOT EXISTS idx_program_link_candidates_host ON program_link_candidates (host);

CREATE TABLE IF NOT EXISTS program_link_selections (
    program_id  UUID        PRIMARY KEY REFERENCES programs(id) ON DELETE CASCADE,
    -- SHA-256 of the candidate inputs and resolver version ('' before the first sync).
    input_hash  TEXT        NOT NULL CHECK (input_hash = '' OR input_hash ~ '^[0-9a-f]{64}$'),
    synced_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- pending: a candidate is not checked yet (the API keeps official_url);
    -- selected: selected_id is shown; none: nothing usable, no button.
    state       TEXT        NOT NULL CHECK (state IN ('pending','selected','none')),
    selected_id UUID        REFERENCES program_link_candidates(id) ON DELETE SET NULL,
    notice_id   UUID        REFERENCES program_link_candidates(id) ON DELETE SET NULL,
    selected_at TIMESTAMPTZ,
    -- A check changed a candidate; the selection is recomputed at the next run
    -- even if that check's own re-selection was cut off.
    reselect_due BOOLEAN    NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS idx_program_link_selections_synced ON program_link_selections (synced_at);
CREATE INDEX IF NOT EXISTS idx_program_link_selections_due ON program_link_selections (program_id) WHERE reselect_due;
CREATE INDEX IF NOT EXISTS idx_program_link_selections_selected ON program_link_selections (selected_id);
CREATE INDEX IF NOT EXISTS idx_program_link_selections_notice ON program_link_selections (notice_id);

CREATE TABLE IF NOT EXISTS link_check_hosts (
    origin        TEXT        PRIMARY KEY CHECK (origin ~ '^https://' AND char_length(origin) <= 300),
    robots_status TEXT        NOT NULL CHECK (robots_status IN ('allow','rules','disallow_all','unreachable')),
    robots_body   TEXT        CHECK (robots_body IS NULL OR octet_length(robots_body) <= 262144),
    failure_code  TEXT        CHECK (failure_code IS NULL OR char_length(failure_code) BETWEEN 1 AND 80),
    fetched_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS link_resolver_metrics (
    day               DATE        NOT NULL,
    source_type       TEXT        NOT NULL CHECK (char_length(source_type) BETWEEN 1 AND 64),
    programmes        BIGINT      NOT NULL CHECK (programmes >= 0),
    application       BIGINT      NOT NULL CHECK (application >= 0),
    notice            BIGINT      NOT NULL CHECK (notice >= 0),
    information       BIGINT      NOT NULL CHECK (information >= 0),
    portal            BIGINT      NOT NULL CHECK (portal >= 0),
    failing           BIGINT      NOT NULL CHECK (failing >= 0),
    no_link           BIGINT      NOT NULL CHECK (no_link >= 0),
    pending           BIGINT      NOT NULL CHECK (pending >= 0),
    candidates        BIGINT      NOT NULL CHECK (candidates >= 0),
    broken_candidates BIGINT      NOT NULL CHECK (broken_candidates >= 0),
    recorded_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (day, source_type)
);

DO $rls$
DECLARE
  t text;
  client_role text;
BEGIN
  FOREACH t IN ARRAY ARRAY['program_link_candidates', 'program_link_selections',
                           'link_check_hosts', 'link_resolver_metrics'] LOOP
    EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', t);
    FOREACH client_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM %I', t, client_role);
      END IF;
    END LOOP;
    EXECUTE format('DROP POLICY IF EXISTS %I ON public.%I', t || '_all_service', t);
    EXECUTE format('CREATE POLICY %I ON public.%I FOR ALL USING (is_service_role()) WITH CHECK (is_service_role())',
                   t || '_all_service', t);
  END LOOP;
END
$rls$;

COMMENT ON TABLE program_link_candidates IS
    'Link candidates of a programme with the worker''s checks (crates/worker/src/link_resolver). Worker-written; the API reads only the selected row.';
COMMENT ON TABLE program_link_selections IS
    'Which candidate a programme shows as its 신청 link (selected_id) and its best non-application link (notice_id). Worker-written.';
