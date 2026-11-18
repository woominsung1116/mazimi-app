// Streams the optional AI summary of a grounded answer. The quotes are
// already on screen; this only fills the "AI 요약" section. Uses expo/fetch,
// whose response body is a readable stream on device.
import { fetch as streamingFetch } from 'expo/fetch';
import { API_BASE_URL, ApiRequestError } from './api';
import { useAuthStore } from '../store/auth';
import type { AnswerStreamEvent, AssistantTopic } from './assistantTypes';
import { parseAnswerStream, streamErrorMessage } from './grounding';

/** A 160-token answer is a few KB of JSON lines; anything far larger is not ours. */
const MAX_BUFFER_CHARS = 64 * 1024;

export async function streamBenefitAnswer(
  programId: string, question: string, previousTopic: AssistantTopic | undefined,
  onEvent: (event: AnswerStreamEvent) => void, signal: AbortSignal,
): Promise<void> {
  const send = (token: string | null) => streamingFetch(`${API_BASE_URL}/api/v1/programs/${encodeURIComponent(programId)}/assistant/answer-stream`, {
    method: 'POST', signal,
    headers: { 'Content-Type': 'application/json', Accept: 'application/x-ndjson', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    body: JSON.stringify({ question, allow_ai_processing: true, previous_topic: previousTopic }),
  });
  const userId = useAuthStore.getState().user?.id;
  // A logout or account switch ends this account's stream.
  const sameAccount = () => !!userId && useAuthStore.getState().user?.id === userId;
  const token = useAuthStore.getState().token;
  let response = await send(token);
  if (response.status === 401 && token) {
    await response.body?.cancel();
    const auth = useAuthStore.getState();
    if (!sameAccount()) throw new ApiRequestError('로그인 상태가 변경됐어요.', 401);
    if (auth.token !== token || await auth.refreshAccessToken()) {
      if (!sameAccount()) throw new ApiRequestError('로그인 상태가 변경됐어요.', 401);
      response = await send(useAuthStore.getState().token);
    }
  }
  if (!response.ok) {
    let code: string | null = null;
    try { const body = await response.json(); code = typeof body?.code === 'string' ? body.code : null; } catch { /* sanitized or non-JSON */ }
    throw new ApiRequestError(streamErrorMessage(response.status, code), response.status);
  }
  const reader = response.body?.getReader();
  if (!reader) throw new ApiRequestError(streamErrorMessage(503), 503);
  const decoder = new TextDecoder();
  let buffer = '';
  const emit = (events: AnswerStreamEvent[]) => {
    if (!sameAccount()) throw new ApiRequestError('로그인 상태가 변경됐어요.', 401);
    events.forEach(onEvent);
  };
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      if (buffer.length > MAX_BUFFER_CHARS) throw new ApiRequestError(streamErrorMessage(503), 503);
      const parsed = parseAnswerStream(buffer);
      buffer = parsed.rest;
      emit(parsed.events);
    }
    emit(parseAnswerStream(`${buffer}${decoder.decode()}\n`).events);
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
  }
}
