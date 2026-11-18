// Document vault encryption (lib/crypto.ts) under the NATIVE expo-crypto contract.
// On iOS/Android expo-crypto 15 calls ExpoCrypto.digest(alg, output, data), and
// expo-modules-core accepts `data` only when ArrayBuffer.isView(data) is true; a bare
// ArrayBuffer throws NotTypedArrayException. A lenient mock that accepts ArrayBuffer hid
// that every vault save failed, so this shim rejects anything that is not a view.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const nodeCrypto = require('node:crypto');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vault-crypto-'));
// iOS keychain semantics (expo-secure-store SecureStoreModule.swift): an add of an existing
// item becomes an update of its value only, so the item keeps the accessibility it was
// created with. `secure` holds { value, accessible }.
const secure = new Map();
const failSet = new Set();
// Per-key read delays, to force an interleaving: key -> list of ms, one per call (then 0).
const readDelays = new Map();
const pause = ms => new Promise(r => setTimeout(r, ms));
const DEFAULT_ACCESS = 'AFTER_FIRST_UNLOCK', DEVICE_ONLY = 'WHEN_UNLOCKED_THIS_DEVICE_ONLY';
const viewOnly = (name, value) => {
  if (!ArrayBuffer.isView(value)) throw new TypeError(`NotTypedArrayException: ${name} got ${Object.prototype.toString.call(value)}`);
  return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
};
const expoCrypto = {
  CryptoDigestAlgorithm: { SHA256: 'SHA-256' },
  CryptoEncoding: { HEX: 'hex', BASE64: 'base64' },
  getRandomBytes: n => new Uint8Array(nodeCrypto.randomBytes(n)),
  digestStringAsync: async (_alg, s, o) => nodeCrypto.createHash('sha256').update(s, 'utf8').digest(o?.encoding === 'base64' ? 'base64' : 'hex'),
  digest: async (_alg, data) => { const out = nodeCrypto.createHash('sha256').update(viewOnly('digest(data)', data)).digest(); return out.buffer.slice(out.byteOffset, out.byteOffset + out.byteLength); },
};
const local = uri => uri.replace(/^file:\/\//, '');
const shims = {
  'expo-crypto': expoCrypto,
  'expo-secure-store': {
    WHEN_UNLOCKED_THIS_DEVICE_ONLY: DEVICE_ONLY,
    getItemAsync: async k => { const d = readDelays.get(k)?.shift(); if (d) await pause(d); return secure.get(k)?.value ?? null; },
    setItemAsync: async (k, v, o) => {
      if (failSet.has(k)) throw new Error('keychain error');
      const old = secure.get(k);
      secure.set(k, { value: v, accessible: old ? old.accessible : (o?.keychainAccessible ?? DEFAULT_ACCESS) });
    },
    deleteItemAsync: async k => { secure.delete(k); },
  },
  'expo-file-system/legacy': {
    EncodingType: { Base64: 'base64' }, cacheDirectory: `${dir}/`,
    readAsStringAsync: async u => fs.readFileSync(local(u)).toString('base64'),
    writeAsStringAsync: async (u, d) => fs.writeFileSync(local(u), Buffer.from(d, 'base64')),
    getInfoAsync: async u => ({ exists: fs.existsSync(local(u)) }),
    makeDirectoryAsync: async u => fs.mkdirSync(local(u), { recursive: true }),
  },
};
// aes-js with pad() recorded, to see that the padded plaintext copy is zeroed.
const aesjs = require('aes-js');
const padded = [];
const realPad = aesjs.padding.pkcs7.pad;
shims['aes-js'] = { ...aesjs, padding: { ...aesjs.padding, pkcs7: { ...aesjs.padding.pkcs7, pad: data => { const out = realPad(data); padded.push(out); return out; } } } };
const load = Module._load;
Module._load = function (request, ...rest) { return shims[request] ?? load.call(this, request, ...rest); };
const vault = require('../lib/crypto.ts');

const macKeyFor = keyHex => Buffer.from(nodeCrypto.createHash('sha256').update(`vault-auth-v2:${keyHex}`, 'utf8').digest('hex'), 'hex');
const write = (name, bytes) => { const p = path.join(dir, name); fs.writeFileSync(p, bytes); return p; };

test('the shim enforces the native contract (a bare ArrayBuffer is rejected)', async () => {
  await assert.rejects(expoCrypto.digest('SHA-256', new Uint8Array(4).buffer), /NotTypedArrayException/);
  assert.equal((await expoCrypto.digest('SHA-256', new Uint8Array(4))).byteLength, 32);
});

test('encryptFile then decryptFile round-trips every size under the native contract', async () => {
  const key = await vault.generateEncryptionKey('11111111-2222-4333-8444-555555555551');
  for (const size of [0, 1, 15, 16, 17, 4096, 1024 * 1024]) {
    const data = nodeCrypto.randomBytes(size);
    const enc = await vault.encryptFile(write(`f${size}.pdf`, data), key);
    const dec = await vault.decryptFile(enc, key);
    assert.equal(Buffer.compare(fs.readFileSync(local(dec)), data), 0, `size ${size}`);
  }
});

test('the MV2H tag is standard HMAC-SHA256 over version, IV and ciphertext (old files keep verifying)', async () => {
  const key = await vault.generateEncryptionKey('11111111-2222-4333-8444-555555555552');
  const enc = fs.readFileSync(local(await vault.encryptFile(write('tag.pdf', Buffer.from('hello vault')), key)));
  assert.equal(enc.subarray(0, 4).toString('latin1'), 'MV2H');
  const body = enc.subarray(0, -32), tag = enc.subarray(-32);
  assert.equal(Buffer.compare(tag, nodeCrypto.createHmac('sha256', macKeyFor(key)).update(body).digest()), 0);
});

test('an envelope built outside the app (the pre-fix byte layout) decrypts', async () => {
  const key = await vault.generateEncryptionKey('11111111-2222-4333-8444-555555555553');
  const plain = nodeCrypto.randomBytes(5000), iv = nodeCrypto.randomBytes(16);
  const cipher = nodeCrypto.createCipheriv('aes-256-cbc', Buffer.from(key, 'hex'), iv);
  const body = Buffer.concat([Buffer.from('MV2H', 'latin1'), iv, cipher.update(plain), cipher.final()]);
  const file = Buffer.concat([body, nodeCrypto.createHmac('sha256', macKeyFor(key)).update(body).digest()]);
  const dec = await vault.decryptFile(write('legacy.pdf.enc', file), key);
  assert.equal(Buffer.compare(fs.readFileSync(local(dec)), plain), 0);
});

test('tampered ciphertext and a wrong key are rejected', async () => {
  const key = await vault.generateEncryptionKey('11111111-2222-4333-8444-555555555554');
  const other = await vault.generateEncryptionKey('11111111-2222-4333-8444-555555555555');
  const encUri = await vault.encryptFile(write('t.pdf', nodeCrypto.randomBytes(100)), key);
  await assert.rejects(vault.decryptFile(encUri, other), /File authentication failed/);
  const bytes = fs.readFileSync(local(encUri)); bytes[30] ^= 1;
  await assert.rejects(vault.decryptFile(write('t2.pdf.enc', bytes), key), /File authentication failed/);
});

// The app before this change wrote both items with the default accessibility under these names.
async function legacyKeyFor(userId) {
  const master = nodeCrypto.randomBytes(32).toString('hex'), salt = nodeCrypto.randomBytes(32).toString('hex');
  secure.set('vault_master_key', { value: master, accessible: DEFAULT_ACCESS });
  secure.set(`vault_salt_${userId}`, { value: salt, accessible: DEFAULT_ACCESS });
  const inner = nodeCrypto.createHash('sha256').update(salt + userId, 'utf8').digest('hex');
  return nodeCrypto.createHash('sha256').update(master + inner, 'utf8').digest('hex');
}
function legacyEnvelope(plain, key) {
  const iv = nodeCrypto.randomBytes(16);
  const cipher = nodeCrypto.createCipheriv('aes-256-cbc', Buffer.from(key, 'hex'), iv);
  const body = Buffer.concat([Buffer.from('MV2H', 'latin1'), iv, cipher.update(plain), cipher.final()]);
  return Buffer.concat([body, nodeCrypto.createHmac('sha256', macKeyFor(key)).update(body).digest()]);
}

test('M1: legacy keychain items move to device-only items once, and old ciphertext still decrypts', async () => {
  secure.clear();
  const user = '11111111-2222-4333-8444-5555555555a1';
  const legacyKey = await legacyKeyFor(user);
  const plain = nodeCrypto.randomBytes(3000);
  const file = write('pre-migration.pdf.enc', legacyEnvelope(plain, legacyKey));
  const key = await vault.generateEncryptionKey(user);
  assert.equal(key, legacyKey, 'the same key is derived: nothing is lost');
  assert.equal(Buffer.compare(fs.readFileSync(local(await vault.decryptFile(file, key))), plain), 0, 'files encrypted before the move still open');
  for (const [name, item] of secure) assert.equal(item.accessible, DEVICE_ONLY, `${name} is device-only`);
  assert.equal(secure.has('vault_master_key'), false, 'legacy master item removed after the verified copy');
  assert.equal(secure.has(`vault_salt_${user}`), false, 'legacy salt item removed after the verified copy');
  assert.equal(await vault.generateEncryptionKey(user), legacyKey, 'stable after the move');
  // A new account on a fresh install gets device-only items straight away.
  secure.clear();
  await vault.generateEncryptionKey('11111111-2222-4333-8444-5555555555a2');
  assert.ok(secure.size === 2 && [...secure.values()].every(i => i.accessible === DEVICE_ONLY));
});

test('M1: a failed copy keeps the legacy item and its key; the move finishes on a later run', async () => {
  secure.clear();
  const user = '11111111-2222-4333-8444-5555555555a3';
  const legacyKey = await legacyKeyFor(user);
  const plain = Buffer.from('주민등록번호가 들어간 작성본');
  const file = write('pre-migration-2.pdf.enc', legacyEnvelope(plain, legacyKey));
  failSet.add('vault_master_key_tdo');
  assert.equal(await vault.generateEncryptionKey(user), legacyKey, 'the legacy key keeps working when the copy fails');
  assert.ok(secure.has('vault_master_key'), 'the legacy master item is not deleted without a verified copy');
  failSet.clear();
  assert.equal(await vault.generateEncryptionKey(user), legacyKey);
  assert.equal(secure.has('vault_master_key'), false);
  assert.equal(Buffer.compare(fs.readFileSync(local(await vault.decryptFile(file, legacyKey))), plain), 0);
  // Interrupted after the copy, before the delete: both exist; the copy wins and the legacy item goes.
  secure.set(`vault_salt_${user}`, { value: secure.get(`vault_salt_tdo_${user}`).value, accessible: DEFAULT_ACCESS });
  assert.equal(await vault.generateEncryptionKey(user), legacyKey);
  assert.equal(secure.has(`vault_salt_${user}`), false);
  // Deleting an account's key removes both names.
  secure.set(`vault_salt_${user}`, { value: 'x', accessible: DEFAULT_ACCESS });
  await vault.deleteEncryptionKey(user);
  assert.equal(secure.has(`vault_salt_${user}`) || secure.has(`vault_salt_tdo_${user}`), false);
});

test('L1: the padded plaintext copy is zeroed after encryption, and encryptFileTo writes only the .enc file', async () => {
  const key = await vault.generateEncryptionKey('11111111-2222-4333-8444-5555555555a4');
  padded.length = 0;
  const source = write('l1-source.pdf', Buffer.from('%PDF-1.7 900101-1234567'));
  const target = path.join(dir, 'l1-target.pdf.enc');
  const before = new Set(fs.readdirSync(dir));
  await vault.encryptFileTo(source, target, key);
  assert.ok(padded.length >= 1, 'pad() was used');
  for (const p of padded) assert.ok(p.every(b => b === 0), 'padded copy zeroed');
  const added = fs.readdirSync(dir).filter(n => !before.has(n));
  assert.deepEqual(added, ['l1-target.pdf.enc'], 'no plaintext copy written');
  assert.equal(fs.readFileSync(target).includes(Buffer.from('900101-1234567')), false);
});

test('M1 race: two key setups at once during the move never replace the key (double tap on 확인, open during a save)', async () => {
  secure.clear(); readDelays.clear();
  const user = '11111111-2222-4333-8444-5555555555a5';
  const legacyKey = await legacyKeyFor(user);
  const plain = Buffer.from('작성본 900101-1234567');
  const file = write('race.pdf.enc', legacyEnvelope(plain, legacyKey));
  // Call B reads the new name before A copies (both null), and the legacy item only after A has deleted it.
  readDelays.set('vault_master_key', [0, 40]);
  readDelays.set(`vault_salt_${user}`, [0, 40]);
  const [a, b] = await Promise.all([vault.generateEncryptionKey(user), vault.generateEncryptionKey(user)]);
  readDelays.clear();
  assert.equal(a, legacyKey, 'call A');
  assert.equal(b, legacyKey, 'call B gets the same key, not a fresh random one');
  assert.equal(await vault.generateEncryptionKey(user), legacyKey, 'the stored key was not replaced');
  assert.equal(Buffer.compare(fs.readFileSync(local(await vault.decryptFile(file, legacyKey))), plain), 0, 'old files still open');
  // A fresh install: two concurrent first calls create one key, not two.
  secure.clear();
  const fresh = '11111111-2222-4333-8444-5555555555a6';
  const [x, y] = await Promise.all([vault.generateEncryptionKey(fresh), vault.generateEncryptionKey(fresh)]);
  assert.equal(x, y);
  assert.equal(await vault.generateEncryptionKey(fresh), x);
});
