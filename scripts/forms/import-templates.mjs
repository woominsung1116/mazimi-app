#!/usr/bin/env node
// Registers prepared form templates (docs/validation/form-templates) with the admin API.
//
//   node scripts/forms/import-templates.mjs --programs programs.json [--api URL] [--apply]
//
// programs.json maps a manifest slug to the program UUID it belongs to:
//   { "busan-scholarship": "3f0c…" }  (a human picks the program; this script never guesses)
//
// Per form, in order:
//   1. no `template` resource for file_url yet  -> queue it as a candidate (POST, kind=template)
//   2. resource exists but is not `verified`     -> verify it with the review recorded in the
//                                                   manifest entry (scripts/forms/auto/verdicts.py:
//                                                   reviewer, verdict sha256, template sha256) as the
//                                                   verifier note. An entry with no passing review is
//                                                   refused; there is no human approval step.
//   3. verified, but the worker-recorded hash differs from the template's content_hash
//                                                -> stop for that form (the agency file changed or
//                                                   was not probed yet); redraw from the new file.
//   4. verified and same hash                    -> PUT the template (expected_revision = current).
//
// HWP/HWPX forms (manifest derived_pdf): the template sits on a PDF Mazimi converted from the
// official file (scripts/forms/hwp). Step 3 compares the worker hash with derived.source_sha256;
// before step 4 the converted PDF (read from --derived, sha256-checked) is uploaded with
// PUT .../form-pdf unless the server already holds exactly that PDF for the current file.
//
// Dry run by default: with no token it only prints the plan from the manifest; with
// MAZIMI_ADMIN_TOKEN it also reads current state (GET only). Writes need --apply AND the token.
// The token is read from the environment only and is never printed.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual, parseArgs } from 'node:util';
import { createHash } from 'node:crypto';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '../..');
const { values: args } = parseArgs({
  options: {
    manifest: { type: 'string', default: join(ROOT, 'docs/validation/form-templates/manifest.json') },
    programs: { type: 'string' },
    api: { type: 'string', default: process.env.MAZIMI_API_URL ?? 'http://localhost:8080' },
    apply: { type: 'boolean', default: false },
    only: { type: 'string' },
    derived: { type: 'string', default: join(ROOT, '.omc/forms/hwp/rhwp/pdf') },
  },
});

const manifestPath = args.manifest;
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const programs = args.programs ? JSON.parse(readFileSync(args.programs, 'utf8')) : {};
const token = process.env.MAZIMI_ADMIN_TOKEN?.trim() || null;
const api = args.api.replace(/\/+$/, '');
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

if (args.apply && !token) {
  console.error('--apply needs MAZIMI_ADMIN_TOKEN in the environment.');
  process.exit(2);
}
// Any request that carries the token (dry runs read state too) must stay off plain http.
if (token && !/^https:\/\//.test(api) && !/^http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/.test(api)) {
  console.error('--api must be https (or http://localhost for a local stack) when MAZIMI_ADMIN_TOKEN is set.');
  process.exit(2);
}

async function call(method, path, body, contentType = 'application/json') {
  const raw = body instanceof Uint8Array;
  const res = await fetch(`${api}${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, ...(body ? { 'Content-Type': contentType } : {}) },
    body: body ? (raw ? body : JSON.stringify(body)) : undefined,
    redirect: 'error', // a redirect could turn a template PUT into another endpoint
  });
  const text = await res.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* non-JSON error body */ }
  if (!res.ok) {
    const code = json?.code ?? json?.error ?? text.slice(0, 120);
    throw new Error(`${method} ${path} -> ${res.status} ${code}`);
  }
  return json;
}

/** Same definition regardless of key order (the server returns keys in its own order). */
export function sameTemplate(stored, local) {
  if (!stored) return false;
  const pick = t => ({ content_hash: t.content_hash, page_count: t.page_count, source: t.source, fields: t.fields, groups: t.groups ?? [], derived: t.derived ?? null });
  return isDeepStrictEqual(pick(stored), pick(local));
}

/** The converted PDF the boxes were drawn on, or an error message. */
export function derivedPdf(entry, template, dir = args.derived) {
  let bytes;
  try { bytes = readFileSync(join(dir, `${entry.slug}.pdf`)); } catch { return { error: `converted PDF missing in ${dir}; run scripts/forms/hwp/convert.sh` }; }
  if (createHash('sha256').update(bytes).digest('hex') !== template.content_hash) return { error: 'local converted PDF differs from the template; reconvert (rhwp is byte-reproducible) or redraw' };
  return { bytes: new Uint8Array(bytes) };
}

/**
 * The verifier note for an entry that verdicts.py made importable, or null. The evidence is
 * re-checked here, not trusted from the manifest: the verdict file in the run's review/ dir
 * hashes to the recorded sha, echoes the packet nonce and passes this template with zero
 * findings, and the template file hashes to what was reviewed (run dir = the manifest's
 * grandparent: <run>/out/import-manifest.json). The server path is api::form_auto_import.
 */
export function reviewNote(entry, runDir = join(dirname(manifestPath), '..')) {
  const r = entry.review;
  const hex = v => /^[0-9a-f]{64}$/.test(v ?? '');
  if (!r || !['claude-reviewer', 'judge'].includes(r.reviewer) || !hex(r.verdict_sha256) || !hex(r.template_sha256)) return null;
  if (!/^[a-z0-9-]{1,64}$/.test(entry.slug) || entry.template !== `${entry.slug}.template.json`) return null;
  const file = r.reviewer === 'judge' ? 'batch.judge.json' : 'batch.verdict.json';
  try {
    const vbytes = readFileSync(join(runDir, 'review', file));
    if (createHash('sha256').update(vbytes).digest('hex') !== r.verdict_sha256) return null;
    const verdicts = JSON.parse(vbytes);
    const key = JSON.parse(readFileSync(join(runDir, 'keys', 'batch.key.json'), 'utf8'));
    const letter = Object.keys(key).find(k => !k.startsWith('_') && key[k].slug === entry.slug);
    const v = letter && verdicts[letter];
    if (!v || verdicts.nonce !== key._nonce || v.verdict !== 'pass' || !Array.isArray(v.findings) || v.findings.length) return null;
    const tsha = createHash('sha256').update(readFileSync(join(runDir, 'out', entry.template))).digest('hex');
    if (tsha !== r.template_sha256 || tsha !== key[letter].template_sha256) return null;
  } catch {
    return null;
  }
  return `자동 검토 통과: reviewer=${r.reviewer} run=${r.run} verdict_sha256=${r.verdict_sha256} template_sha256=${r.template_sha256}`;
}

export async function handle(entry) {
  const tag = entry.slug.padEnd(34);
  if (entry.status !== 'template_ready') return `${tag} skip (${entry.status})`;
  if (!entry.notice_url) return `${tag} blocked: no official notice page recorded; find it and add notice_url first`;
  if (!/^https:\/\//.test(entry.file_url) || !/^https:\/\//.test(entry.notice_url)) {
    return `${tag} blocked: the API accepts only https URLs (file or notice is http)`;
  }
  const programId = programs[entry.slug];
  if (!programId) return `${tag} needs a program id in --programs`;
  if (!UUID.test(programId)) return `${tag} invalid program id`;
  if (!/^[a-z0-9-]{1,64}\.template\.json$/.test(entry.template)) return `${tag} blocked: template file name must be <slug>.template.json`;
  const template = JSON.parse(readFileSync(join(dirname(manifestPath), entry.template), 'utf8'));
  const derived = !!entry.derived_pdf;
  // Official bytes the worker must have verified: the agency PDF, or the HWP/HWPX the PDF came from.
  const official = derived ? template.derived?.source_sha256 : template.content_hash;
  if (official !== entry.sha256 || (derived && template.content_hash !== entry.derived_pdf.sha256)) return `${tag} manifest/template hash mismatch, regenerate`;
  const pdf = derived ? derivedPdf(entry, template) : null;
  if (pdf?.error) return `${tag} blocked: ${pdf.error}`;
  if (!token) return `${tag} plan: queue ${entry.file_url} as template candidate for program ${programId}, then ${derived ? 'upload the converted PDF and ' : ''}PUT ${template.fields.length} fields once the worker probed it`;

  const list = await call('GET', `/api/v1/admin/programs/${programId}/application-resources`);
  let resource = list.items.find(r => r.kind === 'template' && r.url === entry.file_url);
  if (!resource) {
    if (!args.apply) return `${tag} would queue candidate (POST) for "${list.program_title}"`;
    resource = await call('POST', `/api/v1/admin/programs/${programId}/application-resources`, {
      kind: 'template', url: entry.file_url, source_url: entry.notice_url, label: `${entry.program} 신청서`.slice(0, 200),
    });
    return `${tag} queued candidate ${resource.id}; the worker probes it, the next run verifies it with the recorded review`;
  }
  if (!UUID.test(String(resource.id))) return `${tag} unexpected resource id from the server; stopping for this form`;
  if (resource.status === 'rejected') return `${tag} resource ${resource.id} was rejected in review: nothing to do (re-queueing returns the same row)`;
  if (resource.status === 'unavailable') return `${tag} resource ${resource.id} is unavailable (file gone): find the new official file`;
  if (!resource.content_hash || !resource.file_format) return `${tag} resource ${resource.id} not probed yet: wait for the worker`;
  if (resource.file_format !== entry.format || resource.content_hash !== official) {
    return `${tag} resource ${resource.id} file changed (hash/format differs from the template): ${derived ? 'reconvert and re-judge' : 'redraw from'} the new file`;
  }
  if (resource.status !== 'verified') {
    const note = reviewNote(entry);
    if (!note) return `${tag} resource ${resource.id} is ${resource.status} and the entry carries no passing review: run the review (scripts/forms/auto)`;
    if (!args.apply) return `${tag} would verify resource ${resource.id} with the recorded review, then save the template`;
    resource = await call('PUT', `/api/v1/admin/application-resources/${resource.id}`, {
      decision: 'verified', expected_revision: resource.revision, program_fingerprint: resource.program_fingerprint,
      access: 'direct', same_program_confirmed: true, current_intake_confirmed: true, destination_confirmed: true, note,
    });
  }
  let current = await call('GET', `/api/v1/admin/application-resources/${resource.id}/form-template`);
  const pdfStored = !derived || (current.derived_pdf?.content_hash === template.content_hash && current.derived_pdf.converter === template.derived.converter && !current.derived_pdf.stale);
  if (pdfStored && !current.stale && sameTemplate(current.template, template)) return `${tag} up to date (revision ${current.revision})`;
  if (!args.apply) return `${tag} would ${pdfStored ? '' : 'upload the converted PDF and '}PUT ${template.fields.length} fields (expected_revision ${current.revision ?? 'none'})`;
  if (!pdfStored) {
    const q = new URLSearchParams({ source_sha256: official, converter: template.derived.converter });
    // Replacing a PDF that live boxes sit on needs the hash we saw (server optimistic lock).
    if (current.derived_pdf?.content_hash) q.set('expected_content_hash', current.derived_pdf.content_hash);
    const up = await call('PUT', `/api/v1/admin/application-resources/${resource.id}/form-pdf?${q}`, pdf.bytes, 'application/pdf');
    if (up.content_hash !== template.content_hash) return `${tag} server stored another PDF hash (${String(up.content_hash).slice(0, 12)}); stopping`;
    current = await call('GET', `/api/v1/admin/application-resources/${resource.id}/form-template`);
  }
  const saved = await call('PUT', `/api/v1/admin/application-resources/${resource.id}/form-template`, {
    template, expected_revision: current.revision ?? null,
  });
  return `${tag} saved revision ${saved.revision}`;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  console.log(`${args.apply ? 'APPLY' : 'DRY RUN'} · ${token ? api : 'no token: manifest only'}`);
  let failed = 0;
  for (const entry of manifest.forms) {
    if (args.only && entry.slug !== args.only) continue;
    try {
      console.log(await handle(entry));
    } catch (e) {
      failed++;
      console.log(`${entry.slug.padEnd(34)} error: ${e.message}`);
    }
  }
  process.exit(failed ? 1 : 0);
}
