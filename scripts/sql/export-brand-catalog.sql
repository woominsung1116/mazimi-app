-- Read-only evidence export. Seed-only DB results are NOT production inventory.
-- Run using psql -At; output is a single JSON object suitable for review.
SELECT json_build_object(
  'targets', (
    SELECT json_agg(row_to_json(t) ORDER BY coverage_category, display_name, target_key)
    FROM (
      SELECT d.target_key, d.display_name, d.coverage_category, d.discovery_priority,
             d.official_domain, d.domain_status, d.is_active,
             (SELECT count(DISTINCT p.id) FROM program_brands pb
              JOIN programs p ON p.id=pb.program_id
              WHERE pb.brand_id=d.brand_id AND p.program_type='corporate_benefit') AS seeded_benefits
      FROM brand_discovery_targets d
    ) t
  ),
  'extra_brands', (
    SELECT json_agg(row_to_json(b) ORDER BY name)
    FROM (
      SELECT b.name, b.industry_major, b.homepage_url,
             (SELECT count(DISTINCT p.id) FROM program_brands pb JOIN programs p ON p.id=pb.program_id
              WHERE pb.brand_id=b.id AND p.program_type='corporate_benefit') AS seeded_benefits
      FROM brands b WHERE NOT EXISTS(SELECT 1 FROM brand_discovery_targets d WHERE d.brand_id=b.id)
    ) b
  ),
  'linked_corporate_programs', (SELECT count(DISTINCT p.id) FROM programs p JOIN program_brands pb ON pb.program_id=p.id WHERE p.program_type='corporate_benefit'),
  'brands', (SELECT count(*) FROM brands),
  'migrations', (SELECT count(*) FROM _sqlx_migrations WHERE success)
);
