/**
 * lib/vault.ts — Shared vault types, storage and the one encrypted save path
 *
 * Single source of truth for StoredDocument / DocumentType and for how a file
 * enters the vault. Every file write (a picked file on the vault screen, the
 * detail card's official form, the 서류 작성 도우미's filled PDF) goes through
 * storeVaultFile; every list change goes through the one queue (runVault).
 * Documents stay on this device; nothing here uploads to the server.
 */

import { useQuery } from "@tanstack/react-query";
import { useAuthStore } from "@/store/auth";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { Platform } from "react-native";
import * as Crypto from "expo-crypto";
import * as FileSystem from "expo-file-system/legacy";
import { decryptFile, deleteEncryptionKey, encryptBytesToFile, encryptFileTo, generateEncryptionKey } from "./crypto";
import { cleanDocumentName } from "./documentName";
// Kept light on purpose: the list screens import this module, so the save logic
// (download, pdf-lib) lives in lib/vaultSave.ts and the form-fill cleanup loads lazily.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

export const vaultStorageKey = (userId: string) => `document_vault_v2:${userId}`;
export const ACCOUNT_ID = /^[0-9a-f-]{36}$/i;
/** The vault's cap for any stored file (picked files and filled PDFs). */
export const MAX_VAULT_FILE_BYTES = 20 * 1024 * 1024;

export async function clearAccountVault(userId: string): Promise<void> {
  if (!ACCOUNT_ID.test(userId)) throw new Error("Invalid account");
  // A save that began before this must not write the account back after it is cleared.
  generation.set(userId, (generation.get(userId) ?? 0) + 1);
  // Same deadline as every vault operation. Its deletes count as a started write: a
  // stalled clear keeps the queue until it settles, so no later save races its deletes.
  return runVault(async state => { state.writeStarted = true; await clearAccountFiles(userId); });
}

async function clearAccountFiles(userId: string): Promise<void> {
  if (Platform.OS !== "web" && FileSystem.documentDirectory) {
    await FileSystem.deleteAsync(`${FileSystem.documentDirectory}vault/${userId}`, { idempotent: true });
    await deleteEncryptionKey(userId);
  }
  await AsyncStorage.removeItem(vaultStorageKey(userId));
  // 서류 작성 도우미: opt-in remembered contact details and the fill history.
  const { clearFormFillData } = await import("./formFill/device");
  await clearFormFillData(userId);
}

// ---------------------------------------------------------------------------
// Types (canonical — document-vault.tsx re-exports these)
// ---------------------------------------------------------------------------

export type DocumentType =
  | "resident_register"   // 주민등록등본
  | "income_proof"        // 소득증명원
  | "enrollment_cert"     // 재학증명서
  | "leave_cert"          // 휴학증명서
  | "graduation_cert"     // 졸업증명서
  | "health_insurance"    // 건강보험료 납부확인서
  | "family_relation"     // 가족관계증명서
  | "bank_statement"      // 통장사본
  | "disability_cert"     // 장애인증명서
  | "other";              // 기타

/** Where an app-produced file came from; lets a second save of the same official form be detected. */
export type VaultSource =
  | { kind: "official_form"; programId: string; resourceId: string; contentHash: string }
  | { kind: "filled_form"; programId: string; resourceId: string; templateHash: string };

export interface StoredDocument {
  id: string;
  type: DocumentType;
  name: string;
  /** Local file URI (expo-file-system copy) — null if metadata-only */
  fileUri: string | null;
  /** MIME type of the stored file */
  mimeType: string | null;
  /** ISO date string */
  issuedAt: string;
  /** ISO date string — null if no explicit expiry */
  expiresAt: string | null;
  /** ISO date string of when this entry was added to the vault */
  addedAt: string;
  note: string | null;
  /** Whether the stored file has been AES-256-CBC encrypted with the user's derived key. */
  encrypted: boolean;
  /** Set only for files the app saved itself (official form, filled PDF). */
  source?: VaultSource;
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

export async function loadVaultDocuments(userId: string): Promise<StoredDocument[]> {
  try {
    const raw = await AsyncStorage.getItem(vaultStorageKey(userId));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as StoredDocument[]) : [];
  } catch {
    return [];
  }
}

/** For writes: only a missing list is empty. An unreadable one is an error, so a save never overwrites it. */
async function readVaultStrict(userId: string): Promise<StoredDocument[]> {
  const raw = await AsyncStorage.getItem(vaultStorageKey(userId));
  if (raw === null) return [];
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed)) throw new Error("Unreadable vault list");
  return parsed as StoredDocument[];
}

// Saves from the vault screen, the detail card and the form helper, an account clear and
// the startup sweep can overlap: each runs alone, in order, through runVault.
let queue: Promise<unknown> = Promise.resolve();
const generation = new Map<string, number>();
/** Bumped when an account is cleared; a save that began earlier is refused. */
export const vaultGeneration = (userId: string) => generation.get(userId) ?? 0;

/** Long enough to encrypt a 20 MiB file on a slow phone. */
export const vaultWriteTimeout = { ms: 90_000 };
/** An operation that ran past the deadline. `writeStarted`: its storage write had begun and may still land. */
export class VaultTimeoutError extends Error {
  constructor(readonly writeStarted: boolean) { super("Vault write timed out"); this.name = "VaultTimeoutError"; }
}
/** Shared with the running operation: once `cancelled`, it must not leave anything behind. */
export interface VaultRun { readonly cancelled: boolean; writeStarted: boolean }

/**
 * Runs `task` alone, after every earlier vault operation, with a deadline. The
 * caller's promise rejects at the deadline (VaultTimeoutError). The queue then
 * moves on, except when the task had already begun its storage write: that
 * write may still land, so the next operation waits for the task to settle.
 */
export function runVault<T>(task: (run: VaultRun) => Promise<T>, timeoutMs = vaultWriteTimeout.ms): Promise<T> {
  const state = { cancelled: false, writeStarted: false };
  let work: Promise<T> | null = null;
  const outcome = queue.then(() => new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => { state.cancelled = true; reject(new VaultTimeoutError(state.writeStarted)); }, timeoutMs);
    work = task(state);
    work.then(resolve, reject).finally(() => clearTimeout(timer));
  }));
  queue = outcome.catch(e => (e instanceof VaultTimeoutError && e.writeStarted && work ? work.catch(() => {}) : undefined));
  return outcome;
}

export function updateVault<T>(userId: string, change: (docs: StoredDocument[], run: VaultRun) => Promise<{ docs: StoredDocument[]; result: T }>, started = vaultGeneration(userId), timeoutMs = vaultWriteTimeout.ms): Promise<T> {
  return runVault(async state => {
    if (vaultGeneration(userId) !== started) throw new Error("Account cleared");
    const { docs, result } = await change(await readVaultStrict(userId), state);
    if (state.cancelled) throw new VaultTimeoutError(false);
    state.writeStarted = true;
    await AsyncStorage.setItem(vaultStorageKey(userId), JSON.stringify(docs));
    return result;
  }, timeoutMs);
}

/**
 * Files are stored only where the vault can encrypt them: the app's document
 * directory on iOS/Android. The web build has no such directory, so it stores
 * no files at all (metadata only) and the save actions are hidden there.
 */
export function vaultFilesSupported(): boolean {
  return Platform.OS !== "web" && !!FileSystem.documentDirectory;
}

const vaultDir = (userId: string) => `${FileSystem.documentDirectory}vault/${userId}`;
async function ensureVaultDir(userId: string): Promise<string> {
  if (!vaultFilesSupported()) throw new Error("File storage unavailable");
  if (!ACCOUNT_ID.test(userId)) throw new Error("Invalid account");
  const dir = vaultDir(userId);
  const dirInfo = await FileSystem.getInfoAsync(dir);
  if (!dirInfo.exists) await FileSystem.makeDirectoryAsync(dir, { intermediates: true });
  return dir;
}

export function newDocumentId(): string {
  return `doc_${Date.now()}_${Array.from(Crypto.getRandomBytes(4), b => b.toString(16).padStart(2, "0")).join("")}`;
}

/** What a new entry says about itself; the file fields are filled in by storeVaultFile. */
export interface VaultEntry { type: DocumentType; name: string; issuedAt?: string; mimeType?: string | null; source?: VaultSource }
/** The file: bytes the app holds (official form, filled PDF) or a picked file's URI. */
export type VaultFileInput = { bytes: Uint8Array } | { sourceUri: string };
/** Thrown by a caller's `refuse` check (duplicate) inside the queue; passes through unchanged. */
export interface StoreOptions { extension: string; started?: number; refuse?: (docs: StoredDocument[]) => void }

const newEntry = (id: string, entry: VaultEntry, fileUri: string | null, now: string): StoredDocument => ({
  id, type: entry.type, name: cleanDocumentName(entry.name), fileUri, mimeType: fileUri ? entry.mimeType ?? null : null,
  issuedAt: entry.issuedAt ?? now, expiresAt: null, addedAt: now, note: null, encrypted: !!fileUri,
  ...(entry.source ? { source: entry.source } : {}),
});

/**
 * The one way a file enters the vault. Inside the queue, with the account
 * generation taken when the save began: refuse (duplicate) → vault folder →
 * key → encrypt from memory into `<id>.<ext>.enc` → list write. Liveness
 * (not timed out, account not cleared) is checked before the folder, before
 * the key and after encrypting, so a cleared account never gets a new folder,
 * salt or file. No plaintext copy is written. On any refusal the ciphertext is
 * deleted, unless the list write had begun (then entry and file stay together).
 */
export async function storeVaultFile(userId: string, input: VaultFileInput, entry: VaultEntry, options: StoreOptions): Promise<StoredDocument> {
  if (!vaultFilesSupported()) throw new Error("File storage unavailable");
  if (!ACCOUNT_ID.test(userId)) throw new Error("Invalid account");
  if (!/^[a-zA-Z0-9]+$/.test(options.extension)) throw new Error("Invalid file extension");
  const started = options.started ?? vaultGeneration(userId);
  if ("bytes" in input) {
    if (!input.bytes.length || input.bytes.length > MAX_VAULT_FILE_BYTES) throw new Error("File too large or missing");
  } else {
    const info = await FileSystem.getInfoAsync(input.sourceUri);
    if (!info.exists || !info.size || info.size > MAX_VAULT_FILE_BYTES) throw new Error("File too large or missing");
  }
  let written: string | null = null;
  try {
    return await updateVault(userId, async (docs, run) => {
      const live = () => { if (run.cancelled || vaultGeneration(userId) !== started) throw new Error("Save no longer current"); };
      options.refuse?.(docs);
      const id = newDocumentId();
      live();
      const dir = await ensureVaultDir(userId);
      live();
      const key = await generateEncryptionKey(userId);
      live();
      const encUri = `${dir}/${id}.${options.extension}.enc`;
      written = encUri;
      if ("bytes" in input) await encryptBytesToFile(input.bytes, encUri, key);
      else await encryptFileTo(input.sourceUri, encUri, key);
      // Timed out or cleared while encrypting: the caller's cleanup may have run before this
      // file existed, so the task removes what it wrote, where it wrote it.
      try { live(); } catch (e) { await FileSystem.deleteAsync(encUri, { idempotent: true }).catch(() => {}); throw e; }
      const now = new Date().toISOString();
      const doc = newEntry(id, entry, encUri, now);
      // The agency revised an official form: older copies stay (user data is never deleted) but show as expired.
      const src = entry.source;
      const older = (d: StoredDocument) => src?.kind === "official_form" && d.source?.kind === "official_form" && d.source.programId === src.programId && d.source.resourceId === src.resourceId && d.source.contentHash !== src.contentHash && !d.expiresAt;
      return { docs: [doc, ...docs.map(d => older(d) ? { ...d, expiresAt: now } : d)], result: doc };
    }, started);
  } catch (e) {
    const mayLand = e instanceof VaultTimeoutError && e.writeStarted;
    if (written && !mayLand) await FileSystem.deleteAsync(written, { idempotent: true }).catch(() => {});
    throw e;
  }
}

/** The entry is listed but its file is not on this device (the list came back from a backup; the vault folder never does). */
export class VaultFileMissingError extends Error {
  constructor() { super("다른 기기에서 복원된 항목이라 파일이 없어요. 다시 등록해 주세요."); this.name = "VaultFileMissingError"; }
}

/**
 * Decrypts a stored file to a temp copy in cache/vault_tmp for the share sheet.
 * The caller deletes the copy (the startup sweep catches it otherwise). The file
 * is checked before any key is derived, so a restored entry creates no key.
 */
export async function decryptVaultFile(userId: string, doc: StoredDocument): Promise<string> {
  if (!doc.fileUri || !doc.encrypted) throw new Error("Legacy unencrypted file");
  if (!(await FileSystem.getInfoAsync(doc.fileUri)).exists) throw new VaultFileMissingError();
  return decryptFile(doc.fileUri, await generateEncryptionKey(userId), shareName(doc));
}
/** The document's own name for the decrypted copy (what 저장·공유 shows), never the storage id. */
export function shareName(doc: Pick<StoredDocument, "name" | "fileUri">): string {
  const ext = doc.fileUri?.replace(/\.enc$/, "").match(/\.([A-Za-z0-9]{1,5})$/)?.[1] ?? "";
  const base = cleanDocumentName(doc.name.replace(/[\\/:*?"<>|]/g, " "), 80) || "서류";
  return ext ? `${base}.${ext}` : base;
}

/** A list entry without a file (the vault screen's metadata-only add). */
export function addVaultEntry(userId: string, entry: VaultEntry): Promise<StoredDocument> {
  return updateVault(userId, async docs => {
    const doc = newEntry(newDocumentId(), entry, null, new Date().toISOString());
    return { docs: [doc, ...docs], result: doc };
  });
}

/**
 * Startup sweep: decrypted copies left in cache/vault_tmp/ (app killed while a
 * file was open) and any non-.enc file under vault/<account>/ (a plaintext copy
 * from the old copy-then-encrypt path). A file a list entry still points to is
 * kept: that is the user's document, never deleted automatically.
 */
export function sweepVaultLeftovers(): Promise<void> {
  if (!vaultFilesSupported()) return Promise.resolve();
  return runVault(async state => {
    state.writeStarted = true;
    if (FileSystem.cacheDirectory) await FileSystem.deleteAsync(`${FileSystem.cacheDirectory}vault_tmp`, { idempotent: true }).catch(() => {});
    const root = `${FileSystem.documentDirectory}vault`;
    if (!(await FileSystem.getInfoAsync(root)).exists) return;
    for (const account of await FileSystem.readDirectoryAsync(root)) {
      if (!ACCOUNT_ID.test(account)) continue;
      const dir = `${root}/${account}`;
      const referenced = new Set((await loadVaultDocuments(account)).map(d => d.fileUri).filter(Boolean));
      const names = await FileSystem.readDirectoryAsync(dir).catch(() => [] as string[]);
      for (const name of names) {
        const uri = `${dir}/${name}`;
        if (name.endsWith(".enc") || referenced.has(uri)) continue;
        await FileSystem.deleteAsync(uri, { idempotent: true }).catch(() => {});
      }
    }
  });
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/** 보관함 서류 목록을 TanStack Query로 캐싱하여 반환한다. */
export function useVaultDocuments(): StoredDocument[] {
  const userId = useAuthStore(s => s.user?.id);
  const { data = [] } = useQuery({
    queryKey: ["vault-documents", userId],
    enabled: !!userId,
    queryFn: () => loadVaultDocuments(userId!),
    staleTime: 30_000,
  });
  return data;
}
