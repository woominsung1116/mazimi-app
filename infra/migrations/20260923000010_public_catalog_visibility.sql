-- Query-time expiry prevents exposure even while the maintenance worker is down.
CREATE VIEW current_programs WITH (security_invoker=true) AS
SELECT p.* FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));
