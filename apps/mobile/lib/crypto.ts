/**
 * lib/crypto.ts — Document vault encryption helpers
 *
 * AES-256-CBC with PKCS7 padding and encrypt-then-HMAC-SHA256.
 *
 * - Key derivation: HMAC-like double-hash with a per-user random salt
 *   stored in expo-secure-store.
 * - MV2H envelope: version (4), random IV (16), ciphertext, HMAC (32).
 * - Uses `aes-js` for the AES primitive, `expo-crypto` for secure
 *   random bytes and SHA-256, and `expo-secure-store` for persisting
 *   the salt and master key material.
 */

import * as Crypto from "expo-crypto";
import * as FileSystem from "expo-file-system/legacy";
import * as SecureStore from "expo-secure-store";
import aesjs from "aes-js";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const IV_LENGTH = 16; // AES block size
const KEY_LENGTH = 32; // 256 bits
const SALT_LENGTH = 32; // 256-bit random salt
// Original items, written with the platform default accessibility (iOS: after first
// unlock, included in encrypted backups and device migration).
const LEGACY_SALT_PREFIX = "vault_salt_";
const LEGACY_MASTER_KEY = "vault_master_key";
// Current items: readable only while unlocked, never leave this device. iOS keeps an
// item's accessibility on update, so these are new item names, not a rewrite.
const SECURE_STORE_SALT_PREFIX = "vault_salt_tdo_";
const SECURE_STORE_MASTER_KEY = "vault_master_key_tdo";
const DEVICE_ONLY = { keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY };

/**
 * Returns the item stored under `key`, moving a legacy item to it first: written
 * under the new name with device-only access, read back and compared, and only
 * then is the legacy item deleted. At every step one copy exists, so a failure
 * part-way keeps the old key and the files it encrypted.
 */
async function readMigrated(key: string, legacyKey: string): Promise<string | null> {
  const current = await SecureStore.getItemAsync(key, DEVICE_ONLY);
  if (current) {
    // A crash after the copy but before the delete leaves the legacy item: finish the move.
    if (await SecureStore.getItemAsync(legacyKey)) await SecureStore.deleteItemAsync(legacyKey);
    return current;
  }
  const legacy = await SecureStore.getItemAsync(legacyKey);
  // Another setup may have moved it between the two reads: the new name is authoritative.
  if (!legacy) return SecureStore.getItemAsync(key, DEVICE_ONLY);
  try {
    await SecureStore.setItemAsync(key, legacy, DEVICE_ONLY);
    if ((await SecureStore.getItemAsync(key, DEVICE_ONLY)) !== legacy) throw new Error("Key copy mismatch");
  } catch {
    // Keep using the legacy item; the move is retried next time.
    return legacy;
  }
  await SecureStore.deleteItemAsync(legacyKey);
  return legacy;
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/** Convert a hex string to Uint8Array. */
function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/** Convert a Uint8Array to a hex string. */
function bytesToHex(bytes: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < bytes.length; i++) {
    hex += bytes[i].toString(16).padStart(2, "0");
  }
  return hex;
}

/** Convert a base64 string to Uint8Array. */
function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** Convert a Uint8Array to a base64 string. */
function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

// ---------------------------------------------------------------------------
// Master key management (stored in SecureStore, never leaves the device)
// ---------------------------------------------------------------------------

/**
 * Retrieve or generate the app-wide master key.
 * This 256-bit random key is generated once and stored in SecureStore.
 * It is combined with the user-specific salt during key derivation.
 */
// One setup at a time per store item. Two at once (a double tap on 확인, an open during
// a save) could otherwise both miss the item mid-move and the second would write a new
// random key over the real one, leaving every stored file unreadable.
const inFlight = new Map<string, Promise<string>>();
function once(storeKey: string, setup: () => Promise<string>): Promise<string> {
  const running = inFlight.get(storeKey);
  if (running) return running;
  const started = setup().finally(() => inFlight.delete(storeKey));
  inFlight.set(storeKey, started);
  return started;
}

function getOrCreateMasterKey(): Promise<string> {
  return once(SECURE_STORE_MASTER_KEY, createMasterKey);
}

async function createMasterKey(): Promise<string> {
  const existing = await readMigrated(SECURE_STORE_MASTER_KEY, LEGACY_MASTER_KEY);
  if (existing) {
    return existing;
  }

  const randomBytes = Crypto.getRandomBytes(KEY_LENGTH);
  const masterKeyHex = bytesToHex(randomBytes);
  await SecureStore.setItemAsync(SECURE_STORE_MASTER_KEY, masterKeyHex, DEVICE_ONLY);
  return masterKeyHex;
}

/**
 * Retrieve or generate a per-user random salt.
 * Stored in SecureStore keyed by userId.
 */
function getOrCreateSalt(userId: string): Promise<string> {
  return once(`${SECURE_STORE_SALT_PREFIX}${userId}`, () => createSalt(userId));
}

async function createSalt(userId: string): Promise<string> {
  const storeKey = `${SECURE_STORE_SALT_PREFIX}${userId}`;
  const existing = await readMigrated(storeKey, `${LEGACY_SALT_PREFIX}${userId}`);
  if (existing) {
    return existing;
  }

  const randomBytes = Crypto.getRandomBytes(SALT_LENGTH);
  const saltHex = bytesToHex(randomBytes);
  await SecureStore.setItemAsync(storeKey, saltHex, DEVICE_ONLY);
  return saltHex;
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

/**
 * Derive a 256-bit encryption key from the userId.
 *
 * Process: SHA256(masterKey + SHA256(salt + userId))
 *
 * - A per-user random salt prevents rainbow-table attacks on userId.
 * - A device-local master key (from SecureStore) ensures that knowing
 *   the userId alone is not enough to derive the key.
 */
export async function generateEncryptionKey(userId: string): Promise<string> {
  const masterKeyHex = await getOrCreateMasterKey();
  const saltHex = await getOrCreateSalt(userId);

  // Inner hash: SHA256(salt + userId)
  const innerDigest = await Crypto.digestStringAsync(
    Crypto.CryptoDigestAlgorithm.SHA256,
    saltHex + userId,
    { encoding: Crypto.CryptoEncoding.HEX },
  );

  // Outer hash: SHA256(masterKey + innerDigest)
  const derivedKey = await Crypto.digestStringAsync(
    Crypto.CryptoDigestAlgorithm.SHA256,
    masterKeyHex + innerDigest,
    { encoding: Crypto.CryptoEncoding.HEX },
  );

  return derivedKey; // 64 hex chars = 256-bit key
}

/** Remove only this account's key material; other accounts retain access. */
export async function deleteEncryptionKey(userId: string): Promise<void> {
  await SecureStore.deleteItemAsync(`${SECURE_STORE_SALT_PREFIX}${userId}`, DEVICE_ONLY);
  await SecureStore.deleteItemAsync(`${LEGACY_SALT_PREFIX}${userId}`);
}

// ---------------------------------------------------------------------------
// AES-256-CBC encryption / decryption
// ---------------------------------------------------------------------------

/**
 * Encrypt `plainBytes` with AES-256-CBC.
 * Returns an authenticated MV2H envelope.
 */
async function aesEncrypt(plainBytes: Uint8Array, keyHex: string): Promise<Uint8Array> {
  const keyBytes = hexToBytes(keyHex.slice(0, KEY_LENGTH * 2));
  const iv = Crypto.getRandomBytes(IV_LENGTH);

  const padded = aesjs.padding.pkcs7.pad(plainBytes);
  const aesCbc = new aesjs.ModeOfOperation.cbc(keyBytes, iv);
  const cipherBytes = aesCbc.encrypt(padded);
  // The padded copy is the whole plaintext again; it is not needed after encryption.
  padded.fill(0);

  // Prepend IV to ciphertext
  const body = new Uint8Array(4 + IV_LENGTH + cipherBytes.length);
  body.set([77, 86, 50, 72]); // MV2H: versioned encrypt-then-MAC envelope
  body.set(iv, 4);
  body.set(cipherBytes, 4 + IV_LENGTH);
  const result = new Uint8Array(body.length + 32);
  result.set(body);
  result.set(await authenticate(body, keyHex), body.length);
  return result;
}

/**
 * Decrypt data produced by `aesEncrypt`.
 * Authenticates the complete versioned envelope before decryption.
 */
async function aesDecrypt(data: Uint8Array, keyHex: string): Promise<Uint8Array> {
  if (data.length < 68 || data[0] !== 77 || data[1] !== 86 || data[2] !== 50 || data[3] !== 72) throw new Error("Unsupported encrypted file");
  const body = data.slice(0, -32), tag = data.slice(-32), expected = await authenticate(body, keyHex);
  let mismatch = 0;
  for (let i = 0; i < 32; i++) mismatch |= tag[i] ^ expected[i];
  if (mismatch !== 0) throw new Error("File authentication failed");
  data = body.slice(4);
  const keyBytes = hexToBytes(keyHex.slice(0, KEY_LENGTH * 2));
  const iv = data.slice(0, IV_LENGTH);
  const cipherBytes = data.slice(IV_LENGTH);

  const aesCbc = new aesjs.ModeOfOperation.cbc(keyBytes, iv);
  const paddedPlain = aesCbc.decrypt(cipherBytes);
  return aesjs.padding.pkcs7.strip(paddedPlain);
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Read the file at `uri`, encrypt its contents with AES-256-CBC,
 * and write the result as `<originalUri>.enc`.
 *
 * Format: MV2H || IV (16 bytes) || ciphertext || HMAC-SHA256 (32 bytes).
 *
 * Returns the URI of the encrypted file.
 */
export async function encryptFile(uri: string, key: string): Promise<string> {
  const encUri = `${uri}.enc`;
  await encryptFileTo(uri, encUri, key);
  return encUri;
}

/** Encrypt the file at `sourceUri` into `encUri` from memory: no plaintext copy is written. */
export async function encryptFileTo(sourceUri: string, encUri: string, key: string): Promise<void> {
  const base64 = await FileSystem.readAsStringAsync(sourceUri, {
    encoding: FileSystem.EncodingType.Base64,
  });
  const plain = base64ToBytes(base64);
  try {
    await encryptBytesToFile(plain, encUri, key);
  } finally {
    plain.fill(0);
  }
}

/**
 * Encrypt in-memory bytes straight into the MV2H file at `encUri`. Nothing
 * unencrypted is written: used for files the app produced itself (a verified
 * official form, a filled PDF), which never need a plaintext copy on disk.
 */
export async function encryptBytesToFile(plainBytes: Uint8Array, encUri: string, key: string): Promise<void> {
  if (!encUri.endsWith(".enc")) throw new Error("Encrypted files end in .enc");
  const encryptedBytes = await aesEncrypt(plainBytes, key);
  await FileSystem.writeAsStringAsync(encUri, bytesToBase64(encryptedBytes), {
    encoding: FileSystem.EncodingType.Base64,
  });
}

/**
 * Read the encrypted file at `encUri`, decrypt it with AES-256-CBC,
 * write the result to a temp file in the cache directory, and return its URI.
 *
 * The caller is responsible for deleting the temp file when done (e.g. after
 * sharing or displaying).
 */
export async function decryptFile(
  encUri: string,
  key: string,
  /** Name the copy carries into the share sheet; defaults to the stored file's own name. */
  name?: string,
): Promise<string> {
  const originalUri = encUri.endsWith(".enc")
    ? encUri.slice(0, -4)
    : encUri;
  const fileName = name || (originalUri.split("/").pop() ?? "decrypted_file");
  const tempUri = `${FileSystem.cacheDirectory}vault_tmp/${fileName}`;

  // Ensure the temp directory exists
  const tmpDir = `${FileSystem.cacheDirectory}vault_tmp`;
  const dirInfo = await FileSystem.getInfoAsync(tmpDir);
  if (!dirInfo.exists) {
    await FileSystem.makeDirectoryAsync(tmpDir, { intermediates: true });
  }

  const cipherBase64 = await FileSystem.readAsStringAsync(encUri, {
    encoding: FileSystem.EncodingType.Base64,
  });

  const cipherBytes = base64ToBytes(cipherBase64);
  const plainBytes = await aesDecrypt(cipherBytes, key);
  const plainBase64 = bytesToBase64(plainBytes);

  await FileSystem.writeAsStringAsync(tempUri, plainBase64, {
    encoding: FileSystem.EncodingType.Base64,
  });

  return tempUri;
}

// HMAC-SHA256 authenticates the version, IV and ciphertext before decryption.
async function authenticate(data: Uint8Array, keyHex: string): Promise<Uint8Array> {
  const macKey = hexToBytes(await Crypto.digestStringAsync(Crypto.CryptoDigestAlgorithm.SHA256, `vault-auth-v2:${keyHex}`));
  const inner = new Uint8Array(64 + data.length), outer = new Uint8Array(96);
  for (let i = 0; i < 64; i++) { inner[i] = (macKey[i] ?? 0) ^ 0x36; outer[i] = (macKey[i] ?? 0) ^ 0x5c; }
  inner.set(data, 64);
  // Pass the typed arrays, never `.buffer`: the native module accepts only
  // ArrayBuffer views (scripts/check-vault-crypto.cjs). Both arrays own their
  // whole buffer, so the hashed bytes are unchanged for existing MV2H files.
  outer.set(new Uint8Array(await Crypto.digest(Crypto.CryptoDigestAlgorithm.SHA256, inner)), 64);
  return new Uint8Array(await Crypto.digest(Crypto.CryptoDigestAlgorithm.SHA256, outer));
}
