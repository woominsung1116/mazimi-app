// node --test scripts/forms/kordoc/ir.test.mjs   (no kordoc install needed)
import assert from "node:assert/strict";
import { test } from "node:test";
import { anchors, toIr } from "./ir.mjs";

const cell = (text, rowSpan = 1, colSpan = 1, extra = {}) => ({ text, rowSpan, colSpan, ...extra });

test("span placeholders are dropped, real blanks stay", () => {
  // 인적사항 spans two rows; 성명 spans two columns; the value cell after it is a real blank.
  const t = {
    rows: 2, cols: 4,
    cells: [
      [cell("인적사항", 2, 1), cell("성명", 1, 2), cell(""), cell("")],
      [cell(""), cell("주소"), cell(""), cell("")],
    ],
  };
  const w = new Set();
  const got = anchors(t, 0, w).map((c) => [c.r, c.c, c.rs, c.cs, c.text]);
  assert.deepEqual(got, [
    [0, 0, 2, 1, "인적사항"], [0, 1, 1, 2, "성명"], [0, 3, 1, 1, ""],
    [1, 1, 1, 1, "주소"], [1, 2, 1, 1, ""], [1, 3, 1, 1, ""],
  ]);
  assert.equal(w.size, 0);
});

test("a placeholder with text is kept and flagged", () => {
  const w = new Set();
  const got = anchors({ cells: [[cell("a", 1, 2), cell("b")]] }, 0, w);
  assert.equal(got.length, 2);
  assert.ok(w.has("SPAN_PLACEHOLDER_TEXT"));
});

test("nested tables recurse and stop at depth 4", () => {
  let inner = { cells: [[cell("x")]] };
  for (let i = 0; i < 4; i++) inner = { cells: [[cell("", 1, 1, { blocks: [{ type: "table", table: inner }] })]] };
  assert.ok(anchors(inner, 0, new Set()));
  const deeper = { cells: [[cell("", 1, 1, { blocks: [{ type: "table", table: inner }] })]] };
  assert.throws(() => anchors(deeper, 0, new Set()), /too deep/);
});

test("only successful hwp/hwpx parses become IR", () => {
  assert.throws(() => toIr({ success: false, code: "CORRUPTED" }, "f", "h", "v"), /parse failed/);
  assert.throws(() => toIr({ success: true, fileType: "pdf", blocks: [] }, "f", "h", "v"), /not hwp/);
  assert.throws(() => toIr({ success: true, fileType: "hwp", metadata: { pageCount: 51 }, blocks: [] }, "f", "h", "v"), /pages/);
  const ir = toIr({ success: true, fileType: "hwp", metadata: { pageCount: 1, pageMode: "layout" },
    blocks: [{ type: "paragraph", pageNumber: 1, text: "신청서" }] }, "f.hwp", "ab", "4.24.7");
  assert.equal(ir.blocks[0].text, "신청서");
  assert.equal(ir.kordoc_version, "4.24.7");
});

test("long text is clipped", () => {
  const ir = toIr({ success: true, fileType: "hwpx", blocks: [{ type: "paragraph", pageNumber: 1, text: "가".repeat(5000) }] }, "f", "h", "v");
  assert.equal(ir.blocks[0].text.length, 4000);
});
