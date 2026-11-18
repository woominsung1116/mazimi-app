/**
 * lib/vaultSave.ts — app-produced files into the vault: the reviewed official
 * form (detail card) and the 서류 작성 도우미's filled PDF.
 *
 * Separate from lib/vault.ts so the list screens (profile, apply-assistant)
 * don't load the download and PDF code. Every file is written by
 * lib/vault.ts storeVaultFile (the one vault write path); nothing here uploads anything.
 */
import { downloadVerifiedTemplate } from "./formFill/device";
import { MAX_TEMPLATE_BYTES, TemplateIntegrityError } from "./formFill/pdf";
import { applicationFormName, cleanDocumentName } from "./documentName";
import {
  ACCOUNT_ID, MAX_VAULT_FILE_BYTES, loadVaultDocuments, storeVaultFile, vaultFilesSupported, vaultGeneration,
  type StoredDocument, type VaultSource,
} from "./vault";

// ---------------------------------------------------------------------------
// App-produced files: the verified official form and the filled PDF
// ---------------------------------------------------------------------------

export type VaultSaveProblem = "unsupported" | "unverified" | "format" | "too_large" | "duplicate" | "failed";
const PROBLEM_COPY: Record<VaultSaveProblem, string> = {
  unsupported: "이 기기에서는 파일을 보관할 수 없어요. 마지미 앱에서 이용해 주세요.",
  unverified: "내려받은 파일이 확인된 공식 양식과 달라 보관함에 넣지 않았어요. 공식 양식 받기로 기관 파일을 직접 받아 주세요.",
  format: "보관함에 넣을 수 없는 파일 형식이에요. 공식 양식 받기로 기관 파일을 직접 받아 주세요.",
  too_large: "파일이 너무 커서 보관함에 넣지 않았어요. 공식 양식 받기로 기관 파일을 직접 받아 주세요.",
  duplicate: "이미 보관함에 있어요.",
  failed: "보관함에 저장하지 못했어요. 잠시 후 다시 시도해 주세요.",
};
export class VaultSaveError extends Error {
  constructor(readonly problem: VaultSaveProblem, message = PROBLEM_COPY[problem]) {
    super(message);
    this.name = "VaultSaveError";
  }
}

export type OfficialFormat = "pdf" | "hwp" | "hwpx";
const FORMATS: Record<OfficialFormat, { mimeType: string; magic: number[] }> = {
  pdf: { mimeType: "application/pdf", magic: [0x25, 0x50, 0x44, 0x46, 0x2d] },                        // %PDF-
  hwp: { mimeType: "application/x-hwp", magic: [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1] },      // HWP 5 (OLE compound file)
  hwpx: { mimeType: "application/hwp+zip", magic: [0x50, 0x4b, 0x03, 0x04] },                          // HWPX (OWPML zip)
};

/** The declared format, only when the vault takes it. */
export function officialFormat(fileFormat: string | null | undefined): OfficialFormat | null {
  const f = fileFormat?.toLowerCase();
  return f === "pdf" || f === "hwp" || f === "hwpx" ? f : null;
}

/** The downloaded bytes must be the declared format (magic bytes) and within the 8 MiB form cap. */
export function checkOfficialFormBytes(bytes: Uint8Array, format: OfficialFormat): void {
  if (!bytes.length || bytes.length > MAX_TEMPLATE_BYTES) throw new VaultSaveError("too_large");
  const magic = FORMATS[format].magic;
  if (bytes.length < magic.length || magic.some((b, i) => bytes[i] !== b)) throw new VaultSaveError("format");
}

export function officialFormName(programTitle: string, resourceLabel: string): string {
  return cleanDocumentName(`${cleanDocumentName(programTitle)} – ${cleanDocumentName(resourceLabel)}`);
}

export function findOfficialForm(docs: StoredDocument[], programId: string, resourceId: string, contentHash: string): StoredDocument | undefined {
  const hash = contentHash.toLowerCase();
  return docs.find(d => d.source?.kind === "official_form" && d.source.programId === programId && d.source.resourceId === resourceId && d.source.contentHash === hash && !!d.fileUri);
}

/** storeVaultFile with this module's copy: a duplicate stays "duplicate", anything else is the fixed "failed" text. */
async function storeEncrypted(userId: string, bytes: Uint8Array, entry: { name: string; mimeType: string; extension: string; source: VaultSource }, isDuplicate: (docs: StoredDocument[]) => boolean, started: number): Promise<StoredDocument> {
  try {
    return await storeVaultFile(userId, { bytes }, { type: "other", name: entry.name, mimeType: entry.mimeType, source: entry.source }, {
      extension: entry.extension, started, refuse: docs => { if (isDuplicate(docs)) throw new VaultSaveError("duplicate"); },
    });
  } catch (e) {
    if (e instanceof VaultSaveError) throw e;
    throw new VaultSaveError("failed");
  }
}

export interface OfficialFormResource { id: string; url: string; label: string; file_format: string | null; byte_size: number | null; content_hash: string | null }

/**
 * Puts the reviewed official form into the vault. The caller passes the
 * resource it just re-read from the server (currentResource). The file is
 * downloaded over HTTPS, refused unless its SHA-256 equals `content_hash`,
 * it is at most 8 MiB and its magic bytes match `file_format`; then it is
 * encrypted straight from memory. A form already in the vault (same
 * programme, resource and hash) is not saved twice.
 */
export async function saveOfficialForm(userId: string, program: { id: string; title: string }, resource: OfficialFormResource): Promise<StoredDocument> {
  if (!vaultFilesSupported()) throw new VaultSaveError("unsupported");
  if (!ACCOUNT_ID.test(userId)) throw new VaultSaveError("failed");
  const started = vaultGeneration(userId);
  const format = officialFormat(resource.file_format);
  if (!format) throw new VaultSaveError("format");
  const hash = resource.content_hash?.toLowerCase() ?? "";
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new VaultSaveError("unverified");
  if (resource.byte_size != null && resource.byte_size > MAX_TEMPLATE_BYTES) throw new VaultSaveError("too_large");
  if (findOfficialForm(await loadVaultDocuments(userId), program.id, resource.id, hash)) throw new VaultSaveError("duplicate");
  let bytes: Uint8Array | null = null;
  try {
    try {
      bytes = await downloadVerifiedTemplate({ url: resource.url, content_hash: hash, byte_size: resource.byte_size });
    } catch (e) {
      if (e instanceof TemplateIntegrityError) throw new VaultSaveError(e.problem === "size" ? "too_large" : "unverified");
      throw new VaultSaveError("failed");
    }
    checkOfficialFormBytes(bytes, format);
    return await storeEncrypted(userId, bytes, {
      name: officialFormName(program.title, resource.label),
      mimeType: FORMATS[format].mimeType,
      extension: format,
      source: { kind: "official_form", programId: program.id, resourceId: resource.id, contentHash: hash },
    }, docs => !!findOfficialForm(docs, program.id, resource.id, hash), started);
  } finally {
    if (bytes) bytes.fill(0);
  }
}

/**
 * Saves the PDF the 서류 작성 도우미 just exported. It may hold 주민등록번호 or
 * 계좌번호, so it goes from memory straight into the encrypted file; no
 * plaintext copy is written and nothing leaves the device. The buffer is
 * zeroed once saved, or when no retry could help; only a transient failure
 * ("failed") leaves it for 다시 시도. The caller drops its reference either way
 * when the screen closes.
 */
export async function saveFilledForm(userId: string, bytes: Uint8Array, form: { programId: string; programTitle: string; resourceId: string; resourceLabel: string; templateHash: string }): Promise<StoredDocument> {
  let retryable = false;
  try {
    if (!vaultFilesSupported()) throw new VaultSaveError("unsupported");
    if (!ACCOUNT_ID.test(userId)) throw new VaultSaveError("failed");
    const started = vaultGeneration(userId);
    if (bytes.length > MAX_VAULT_FILE_BYTES) throw new VaultSaveError("too_large");
    const magic = FORMATS.pdf.magic;
    if (bytes.length < magic.length || magic.some((b, i) => bytes[i] !== b)) throw new VaultSaveError("format");
    return await storeEncrypted(userId, bytes, {
      name: cleanDocumentName(`${applicationFormName(form.programTitle)} (작성본)`),
      mimeType: FORMATS.pdf.mimeType,
      extension: "pdf",
      source: { kind: "filled_form", programId: form.programId, resourceId: form.resourceId, templateHash: form.templateHash.toLowerCase() },
    }, () => false, started);
  } catch (e) {
    retryable = e instanceof VaultSaveError && e.problem === "failed";
    throw e;
  } finally {
    if (!retryable) bytes.fill(0);
  }
}

