import { tokenStorage } from './tokenStorage';
const KEY = 'mazimi_pending_application';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// Store only a program ID, never a caller-supplied redirect URL.
export async function rememberApplication(programId: string) {
  if (!UUID.test(programId)) return;
  await tokenStorage.setItemAsync(KEY, JSON.stringify({ programId, expiresAt: Date.now() + 30 * 60 * 1000 }));
}
export async function takePendingApplication() {
  const raw = await tokenStorage.getItemAsync(KEY);
  await clearPendingApplication();
  if (!raw) return null;
  try {
    const data = JSON.parse(raw);
    if (typeof data.programId !== 'string' || !UUID.test(data.programId) || typeof data.expiresAt !== 'number' || data.expiresAt < Date.now()) return null;
    return `/apply-assistant?programId=${data.programId}` as const;
  } catch { return null; }
}
export async function clearPendingApplication() {
  await tokenStorage.deleteItemAsync(KEY);
}
