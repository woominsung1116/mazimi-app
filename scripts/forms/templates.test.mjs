// Every prepared template must pass the app's own parser, carry no key the
// server would refuse (serde deny_unknown_fields) and agree with the manifest.
// Run: node --test scripts/forms/templates.test.mjs   (needs apps/mobile deps)
import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '../..');
const DIR = join(ROOT, 'docs/validation/form-templates');
const require = createRequire(join(ROOT, 'apps/mobile/package.json'));
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, filename);
const F = require(join(ROOT, 'apps/mobile/lib/formFill/fields.ts'));

// Mirror of OTHER_PERSON in scripts/forms/propose_fields.py (broader than the app's own list).
const OTHER_PERSON = /보호자|부모|부친|모친|아버지|어머니|배우자|대리인|추천인|담당자|교수|지도교사|가구원|세대원|임대인|학과장|확인자|대표자|관계인|가족|동거인|원가구|독립가구|기업|회사|사업주|고용주|수령인|위임받는|수임인|면접관|채용|부양자|구성원|타인|직계존속|비상연락/;
const ctx = { profile: { school_name: '테스트대학교', school_year: 3, birth_year: 2000 }, remembered: { name: '홍길동', phone: '010-0000-0000', address: '테스트시', email: 'a@example.org' }, today: new Date('2026-10-12T00:00:00Z') };
const TEMPLATE_KEYS = ['version', 'content_hash', 'page_count', 'source', 'fields'];
const FIELD_KEYS = ['id', 'page', 'rect', 'type', 'label', 'mapping', 'required', 'format', 'font_size', 'acro_name', 'group', 'part'];
const manifest = JSON.parse(readFileSync(join(DIR, 'manifest.json'), 'utf8'));
const conversions = JSON.parse(readFileSync(join(ROOT, 'scripts/forms/hwp/conversions.json'), 'utf8'));
// Converted PDFs are not committed (like the official files); checked when present locally.
const DERIVED_DIR = process.env.FORMS_DERIVED_DIR ?? join(ROOT, '.omc/forms/hwp/rhwp/pdf');
const files = readdirSync(DIR).filter(f => f.endsWith('.template.json'));

test('there are prepared templates and each one is listed in the manifest', () => {
  assert.ok(files.length > 0);
  const listed = manifest.forms.filter(e => e.status === 'template_ready').map(e => e.template);
  assert.deepEqual([...listed].sort(), [...files].sort());
});

test('manifest entries carry provenance and a reason when no template exists', () => {
  for (const e of manifest.forms) {
    for (const key of ['slug', 'program', 'file_url', 'retrieved_at', 'format', 'sha256', 'byte_size', 'status']) assert.ok(e[key] !== undefined && e[key] !== null && e[key] !== '', `${e.slug}: ${key}`);
    // A missing notice page must be explained; the import script refuses it until a human fills it in.
    if (!e.notice_url) assert.match(e.notes, /notice_url/, `${e.slug}: why notice_url is missing`);
    assert.match(e.sha256, /^[0-9a-f]{64}$/);
    assert.ok(['template_ready', 'hwp_unconverted', 'hwp_unfaithful', 'skipped'].includes(e.status), e.slug);
    if (e.status !== 'template_ready') assert.ok(e.reason, `${e.slug}: reason`);
    assert.match(new URL(e.file_url).hostname, /\.(go|or|ac)\.kr$/, `${e.slug}: official domain`);
  }
});

test('HWP/HWPX templates exist only for conversions judged faithful, on accepted pages, bound to both files', () => {
  const identity = conversions.converter.identity;
  assert.match(identity, /^[\x20-\x7e]{1,200}$/, 'converter identity fits the server rule');
  for (const e of manifest.forms.filter(f => f.format.startsWith('hwp'))) {
    const c = conversions.forms[e.slug];
    assert.ok(c, `${e.slug}: every HWP/HWPX form has a recorded verdict`);
    assert.equal(c.source_sha256, e.sha256, `${e.slug}: verdict is about the recorded official bytes`);
    if (c.verdict !== 'faithful') {
      assert.equal(e.status, 'hwp_unfaithful', e.slug);
      assert.ok(c.reason && e.reason.includes(c.reason), `${e.slug}: rejection reason carried to the manifest`);
      continue;
    }
    assert.equal(e.status, 'template_ready', e.slug);
    const t = JSON.parse(readFileSync(join(DIR, e.template), 'utf8'));
    assert.deepEqual(t.derived, { source_sha256: e.sha256, converter: identity });
    assert.equal(t.content_hash, c.pdf_sha256);
    assert.equal(e.derived_pdf.sha256, c.pdf_sha256);
    assert.equal(e.derived_pdf.converter, identity);
    const accepted = new Set(c.pages.map(p => p.page));
    for (const p of c.pages) assert.ok(['preview', 'two_engines'].includes(p.evidence), `${e.slug} p${p.page}`);
    for (const f of t.fields) assert.ok(accepted.has(f.page + 1), `${e.slug} ${f.id}: page ${f.page + 1} was not judged faithful`);
    const pdf = join(DERIVED_DIR, `${e.slug}.pdf`);
    if (existsSync(pdf)) {
      const bytes = readFileSync(pdf);
      assert.equal(createHash('sha256').update(bytes).digest('hex'), t.content_hash, `${e.slug}: local converted PDF is the one the boxes were drawn on`);
      assert.equal(bytes.length, e.derived_pdf.byte_size);
    }
  }
});

for (const file of files) {
  test(`${file} passes the app parser and the server's field whitelist`, () => {
    const raw = JSON.parse(readFileSync(join(DIR, file), 'utf8'));
    const entry = manifest.forms.find(e => e.template === file);
    // A derived template is bound to the converted PDF and, through derived, to the official file.
    assert.equal(raw.content_hash, entry.derived_pdf?.sha256 ?? entry.sha256, 'bound to the recorded file bytes');
    assert.deepEqual(Object.keys(raw).sort(), [...TEMPLATE_KEYS, ...(entry.derived_pdf ? ['derived'] : []), ...(raw.groups ? ['groups'] : [])].sort());
    // Checkbox groups: never an empty list, each with 2+ checkbox options on one page.
    if (raw.groups) assert.ok(raw.groups.length > 0, 'an empty groups list is omitted, not stored');
    for (const g of raw.groups ?? []) {
      const options = raw.fields.filter(f => f.group === g.id);
      assert.ok(options.length >= 2, `${g.id}: at least two options`);
      assert.ok(options.every(f => f.page === options[0].page && f.type === 'checkbox'), `${g.id}: checkboxes on one page`);
    }
    const parsed = F.parseTemplate(raw, entry.sha256, entry.format);
    assert.equal(parsed.fields.length, entry.field_count);
    assert.equal(raw.page_count, entry.pages);
    for (const f of raw.fields) {
      for (const key of Object.keys(f)) assert.ok(FIELD_KEYS.includes(key), `${f.id}: unknown key ${key}`);
      assert.deepEqual(Object.keys(f.rect).sort(), ['h', 'w', 'x', 'y']);
      // A mapping is the app's own reading of the label, so suggestions and
      // sensitivity follow one rule set (apps/mobile/lib/formFill/fields.ts).
      if (f.mapping !== null) assert.equal(f.mapping, F.classifyLabel(f.label), `${f.id} ${f.label}`);
      const byLabel = F.classifyLabel(f.label);
      if (byLabel === 'rrn' || byLabel === 'account') assert.equal(f.mapping, byLabel, `${f.id}: sensitive label must be mapped`);
      if (f.type === 'signature' || f.type === 'stamp') assert.equal(f.mapping, f.type);
      if (F.isOtherPerson(f.label)) assert.equal(f.required, false, `${f.id}: another person's field is never required`);
    }
    for (const f of raw.fields) {
      assert.doesNotMatch(f.label, /[\p{Cc}\p{Cf}]/u, `${f.id}: invisible characters in label`);
      if (f.format !== undefined) assert.equal(f.type, 'date', `${f.id}: format only on date boxes`);
      if (f.part !== undefined) assert.ok(f.type === 'text' && F.DATE_PARTS.includes(f.part), `${f.id}: a date part only on a text box`);
      // Another person's box never receives the user's own data as a suggestion.
      if (OTHER_PERSON.test(f.label.replace(/\s+/g, ''))) assert.equal(F.suggest(f, ctx), null, `${f.id} "${f.label}" would be prefilled with the user's data`);
    }
    const sensitive = raw.fields.filter(f => F.isSensitive(f)).map(f => f.label);
    assert.deepEqual(sensitive, entry.sensitive_fields);
    // Two boxes over the same spot would print two answers on top of each other.
    const area = r => r.w * r.h;
    for (const [i, a] of raw.fields.entries()) for (const b of raw.fields.slice(i + 1)) {
      if (a.page !== b.page) continue;
      const ix = Math.max(0, Math.min(a.rect.x + a.rect.w, b.rect.x + b.rect.w) - Math.max(a.rect.x, b.rect.x));
      const iy = Math.max(0, Math.min(a.rect.y + a.rect.h, b.rect.y + b.rect.h) - Math.max(a.rect.y, b.rect.y));
      assert.ok(ix * iy <= 0.3 * Math.min(area(a.rect), area(b.rect)), `${a.id} "${a.label}" overlaps ${b.id} "${b.label}"`);
    }
  });
}

test('import treats a stored template with another key order as unchanged', async () => {
  const { sameTemplate } = await import('./import-templates.mjs');
  const local = { version: 1, content_hash: 'a'.repeat(64), page_count: 1, source: 'admin', fields: [{ id: 'f', page: 0, rect: { x: 0.1, y: 0.1, w: 0.2, h: 0.03 }, type: 'text', label: '성명', mapping: 'name', required: true }] };
  const stored = { fields: [{ label: '성명', mapping: 'name', page: 0, rect: { h: 0.03, w: 0.2, x: 0.1, y: 0.1 }, required: true, type: 'text', id: 'f' }], page_count: 1, content_hash: 'a'.repeat(64), source: 'admin', version: 1 };
  assert.equal(sameTemplate(stored, local), true);
  assert.equal(sameTemplate({ ...stored, fields: [{ ...stored.fields[0], label: '이름' }] }, local), false);
  assert.equal(sameTemplate(null, local), false);
});
