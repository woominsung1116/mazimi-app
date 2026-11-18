#!/usr/bin/env node
// Report whether an official blank form can be filled field-by-field.
// Usage: node scripts/forms/inspect-template.mjs <file> [--json]
// Uses the mobile app's pdf-lib (cd apps/mobile && npm ci). Reads the local
// file only; it never fetches, executes or unpacks anything.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(fileURLToPath(new URL('../../apps/mobile/package.json', import.meta.url)));
const { PDFDocument, PDFName, PDFTextField, PDFCheckBox, PDFRadioGroup, PDFDropdown, PDFOptionList, PDFSignature, PDFButton } = require('pdf-lib');

const [file, flag] = process.argv.slice(2);
if (!file) { console.error('usage: inspect-template.mjs <file> [--json]'); process.exit(2); }
const bytes = readFileSync(file);
const report = { file, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };

const startsWith = magic => magic.every((b, i) => bytes[i] === b);
if (bytes.subarray(0, 5).toString('latin1') === '%PDF-') {
  report.format = 'pdf';
  try {
    const doc = await PDFDocument.load(bytes, { updateMetadata: false });
    const acro = doc.catalog.getAcroForm();
    report.pages = doc.getPageCount();
    report.acroform = !!acro;
    report.xfa = !!acro?.dict.has(PDFName.of('XFA'));
    const kind = f => f instanceof PDFTextField ? 'text' : f instanceof PDFCheckBox ? 'checkbox' : f instanceof PDFRadioGroup ? 'radio'
      : f instanceof PDFDropdown ? 'dropdown' : f instanceof PDFOptionList ? 'list' : f instanceof PDFSignature ? 'signature' : f instanceof PDFButton ? 'button' : 'other';
    // getForm() deletes XFA; only call it on a plain AcroForm.
    report.fields = acro && !report.xfa ? doc.getForm().getFields().map(f => ({ name: f.getName(), type: kind(f) })) : [];
  } catch (e) {
    report.error = /encrypt/i.test(String(e)) ? 'encrypted' : String(e.message ?? e).slice(0, 200);
  }
} else if (startsWith([0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1])) {
  report.format = bytes.includes(Buffer.from('HWP Document File')) ? 'hwp' : 'ole';
} else if (startsWith([0x50, 0x4b, 0x03, 0x04])) {
  report.format = 'zip (hwpx/docx/xlsx)';
} else {
  report.format = 'unknown';
}
const fillable = report.format === 'pdf' && report.acroform && !report.xfa && report.fields?.length > 0;
report.mode = fillable ? 'acroform' : 'guided';

if (flag === '--json') console.log(JSON.stringify(report, null, 2));
else {
  console.log(`${file}\n  format=${report.format} bytes=${report.bytes} sha256=${report.sha256.slice(0, 16)}…`);
  if (report.format === 'pdf') console.log(`  pages=${report.pages ?? '?'} acroform=${report.acroform ?? '?'} xfa=${report.xfa ?? '?'} fields=${report.fields?.length ?? 0}${report.error ? ` error=${report.error}` : ''}`);
  for (const f of report.fields ?? []) console.log(`    - [${f.type}] ${f.name}`);
  console.log(`  mode=${report.mode}`);
}
