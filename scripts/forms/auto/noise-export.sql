-- Candidate (and automatically rejected) form templates with the facts the
-- collector's noise rule needs. Same query as NOISE_ROWS_SQL in
-- crates/worker/src/application_resources/repository.rs, plus `hashed`.
-- Read only. Feeds the `noise_dry_run` test:
--   ssh … "sudo $MAZIMI_SQL_RO" < noise-export.sql > noise.json
--   NOISE_DRY_RUN=noise.json cargo test -p mazimi-worker --lib noise_dry_run -- --ignored --nocapture
\t on
\pset format unaligned
begin read only;
with shared as (
    select url, count(distinct program_id) as programs from program_application_resources
     where kind = 'template' group by url),
  ended as (
    select o.url, o.program_id, q.title from program_application_resources o join programs q on q.id = o.program_id
     where o.kind = 'template' and (q.program_status = 'closed'
       or least(q.benefit_valid_until, q.deadline_at, q.application_end_at) <= now()))
select coalesce(json_agg(x order by x.id), '[]'::json) from (
  select r.id, r.revision, r.label, r.discovery_depth, p.title as program_title,
    r.content_hash is not null as hashed,
    (select s.label from program_application_resources s
      where s.program_id = r.program_id and s.url = r.source_url and s.kind <> 'template'
      order by s.discovery_depth, s.id limit 1) as source_label,
    coalesce(sh.programs, 0) as programs_sharing_url,
    array(select distinct e.title from ended e where e.url = r.url and e.program_id <> r.program_id order by e.title) as ended_titles,
    r.status = 'rejected' as auto_rejected
  from program_application_resources r join programs p on p.id = r.program_id
  left join shared sh on sh.url = r.url
  where r.kind = 'template' and (r.status = 'candidate'
    or (r.status = 'rejected' and r.reviewed_by is null and starts_with(coalesce(r.check_error, ''), 'auto_rejected:')))
) x;
rollback;
