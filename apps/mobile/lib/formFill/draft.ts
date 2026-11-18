/**
 * 서류 작성 도우미 — the draft the agent's approved card opens.
 *
 * Pure logic. The draft is built ON THIS DEVICE from the saved profile and
 * the opt-in remembered contact details, with the same suggestion rules as
 * the form helper (`suggest`): every value is a proposal the user confirms,
 * 주민등록번호·계좌번호·예금주 are never filled, and fields about another person
 * are left empty. The draft lives in memory only; nothing here stores or
 * sends a value.
 */
import { KIND_LABELS, cleanValue, formatDate, initialState, isOtherPerson, looksSensitive, normalizePart, parseDate, textValue, sensitiveKindOf, type FormState, type SuggestionContext, type TemplateField } from './fields';

export interface DraftSummary {
  /** Fields with a proposed value waiting for the user's confirmation. */
  suggested: number;
  /** Labels of the fields the assistant pre-filled, in form order. */
  suggestedLabels: string[];
  /** 주민등록번호·계좌번호·예금주: left empty on purpose, typed by the user at export time. */
  sensitive: number;
  /** Names of the sensitive kinds left empty, in form order, each once. */
  sensitiveLabels: string[];
  /** Fields about someone else (보호자, 부모 …): left empty. */
  otherPerson: number;
  /** Everything else the user fills in. */
  empty: number;
  total: number;
  /** Labels of the boxes holding values the user said to the 비서 (chatFill.ts). */
  chatLabels?: string[];
}

export interface Draft { state: FormState; summary: DraftSummary }

export function buildDraft(fields: TemplateField[], ctx: SuggestionContext): Draft {
  const state = initialState(fields, ctx);
  const summary: DraftSummary = { suggested: 0, suggestedLabels: [], sensitive: 0, sensitiveLabels: [], otherPerson: 0, empty: 0, total: fields.length };
  for (const field of fields) {
    // Defence in depth: whatever a suggestion rule says, a sensitive or
    // another person's field starts empty.
    const secret = sensitiveKindOf(field);
    if (secret || isOtherPerson(field.label)) {
      state[field.id] = { value: null, status: 'empty' };
      if (secret) {
        summary.sensitive++;
        if (!summary.sensitiveLabels.includes(KIND_LABELS[secret])) summary.sensitiveLabels.push(KIND_LABELS[secret]);
      } else summary.otherPerson++;
      continue;
    }
    if (state[field.id]?.status === 'suggested') { summary.suggested++; summary.suggestedLabels.push(field.label); }
    else summary.empty++;
  }
  return { state, summary };
}

/** Values the user said to the 비서, put in as proposals after the approved
 * card (`chatFill.ts`). Defence in depth: a sensitive box, another person's
 * box, a non-text box or an rrn/account-shaped value is skipped whatever the
 * server named; a date is re-read and written in the box's format. */
export function applyChatValues(draft: Draft, fields: TemplateField[], values: Record<string, string>): Draft {
  const state: FormState = { ...draft.state };
  const summary: DraftSummary = { ...draft.summary, suggestedLabels: [...draft.summary.suggestedLabels], chatLabels: [] };
  for (const field of fields) {
    const said = values[field.id];
    if (said === undefined || sensitiveKindOf(field) || isOtherPerson(field.label) || (field.type !== 'text' && field.type !== 'date') || looksSensitive(said)) continue;
    let value = cleanValue(said.replace(/\p{Cf}/gu, ''), field.type).trim();
    if (field.type === 'date') {
      const parts = parseDate(value);
      if (!parts) continue;
      value = formatDate(parts, field.format);
    } else if (field.part) value = normalizePart(value, field.part);
    if (!value) continue;
    if (state[field.id]?.status !== 'suggested') {
      summary.suggested++;
      if (summary.empty > 0) summary.empty--;
    } else {
      // Was a profile suggestion: now the user's own value.
      const at = summary.suggestedLabels.indexOf(field.label);
      if (at >= 0) summary.suggestedLabels.splice(at, 1);
    }
    state[field.id] = { value: textValue(value), status: 'suggested', sourceLabel: '비서에게 말한 값' };
    if (!summary.chatLabels?.includes(field.label)) summary.chatLabels?.push(field.label);
  }
  return { state, summary };
}

/** The banner on a draft the agent opened. */
/** Only the fields actually pre-filled, at most four by name. */
function prefilledList(labels: string[]): string {
  const names = [...new Set(labels.map(l => l.trim()).filter(Boolean))];
  return names.length > 4 ? `${names.slice(0, 4).join('·')} 등` : names.join('·');
}

export function draftNote(summary: DraftSummary): string {
  const said = summary.chatLabels?.length ? [`대화에서 말한 값(${prefilledList(summary.chatLabels)})을 넣어 뒀어요.`] : [];
  const basic = summary.suggestedLabels.length ? [`기본 정보(${prefilledList(summary.suggestedLabels)})는 비서가 미리 채워뒀어요.`] : [];
  const parts = summary.suggested
    ? [...said, ...basic, `나머지 칸은 직접 적어주세요. 미리 채운 ${summary.suggested}칸도 하나씩 확인해야 반영돼요.`]
    : ['미리 채울 수 있는 칸이 없었어요. 직접 입력해 주세요.'];
  if (summary.sensitive) parts.push(`${summary.sensitiveLabels.join('·')} ${summary.sensitive}칸은 비워 뒀어요.`);
  if (summary.otherPerson) parts.push(`본인이 아닌 분의 칸 ${summary.otherPerson}개는 직접 입력해 주세요.`);
  return parts.join(' ');
}
