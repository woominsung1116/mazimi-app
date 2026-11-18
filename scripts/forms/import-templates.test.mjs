// import-templates.mjs against a fake admin API: the HWP/HWPX path uploads the
// converted PDF for the verified official hash, then saves the boxes, and stops
// when the official file changed. Run: node --test scripts/forms/import-templates.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const sha = b => createHash('sha256').update(b).digest('hex');
const PROGRAM = '3f0c2b1a-0000-4000-8000-000000000001';
const RID = '3f0c2b1a-0000-4000-8000-000000000002';
const SOURCE = 'a'.repeat(64);
const PDF = Buffer.from('%PDF-1.7\n% converted blank form\n%%EOF\n');
const CONVERTER = 'rhwp 0.8.6 export-pdf svg; image sha256:test';

const run = mkdtempSync(join(tmpdir(), 'import-templates-'));
const dir = join(run, 'out');  // <run>/out/import-manifest.json, as verdicts.py writes it
mkdirSync(dir); mkdirSync(join(run, 'keys')); mkdirSync(join(run, 'review'));
const template = { version: 1, content_hash: sha(PDF), page_count: 1, source: 'admin',
  fields: [{ id: 'p1-f01', page: 0, rect: { x: 0.1, y: 0.1, w: 0.3, h: 0.03 }, type: 'text', label: '성명', mapping: 'name', required: true }],
  derived: { source_sha256: SOURCE, converter: CONVERTER } };
const entry = { slug: 'f', program: '테스트 장학금', notice_url: 'https://x.ac.kr/notice', file_url: 'https://x.ac.kr/f.hwp', format: 'hwp',
  sha256: SOURCE, status: 'template_ready', template: 'f.template.json', derived_pdf: { sha256: sha(PDF), byte_size: PDF.length, converter: CONVERTER } };
writeFileSync(join(dir, 'f.template.json'), JSON.stringify(template));
writeFileSync(join(dir, 'manifest.json'), JSON.stringify({ version: 1, forms: [entry] }));
writeFileSync(join(dir, 'f.pdf'), PDF);
writeFileSync(join(dir, 'programs.json'), JSON.stringify({ f: PROGRAM }));

// Fake server state and request log.
const state = { resource: null, stored: null, pdf: null, revision: null };
const log = [];
const server = createServer((req, res) => {
  const chunks = [];
  req.on('data', c => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    const url = new URL(req.url, 'http://localhost');
    log.push({ method: req.method, path: url.pathname, query: Object.fromEntries(url.searchParams), type: req.headers['content-type'], body });
    const json = (code, v) => { res.writeHead(code, { 'content-type': 'application/json' }); res.end(JSON.stringify(v)); };
    if (req.headers.authorization !== 'Bearer t') return json(401, { code: 'unauthorized' });
    if (url.pathname === `/api/v1/admin/programs/${PROGRAM}/application-resources`) {
      if (req.method === 'POST') { state.resource = { id: RID, kind: 'template', url: entry.file_url, status: 'candidate', content_hash: null, file_format: null, revision: 1, program_fingerprint: 'fp' }; return json(200, state.resource); }
      return json(200, { program_title: '테스트', items: state.resource ? [state.resource] : [] });
    }
    if (url.pathname === `/api/v1/admin/application-resources/${RID}` && req.method === 'PUT') {
      const r = JSON.parse(body);
      if (r.decision !== 'verified' || r.expected_revision !== state.resource.revision || !/reviewer=claude-reviewer/.test(r.note)) return json(409, { code: 'conflict' });
      Object.assign(state.resource, { status: 'verified', revision: state.resource.revision + 1 });
      return json(200, state.resource);
    }
    if (url.pathname === `/api/v1/admin/application-resources/${RID}/form-pdf` && req.method === 'PUT') {
      if (req.headers['content-type'] !== 'application/pdf' || url.searchParams.get('source_sha256') !== state.resource.content_hash) return json(409, { code: 'resource_changed' });
      state.pdf = { content_hash: sha(body), source_sha256: url.searchParams.get('source_sha256'), converter: url.searchParams.get('converter'), stale: false };
      return json(200, { content_hash: state.pdf.content_hash, byte_size: body.length });
    }
    if (url.pathname === `/api/v1/admin/application-resources/${RID}/form-template`) {
      if (req.method === 'GET') return json(200, { template: state.stored, revision: state.revision, stale: false, derived_pdf: state.pdf });
      const put = JSON.parse(body);
      if (put.expected_revision !== state.revision || put.template.content_hash !== state.pdf?.content_hash) return json(409, { code: 'resource_changed' });
      state.stored = put.template; state.revision = (state.revision ?? 0) + 1;
      return json(200, { revision: state.revision });
    }
    json(404, { code: 'not_found' });
  });
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
process.env.MAZIMI_ADMIN_TOKEN = 't';
process.argv = [process.argv[0], 'import-templates.mjs', '--apply', '--api', `http://127.0.0.1:${server.address().port}`,
  '--manifest', join(dir, 'manifest.json'), '--programs', join(dir, 'programs.json'), '--derived', dir];
const { handle, derivedPdf, sameTemplate, reviewNote } = await import('./import-templates.mjs');
test.after(() => server.close());

// The reviewed run: packet key, the reviewer's verdict, both bound by sha256.
const TSHA = sha(readFileSync(join(dir, 'f.template.json')));
writeFileSync(join(run, 'keys', 'batch.key.json'), JSON.stringify({ _nonce: 'n0', A: { slug: 'f', template: 'x', template_sha256: TSHA } }));
const VERDICT = JSON.stringify({ nonce: 'n0', A: { verdict: 'pass', findings: [] } });
writeFileSync(join(run, 'review', 'batch.verdict.json'), VERDICT);
const REVIEW = { reviewer: 'claude-reviewer', run: 'run-1', verdict_sha256: sha(Buffer.from(VERDICT)), template_sha256: TSHA };

test('HWP form: queue, verify with the recorded review (no human step), upload the converted PDF, save boxes, then stop on a changed file', async () => {
  assert.match(await handle(entry), /queued candidate/);
  assert.match(await handle(entry), /not probed yet/);
  // Worker probe happens outside the script.
  Object.assign(state.resource, { content_hash: SOURCE, file_format: 'hwp' });
  log.length = 0;
  assert.match(await handle(entry), /carries no passing review/, 'an unreviewed entry is refused, never left waiting');
  assert.ok(!log.some(r => r.method === 'PUT'));
  assert.equal(reviewNote({ ...entry, review: { ...REVIEW, reviewer: 'someone' } }), null);
  assert.equal(reviewNote({ ...entry, review: { ...REVIEW, verdict_sha256: 'e'.repeat(64) } }), null, 'a verdict file that is not the reviewed one');
  assert.equal(reviewNote({ ...entry, review: { ...REVIEW, template_sha256: 'e'.repeat(64) } }), null, 'a template that is not the reviewed one');
  assert.equal(reviewNote({ ...entry, template: '../f.template.json', review: REVIEW }), null);
  log.length = 0;
  assert.match(await handle({ ...entry, review: REVIEW }), /saved revision 1/);
  const verify = log.find(r => r.method === 'PUT' && r.path.endsWith(RID));
  assert.ok(JSON.parse(verify.body).note.includes(`verdict_sha256=${REVIEW.verdict_sha256} template_sha256=${TSHA}`), 'the review is the verifier');
  assert.equal(state.resource.status, 'verified');
  const upload = log.find(r => r.path.endsWith('/form-pdf'));
  assert.equal(upload.type, 'application/pdf');
  assert.deepEqual(upload.query, { source_sha256: SOURCE, converter: CONVERTER });
  assert.ok(upload.body.equals(PDF), 'uploads exactly the judged PDF bytes');
  const order = log.map(r => `${r.method} ${r.path.split('/').pop()}`);
  assert.ok(order.indexOf('PUT form-pdf') < order.indexOf('PUT form-template'), 'PDF before boxes');
  assert.deepEqual(state.stored.derived, template.derived);

  assert.match(await handle(entry), /up to date/);
  // The agency replaced the HWP: nothing is written.
  state.resource.content_hash = 'b'.repeat(64);
  log.length = 0;
  assert.match(await handle(entry), /file changed.*reconvert/);
  assert.ok(!log.some(r => r.method === 'PUT'));
});

test('replacing a stored PDF that live boxes sit on sends the hash the importer saw', async () => {
  // Server holds an older conversion with boxes on it.
  const old = { content_hash: 'c'.repeat(64), source_sha256: SOURCE, converter: 'rhwp 0.8.5 old', stale: false };
  Object.assign(state.resource, { content_hash: SOURCE });
  state.pdf = old; state.stored = { ...template, content_hash: old.content_hash, derived: { source_sha256: SOURCE, converter: old.converter } };
  log.length = 0;
  assert.match(await handle(entry), /saved revision/);
  const upload = log.find(r => r.path.endsWith('/form-pdf'));
  assert.equal(upload.query.expected_content_hash, old.content_hash);
});

test('a local PDF that is not the one the boxes were drawn on is refused before any request', () => {
  assert.deepEqual(derivedPdf(entry, template, dir).bytes.length, PDF.length);
  writeFileSync(join(dir, 'other.pdf'), Buffer.from('%PDF-1.7\n%%EOF\n'));
  assert.match(derivedPdf({ ...entry, slug: 'other' }, template, dir).error, /differs/);
  assert.match(derivedPdf({ ...entry, slug: 'absent' }, template, dir).error, /missing/);
});

test('a template whose checkbox groups changed is not "up to date"', () => {
  const box = (id, x, group) => ({ id, page: 0, rect: { x, y: 0.5, w: 0.02, h: 0.02 }, type: 'checkbox', label: id, mapping: null, required: false, group });
  const base = { ...template, fields: [...template.fields, box('a', 0.1, 'g1'), box('b', 0.2, 'g1')], groups: [{ id: 'g1', mode: 'multi', label: '구분' }] };
  assert.ok(sameTemplate(base, structuredClone(base)));
  assert.ok(!sameTemplate(base, { ...base, groups: [{ id: 'g1', mode: 'single', label: '구분' }] }), 'mode change');
  assert.ok(!sameTemplate(base, { ...base, groups: [{ id: 'g1', mode: 'multi', label: '구분(택1)' }] }), 'label change');
  // No groups on either side compares equal whether the key is absent or empty.
  const { groups, ...plain } = template;
  assert.ok(sameTemplate(plain, { ...plain, groups: [] }));
});
