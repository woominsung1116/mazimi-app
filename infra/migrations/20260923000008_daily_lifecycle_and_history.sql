-- A daily run never deletes catalogue rows or application records.
CREATE TABLE worker_daily_jobs (
    job_name TEXT PRIMARY KEY,
    last_attempt_at TIMESTAMPTZ,
    last_success_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'pending',
    lease_token UUID,
    lease_until TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0,
    details JSONB NOT NULL DEFAULT '{}'
);
ALTER TABLE worker_daily_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE worker_daily_jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY worker_daily_jobs_service ON worker_daily_jobs FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());

CREATE TABLE application_program_snapshots (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    program_id UUID NOT NULL REFERENCES programs(id) ON DELETE RESTRICT,
    program JSONB NOT NULL,
    captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    capture_reason TEXT NOT NULL,
    submitted BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY(user_id,program_id)
);
ALTER TABLE application_program_snapshots ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_program_snapshots FORCE ROW LEVEL SECURITY;
CREATE POLICY application_snapshots_service ON application_program_snapshots FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());

CREATE FUNCTION application_snapshot_payload(p_id UUID) RETURNS JSONB LANGUAGE sql STABLE AS $$
    SELECT jsonb_build_object('id',p.id,'title',p.title,'summary',p.summary,
      'program_type',p.program_type,'provider_name',p.provider_name,'official_url',p.official_url,
      'application_start_at',p.application_start_at,'application_end_at',p.application_end_at,'deadline_at',p.deadline_at,
      'application_method',p.application_method,'conditions',p.normalized_payload->>'conditions',
      'benefit_amount_monthly',p.benefit_amount_monthly,'benefit_amount_once',p.benefit_amount_once,
      'benefit_amount_semester',p.benefit_amount_semester,
      'required_documents',COALESCE((SELECT jsonb_agg(jsonb_build_object('document_name',d.document_name,'description',d.description,'is_required',d.is_required) ORDER BY d.sort_order) FROM program_documents d WHERE d.program_id=p.id),'[]'::jsonb))
    FROM programs p WHERE p.id=p_id;
$$;

CREATE FUNCTION capture_application_program() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE is_submitted BOOLEAN := NEW.state IN ('applied','waiting','received');
BEGIN
    INSERT INTO application_program_snapshots(user_id,program_id,program,capture_reason,submitted)
    VALUES(NEW.user_id,NEW.program_id,application_snapshot_payload(NEW.program_id),
        CASE WHEN is_submitted THEN 'submission' ELSE 'first_saved' END,is_submitted)
    ON CONFLICT(user_id,program_id) DO UPDATE SET program=excluded.program,captured_at=now(),
        capture_reason=excluded.capture_reason,submitted=true
    WHERE NOT application_program_snapshots.submitted AND is_submitted;
    RETURN NEW;
END;
$$;
CREATE TRIGGER capture_application_program AFTER INSERT OR UPDATE OF state ON user_program_states
FOR EACH ROW EXECUTE FUNCTION capture_application_program();
-- Historical contents before this migration cannot be reconstructed. Label that honestly.
INSERT INTO application_program_snapshots(user_id,program_id,program,capture_reason,submitted)
SELECT s.user_id,s.program_id,application_snapshot_payload(s.program_id),'migration_current',
 s.applied_at IS NOT NULL OR s.state IN ('applied','waiting','received') FROM user_program_states s;

CREATE FUNCTION expire_all_programs() RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE expired BIGINT;
BEGIN
    UPDATE programs SET is_active=false, program_status='closed', updated_at=now()
    WHERE is_active AND LEAST(benefit_valid_until,deadline_at,application_end_at)<=now();
    GET DIAGNOSTICS expired=ROW_COUNT;
    RETURN expired;
END;
$$;
REVOKE EXECUTE ON FUNCTION expire_all_programs() FROM PUBLIC;
