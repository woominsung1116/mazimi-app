/**
 * 서류 작성 도우미 — device I/O: verified template download, font loading,
 * share/save, opt-in memory and the on-device 작성 기록.
 *
 * Nothing here uploads anything. Template bytes and filled PDFs live in the
 * app cache only for the moment they are needed and are deleted right after.
 */
import { Platform, Share } from 'react-native';
import * as Crypto from 'expo-crypto';
import * as FileSystem from 'expo-file-system/legacy';
import * as SecureStore from 'expo-secure-store';
import * as Sharing from 'expo-sharing';
import { Asset } from 'expo-asset';
import AsyncStorage from '@react-native-async-storage/async-storage';
import { base64ToBytes, bytesToBase64, MAX_TEMPLATE_BYTES, TemplateIntegrityError, verifyTemplateBytes, type TemplateSource } from './pdf';
import { sanitizeRemembered, type Remembered } from './fields';
import { cleanDocumentName } from '../documentName';

import { CACHE_DIR, clearFormFillCache } from './cache';
export { clearFormFillCache };
const randomName = () => Array.from(Crypto.getRandomBytes(8), b => b.toString(16).padStart(2, '0')).join('');

export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await Crypto.digest(Crypto.CryptoDigestAlgorithm.SHA256, bytes as Uint8Array<ArrayBuffer>);
  return Array.from(new Uint8Array(digest), b => b.toString(16).padStart(2, '0')).join('');
}

/** Same public-host rule as the server: HTTPS, no credentials/ports, no IP or local hosts. */
export function isSafeTemplateUrl(value: string): boolean {
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    const ip = /^[\d.]+$/.test(host) || host.includes(':');
    return url.protocol === 'https:' && !url.username && !url.password && (!url.port || url.port === '443')
      && host.includes('.') && !ip && host !== 'localhost' && !host.endsWith('.localhost');
  } catch { return false; }
}

/**
 * Downloads the verified template to the cache, reads it into memory,
 * deletes the file and refuses the bytes unless size and SHA-256 equal what
 * the server verified (the official PDF, or Mazimi's converted PDF).
 */
export async function downloadVerifiedTemplate(resource: TemplateSource): Promise<Uint8Array> {
  if (Platform.OS === 'web') throw new Error('웹에서는 양식을 직접 작성할 수 없어요.');
  if (!isSafeTemplateUrl(resource.url)) throw new Error('공식 자료 주소를 다시 확인해 주세요.');
  await FileSystem.makeDirectoryAsync(CACHE_DIR(), { intermediates: true });
  const target = `${CACHE_DIR()}template-${randomName()}.bin`;
  try {
    // The cap holds while downloading: the progress callback cancels as soon as the
    // declared length or the bytes written pass 8 MiB. (The native call follows
    // redirects and does not report the final URL, so only the requested URL is checked.)
    let tooLarge = false;
    const task: FileSystem.DownloadResumable = FileSystem.createDownloadResumable(resource.url, target, { cache: false }, progress => {
      if (tooLarge) return;
      if (progress.totalBytesWritten > MAX_TEMPLATE_BYTES || progress.totalBytesExpectedToWrite > MAX_TEMPLATE_BYTES) {
        tooLarge = true;
        void task.cancelAsync().catch(() => {});
      }
    });
    let result: FileSystem.FileSystemDownloadResult | undefined;
    try { result = await task.downloadAsync(); } catch (e) { if (!tooLarge) throw e; }
    if (tooLarge) throw new TemplateIntegrityError('size');
    if (!result || result.status !== 200) throw new Error('공식 양식을 내려받지 못했어요. 잠시 후 다시 시도해 주세요.');
    const info = await FileSystem.getInfoAsync(target);
    if (!info.exists || !info.size) throw new Error('공식 양식 파일 크기를 확인하지 못했어요.');
    if (info.size > MAX_TEMPLATE_BYTES) throw new TemplateIntegrityError('size');
    const bytes = base64ToBytes(await FileSystem.readAsStringAsync(target, { encoding: FileSystem.EncodingType.Base64 }));
    await verifyTemplateBytes(bytes, resource, sha256Hex);
    return bytes;
  } finally {
    await FileSystem.deleteAsync(target, { idempotent: true }).catch(() => {});
  }
}

let fontCache: Uint8Array | null = null;
/** Bundled NanumGothic (OFL), embedded unmodified into the filled PDF for Korean text. */
export async function loadFormFont(): Promise<Uint8Array> {
  if (fontCache) return fontCache;
  const asset = Asset.fromModule(require('@/assets/fonts/NanumGothic-Regular.ttf'));
  await asset.downloadAsync();
  if (!asset.localUri) throw new Error('글꼴을 불러오지 못했어요.');
  fontCache = base64ToBytes(await FileSystem.readAsStringAsync(asset.localUri, { encoding: FileSystem.EncodingType.Base64 }));
  return fontCache;
}

/**
 * What the share sheet reports. iOS says whether the user completed an
 * activity (저장, 메일 …) or dismissed the sheet. Android's chooser reports
 * nothing, so the outcome there is `unknown` and the user says whether the
 * file went out.
 */
export type ShareOutcome = 'shared' | 'dismissed' | 'unknown';
export function shareOutcome(platform: string, result: { action: string } | null): ShareOutcome {
  if (platform !== 'ios' || !result) return 'unknown';
  return result.action === Share.sharedAction ? 'shared' : 'dismissed';
}

/** Writes the filled PDF to the cache, opens the system share/save sheet, then deletes the cache copy. */
export async function shareFilledPdf(bytes: Uint8Array, title: string): Promise<ShareOutcome> {
  if (Platform.OS !== 'ios' && !(await Sharing.isAvailableAsync())) throw new Error('이 기기에서는 파일 공유를 사용할 수 없어요.');
  await FileSystem.makeDirectoryAsync(CACHE_DIR(), { intermediates: true });
  const safe = cleanDocumentName(title.replace(/[\\/:*?"<>|]/g, ' '), 60) || '신청서';
  const dir = `${CACHE_DIR()}${randomName()}/`;
  await FileSystem.makeDirectoryAsync(dir, { intermediates: true });
  const uri = `${dir}${safe} (작성본).pdf`;
  try {
    await FileSystem.writeAsStringAsync(uri, bytesToBase64(bytes), { encoding: FileSystem.EncodingType.Base64 });
    // iOS: React Native's Share resolves with the activity result (expo-sharing resolves the same way on cancel).
    if (Platform.OS === 'ios') return shareOutcome('ios', await Share.share({ url: uri }));
    await Sharing.shareAsync(uri, { mimeType: 'application/pdf', UTI: 'com.adobe.pdf', dialogTitle: '작성한 신청서 저장·공유' });
    return 'unknown';
  } finally {
    await FileSystem.deleteAsync(dir, { idempotent: true }).catch(() => {});
  }
}


// ---------------------------------------------------------------------------
// Opt-in memory: name/phone/address/email only, in the OS keychain/keystore.
// ---------------------------------------------------------------------------

const rememberKey = (userId: string) => `form_fill_remembered_v1_${userId.replace(/[^A-Za-z0-9_.-]/g, '')}`;
export async function loadRemembered(userId: string | undefined): Promise<Remembered> {
  if (!userId || Platform.OS === 'web') return {};
  try {
    const raw = await SecureStore.getItemAsync(rememberKey(userId));
    return raw ? sanitizeRemembered(JSON.parse(raw)) : {};
  } catch { return {}; }
}
export async function saveRemembered(userId: string, values: Remembered): Promise<void> {
  if (Platform.OS === 'web') return;
  const clean = sanitizeRemembered(values);
  if (!Object.keys(clean).length) return;
  const merged = { ...(await loadRemembered(userId)), ...clean };
  await SecureStore.setItemAsync(rememberKey(userId), JSON.stringify(merged), { keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY });
}
export async function forgetRemembered(userId: string): Promise<void> {
  if (Platform.OS === 'web') return;
  await SecureStore.deleteItemAsync(rememberKey(userId));
}

// ---------------------------------------------------------------------------
// 작성 기록: when and which template version, never any value.
// ---------------------------------------------------------------------------

export interface FillRecord { id: string; program_id: string; program_title: string; resource_label: string; template_hash: string; created_at: string; filled_fields: number }
export const recordsKey = (userId: string) => `form_fill_records_v1:${userId}`;
export async function loadRecords(userId: string | undefined): Promise<FillRecord[]> {
  if (!userId) return [];
  try {
    const parsed = JSON.parse((await AsyncStorage.getItem(recordsKey(userId))) ?? '[]');
    return Array.isArray(parsed) ? parsed.filter((r): r is FillRecord => !!r && typeof r.id === 'string' && typeof r.template_hash === 'string') : [];
  } catch { return []; }
}
export async function addRecord(userId: string, record: Omit<FillRecord, 'id' | 'created_at'>): Promise<void> {
  const next: FillRecord = { ...record, id: randomName(), created_at: new Date().toISOString(), program_title: cleanDocumentName(record.program_title), resource_label: cleanDocumentName(record.resource_label) };
  await AsyncStorage.setItem(recordsKey(userId), JSON.stringify([next, ...(await loadRecords(userId))].slice(0, 50)));
}
export async function clearFormFillData(userId: string): Promise<void> {
  await AsyncStorage.removeItem(recordsKey(userId));
  await forgetRemembered(userId).catch(() => {});
  await clearFormFillCache();
}
