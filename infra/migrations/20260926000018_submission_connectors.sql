-- Explicit institution-authorized intake, separate from publication/crawl permissions.
CREATE TABLE application_connectors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id UUID NOT NULL REFERENCES programs(id) ON DELETE RESTRICT,
    provider_name TEXT NOT NULL,
    authority_reference TEXT NOT NULL,
    authorized_until TIMESTAMPTZ NOT NULL,
    privacy_notice TEXT NOT NULL,
    schema JSONB NOT NULL,
    schema_hash TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    revoked_at TIMESTAMPTZ,
    healthy_at TIMESTAMPTZ,
    observed_hash TEXT,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX application_connector_one_live ON application_connectors(program_id) WHERE revoked_at IS NULL;

CREATE TABLE application_submissions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    program_id UUID NOT NULL REFERENCES programs(id) ON DELETE RESTRICT,
    connector_id UUID NOT NULL REFERENCES application_connectors(id) ON DELETE RESTRICT,
    request_hash TEXT NOT NULL,
    schema_hash TEXT NOT NULL,
    plan_revision TEXT NOT NULL,
    -- Authenticated ciphertext; the database never receives plaintext answers.
    encrypted_answers BYTEA,
    state TEXT NOT NULL CHECK(state IN ('queued','submitting','unknown','accepted','rejected','cancelled','expired')),
    consent_snapshot JSONB NOT NULL,
    program_snapshot JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '24 hours',
    started_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    receipt_reference TEXT CHECK(char_length(receipt_reference) BETWEEN 1 AND 200),
    received_at TIMESTAMPTZ,
    CHECK((state='accepted')=(receipt_reference IS NOT NULL AND received_at IS NOT NULL))
);
CREATE UNIQUE INDEX application_submission_no_duplicate ON application_submissions(user_id,program_id)
    WHERE state IN ('queued','submitting','unknown','accepted');
CREATE INDEX application_submission_queue ON application_submissions(connector_id,created_at)
    WHERE state IN ('queued','submitting','unknown');

CREATE FUNCTION maintain_application_submissions() RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    UPDATE application_submissions s SET state='expired',encrypted_answers=NULL,updated_at=now()
      WHERE state='queued' AND (expires_at<=now() OR EXISTS(
        SELECT 1 FROM application_connectors c WHERE c.id=s.connector_id
          AND (c.revoked_at IS NOT NULL OR c.authorized_until<=now())));
    -- A lost acknowledgement is not permission to repeat an external side effect.
    UPDATE application_submissions SET state='unknown',encrypted_answers=NULL,updated_at=now()
      WHERE state='submitting' AND started_at<now()-interval '10 minutes';
    UPDATE application_submissions SET encrypted_answers=NULL
      WHERE encrypted_answers IS NOT NULL AND (expires_at<=now() OR state<>'queued');
END $$;
REVOKE ALL ON FUNCTION maintain_application_submissions() FROM PUBLIC;

ALTER TABLE application_connectors ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_connectors FORCE ROW LEVEL SECURITY;
CREATE POLICY connector_service ON application_connectors FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());
ALTER TABLE application_submissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_submissions FORCE ROW LEVEL SECURITY;
CREATE POLICY submission_service ON application_submissions FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());
REVOKE ALL ON application_connectors,application_submissions FROM PUBLIC;
DO $$ DECLARE r text; BEGIN
  FOREACH r IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname=r) THEN
      EXECUTE format('REVOKE ALL ON application_connectors,application_submissions FROM %I',r);
      EXECUTE format('REVOKE ALL ON FUNCTION maintain_application_submissions() FROM %I',r);
    END IF;
  END LOOP;
END $$;
