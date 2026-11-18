-- Form-template candidates for the daily auto pipeline (read only).
-- Run: ssh … "sudo $MAZIMI_SQL_RO" < candidates.sql > candidates.raw
-- Prints one JSON array. Every `template` resource whose bytes the collector
-- fetched (content_hash recorded) is listed with the facts select.py needs to
-- accept or reject it; nothing is filtered silently here except rows with no
-- recorded bytes, which cannot be checked against a download.
\t on
\pset format unaligned
begin read only;
with approved as (
  -- Same predicate as the collector's APPROVED_SCOPE_SQL (crates/worker/src/application_resources/fetch.rs).
  select d.domain, d.allow_subdomains, d.allowed_path_prefixes
  from benefit_source_domains d
  where d.is_active and d.verification_status = 'verified' and d.crawl_allowed
    and d.compliance_status = 'approved' and d.robots_status = 'allowed'
    and d.approval_expires_at > now() and d.collection_mode = 'direct_public'
),
rows as (
  select r.*, rtrim(lower(substring(r.url from '^[A-Za-z][A-Za-z0-9+.-]*://([^/?#:@]+)')), '.') as host,
         substring(r.url from '^[A-Za-z][A-Za-z0-9+.-]*://[^/?#]+(/[^?#]*)') as path
  from program_application_resources r
  where r.kind = 'template' and r.content_hash is not null
)
select coalesce(json_agg(x order by x.host, x.program_title, x.id), '[]'::json) from (
  select r.id, r.status, r.check_status, r.check_error, r.last_failure_code, r.file_format, r.content_hash,
         r.source_hash, r.byte_size, r.checked_at, r.valid_until, r.url, r.source_url, r.label, r.discovery_depth,
         r.revision, r.program_fingerprint, r.host, r.path,
         p.id as program_id, p.title as program_title, p.provider_name, p.program_status, p.is_active,
         p.application_start_at, p.application_end_at, p.deadline_at, p.benefit_valid_until, p.official_url,
         -- Twin of currently_available_sql (crates/core/src/lib.rs).
         (p.is_active and p.program_status in ('open', 'active', 'always_open')
           and (p.application_start_at is null or p.application_start_at <= now())
           and (least(p.benefit_valid_until, p.deadline_at, p.application_end_at) is null
                or least(p.benefit_valid_until, p.deadline_at, p.application_end_at) > now())) as available,
         exists (select 1 from approved a
                 where (a.domain = r.host or (a.allow_subdomains and right(r.host, length(a.domain) + 1) = '.' || a.domain))
                   -- Twin of path_is_allowed (crates/core/src/source_policy.rs).
                   and exists (select 1 from unnest(a.allowed_path_prefixes) pre
                               where pre = '/' or r.path = pre
                                  or (starts_with(r.path, pre) and (right(pre, 1) = '/' or substr(r.path, length(pre) + 1, 1) = '/'))))
           as in_approved_scope,
         -- The same file URL attached to several programs is a board-wide attachment, not this program's form.
         (select count(distinct o.program_id) from program_application_resources o where o.kind = 'template' and o.url = r.url) as programs_sharing_url,
         -- The same file URL attached to a program that is no longer available: an earlier round's form.
         (select count(distinct o.program_id) from program_application_resources o join programs q on q.id = o.program_id
           where o.url = r.url and o.program_id <> r.program_id
             and not (q.is_active and q.program_status in ('open', 'active', 'always_open')
                      and (least(q.benefit_valid_until, q.deadline_at, q.application_end_at) is null
                           or least(q.benefit_valid_until, q.deadline_at, q.application_end_at) > now()))) as closed_programs_sharing_url,
         -- An operator rolled back an automatic import of these exact bytes: never tried again.
         exists (select 1 from form_auto_imports i where i.action = 'rollback' and i.resource_id = r.id
                 and i.official_sha256 = r.content_hash) as rolled_back,
         t.revision as template_revision, t.content_hash as template_content_hash,
         t.source_sha256 as template_source_sha256,
         now() as queried_at
  from rows r
  join programs p on p.id = r.program_id
  left join application_form_templates t on t.resource_id = r.id
) x;
rollback;
