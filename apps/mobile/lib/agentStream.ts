// Streams an agent run's progress events (NDJSON). The stream ends at
// `done`, or at `backgrounded` when the run outlives the wait: the screen then
// polls GET /runs/{id} and a push arrives when it finishes.
import { fetch as streamingFetch } from 'expo/fetch';
import { API_BASE_URL, ApiRequestError } from './api';
import { useAuthStore } from '../store/auth';
import type { AgentEvent } from './agentTypes';
import { parseAgentEvents } from './agent';

const MAX_BUFFER_CHARS = 512 * 1024;

export async function streamAgentRun(runId: string, onEvent: (event: AgentEvent) => void, signal: AbortSignal): Promise<void> {
  const send = (token: string | null) => streamingFetch(`${API_BASE_URL}/api/v1/assistant/runs/${encodeURIComponent(runId)}/events`, {
    method: 'GET', signal,
    headers: { Accept: 'application/x-ndjson', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
  });
  const userId = useAuthStore.getState().user?.id;
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
  if (!response.ok) throw new ApiRequestError('진행 상황을 불러오지 못했어요.', response.status);
  const reader = response.body?.getReader();
  if (!reader) throw new ApiRequestError('진행 상황을 불러오지 못했어요.', 503);
  const decoder = new TextDecoder();
  let buffer = '';
  const emit = (events: AgentEvent[]) => {
    if (!sameAccount()) throw new ApiRequestError('로그인 상태가 변경됐어요.', 401);
    events.forEach(onEvent);
  };
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      if (buffer.length > MAX_BUFFER_CHARS) throw new ApiRequestError('진행 상황이 너무 길어요.', 503);
      const parsed = parseAgentEvents(buffer);
      buffer = parsed.rest;
      emit(parsed.events);
    }
    emit(parseAgentEvents(`${buffer}${decoder.decode()}\n`).events);
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
  }
}
