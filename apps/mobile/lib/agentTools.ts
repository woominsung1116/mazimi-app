// Narrow readers of the agent-tools-v3 results (check_official_page,
// document_issue_link). Pure: `npm run test:agent-tools` loads this file.
// Page quotes are shown as quotes only; links are HTTPS and, for issuing
// sites, limited to the server catalog's hosts.
import type { AgentToolResult } from './agentTypes';

/** Mirrors crates/api/src/agent/documents.rs ISSUER_HOSTS. */
export const ISSUER_HOSTS = ['www.gov.kr', 'efamily.scourt.go.kr', 'www.nhis.or.kr', 'hometax.go.kr'] as const;

const str = (v: unknown): string | null => (typeof v === 'string' && v.trim() ? v : null);
const list = (v: unknown): Record<string, unknown>[] =>
  Array.isArray(v) ? v.filter((x): x is Record<string, unknown> => !!x && typeof x === 'object') : [];

function httpsUrl(value: unknown, hosts?: readonly string[]): string | null {
  if (typeof value !== 'string') return null;
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.username || url.password || (url.port && url.port !== '443')) return null;
    if (hosts && !hosts.includes(url.hostname.toLowerCase())) return null;
    return url.toString();
  } catch {
    return null;
  }
}

export type LiveStatus = 'open' | 'closed' | 'upcoming' | 'unknown' | 'not_checked';

export const liveStatusLabels: Record<LiveStatus, string> = {
  open: '공식 페이지: 접수 중 표시',
  closed: '공식 페이지: 접수 마감',
  upcoming: '공식 페이지: 접수 전',
  unknown: '공식 페이지: 상태 확인 필요',
  not_checked: '공식 페이지를 확인하지 못했어요',
};

export interface OfficialPageView {
  status: LiveStatus;
  label: string;
  /** "10월 16일 14:03 확인" in Korea time, or null. */
  checkedAt: string | null;
  quotes: { label: string; text: string }[];
  changes: string[];
  link: string | null;
  cached: boolean;
}

function kstTime(iso: unknown): string | null {
  if (typeof iso !== 'string') return null;
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return null;
  const d = new Date(t + 9 * 3600_000);
  const hh = String(d.getUTCHours()).padStart(2, '0');
  const mm = String(d.getUTCMinutes()).padStart(2, '0');
  return `${d.getUTCMonth() + 1}월 ${d.getUTCDate()}일 ${hh}:${mm} 확인`;
}

export function officialPageView(result: AgentToolResult): OfficialPageView | null {
  if (result.tool !== 'check_official_page' || !result.ok) return null;
  const raw = result.data.live_status;
  const status: LiveStatus = raw === 'open' || raw === 'closed' || raw === 'upcoming' || raw === 'unknown' ? raw : 'not_checked';
  const changes = list(result.data.changes).flatMap((c): string[] => {
    switch (c.field) {
      case 'status': return [`접수 상태: 마지미 ‘${str(c.ours) ?? '정보 없음'}’ → 공식 ‘${str(c.live) ?? ''}’`];
      case 'deadline': return [`마감일: 마지미 ${str(c.ours) ?? '정보 없음'} → 공식 ${str(c.live) ?? ''}`];
      case 'attachments_added': return [`마지미에 없는 첨부: ${Array.isArray(c.live) ? c.live.length : 0}개`];
      case 'attachments_missing': return [`페이지에 없는 첨부: ${Array.isArray(c.ours) ? c.ours.length : 0}개`];
      default: return [];
    }
  });
  return {
    status,
    label: liveStatusLabels[status],
    checkedAt: status === 'not_checked' ? null : kstTime(result.data.checked_at),
    quotes: list(result.data.quotes).flatMap(q => {
      const text = str(q.text);
      return text ? [{ label: str(q.label) ?? '공식 페이지', text: text.slice(0, 200) }] : [];
    }),
    changes,
    link: httpsUrl(result.data.source_url) ?? httpsUrl(result.data.official_url),
    cached: result.data.cached === true,
  };
}

export interface IssueLinkRow {
  name: string; known: boolean; issuer: string | null; guidance: string;
  /** The issuing site, only on a catalog host; null for school documents and unknown ones. */
  url: string | null; verifiedOn: string | null;
}

export function issueLinkRows(result: AgentToolResult): IssueLinkRow[] {
  if (result.tool !== 'document_issue_link' || !result.ok) return [];
  return list(result.data.documents).flatMap(d => {
    const name = str(d.name);
    if (!name) return [];
    const known = d.known === true;
    return [{
      name, known,
      issuer: known ? str(d.issuer) : null,
      guidance: str(d.guidance) ?? (known ? '' : '발급처를 안내할 수 없어요. 공고문에서 발급 기관을 확인해 주세요.'),
      url: known ? httpsUrl(d.url, ISSUER_HOSTS) : null,
      verifiedOn: known ? str(d.verified_on) : null,
    }];
  });
}

/** The programme's notice link under an issue-link card (unknown documents). */
export function noticeLink(result: AgentToolResult): string | null {
  return result.tool === 'document_issue_link' ? httpsUrl(result.data.official_url) : null;
}
