/**
 * 서류 준비 상태 — which checklist documents the user already keeps in the
 * on-device vault, matched ON THIS DEVICE.
 *
 * Pure logic (no React Native imports) so node tests run it directly. The
 * vault never leaves the device: this module only reads the checklist the
 * server sent (titles and details of the programme's documents) and the
 * local vault list, and returns per-document rows. A match is a
 * convenience, not proof: the user still checks the document itself.
 */
import type { DocumentType, StoredDocument } from './vault';

export interface ReadinessTask { id: string; title: string; detail?: string | null; kind?: string | null; done?: boolean }

/** have: in the vault and inside the notice's window; stale: older than the
 * window or past its own expiry; check_date: in the vault, the notice gives
 * no window (show the date, ask the user to check); missing: not in the vault. */
export type ReadinessStatus = 'have' | 'stale' | 'check_date' | 'missing';

export interface Issuer { name: string; url: string | null }

export interface ReadinessRow {
  taskId: string; title: string; done: boolean;
  type: DocumentType | null; status: ReadinessStatus;
  document: { id: string; name: string; issuedAt: string; ageDays: number | null } | null;
  /** Freshness window the notice states, in days ("최근 3개월 이내" = 90). */
  windowDays: number | null;
  issuer: Issuer | null;
}

interface CatalogEntry { type: DocumentType; pattern: RegExp; issuer: Issuer }

/** Document names in notices → vault types, with where to get each one.
 * Order matters (the first match wins): 가족관계 before 주민등록, 휴학 and
 * 졸업 before 재학. Issuer links are official HTTPS home pages only. */
export const DOCUMENT_CATALOG: readonly CatalogEntry[] = [
  { type: 'family_relation', pattern: /가족관계/, issuer: { name: '대법원 전자가족관계등록시스템', url: 'https://efamily.scourt.go.kr' } },
  { type: 'resident_register', pattern: /주민등록\s*(등|초)본|^(등|초)본|주민등록표/, issuer: { name: '정부24', url: 'https://www.gov.kr' } },
  { type: 'health_insurance', pattern: /건강\s*보험/, issuer: { name: '국민건강보험공단', url: 'https://www.nhis.or.kr' } },
  { type: 'income_proof', pattern: /소득(금액)?\s*(증명|확인)|원천징수/, issuer: { name: '홈택스', url: 'https://www.hometax.go.kr' } },
  { type: 'disability_cert', pattern: /장애(인)?\s*(증명|확인)/, issuer: { name: '정부24', url: 'https://www.gov.kr' } },
  { type: 'leave_cert', pattern: /휴학\s*(증명|확인)/, issuer: { name: '다니는 학교(학교 증명서 발급)', url: null } },
  { type: 'graduation_cert', pattern: /졸업(예정)?\s*(증명|확인)/, issuer: { name: '졸업한 학교(학교 증명서 발급)', url: null } },
  { type: 'enrollment_cert', pattern: /재학\s*(증명|확인)/, issuer: { name: '다니는 학교(학교 증명서 발급)', url: null } },
  { type: 'bank_statement', pattern: /통장\s*사본|통장|계좌\s*사본/, issuer: { name: '거래 은행 앱', url: null } },
];

export function documentTypeOf(title: string): CatalogEntry | null {
  const text = title.replace(/\s+/g, ' ').trim();
  return text ? DOCUMENT_CATALOG.find(entry => entry.pattern.test(text)) ?? null : null;
}

/** The freshness window a notice states for a document, in days:
 * "최근 3개월 이내", "1개월 이내 발급", "발급일로부터 30일 이내", "2주 이내". */
export function windowDaysOf(text: string): number | null {
  const m = text.replace(/\s+/g, '').match(/(\d{1,3})(개월|달|주|일)이내/);
  if (!m) return null;
  const n = Number(m[1]);
  const days = m[2] === '개월' || m[2] === '달' ? n * 30 : m[2] === '주' ? n * 7 : n;
  return days >= 1 && days <= 366 ? days : null;
}

/** Whole days between an ISO date and today, in Korea time. */
export function ageInDays(issuedAt: string, today: Date): number | null {
  const issued = Date.parse(issuedAt);
  if (!Number.isFinite(issued)) return null;
  const day = (t: number) => Math.floor((t + 9 * 3600_000) / 86_400_000);
  const age = day(today.getTime()) - day(issued);
  return age >= 0 ? age : null;
}

/** Match each document task of a checklist against the vault. Non-document
 * tasks are skipped. Among several vault copies of one type the newest
 * issue date wins. Nothing is sent anywhere. */
export function documentReadiness(tasks: ReadinessTask[], vault: StoredDocument[], today = new Date()): ReadinessRow[] {
  return tasks.flatMap((task): ReadinessRow[] => {
    if (task.kind && task.kind !== 'document') return [];
    const entry = documentTypeOf(task.title);
    if (!task.kind && !entry) return [];
    const windowDays = windowDaysOf(`${task.title} ${task.detail ?? ''}`);
    // A metadata-only entry (no file kept) is not a document in hand.
    const candidates = entry ? vault.filter(d => d.type === entry.type && !!d.fileUri) : [];
    const newest = [...candidates].sort((a, b) => (Date.parse(b.issuedAt) || 0) - (Date.parse(a.issuedAt) || 0))[0];
    const base = { taskId: task.id, title: task.title, done: task.done === true, type: entry?.type ?? null, windowDays, issuer: entry?.issuer ?? null };
    if (!newest) return [{ ...base, status: 'missing', document: null }];
    const ageDays = ageInDays(newest.issuedAt, today);
    const expired = !!newest.expiresAt && Number.isFinite(Date.parse(newest.expiresAt)) && Date.parse(newest.expiresAt) <= today.getTime();
    const status: ReadinessStatus = expired || (windowDays !== null && ageDays !== null && ageDays > windowDays) ? 'stale'
      : windowDays === null || ageDays === null ? 'check_date' : 'have';
    return [{ ...base, status, document: { id: newest.id, name: newest.name, issuedAt: newest.issuedAt, ageDays } }];
  });
}

export const readinessLabels: Record<ReadinessStatus, string> = {
  have: '보관함에 있어요', stale: '다시 발급이 필요해 보여요', check_date: '보관함에 있어요 · 발급 기한 확인', missing: '보관함에 없어요',
};

/** One line under a document: issue date, age and the notice's window. */
export function readinessNote(row: ReadinessRow): string {
  const parts: string[] = [];
  if (row.document) {
    parts.push(`${row.document.issuedAt.slice(0, 10).replace(/-/g, '.')} 발급${row.document.ageDays !== null ? `(${row.document.ageDays}일 전)` : ''}`);
  }
  if (row.windowDays !== null) parts.push(`공고 기준 ${row.windowDays}일 이내`);
  else if (row.document) parts.push('공고의 발급 기한을 확인해 주세요');
  if (row.status === 'missing' && row.issuer) parts.push(`발급: ${row.issuer.name}`);
  return parts.join(' · ');
}

export function readinessSummary(rows: ReadinessRow[]): { have: number; needAction: number; total: number } {
  const have = rows.filter(r => r.status === 'have' || r.status === 'check_date').length;
  return { have, needAction: rows.length - have, total: rows.length };
}
