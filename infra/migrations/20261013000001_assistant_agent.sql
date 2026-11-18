-- AI 혜택 비서 에이전트 (docs/mazimi-ai-agent.md).
--
-- One run = PLAN -> EXECUTE -> CONFIRM -> RESULT for one goal in one
-- conversation. Write tools never run from a plan: the server stores a
-- confirmation bound to (user, run, tool call, normalized args, precondition,
-- expiry) and executes it once, after the user approves that exact hash.
--
-- Privacy: goals are stored masked (contact details, long digit runs), at
-- most 300 chars. Tool results kept for history are the same server data the
-- user can already read. Nothing here is sent to a model. Conversations are
-- hard-deleted by the user, by retention (ASSISTANT_RETENTION_DAYS, default
-- 30) and by account deletion (ON DELETE CASCADE). The action audit keeps no
-- content and is de-identified on account deletion (ON DELETE SET NULL).
--
-- Every table: RLS forced, service-role policy only, client roles revoked.
-- Additive only. Rollback (do not run automatically): set
-- ASSISTANT_AGENT_ENABLED=false, then DROP the tables below in reverse order.

CREATE TABLE assistant_conversations (
    id                 UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id            UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title              TEXT        NOT NULL DEFAULT '' CHECK (char_length(title) <= 60),
    context_program_id UUID        NULL REFERENCES programs(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX assistant_conversations_user ON assistant_conversations(user_id, updated_at DESC);
CREATE INDEX assistant_conversations_retention ON assistant_conversations(updated_at);

CREATE TABLE assistant_messages (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID        NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    run_id          UUID        NULL,
    role            TEXT        NOT NULL CHECK (role IN ('user', 'assistant', 'tool_summary')),
    content         TEXT        NOT NULL CHECK (char_length(content) <= 2000),
    citations       JSONB       NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(citations) = 'array'),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX assistant_messages_conversation ON assistant_messages(conversation_id, created_at);

CREATE TABLE assistant_runs (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID        NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    user_id         UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status          TEXT        NOT NULL DEFAULT 'received' CHECK (status IN (
                        'received', 'planning', 'tool_running', 'awaiting_confirmation',
                        'executing', 'validating_response', 'completed',
                        'failed', 'timed_out', 'cancelled')),
    planner         TEXT        NULL CHECK (planner IN ('fast_path', 'model', 'refused', 'fallback')),
    goal_kind       TEXT        NULL CHECK (char_length(goal_kind) <= 40),
    plan            JSONB       NULL,
    result          JSONB       NULL,
    context_program_id UUID     NULL,
    selected_program_ids UUID[] NOT NULL DEFAULT '{}' CHECK (cardinality(selected_program_ids) <= 4),
    idempotency_key TEXT        NULL CHECK (char_length(idempotency_key) BETWEEN 8 AND 128),
    provider        TEXT        NULL,
    model           TEXT        NULL,
    prompt_version  TEXT        NULL,
    toolset_version TEXT        NOT NULL,
    latency_ms      INTEGER     NULL,
    error_code      TEXT        NULL CHECK (char_length(error_code) <= 60),
    notify_requested BOOLEAN    NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ NULL
);
CREATE INDEX assistant_runs_conversation ON assistant_runs(conversation_id, created_at);
CREATE INDEX assistant_runs_user ON assistant_runs(user_id, created_at DESC);
CREATE INDEX assistant_runs_created ON assistant_runs(created_at DESC);
-- At most one active run per conversation (spec §6): a second message gets
-- 409 run_in_progress instead of cancelling the first implicitly.
CREATE UNIQUE INDEX assistant_runs_one_active ON assistant_runs(conversation_id)
    WHERE status IN ('received', 'planning', 'tool_running', 'awaiting_confirmation',
                     'executing', 'validating_response');
CREATE UNIQUE INDEX assistant_runs_idempotency ON assistant_runs(user_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
ALTER TABLE assistant_messages ADD CONSTRAINT assistant_messages_run
    FOREIGN KEY (run_id) REFERENCES assistant_runs(id) ON DELETE CASCADE;

CREATE TABLE assistant_tool_calls (
    id                 UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id             UUID        NOT NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
    seq                INTEGER     NOT NULL CHECK (seq BETWEEN 0 AND 63),
    tool_name          TEXT        NOT NULL CHECK (char_length(tool_name) <= 40),
    tool_version       TEXT        NOT NULL,
    permission         TEXT        NOT NULL CHECK (permission IN ('read', 'confirm_write', 'forbidden')),
    program_id         UUID        NULL,
    arguments_redacted JSONB       NOT NULL DEFAULT '{}',
    result_summary     JSONB       NULL,
    status             TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN (
                           'pending', 'ok', 'error', 'skipped', 'awaiting_confirmation',
                           'executed', 'rejected', 'expired', 'stale', 'failed')),
    error_code         TEXT        NULL CHECK (char_length(error_code) <= 60),
    latency_ms         INTEGER     NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (run_id, seq)
);

CREATE TABLE assistant_confirmations (
    id             UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id         UUID        NOT NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
    user_id        UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tool_call_id   UUID        NOT NULL UNIQUE REFERENCES assistant_tool_calls(id) ON DELETE CASCADE,
    tool_name      TEXT        NOT NULL,
    program_id     UUID        NOT NULL,
    arguments      JSONB       NOT NULL,
    precondition   JSONB       NOT NULL,
    preview        JSONB       NOT NULL,
    payload_hash   TEXT        NOT NULL CHECK (payload_hash ~ '^[0-9a-f]{64}$'),
    status         TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN (
                       'pending', 'approved', 'rejected', 'expired', 'executed', 'failed', 'stale')),
    expires_at     TIMESTAMPTZ NOT NULL,
    decided_at     TIMESTAMPTZ NULL,
    executed_at    TIMESTAMPTZ NULL,
    result         JSONB       NULL,
    idempotency_key TEXT       NULL CHECK (char_length(idempotency_key) BETWEEN 8 AND 128),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX assistant_confirmations_run ON assistant_confirmations(run_id);
CREATE INDEX assistant_confirmations_pending ON assistant_confirmations(expires_at) WHERE status = 'pending';

-- Minimal, content-free trail of executed or refused write actions. Kept
-- after the conversation is deleted; de-identified on account deletion.
CREATE TABLE assistant_action_audit (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID        NULL REFERENCES users(id) ON DELETE SET NULL,
    confirmation_id UUID        NULL,
    tool_name       TEXT        NOT NULL,
    outcome         TEXT        NOT NULL CHECK (outcome IN ('executed', 'rejected', 'stale', 'failed', 'expired')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX assistant_action_audit_created ON assistant_action_audit(created_at);

-- Proactive assistant (Phase 3). Everything is opt-in per user and off by
-- default; the worker job itself is off unless ASSISTANT_AGENT_PROACTIVE_ENABLED.
CREATE TABLE assistant_proactive_settings (
    user_id            UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    weekly_briefing    BOOLEAN     NOT NULL DEFAULT false,
    change_alerts      BOOLEAN     NOT NULL DEFAULT false,
    resume_suggestions BOOLEAN     NOT NULL DEFAULT false,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- What a saved programme looked like when an opted-in user last saw it, so
-- a change of deadline/conditions/documents/amounts can be told apart.
CREATE TABLE assistant_program_fingerprints (
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    program_id  UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    fingerprint JSONB       NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, program_id)
);

-- One row per proactive message; the unique key is the frequency cap.
CREATE TABLE assistant_proactive_deliveries (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind        TEXT        NOT NULL CHECK (kind IN ('weekly_briefing', 'change_impact', 'resume', 'run_done')),
    program_id  UUID        NULL REFERENCES programs(id) ON DELETE CASCADE,
    dedupe_key  TEXT        NOT NULL CHECK (char_length(dedupe_key) <= 200),
    kst_date    DATE        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, kind, dedupe_key)
);
CREATE INDEX assistant_proactive_deliveries_day ON assistant_proactive_deliveries(user_id, kst_date);

-- Push for a run that finished after the client stopped waiting. The worker
-- sends it inside 09:00-20:59 KST and drops it after 6 hours.
CREATE TABLE assistant_notification_outbox (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    run_id     UUID        NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
    title      TEXT        NOT NULL CHECK (char_length(title) <= 60),
    body       TEXT        NOT NULL CHECK (char_length(body) <= 200),
    status     TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'dropped')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at    TIMESTAMPTZ NULL
);
CREATE INDEX assistant_notification_outbox_pending ON assistant_notification_outbox(created_at) WHERE status = 'pending';

-- Expire overdue confirmations (one user, or everyone when p_user IS NULL),
-- audit them, and complete runs left with nothing to confirm. The API calls
-- it lazily before reading or writing a user's runs; the worker's purge job
-- calls it for everyone. One definition for both.
CREATE FUNCTION assistant_expire_confirmations(p_user UUID) RETURNS INTEGER
LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE expired_count INTEGER;
BEGIN
    WITH expired AS (
        UPDATE assistant_confirmations SET status = 'expired', decided_at = now()
        WHERE status = 'pending' AND expires_at <= now()
          AND (p_user IS NULL OR user_id = p_user)
        RETURNING id, user_id, tool_call_id, tool_name
    ), calls AS (
        UPDATE assistant_tool_calls t SET status = 'expired'
        FROM expired e WHERE t.id = e.tool_call_id
        RETURNING t.id
    ), audit AS (
        INSERT INTO assistant_action_audit(user_id, confirmation_id, tool_name, outcome)
        SELECT user_id, id, tool_name, 'expired' FROM expired
        RETURNING id
    )
    SELECT count(*) INTO expired_count FROM expired;
    UPDATE assistant_runs r SET status = 'completed', updated_at = now(), completed_at = now()
    WHERE r.status = 'awaiting_confirmation'
      AND (p_user IS NULL OR r.user_id = p_user)
      AND NOT EXISTS (SELECT 1 FROM assistant_confirmations c
                      WHERE c.run_id = r.id AND c.status = 'pending');
    RETURN expired_count;
END;
$$;
REVOKE ALL ON FUNCTION assistant_expire_confirmations(UUID) FROM PUBLIC;

DO $access$
DECLARE
  t text;
  client_role text;
BEGIN
  FOREACH t IN ARRAY ARRAY['assistant_conversations','assistant_messages','assistant_runs',
      'assistant_tool_calls','assistant_confirmations','assistant_action_audit',
      'assistant_proactive_settings','assistant_program_fingerprints',
      'assistant_proactive_deliveries','assistant_notification_outbox'] LOOP
    EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('CREATE POLICY %I ON public.%I FOR ALL USING (is_service_role()) WITH CHECK (is_service_role())',
                   t || '_service', t);
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', t);
    FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM %I', t, client_role);
      END IF;
    END LOOP;
  END LOOP;
END $access$;

COMMENT ON TABLE assistant_conversations IS
    'AI benefit agent conversations. Hard-deleted by the user, by retention and with the account.';
COMMENT ON TABLE assistant_confirmations IS
    'Server-issued confirmation for one write tool call, bound by payload_hash; executed at most once.';
COMMENT ON TABLE assistant_action_audit IS
    'Content-free audit of agent write actions; de-identified on account deletion.';
