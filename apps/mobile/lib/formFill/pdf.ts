/**
 * 서류 작성 도우미 — on-device PDF work: integrity check, AcroForm import and
 * flattening the user's confirmed values into the official form.
 *
 * Pure pdf-lib (no native modules). Nothing here touches the network or
 * storage; callers pass bytes in and get bytes out.
 */
import { LineCapStyle, PDFArray, PDFCheckBox, PDFDict, PDFDocument, PDFDropdown, PDFHexString, PDFName, PDFOptionList, PDFRadioGroup, PDFSignature, PDFString, PDFTextField, rgb, type PDFFont, type PDFPage } from 'pdf-lib';
import fontkit from '@pdf-lib/fontkit';
import { classifyLabel, defaultTypeFor, valueKindOf, isStampName, MAX_GROUPS, readingOrder, type FieldGroup, type FieldType, type FormState, type FormTemplate, type Rect, type TemplateField } from './fields';
import { fitSignature, signatureSegments, type Signature } from './signature';

export const MAX_TEMPLATE_BYTES = 8 * 1024 * 1024;
export const DERIVABLE_FORMATS = ['hwp', 'hwpx'];

export interface TemplateSource { url: string; content_hash: string | null; byte_size: number | null }
/**
 * Which bytes to download and the hash they must have: the agency's own PDF,
 * or, for an HWP/HWPX form, the PDF Mazimi converted from that exact official
 * file (only when a server template is bound to it). Null: no fillable PDF.
 */
export function templateSource(resource: TemplateSource & { file_format: string | null }, serverTemplate: FormTemplate | null, derivedUrl: string): TemplateSource | null {
  if (resource.file_format === 'pdf') return { url: resource.url, content_hash: resource.content_hash, byte_size: resource.byte_size };
  const d = serverTemplate?.derived;
  if (!resource.file_format || !DERIVABLE_FORMATS.includes(resource.file_format) || !d || !resource.content_hash) return null;
  if (d.source_sha256 !== resource.content_hash.toLowerCase()) return null;
  return { url: derivedUrl, content_hash: serverTemplate!.content_hash, byte_size: null };
}
export const MAX_PAGES = 50;

export type IntegrityProblem = 'missing_hash' | 'size' | 'size_mismatch' | 'hash_mismatch';
export class TemplateIntegrityError extends Error {
  constructor(readonly problem: IntegrityProblem) {
    super(problem === 'missing_hash'
      ? '이 양식은 아직 파일 버전이 확인되지 않았어요. 원본 양식을 열어 직접 작성해 주세요.'
      : '내려받은 파일이 확인된 공식 양식과 달라요. 기관이 양식을 바꿨을 수 있어요. 원본 양식을 열어 확인해 주세요.');
    this.name = 'TemplateIntegrityError';
  }
}

/** Refuses any byte that differs from what the server verified (SHA-256 hex, exact size). */
export async function verifyTemplateBytes(bytes: Uint8Array, expected: { content_hash: string | null; byte_size: number | null }, sha256Hex: (bytes: Uint8Array) => Promise<string>): Promise<void> {
  const hash = expected.content_hash?.toLowerCase();
  if (!hash || !/^[0-9a-f]{64}$/.test(hash)) throw new TemplateIntegrityError('missing_hash');
  if (bytes.length === 0 || bytes.length > MAX_TEMPLATE_BYTES) throw new TemplateIntegrityError('size');
  if (expected.byte_size != null && bytes.length !== expected.byte_size) throw new TemplateIntegrityError('size_mismatch');
  if ((await sha256Hex(bytes)).toLowerCase() !== hash) throw new TemplateIntegrityError('hash_mismatch');
}

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
export function base64ToBytes(base64: string): Uint8Array {
  const clean = base64.replace(/[^A-Za-z0-9+/]/g, '');
  const out = new Uint8Array(Math.floor(clean.length * 3 / 4));
  let buffer = 0, bits = 0, n = 0;
  for (let i = 0; i < clean.length; i++) {
    buffer = (buffer << 6) | B64.indexOf(clean[i]); bits += 6;
    if (bits >= 8) { bits -= 8; out[n++] = (buffer >> bits) & 0xff; }
  }
  return out.subarray(0, n);
}
export function bytesToBase64(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i], b = bytes[i + 1], c = bytes[i + 2];
    out += B64[a >> 2] + B64[((a & 3) << 4) | ((b ?? 0) >> 4)] + (b === undefined ? '=' : B64[((b & 15) << 2) | ((c ?? 0) >> 6)]) + (c === undefined ? '=' : B64[c & 63]);
  }
  return out;
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

export type Unsupported = 'encrypted' | 'xfa' | 'rotated' | 'too_many_pages' | 'invalid';
export type Analysis =
  | { ok: true; pageCount: number; aspects: number[]; acroform: FormTemplate | null }
  | { ok: false; reason: Unsupported };

export const UNSUPPORTED_MESSAGES: Record<Unsupported, string> = {
  encrypted: '암호가 걸린 PDF라 앱에서 작성할 수 없어요.',
  xfa: '전용 프로그램(XFA)용 양식이라 앱에서 작성할 수 없어요.',
  rotated: '회전된 쪽이 있는 양식이라 칸 위치를 맞출 수 없어요.',
  too_many_pages: '쪽수가 너무 많은 양식이에요.',
  invalid: 'PDF 파일을 읽지 못했어요.',
};

export async function loadPdf(bytes: Uint8Array): Promise<PDFDocument | Unsupported> {
  try {
    return await PDFDocument.load(bytes, { updateMetadata: false });
  } catch (e) {
    return /encrypt/i.test(String(e)) ? 'encrypted' : 'invalid';
  }
}

export async function analyzePdf(bytes: Uint8Array, contentHash: string): Promise<Analysis> {
  const doc = await loadPdf(bytes);
  if (typeof doc === 'string') return { ok: false, reason: doc };
  try {
    return inspect(doc, contentHash);
  } catch {
    return { ok: false, reason: 'invalid' };
  }
}
function inspect(doc: PDFDocument, contentHash: string): Analysis {
  const pages = doc.getPages();
  if (!pages.length || pages.length > MAX_PAGES) return { ok: false, reason: pages.length ? 'too_many_pages' : 'invalid' };
  if (pages.some(p => ((p.getRotation().angle % 360) + 360) % 360 !== 0)) return { ok: false, reason: 'rotated' };
  const acro = doc.catalog.getAcroForm();
  if (acro?.dict.has(PDFName.of('XFA'))) return { ok: false, reason: 'xfa' };
  const aspects = pages.map(p => { const c = p.getCropBox(); return c.width / c.height; });
  return { ok: true, pageCount: pages.length, aspects, acroform: acro ? importAcroForm(doc, contentHash) : null };
}

export type FillMode = { mode: 'overlay'; template: FormTemplate } | { mode: 'guided'; reason: string };
/**
 * PDF + admin template (same file hash, same page count) → overlay;
 * PDF with its own AcroForm fields → overlay from the imported fields;
 * anything else (HWP/HWPX/DOCX, flat PDF, unsupported PDF) → guided copy sheet.
 */
export function chooseMode(format: string | null, analysis: Analysis | null, serverTemplate: FormTemplate | null): FillMode {
  if (format !== 'pdf') return { mode: 'guided', reason: format === 'hwp' ? '한글(HWP) 양식은 앱에서 바로 작성할 수 없어요. 아래 내용을 보고 원본 양식에 옮겨 적어 주세요.' : '이 형식의 양식은 앱에서 바로 작성할 수 없어요. 원본 양식에 옮겨 적어 주세요.' };
  if (!analysis) return { mode: 'guided', reason: '이 기기에서는 양식을 직접 작성할 수 없어요. 원본 양식에 옮겨 적어 주세요.' };
  if (!analysis.ok) return { mode: 'guided', reason: `${UNSUPPORTED_MESSAGES[analysis.reason]} 원본 양식에 옮겨 적어 주세요.` };
  if (serverTemplate && serverTemplate.page_count === analysis.pageCount) return { mode: 'overlay', template: serverTemplate };
  if (analysis.acroform) return { mode: 'overlay', template: analysis.acroform };
  return { mode: 'guided', reason: '아직 입력 칸 위치가 준비되지 않은 양식이에요. 아래 내용을 보고 원본 양식에 옮겨 적어 주세요.' };
}

function text(dict: PDFDict, key: string): string {
  const value = dict.lookup(PDFName.of(key));
  return value instanceof PDFString || value instanceof PDFHexString ? value.decodeText().trim() : '';
}

/** Converts AcroForm widgets into overlay fields: the starting template when no admin template exists. */
export function importAcroForm(doc: PDFDocument, contentHash: string): FormTemplate | null {
  const pageOf = new Map<PDFDict, number>();
  doc.getPages().forEach((page, index) => {
    const annots = page.node.Annots();
    for (let i = 0; i < (annots?.size() ?? 0); i++) {
      const annot = annots!.lookupMaybe(i, PDFDict);
      if (annot) pageOf.set(annot, index);
    }
  });
  const fields: TemplateField[] = [];
  const groups: FieldGroup[] = [];
  for (const field of doc.getForm().getFields()) {
    let type: FieldType | null = null;
    if (field instanceof PDFTextField) type = field.isMultiline() ? 'multiline' : 'text';
    else if (field instanceof PDFCheckBox || field instanceof PDFRadioGroup) type = 'checkbox';
    else if (field instanceof PDFSignature) type = 'signature';
    else if (field instanceof PDFDropdown || field instanceof PDFOptionList) type = 'text';
    if (!type) continue;
    const name = field.getName();
    const label = (text(field.acroField.dict, 'TU') || name.split('.').pop() || name).slice(0, 80);
    const mapping = classifyLabel(label) ?? classifyLabel(name);
    if (type === 'text') type = defaultTypeFor(mapping) === 'text' ? 'text' : defaultTypeFor(mapping);
    const options = field instanceof PDFRadioGroup ? field.getOptions() : [];
    const first = fields.length;
    field.acroField.getWidgets().forEach((widget, w) => {
      const page = pageOf.get(widget.dict);
      if (page === undefined) return;
      const rect = toFraction(doc.getPage(page), widget.getRectangle());
      if (!rect) return;
      const option = options[w];
      fields.push({
        id: `acro-${fields.length}`, page, rect, type: type!, mapping: type === 'checkbox' ? null : mapping,
        // Radio options share acro_name (one may be checked); an option itself is never required.
        label: option ? `${label} · ${option}`.slice(0, 80) : label, required: field instanceof PDFRadioGroup ? false : field.isRequired(), acro_name: name.slice(0, 200),
      });
    });
    // A radio is one question with one answer: a single group, when its options
    // satisfy the group rules (2+ options on one page). acro_name keeps it exclusive otherwise.
    const imported = fields.slice(first);
    const groupLabel = label.replace(/\p{Cc}/gu, ' ').trim().slice(0, 80);
    if (field instanceof PDFRadioGroup && imported.length >= 2 && imported.every(f => f.page === imported[0].page) && groupLabel && groups.length < MAX_GROUPS) {
      const id = `radio-${groups.length + 1}`;
      groups.push({ id, mode: 'single', label: groupLabel });
      for (const f of imported) f.group = id;
    }
  }
  if (!fields.length) return null;
  const kept = readingOrder(fields).slice(0, 200);
  // The 200-field cap may cut a group: keep only groups that still have 2+ options.
  const live = groups.filter(g => kept.filter(f => f.group === g.id).length >= 2);
  for (const f of kept) if (f.group && !live.some(g => g.id === f.group)) delete f.group;
  return { version: 1, content_hash: contentHash.toLowerCase(), page_count: doc.getPageCount(), source: 'acroform', fields: kept, ...(live.length ? { groups: live } : {}) };
}

function toFraction(page: PDFPage, r: { x: number; y: number; width: number; height: number }): Rect | null {
  const c = page.getCropBox();
  const clamp = (v: number) => Math.min(1, Math.max(0, v));
  const x = clamp((r.x - c.x) / c.width), right = clamp((r.x + r.width - c.x) / c.width);
  const top = clamp((c.y + c.height - (r.y + r.height)) / c.height), bottom = clamp((c.y + c.height - r.y) / c.height);
  const rect = { x: round4(x), y: round4(top), w: round4(right - x), h: round4(bottom - top) };
  return rect.w >= 0.005 && rect.h >= 0.005 ? rect : null;
}
const round4 = (v: number) => Math.round(v * 10000) / 10000;

// ---------------------------------------------------------------------------
// Flattening
// ---------------------------------------------------------------------------

/** A value the flattener could not draw, by field: the screen takes the user to each box. */
export class FieldDrawError extends Error {
  constructor(readonly fields: Pick<TemplateField, 'id' | 'label'>[], message: string) {
    super(message);
    this.name = 'FieldDrawError';
  }
  get fieldIds(): string[] { return this.fields.map(f => f.id); }
  get labels(): string[] { return this.fields.map(f => f.label); }
}
/** A confirmed value the flattener cannot draw (invalid seal name, wrong value kind). Never skipped silently. */
export class FieldRenderError extends FieldDrawError {
  constructor(fields: Pick<TemplateField, 'id' | 'label'>[]) {
    super(fields, `이 칸을 그리지 못했어요: ${fields.map(f => f.label).join(', ')}. 다시 입력해 주세요.`);
    this.name = 'FieldRenderError';
  }
}
export class FieldOverflowError extends FieldDrawError {
  constructor(fields: Pick<TemplateField, 'id' | 'label'>[]) {
    super(fields, `칸에 비해 내용이 길어요: ${fields.map(f => f.label).join(', ')}. 줄여서 입력해 주세요.`);
    this.name = 'FieldOverflowError';
  }
}

const INK = rgb(0.05, 0.05, 0.1);
const STAMP_RED = rgb(0.82, 0.12, 0.12);

/**
 * Draws every confirmed value into the page, removes interactive form
 * widgets so the result cannot be silently edited later, and returns the
 * new PDF bytes. Values left 'suggested' are never drawn.
 */
export async function flattenForm(bytes: Uint8Array, template: FormTemplate, state: FormState, fontBytes: Uint8Array): Promise<Uint8Array> {
  const doc = await loadPdf(bytes);
  if (typeof doc === 'string') throw new Error(UNSUPPORTED_MESSAGES[doc]);
  if (doc.getPageCount() !== template.page_count) throw new Error('양식 쪽수가 정의와 달라요. 원본 양식을 확인해 주세요.');
  doc.registerFontkit(fontkit);
  // Full embedding (~0.75 MB compressed): fontkit's subsetter drops the
  // composite glyphs NanumGothic uses for Hangul syllables, which then
  // render blank. The unmodified OFL font is embedded as shipped.
  const font = await doc.embedFont(fontBytes, { subset: false });
  // Keep what the agency printed through fields (기관명, 연도 …): bake existing
  // appearances into the page first, then drop every remaining widget.
  try { doc.getForm().flatten({ updateFieldAppearances: false }); } catch { /* widgets without appearances are removed below */ }
  removeInteractiveFields(doc);
  const overflow: TemplateField[] = [], undrawable: TemplateField[] = [];
  for (const field of template.fields) {
    const s = state[field.id];
    const value = s?.status === 'confirmed' ? s.value : null;
    if (!value) continue;
    const page = doc.getPage(field.page);
    const c = page.getCropBox();
    const box = { left: c.x + field.rect.x * c.width, bottom: c.y + (1 - field.rect.y - field.rect.h) * c.height, w: field.rect.w * c.width, h: field.rect.h * c.height };
    // A value of another kind than the box takes (a hand-built or stale state) is refused, never drawn as something else.
    if (value.kind !== valueKindOf(field.type)) undrawable.push(field);
    else if (value.kind === 'check') { if (value.on) drawCheck(page, box); }
    else if (value.kind === 'signature') drawSignature(page, box, value);
    else if (!value.value) continue;
    else if (field.type === 'stamp') { if (isStampName(value.value)) drawStamp(page, box, value.value, font); else undrawable.push(field); }
    else if (!drawText(page, box, value.value, font, field)) overflow.push(field);
  }
  if (undrawable.length) throw new FieldRenderError(undrawable);
  if (overflow.length) throw new FieldOverflowError(overflow);
  return doc.save({ updateFieldAppearances: false });
}

function removeInteractiveFields(doc: PDFDocument) {
  for (const page of doc.getPages()) {
    const annots = page.node.Annots();
    if (!annots) continue;
    const kept = PDFArray.withContext(doc.context);
    for (let i = 0; i < annots.size(); i++) {
      const annot = annots.lookupMaybe(i, PDFDict);
      // Dangling references (objects removed by flatten) are dropped too.
      if (annot && annot.get(PDFName.of('Subtype')) !== PDFName.of('Widget')) kept.push(annots.get(i));
    }
    page.node.set(PDFName.of('Annots'), kept);
  }
  doc.catalog.delete(PDFName.of('AcroForm'));
}

type Box = { left: number; bottom: number; w: number; h: number };

/** Fits text in the box by shrinking (down to 5pt) and, for multi-line fields, wrapping. */
export function layoutText(value: string, font: Pick<PDFFont, 'widthOfTextAtSize'>, box: { w: number; h: number }, multiline: boolean, preferred?: number): { size: number; lines: string[] } | null {
  const pad = Math.min(2, box.w * 0.05);
  const width = box.w - pad * 2;
  // Unit glyph widths measured once per character: fontkit layout per call is slow on Hermes.
  const unit = new Map<string, number>();
  const w1 = (ch: string) => { let w = unit.get(ch); if (w === undefined) { w = font.widthOfTextAtSize(ch, 1); unit.set(ch, w); } return w; };
  const measure = (text: string) => { let total = 0; for (const ch of text) total += w1(ch); return total; };
  for (let size = preferred ?? Math.max(5, Math.min(box.h * 0.7, 12)); size >= 5; size -= 0.25) {
    const lines = multiline ? wrap(value, w1, size, width) : [value];
    const fitsHeight = multiline ? lines.length * size * 1.2 <= box.h : size <= box.h * 1.2;
    if (fitsHeight && lines.every(l => measure(l) * size <= width)) return { size, lines };
  }
  return null;
}
function wrap(value: string, w1: (ch: string) => number, size: number, width: number): string[] {
  const lines: string[] = [];
  for (const paragraph of value.split('\n')) {
    let line = '', used = 0;
    for (const ch of paragraph) {
      const w = w1(ch) * size;
      if (used + w > width && line) { lines.push(line); line = ch.trimStart(); used = line ? w : 0; }
      else { line += ch; used += w; }
    }
    lines.push(line);
  }
  return lines;
}
/** Height of the em box's middle above the baseline, from the font's own ascent and descent. */
export function emMiddle(font: Pick<PDFFont, 'heightAtSize'>, size: number): number {
  const ascent = font.heightAtSize(size, { descender: false });
  const descent = font.heightAtSize(size) - ascent;
  return (ascent - descent) / 2;
}
function drawText(page: PDFPage, box: Box, value: string, font: PDFFont, field: TemplateField): boolean {
  const layout = layoutText(value, font, box, field.type === 'multiline', field.font_size);
  if (!layout) return false;
  const pad = Math.min(2, box.w * 0.05);
  const { size, lines } = layout;
  const lineHeight = size * 1.2;
  // Multi-line text starts at the top of the box; a single line is centred.
  // Each line's em box is centred on its line, so glyphs sit on the box's middle, not above it.
  const blockTop = field.type === 'multiline' ? box.bottom + box.h : box.bottom + (box.h + lineHeight * lines.length) / 2;
  const middle = emMiddle(font, size);
  lines.forEach((line, i) => page.drawText(line, { x: box.left + pad, y: blockTop - lineHeight * (i + 0.5) - middle, size, font, color: INK }));
  return true;
}
function drawCheck(page: PDFPage, box: Box) {
  const s = Math.min(box.w, box.h) * 0.8;
  const x = box.left + (box.w - s) / 2, y = box.bottom + (box.h - s) / 2;
  const t = Math.max(0.8, s * 0.12);
  page.drawLine({ start: { x: x + s * 0.1, y: y + s * 0.5 }, end: { x: x + s * 0.4, y: y + s * 0.15 }, thickness: t, color: INK });
  page.drawLine({ start: { x: x + s * 0.4, y: y + s * 0.15 }, end: { x: x + s * 0.92, y: y + s * 0.9 }, thickness: t, color: INK });
}
/** SVG path of the strokes in box coordinates (points, y down): the ink's bounding box fitted into w×h, aspect preserved. */
export function signaturePath(sig: Signature, w: number, h: number): string {
  const segments = signatureSegments(sig, fitSignature(sig, w, h));
  // A dot is a zero-length segment: nudge it so the round cap shows.
  return segments.map(([x0, y0, x1, y1]) => `M${x0.toFixed(3)} ${y0.toFixed(3)} L${(x1 === x0 && y1 === y0 ? x1 + 0.3 : x1).toFixed(3)} ${y1.toFixed(3)}`).join(' ');
}
function drawSignature(page: PDFPage, box: Box, sig: Signature) {
  page.drawSvgPath(signaturePath(sig, box.w, box.h), { x: box.left, y: box.bottom + box.h, borderColor: INK, borderWidth: 1.2, borderLineCap: LineCapStyle.Round });
}
function drawStamp(page: PDFPage, box: Box, name: string, font: PDFFont) {
  const r = Math.min(box.w, box.h) / 2 * 0.95;
  const cx = box.left + box.w / 2, cy = box.bottom + box.h / 2;
  page.drawEllipse({ x: cx, y: cy, xScale: r, yScale: r, borderColor: STAMP_RED, borderWidth: Math.max(0.6, r * 0.08) });
  for (const { ch, dx, dy, size } of stampLayout(name, r)) {
    const w = font.widthOfTextAtSize(ch, size);
    page.drawText(ch, { x: cx + dx - w / 2, y: cy + dy - size * 0.36, size, font, color: STAMP_RED });
  }
}
/** Traditional seal layout: one column for up to 3 characters, 2×2 (right column first) for 4. */
export function stampLayout(name: string, r: number): { ch: string; dx: number; dy: number; size: number }[] {
  const chars = [...name];
  if (chars.length === 4) {
    const size = r * 0.62, o = r * 0.33;
    return [{ ch: chars[0], dx: o, dy: o, size }, { ch: chars[1], dx: o, dy: -o, size }, { ch: chars[2], dx: -o, dy: o, size }, { ch: chars[3], dx: -o, dy: -o, size }];
  }
  const size = chars.length === 1 ? r * 1.1 : chars.length === 2 ? r * 0.8 : r * 0.56;
  const step = size * 1.02;
  return chars.map((ch, i) => ({ ch, dx: 0, dy: ((chars.length - 1) / 2 - i) * step, size }));
}
