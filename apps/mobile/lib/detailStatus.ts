// Row states of the detail page's 자격 조건 (34065:11030) and 필요 서류 (34065:11091)
// boxes. Each row's mark comes from real data only: the server's per-condition
// checks for this member, and the vault on this device. Without that data a row
// keeps the neutral mark and no count is shown. Pure logic (no React Native).
import { documentReadiness } from './documentReadiness';
import type { EligibilityStatus } from './assistantTypes';
import type { StoredDocument } from './vault';

/** pass → check on Primary, fail → x on Orange, unknown → minus on Secondary 32% (34065:11034 / 11066 / 11058). */
export type RowMark = 'pass' | 'fail' | 'unknown';
export interface StatusRow { title: string; note: string | null; mark: RowMark }

/** 자격 조건 rows from the server's checks; `count` is "n/m 맞음" (scripts/check-claim-copy.mjs bans the claim word). */
export function conditionStatus(checks: { label: string; status: EligibilityStatus; detail: string }[] | undefined): { rows: StatusRow[]; count: string } | null {
  const rows = (Array.isArray(checks) ? checks : []).filter(c => typeof c?.label === 'string' && c.label.trim()).map((c): StatusRow => ({ title: c.label, note: (c.detail ?? '').trim() || null, mark: c.status === 'pass' || c.status === 'fail' ? c.status : 'unknown' }));
  if (!rows.length) return null;
  return { rows, count: `${rows.filter(r => r.mark === 'pass').length}/${rows.length} 맞음` };
}

const DOC_NOTES = {
  have: '이 기기 보관함에 있어요.',
  check_date: '보관함에 있어요. 발급일 기준을 확인해 주세요.',
  stale: '보관함 서류의 발급일이나 유효기간을 다시 확인해 주세요.',
  missing: '이 기기 보관함에 아직 없어요.',
} as const;

/** 필요 서류 rows matched against this device's vault by the app's one matcher
 * (`documentReadiness`). A document the matcher has no type for keeps the
 * neutral mark and no note. The notice's own line (`detail`, e.g. "최근 3개월 이내") sets the issue
 * window. `count` is "n/m 준비": the rows with a check mark; null when no row could be matched. */
export function documentStatus(docs: { name: string; detail?: string | null }[], vault: StoredDocument[], today: Date = new Date()): { rows: StatusRow[]; count: string | null } {
  const readiness = documentReadiness(docs.map((doc, i) => ({ id: `doc-${i}`, title: doc.name, detail: doc.detail ?? null, kind: 'document' })), vault, today);
  let typed = 0;
  const rows = docs.map(({ name: title }, i): StatusRow => {
    const row = readiness.find(r => r.taskId === `doc-${i}`);
    if (!row || !row.type) return { title, note: null, mark: 'unknown' };
    typed++;
    return { title, note: DOC_NOTES[row.status], mark: row.status === 'have' ? 'pass' : row.status === 'missing' ? 'fail' : 'unknown' };
  });
  return { rows, count: typed ? `${rows.filter(r => r.mark === 'pass').length}/${docs.length} 준비` : null };
}
