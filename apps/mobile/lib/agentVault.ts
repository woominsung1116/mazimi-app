// The agent card's document rows: the required list the server sent
// (match_vault_documents), matched against the vault ON THIS DEVICE by the
// app's one matcher, `documentReadiness`. No second matcher (re-review H1):
// the agent card and the journey card can never disagree. Pure logic.
import { documentReadiness, type ReadinessStatus } from './documentReadiness';
import type { AgentToolResult } from './agentTypes';
import type { StoredDocument } from './vault';

export type VaultStatus = 'ready' | 'missing' | 'check';
export interface VaultRow { name: string; status: VaultStatus; note: string | null }

const str = (v: unknown): string | null => typeof v === 'string' ? v : null;
const num = (v: unknown): number | null => typeof v === 'number' && Number.isFinite(v) ? v : null;
const list = (v: unknown): Record<string, unknown>[] => Array.isArray(v) ? v.filter((x): x is Record<string, unknown> => !!x && typeof x === 'object') : [];

/** Someone else's paper (부모, 배우자, 세대원 …) or a particular form of it
 * (상세, 확정일자 …): the server's subject rule, mirrored here so a vault
 * document is held to the same rule as a checklist mark (re-review 3 M4).
 * Copy counts ("1부", "각 2부") are no subject. */
const SUBJECTS = ['부모', '부(', '모(', '부)', '모)', '부의', '모의', '父', '母', '배우자', '세대원', '가구원', '보호자', '형제', '자매', '자녀', '본인및'];
const QUALIFIERS = ['상세', '확정일자', '원본', '공증', '번역', '아포스티유', '직인'];
export function qualifiedName(name: string): boolean {
  const plain = name.replace(/(각\s*)?\d+\s*부/g, ' ').replace(/\s+/g, '');
  return SUBJECTS.some(s => plain.includes(s)) || QUALIFIERS.some(q => plain.includes(q));
}

const NOTES: Record<ReadinessStatus, string | null> = {
  have: '이 기기 보관함에 있어요',
  check_date: '보관함에 있어요 · 발급 기한을 확인해 주세요',
  stale: '보관함 서류의 발급일이나 유효기간을 다시 확인해 주세요',
  missing: null,
};

/** have → 준비됨, check_date/stale → 확인 필요, missing → 없음. A paper that is
 * someone else's or a particular form (`qualified`), or that carries an issue
 * condition the device cannot prove (`issue_condition` without a digit
 * window), is never 준비됨 from a match. The server's checklist mark counts
 * only for a plain paper with no issue condition (server status `have`). */
export function vaultRows(result: AgentToolResult, vault: StoredDocument[] = [], today: Date = new Date()): VaultRow[] {
  if (result.tool !== 'match_vault_documents') return [];
  const docs = list(result.data.documents).filter(d => !!str(d.name));
  const windows = docs.map(d => num(d.window_days));
  const tasks = docs.map((d, i) => ({
    id: `doc-${i}`, title: str(d.name) as string, kind: 'document',
    // The server's reading of the window reaches the same matcher.
    detail: [str(d.detail), windows[i] !== null ? `최근 ${windows[i]}일 이내` : null].filter(Boolean).join(' '),
  }));
  const readiness = documentReadiness(tasks, vault, today);
  return docs.map((d, i): VaultRow => {
    const name = str(d.name) as string;
    const row = readiness.find(r => r.taskId === `doc-${i}`);
    const qualified = d.qualified === true;
    const undated = d.issue_condition === true && windows[i] === null;
    if (d.status === 'have') return { name, status: 'ready', note: '준비 목록에 완료로 표시함' };
    if (row && row.status !== 'missing') {
      // The vault copy is someone else's (or the applicant's where someone
      // else's is required): never ready from the type alone.
      if (row.document && qualifiedName(row.document.name) !== qualified) {
        return { name, status: 'check', note: '보관함 서류가 공고가 요구하는 대상자의 서류인지 확인해 주세요' };
      }
      if (row.status === 'have' && !qualified && !undated) return { name, status: 'ready', note: NOTES.have };
      return { name, status: 'check', note: qualified ? '대상자·서류 형식을 공고에서 확인해 주세요' : NOTES[row.status === 'have' ? 'check_date' : row.status] };
    }
    if (d.status === 'needs_check') return { name, status: 'check', note: '준비 목록에 표시했지만 공고 기준을 확인해 주세요' };
    return { name, status: 'missing', note: null };
  });
}

export const vaultStatusLabels: Record<VaultStatus, string> = { ready: '준비됨', missing: '없음', check: '확인 필요' };
