import type { AnswerStreamEvent, AssistantTopic, GroundedVerdict, PassageHighlight } from './assistantTypes';

// ── Grounded Q&A ──

/** Plain and highlighted runs of a quoted passage. Offsets are code points,
 * so text is split with Array.from rather than UTF-16 indexes. */
export function highlightSegments(text: string, highlights: PassageHighlight[] | null | undefined): { text: string; marked: boolean }[] {
  const chars = Array.from(text);
  const spans = [...(highlights ?? [])]
    .filter(h => Number.isInteger(h.start) && Number.isInteger(h.end) && h.start >= 0 && h.end <= chars.length && h.start < h.end)
    .sort((a, b) => a.start - b.start);
  const out: { text: string; marked: boolean }[] = [];
  let at = 0;
  for (const span of spans) {
    if (span.start < at) continue;
    if (span.start > at) out.push({ text: chars.slice(at, span.start).join(''), marked: false });
    out.push({ text: chars.slice(span.start, span.end).join(''), marked: true });
    at = span.end;
  }
  if (at < chars.length || out.length === 0) out.push({ text: chars.slice(at).join(''), marked: false });
  return out;
}

const FOLLOW_UPS: Record<AssistantTopic, string[]> = {
  summary: ['누가 받을 수 있어?', '필요한 서류는?'],
  eligibility: ['필요한 서류는?', '언제까지 신청해?'],
  documents: ['어떻게 신청해?', '언제까지 신청해?'],
  deadline: ['어떻게 신청해?', '필요한 서류는?'],
  steps: ['필요한 서류는?', '누가 받을 수 있어?'],
  alerts: ['언제까지 신청해?', '필요한 서류는?'],
  unsupported: ['필요한 서류는?', '누가 받을 수 있어?', '언제까지 신청해?'],
};

/** Suggested next questions after an answer on `topic`. */
export function followUpSuggestions(topic: AssistantTopic | null | undefined): string[] {
  return FOLLOW_UPS[topic ?? 'unsupported'];
}

/** Badge for a verdict. A value named in the question (휴학) is reported
 * against the notice's registered condition, not as the user's result. */
export function verdictBadge(verdict: GroundedVerdict): { label: string; tone: 'pass' | 'fail' | 'unknown' } {
  if (verdict.asked_value && verdict.asked_status) {
    const label = verdict.asked_status === 'pass' ? `‘${verdict.asked_value}’ 등록 조건에 포함`
      : verdict.asked_status === 'fail' ? `‘${verdict.asked_value}’ 등록 조건에 없음` : `‘${verdict.asked_value}’ 확인 필요`;
    return { label, tone: verdict.asked_status };
  }
  if (!verdict.registered) return { label: '등록된 조건 없음 · 확인 필요', tone: 'unknown' };
  const label = verdict.status === 'pass' ? '일부 조건 맞음' : verdict.status === 'fail' ? '내 조건 불일치' : '확인 필요';
  return { label, tone: verdict.status };
}

/** Split streamed NDJSON into complete events; `rest` is an unfinished line.
 * Unknown or malformed lines are skipped, never shown. */
export function parseAnswerStream(buffer: string): { events: AnswerStreamEvent[]; rest: string } {
  const lines = buffer.split('\n');
  const rest = lines.pop() ?? '';
  const events: AnswerStreamEvent[] = [];
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      const event = JSON.parse(line);
      if (event?.type === 'start') events.push({ type: 'start' });
      else if (event?.type === 'delta' && typeof event.text === 'string') events.push({ type: 'delta', text: event.text });
      else if (event?.type === 'final' && typeof event.valid === 'boolean' && typeof event.text === 'string') events.push(event);
    } catch { /* skip */ }
  }
  return { events, rest };
}

/** The only text that may be shown as final: the server-validated summary,
 * or the fallback when validation failed. Drafts never become final. */
export const GENERATION_FALLBACK = '근거 문장을 확인해 주세요';
export function finalSummary(event: Extract<AnswerStreamEvent, { type: 'final' }>): { text: string; grounded: boolean } {
  return event.valid ? { text: event.text, grounded: true } : { text: GENERATION_FALLBACK, grounded: false };
}

/** User-facing reason a summary was not started. 5xx bodies are sanitized
 * outside local, so the status decides; the code refines 429. */
export function streamErrorMessage(status: number, code?: string | null): string {
  if (status === 400) return 'AI 요약은 질문 처리에 동의한 경우에만 만들어요.';
  if (status === 409) return '요약할 근거 문장이 없어요.';
  if (status === 429 && code === 'busy') return '지금은 요약이 붐벼요. 근거 문장을 먼저 확인해 주세요.';
  if (status === 429 && code === 'too_soon') return 'AI 요약은 10초에 한 번 만들 수 있어요.';
  if (status === 429) return '질문이 너무 많아요. 1분 뒤에 다시 시도해 주세요.';
  return '지금은 AI 요약을 쓸 수 없어요. 근거 문장을 확인해 주세요.';
}
