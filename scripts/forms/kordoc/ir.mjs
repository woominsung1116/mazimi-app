// Table IR of one official HWP/HWPX blank, through kordoc's parser only.
//
//   node scripts/forms/kordoc/ir.mjs <in.hwp|in.hwpx> <out.ir.json>
//
// Runs inside the pinned job image (scripts/forms/kordoc/Dockerfile) with no network, a
// read-only root and the input mounted read-only; see scripts/forms/hwp/convert.sh (engine
// "kordoc"). It never fills, patches or writes a document: the only output is one JSON file.
//
// Output (version 1), everything in source reading order:
//   { version, tool: "kordoc", kordoc_version, file, source_sha256, file_type, page_count,
//     page_mode, warnings: [code], blocks: [ { page, kind, text } | { page, kind: "table", table } ] }
//   table = { rows, cols, cells: [ { r, c, rs, cs, text, header, tables: [table] } ] }
// Only anchor cells are listed: kordoc fills every position a span covers with an empty 1x1
// placeholder, which would look exactly like a blank the applicant writes in. A placeholder
// that carries text is reported as warning "SPAN_PLACEHOLDER_TEXT" and kept as its own cell.
// Limits (the input is untrusted): 50 pages, 4000 blocks, 4000 characters per text, table
// nesting depth 4, 8 MiB of JSON. Over a limit, or any parse failure: exit 1, no output.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";

const MAX_INPUT = 16 * 1024 * 1024;
const MAX_PAGES = 50;
const MAX_BLOCKS = 4000;
const MAX_TEXT = 4000;
const MAX_DEPTH = 4;
const MAX_JSON = 8 * 1024 * 1024;

export function clip(text) {
  const t = typeof text === "string" ? text : "";
  return t.length > MAX_TEXT ? t.slice(0, MAX_TEXT) : t;
}

/** Anchor cells of a dense kordoc grid (span placeholders dropped). */
export function anchors(table, depth, warnings) {
  const rows = Array.isArray(table?.cells) ? table.cells : [];
  const taken = new Set();
  const cells = [];
  rows.forEach((row, r) => {
    (Array.isArray(row) ? row : []).forEach((cell, c) => {
      const text = clip(cell?.text);
      if (taken.has(`${r},${c}`)) {
        if (text.trim()) warnings.add("SPAN_PLACEHOLDER_TEXT");
        else return;
      }
      const rs = Math.max(1, Number(cell?.rowSpan) || 1);
      const cs = Math.max(1, Number(cell?.colSpan) || 1);
      for (let i = r; i < r + rs; i++) for (let j = c; j < c + cs; j++) taken.add(`${i},${j}`);
      const nested = [];
      for (const b of Array.isArray(cell?.blocks) ? cell.blocks : []) {
        if (b?.type === "table" && b.table) {
          if (depth + 1 > MAX_DEPTH) throw new Error("table nesting too deep");
          nested.push(tableOf(b.table, depth + 1, warnings));
        }
      }
      cells.push({ r, c, rs, cs, text, header: cell?.isHeader === true, tables: nested });
    });
  });
  return cells;
}

export function tableOf(table, depth, warnings) {
  return { rows: Number(table?.rows) || 0, cols: Number(table?.cols) || 0, cells: anchors(table, depth, warnings) };
}

export function toIr(result, file, sha256, version) {
  if (!result?.success) throw new Error(`parse failed: ${result?.code ?? "unknown"}`);
  if (!["hwp", "hwpx"].includes(result.fileType)) throw new Error(`not hwp/hwpx: ${result.fileType}`);
  const pageCount = Number(result.metadata?.pageCount ?? result.pageCount) || 0;
  if (pageCount > MAX_PAGES) throw new Error("too many pages");
  if (result.blocks.length > MAX_BLOCKS) throw new Error("too many blocks");
  const warnings = new Set((result.warnings ?? []).map((w) => String(w.code)));
  const blocks = result.blocks.map((b) => {
    const page = Number(b.pageNumber) || 0;
    if (b.type === "table" && b.table) return { page, kind: "table", table: tableOf(b.table, 0, warnings) };
    return { page, kind: String(b.type), text: clip(b.text) };
  });
  return {
    version: 1, tool: "kordoc", kordoc_version: version, file, source_sha256: sha256,
    file_type: result.fileType, page_count: pageCount, page_mode: result.metadata?.pageMode ?? null,
    warnings: [...warnings].sort(), blocks,
  };
}

async function main([input, output]) {
  if (!input || !output) throw new Error("usage: ir.mjs <in> <out.ir.json>");
  if (process.env.KORDOC_OFFLINE !== "1") throw new Error("KORDOC_OFFLINE=1 is required");
  const buf = readFileSync(input);
  if (buf.length === 0 || buf.length > MAX_INPUT) throw new Error("input size");
  const { parse, VERSION } = await import("kordoc");
  const ab = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
  const result = await parse(ab, {
    includeFieldPlaceholders: true, keepTrailingEmptyCols: true, layoutTables: "keep",
    images: false, ocr: false, formulaOcr: false,
  });
  const ir = toIr(result, basename(input), createHash("sha256").update(buf).digest("hex"), VERSION);
  const json = JSON.stringify(ir);
  if (Buffer.byteLength(json) > MAX_JSON) throw new Error("output too large");
  writeFileSync(output, json, { flag: "wx" });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main(process.argv.slice(2)).catch((e) => {
    console.error(`kordoc ir: ${String(e?.message ?? e).slice(0, 200)}`);
    process.exit(1);
  });
}
