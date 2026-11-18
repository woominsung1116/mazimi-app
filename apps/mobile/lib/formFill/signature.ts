/**
 * 서류 작성 도우미 — finger-drawn signatures as data, and how they are fitted
 * into a box. Pure logic shared by the pad, the on-screen preview and the PDF
 * flattener, so all three place the ink the same way.
 */

/** Points as fractions of the pad (0..1, y down). */
export type Stroke = [number, number][];
/** `aspect` is the pad's width / height when it was drawn: x·aspect and y share one unit. */
export interface Signature { strokes: Stroke[]; aspect: number }

/** Upper bound on stored points (the pad stops adding at this count). */
export const MAX_SIGNATURE_POINTS = 2000;
const MIN_ASPECT = 0.05, MAX_ASPECT = 50;
const round = (v: number) => Math.round(Math.min(1, Math.max(0, v)) * 1000) / 1000;

/** Drops empty strokes and non-finite points, clamps points into the pad and keeps the true aspect. Null: nothing drawn. */
export function cleanSignature(strokes: readonly (readonly (readonly number[])[])[], aspect: number): Signature | null {
  if (!Number.isFinite(aspect) || aspect <= 0) return null;
  let budget = MAX_SIGNATURE_POINTS;
  const kept: Stroke[] = [];
  for (const stroke of strokes) {
    if (!Array.isArray(stroke) || budget <= 0) continue;
    const points = stroke
      .filter(p => Array.isArray(p) && p.length === 2 && Number.isFinite(p[0]) && Number.isFinite(p[1]))
      .slice(0, budget)
      .map(([x, y]) => [round(x), round(y)] as [number, number]);
    if (!points.length) continue;
    budget -= points.length;
    kept.push(points);
  }
  return kept.length ? { strokes: kept, aspect: Math.min(MAX_ASPECT, Math.max(MIN_ASPECT, aspect)) } : null;
}

export interface SignatureFit {
  /** Box units per pad unit (one pad unit = the pad's height). */
  scale: number;
  /** Box position of a pad point: x = ox + px·aspect·scale, y = oy + py·scale (y down). */
  ox: number; oy: number;
}
/** Margin left on each side of the ink, as a fraction of the box. */
const INSET = 0.08;
/**
 * Fits the ink's bounding box (not the whole pad) into a w×h box with its
 * aspect preserved and centred, so a signature drawn small or off-centre on
 * the pad still fills the box without being stretched.
 */
export function fitSignature(sig: Signature, w: number, h: number): SignatureFit {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const stroke of sig.strokes) for (const [px, py] of stroke) {
    const x = px * sig.aspect;
    x0 = Math.min(x0, x); x1 = Math.max(x1, x); y0 = Math.min(y0, py); y1 = Math.max(y1, py);
  }
  // A straight line has no extent in one direction: the other one alone sets the scale.
  const inkW = x1 - x0, inkH = y1 - y0;
  const scale = Math.min((w * (1 - 2 * INSET)) / Math.max(inkW, 1e-3), (h * (1 - 2 * INSET)) / Math.max(inkH, 1e-3));
  return { scale, ox: (w - inkW * scale) / 2 - x0 * scale, oy: (h - inkH * scale) / 2 - y0 * scale };
}

/** Line segments in box coordinates (y down); a single-point stroke becomes a dot. */
export function signatureSegments(sig: Signature, fit: SignatureFit): [number, number, number, number][] {
  const at = ([px, py]: [number, number]): [number, number] => [fit.ox + px * sig.aspect * fit.scale, fit.oy + py * fit.scale];
  const out: [number, number, number, number][] = [];
  for (const stroke of sig.strokes) {
    const points = stroke.length === 1 ? [stroke[0], stroke[0]] : stroke;
    for (let i = 1; i < points.length; i++) {
      const [x0, y0] = at(points[i - 1]), [x1, y1] = at(points[i]);
      out.push([x0, y0, x1, y1]);
    }
  }
  return out;
}
