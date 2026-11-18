/** 서류 작성 도우미 — touch areas for the boxes drawn over a page. Pure, so node checks run it. */

/** Minimum touch target (pt). */
const TOUCH = 44;
export type Frame = { left: number; top: number; w: number; h: number };
/**
 * Pads a box smaller than the touch target out to it on each side, but never
 * past half the gap to a neighbouring box, so two tiny options next to each
 * other (동의함 / 동의안함) keep separate touch areas.
 */
export function hitSlopFor(box: Frame, others: Frame[]) {
  const want = { x: Math.max(0, (TOUCH - box.w) / 2), y: Math.max(0, (TOUCH - box.h) / 2) };
  let left = want.x, right = want.x, top = want.y, bottom = want.y;
  for (const o of others) {
    if (o === box) continue;
    const rowOverlap = o.top < box.top + box.h + want.y && box.top < o.top + o.h + want.y;
    const colOverlap = o.left < box.left + box.w + want.x && box.left < o.left + o.w + want.x;
    if (rowOverlap && o.left >= box.left + box.w) right = Math.min(right, (o.left - box.left - box.w) / 2);
    if (rowOverlap && o.left + o.w <= box.left) left = Math.min(left, (box.left - o.left - o.w) / 2);
    if (colOverlap && o.top >= box.top + box.h) bottom = Math.min(bottom, (o.top - box.top - box.h) / 2);
    if (colOverlap && o.top + o.h <= box.top) top = Math.min(top, (box.top - o.top - o.h) / 2);
  }
  const z = (v: number) => Math.max(0, v);
  return { top: z(top), bottom: z(bottom), left: z(left), right: z(right) };
}

