// 서류 보관함에 넣기 (detail card) and 보관함에 저장 (서류 작성 도우미): the shipped
// lib/vault.ts with the real lib/crypto.ts and lib/formFill/device.ts, over a
// file-system shim on a temp directory. No network: downloads come from a fixture map.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const nodeCrypto = require('node:crypto');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

const root = path.resolve(__dirname, '..');
const read = rel => fs.readFileSync(path.join(root, rel), 'utf8');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'form-to-vault-'));
const local = uri => uri.replace(/^file:\/\//, '');
const platform = { OS: 'ios' };
const storage = new Map();
const secure = new Map();
const downloads = new Map(); // url -> { status, body }
const calls = { download: [] };
const failures = { setItem: false, write: false };
const CHUNK = 1024 * 1024;
const download = { chunks: 0, cancelled: false, hideLength: false };
const fsShim = {
  EncodingType: { Base64: 'base64' },
  get documentDirectory() { return platform.OS === 'web' ? null : `file://${dir}/docs/`; },
  get cacheDirectory() { return platform.OS === 'web' ? null : `file://${dir}/cache/`; },
  readAsStringAsync: async u => fs.readFileSync(local(u)).toString('base64'),
  writeAsStringAsync: async (u, d) => { if (failures.slowWrite) await new Promise(r => setTimeout(r, failures.slowWrite)); if (failures.write) throw new Error('disk full'); fs.writeFileSync(local(u), Buffer.from(d, 'base64')); },
  getInfoAsync: async u => { const p = local(u); return fs.existsSync(p) ? { exists: true, size: fs.statSync(p).size } : { exists: false }; },
  makeDirectoryAsync: async u => { fs.mkdirSync(local(u), { recursive: true }); },
  // The vault never copies a plaintext file anywhere: every write encrypts from memory.
  copyAsync: async () => { throw new Error('copyAsync must not be used by the vault'); },
  deleteAsync: async u => fs.rmSync(local(u), { recursive: true, force: true }),
  // Streams the body in 1 MiB chunks with progress callbacks, as the native task does;
  // cancelAsync stops it and downloadAsync then resolves undefined.
  createDownloadResumable: (url, target, _options, callback) => {
    let cancelled = false;
    return {
      cancelAsync: async () => { cancelled = true; download.cancelled = true; },
      downloadAsync: async () => {
        calls.download.push(url);
        if (download.gate) await download.gate;
        const hit = downloads.get(url);
        if (!hit) return { status: 404, uri: target, headers: {} };
        fs.mkdirSync(path.dirname(local(target)), { recursive: true });
        fs.writeFileSync(local(target), Buffer.alloc(0));
        const expected = download.hideLength ? -1 : hit.body.length;
        for (let at = 0; at < hit.body.length; at += CHUNK) {
          if (cancelled) return undefined;
          fs.appendFileSync(local(target), hit.body.subarray(at, at + CHUNK));
          download.chunks++;
          callback?.({ totalBytesWritten: Math.min(at + CHUNK, hit.body.length), totalBytesExpectedToWrite: expected });
          await new Promise(r => setImmediate(r));
        }
        return cancelled ? undefined : { status: hit.status ?? 200, uri: target, headers: {} };
      },
    };
  },
  readDirectoryAsync: async u => fs.readdirSync(local(u)),
};
const shims = {
  'react-native': { Platform: platform },
  'expo-crypto': {
    CryptoDigestAlgorithm: { SHA256: 'SHA-256' }, CryptoEncoding: { HEX: 'hex', BASE64: 'base64' },
    getRandomBytes: n => new Uint8Array(nodeCrypto.randomBytes(n)),
    digestStringAsync: async (_a, s, o) => nodeCrypto.createHash('sha256').update(s, 'utf8').digest(o?.encoding === 'base64' ? 'base64' : 'hex'),
    digest: async (_a, data) => { if (!ArrayBuffer.isView(data)) throw new TypeError('NotTypedArrayException'); const out = nodeCrypto.createHash('sha256').update(Buffer.from(data.buffer, data.byteOffset, data.byteLength)).digest(); return out.buffer.slice(out.byteOffset, out.byteOffset + out.byteLength); },
  },
  'expo-secure-store': { getItemAsync: async k => secure.get(k) ?? null, setItemAsync: async (k, v) => { secure.set(k, v); }, deleteItemAsync: async k => { secure.delete(k); }, WHEN_UNLOCKED_THIS_DEVICE_ONLY: 'WHEN_UNLOCKED_THIS_DEVICE_ONLY' },
  'expo-file-system/legacy': fsShim,
  'expo-sharing': { isAvailableAsync: async () => true, shareAsync: async () => {} },
  'expo-asset': { Asset: { fromModule: () => ({ downloadAsync: async () => {}, localUri: null }) } },
  '@react-native-async-storage/async-storage': { __esModule: true, default: {
    getItem: async k => storage.get(k) ?? null,
    setItem: async (k, v) => { if (storage.hang) await new Promise(r => setTimeout(r, 120)); if (failures.setItem) throw new Error('quota'); storage.set(k, v); },
    removeItem: async k => { if (storage.hangRemove) await new Promise(r => setTimeout(r, 120)); storage.delete(k); },
  } },
  '@tanstack/react-query': { useQuery: () => ({ data: [] }) },
  '@/store/auth': { useAuthStore: () => undefined },
};
const load = Module._load;
Module._load = function (request, ...rest) { return shims[request] ?? load.call(this, request, ...rest); };
const V = { ...require('../lib/vault.ts'), ...require('../lib/vaultSave.ts') };
const C = require('../lib/crypto.ts');

const USER = '11111111-2222-4333-8444-555555555555';
const PROGRAM = { id: 'aaaaaaaa-1111-4111-8111-111111111111', title: '2026 부산 청년 월세 지원' };
const MAGIC = { pdf: Buffer.from('%PDF-1.7\n'), hwp: Buffer.from([0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]), hwpx: Buffer.from([0x50, 0x4b, 0x03, 0x04]) };
const sha = b => nodeCrypto.createHash('sha256').update(b).digest('hex');
let seq = 0;
function official(format = 'pdf', over = {}) {
  const body = Buffer.concat([MAGIC[format], nodeCrypto.randomBytes(2048)]);
  const id = `bbbbbbbb-2222-4222-8222-${String(++seq).padStart(12, '0')}`;
  const url = `https://official.example.org/forms/${id}.${format}`;
  downloads.set(url, { body });
  return { body, resource: { id, url, label: '신청서', file_format: format, byte_size: body.length, content_hash: sha(body), ...over } };
}
function files(sub) {
  const out = [];
  const walk = p => { if (!fs.existsSync(p)) return; for (const e of fs.readdirSync(p, { withFileTypes: true })) { const f = path.join(p, e.name); if (e.isDirectory()) walk(f); else out.push(f); } };
  walk(path.join(dir, sub));
  return out;
}
const docs = async () => V.loadVaultDocuments(USER);
const reset = () => { V.vaultWriteTimeout.ms = 90_000; storage.hang = false; storage.hangRemove = false; download.chunks = 0; download.cancelled = false; download.hideLength = false; download.gate = null; storage.clear(); fs.rmSync(path.join(dir, 'docs'), { recursive: true, force: true }); fs.rmSync(path.join(dir, 'cache'), { recursive: true, force: true }); calls.download.length = 0; platform.OS = 'ios'; failures.setItem = failures.write = false; failures.slowWrite = 0; };
const rejectsWith = (promise, problem) => assert.rejects(promise, e => e instanceof V.VaultSaveError && e.problem === problem, problem);

test('a reviewed official form is stored encrypted only, named "<title> – <label>", with its source', async () => {
  reset();
  const { body, resource } = official('pdf');
  const doc = await V.saveOfficialForm(USER, PROGRAM, resource);
  assert.equal(doc.name, '2026 부산 청년 월세 지원 – 신청서');
  assert.equal(doc.type, 'other', 'an existing type: no new vault tab');
  assert.equal(doc.encrypted, true);
  assert.equal(doc.mimeType, 'application/pdf');
  assert.deepEqual(doc.source, { kind: 'official_form', programId: PROGRAM.id, resourceId: resource.id, contentHash: resource.content_hash });
  assert.deepEqual((await docs()).map(d => d.id), [doc.id]);
  const stored = files('docs');
  assert.deepEqual(stored.map(f => path.basename(f)), [`${doc.id}.pdf.enc`], 'only ciphertext in the vault directory');
  const cipher = fs.readFileSync(stored[0]);
  assert.equal(cipher.subarray(0, 4).toString('latin1'), 'MV2H');
  assert.equal(cipher.includes(body.subarray(0, 64)), false, 'no plaintext run in the stored file');
  assert.deepEqual(files('cache'), [], 'the download temp file is gone');
  const plain = await C.decryptFile(doc.fileUri, await C.generateEncryptionKey(USER));
  assert.equal(Buffer.compare(fs.readFileSync(local(plain)), body), 0, 'decrypts to the reviewed file');
});

test('hash mismatch or a missing hash: nothing is saved', async () => {
  reset();
  const { resource } = official('pdf');
  downloads.set(resource.url, { body: Buffer.concat([MAGIC.pdf, Buffer.from('agency replaced the form')]) });
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, byte_size: null }), 'unverified');
  for (const content_hash of [null, '', 'hash1', 'z'.repeat(64)]) await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, content_hash }), 'unverified');
  assert.equal(calls.download.length, 1, 'a missing or malformed hash is refused before any download');
  assert.deepEqual(await docs(), []);
  assert.deepEqual(files('docs'), []);
  assert.deepEqual(files('cache'), []);
  // The size the server recorded must match too.
  const other = official('pdf');
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...other.resource, byte_size: other.body.length + 1 }), 'unverified');
  assert.deepEqual(await docs(), []);
});

test('8 MiB cap: refused from the declared size before downloading, and from the bytes', async () => {
  reset();
  const { resource } = official('pdf');
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, byte_size: 8 * 1024 * 1024 + 1 }), 'too_large');
  assert.equal(calls.download.length, 0);
  const big = Buffer.concat([MAGIC.pdf, Buffer.alloc(8 * 1024 * 1024)]);
  const url = 'https://official.example.org/forms/big.pdf';
  downloads.set(url, { body: big });
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, url, byte_size: null, content_hash: sha(big) }), 'too_large');
  assert.throws(() => V.checkOfficialFormBytes(new Uint8Array(big), 'pdf'), e => e.problem === 'too_large');
  assert.throws(() => V.checkOfficialFormBytes(new Uint8Array(0), 'pdf'), e => e.problem === 'too_large');
  V.checkOfficialFormBytes(new Uint8Array(Buffer.concat([MAGIC.pdf, Buffer.alloc(8 * 1024 * 1024 - MAGIC.pdf.length)])), 'pdf');
  assert.deepEqual(await docs(), []);
  assert.deepEqual(files('docs'), []);
  assert.deepEqual(files('cache'), []);
});

test('format: only pdf, hwp and hwpx, and the magic bytes must match file_format', async () => {
  reset();
  for (const format of ['pdf', 'hwp', 'hwpx', 'PDF']) assert.equal(V.officialFormat(format), format.toLowerCase());
  for (const format of [null, undefined, '', 'docx', 'zip', 'exe', 'hwp.exe']) assert.equal(V.officialFormat(format), null, String(format));
  for (const format of ['pdf', 'hwp', 'hwpx']) {
    V.checkOfficialFormBytes(new Uint8Array(Buffer.concat([MAGIC[format], Buffer.alloc(16)])), format);
    for (const otherFormat of ['pdf', 'hwp', 'hwpx'].filter(f => f !== format)) assert.throws(() => V.checkOfficialFormBytes(new Uint8Array(Buffer.concat([MAGIC[otherFormat], Buffer.alloc(16)])), format), e => e.problem === 'format', `${otherFormat} bytes as ${format}`);
  }
  assert.throws(() => V.checkOfficialFormBytes(new Uint8Array([0x25, 0x50]), 'pdf'), e => e.problem === 'format', 'shorter than the magic');
  // Declared docx: refused before any download.
  const docx = official('pdf', { file_format: 'docx' });
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, docx.resource), 'format');
  assert.equal(calls.download.length, 0);
  // Declared pdf, reviewed hash matches, but the file is an HWP: refused after the hash check.
  const disguised = official('hwp', { file_format: 'pdf' });
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, disguised.resource), 'format');
  assert.deepEqual(await docs(), []);
  assert.deepEqual(files('docs'), []);
  // HWP and HWPX are kept with their own extension and type.
  const hwp = await V.saveOfficialForm(USER, PROGRAM, official('hwp').resource);
  const hwpx = await V.saveOfficialForm(USER, PROGRAM, official('hwpx').resource);
  assert.deepEqual([hwp.mimeType, hwpx.mimeType], ['application/x-hwp', 'application/hwp+zip']);
  assert.match(hwp.fileUri, /\.hwp\.enc$/); assert.match(hwpx.fileUri, /\.hwpx\.enc$/);
});

test('https only: an http or local URL is never downloaded', async () => {
  reset();
  const { resource } = official('pdf');
  for (const url of ['http://official.example.org/a.pdf', 'https://127.0.0.1/a.pdf', 'https://localhost/a.pdf', 'https://user:pw@official.example.org/a.pdf', 'https://official.example.org:8443/a.pdf', 'javascript:alert(1)']) {
    await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, url }), 'failed');
  }
  assert.equal(calls.download.length, 0);
  assert.deepEqual(await docs(), []);
});

test('duplicate: the same programme, resource and hash is detected ("이미 보관함에 있어요"), even when two saves race', async () => {
  reset();
  const { resource } = official('pdf');
  await V.saveOfficialForm(USER, PROGRAM, resource);
  const before = calls.download.length;
  await assert.rejects(V.saveOfficialForm(USER, PROGRAM, { ...resource, content_hash: resource.content_hash.toUpperCase() }), e => e.problem === 'duplicate' && e.message === '이미 보관함에 있어요.');
  assert.equal(calls.download.length, before, 'no second download');
  assert.equal((await docs()).length, 1);
  // A revised form (new hash) is a different file and is kept.
  const revised = Buffer.concat([MAGIC.pdf, Buffer.from('2027 revision')]);
  downloads.set(resource.url, { body: revised });
  await V.saveOfficialForm(USER, PROGRAM, { ...resource, byte_size: revised.length, content_hash: sha(revised) });
  assert.equal((await docs()).length, 2);
  const [newest, older] = await docs();
  assert.equal(newest.expiresAt, null);
  assert.ok(older.expiresAt && Date.parse(older.expiresAt) <= Date.now(), 'the superseded copy is kept but shows as expired');
  assert.ok(older.fileUri, 'never deleted');
  // Two taps at once: the second sees the first inside the serialised update.
  reset();
  const race = official('hwpx').resource;
  const results = await Promise.allSettled([V.saveOfficialForm(USER, PROGRAM, race), V.saveOfficialForm(USER, PROGRAM, race)]);
  assert.deepEqual(results.map(r => r.status).sort(), ['fulfilled', 'rejected']);
  assert.equal(results.find(r => r.status === 'rejected').reason.problem, 'duplicate');
  assert.equal((await docs()).length, 1);
  assert.equal(files('docs').length, 1, 'no orphaned ciphertext from the losing save');
  // A metadata-only entry (file removed) does not count as already stored.
  assert.equal(V.findOfficialForm([{ ...(await docs())[0], fileUri: null }], PROGRAM.id, race.id, race.content_hash), undefined);
});

test('a failed metadata write leaves no ciphertext behind; a failed encryption leaves no file at all', async () => {
  reset();
  const { resource } = official('pdf');
  failures.setItem = true;
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, resource), 'failed');
  failures.setItem = false;
  assert.deepEqual(files('docs'), []);
  failures.write = true;
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, resource), 'failed');
  failures.write = false;
  assert.deepEqual(files('docs'), []);
  assert.deepEqual(await docs(), []);
});

test('web: no encrypted file storage, so nothing is stored and both actions are hidden', async () => {
  reset();
  platform.OS = 'web';
  assert.equal(V.vaultFilesSupported(), false);
  const { resource } = official('pdf');
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, resource), 'unsupported');
  await rejectsWith(V.saveFilledForm(USER, new Uint8Array(MAGIC.pdf), { programId: PROGRAM.id, programTitle: PROGRAM.title, resourceId: resource.id, resourceLabel: '신청서', templateHash: 'a'.repeat(64) }), 'unsupported');
  assert.equal(calls.download.length, 0);
  assert.deepEqual(await docs(), [], 'no plaintext metadata-only entry either');
  assert.deepEqual(files(''), []);
  platform.OS = 'ios';
  assert.equal(V.vaultFilesSupported(), true);
  // The UI renders each action only behind vaultFilesSupported().
  const card = read('components/ApplicationResourcesCard.tsx');
  assert.match(card, /const canStore = vaultFilesSupported\(\);/);
  assert.match(card, /\{canStore && <Pressable[^\n]*>서류 보관함에 넣기<\/Text><\/Pressable>\}/);
  const fill = read('app/form-fill.tsx');
  assert.match(fill, /if \(user && vaultFilesSupported\(\)\) \{ vaultBytes\.current = filledBytes\.current; filledBytes\.current = null; setVaultOffer\('ready'\); \} else \{ setVaultOffer\('none'\); dropFilledBytes\(\); \}/);
  assert.match(fill, /\{vaultOffer === 'ready' && <Pressable[^\n]*>\{busy \? '저장 중…' : '보관함에 저장'\}<\/Text><\/Pressable>\}/);
  // The vault screen itself still stores no file on web.
  assert.match(read('app/document-vault.tsx'), /파일은 마지미 앱에서 보관할 수 있어요/);
});

test('filled PDF: encrypted straight from memory, the buffer is zeroed, and no plaintext exists anywhere', async () => {
  reset();
  const RRN = '900101-1234567', ACCOUNT = '110-123-456789';
  const plain = Buffer.concat([MAGIC.pdf, Buffer.from(`BT (${RRN}) Tj (${ACCOUNT}) Tj ET`), nodeCrypto.randomBytes(512)]);
  const bytes = new Uint8Array(plain);
  const doc = await V.saveFilledForm(USER, bytes, { programId: PROGRAM.id, programTitle: PROGRAM.title, resourceId: 'r1', resourceLabel: '신청서', templateHash: 'A'.repeat(64) });
  // Named after the programme (applicationFormName), never the agency's file label.
  assert.equal(doc.name, '2026 부산 청년 월세 지원 신청서 (작성본)');
  assert.equal(doc.type, 'other');
  assert.deepEqual(doc.source, { kind: 'filled_form', programId: PROGRAM.id, resourceId: 'r1', templateHash: 'a'.repeat(64) });
  assert.ok(bytes.every(b => b === 0), 'the in-memory PDF is zeroed once saved');
  for (const file of files('')) {
    const data = fs.readFileSync(file);
    assert.equal(data.includes(Buffer.from(RRN)) || data.includes(Buffer.from(ACCOUNT)), false, `plaintext in ${path.relative(dir, file)}`);
  }
  assert.deepEqual(files('docs').map(f => path.basename(f)), [`${doc.id}.pdf.enc`]);
  assert.doesNotMatch(storage.get(V.vaultStorageKey(USER)), new RegExp(`${RRN}|${ACCOUNT}`), 'metadata holds no field value');
  const out = await C.decryptFile(doc.fileUri, await C.generateEncryptionKey(USER));
  assert.equal(Buffer.compare(fs.readFileSync(local(out)), plain), 0);
  // Not a PDF: refused and zeroed. A transient failure keeps the bytes for 다시 시도, with nothing written.
  const junk = new Uint8Array(Buffer.from('not a pdf at all'));
  await rejectsWith(V.saveFilledForm(USER, junk, { programId: PROGRAM.id, programTitle: 'x', resourceId: 'r1', resourceLabel: 'y', templateHash: 'a' }), 'format');
  assert.ok(junk.every(b => b === 0));
  reset();
  const retry = new Uint8Array(plain);
  failures.write = true;
  await rejectsWith(V.saveFilledForm(USER, retry, { programId: PROGRAM.id, programTitle: 'x', resourceId: 'r1', resourceLabel: 'y', templateHash: 'a' }), 'failed');
  failures.write = false;
  assert.equal(Buffer.compare(Buffer.from(retry), plain), 0);
  assert.deepEqual(files('docs'), []);
});

test('form helper: sensitive fields are still cleared at export, and the kept PDF is dropped on every exit', () => {
  const fill = read('app/form-fill.tsx');
  const exportBody = fill.slice(fill.indexOf('async function exportPdf()'), fill.indexOf("const title = '서류 작성 도우미';"));
  const cleared = exportBody.indexOf('setState(s => clearSensitive(fields, s));');
  assert.ok(cleared > 0 && cleared < exportBody.indexOf("setPhase({ kind: 'done'"), 'clearSensitive runs before the done phase');
  assert.ok(cleared < exportBody.indexOf('vaultBytes.current = filledBytes.current'), 'state is cleared whether or not the PDF is kept');
  // I1: every other drop of the filled PDF zeroes it first; the only bare null is the move to the vault offer.
  assert.match(fill, /function dropFilledBytes\(\) \{ if \(filledBytes\.current && filledBytes\.current !== vaultBytes\.current\) filledBytes\.current\.fill\(0\); filledBytes\.current = null; \}/);
  assert.deepEqual([...fill.matchAll(/filledBytes\.current = null/g)].length, 2, 'the move and the helper itself');
  assert.match(fill, /useEffect\(\(\) => \(\) => \{ templateBytes\.current = null; dropFilledBytes\(\); dropVaultBytes\(\);/);
  assert.match(fill, /dropFilledBytes\(\); filledBytes\.current = out;/);
  assert.match(fill, /onPress=\{\(\) => \{ dropFilledBytes\(\); setReviewPages\(null\); setExportUnconfirmed\(false\); setPhase\(\{ kind: 'fill' \}\);/, '수정하기 zeroes it');

  assert.match(fill, /useFocusEffect\(useCallback\(\(\) => \(\) => \{ if \(vaultBytes\.current\) \{ dropVaultBytes\(\); setVaultOffer\('none'\); \} \}, \[\]\)\);/, 'moving on (router.push) drops it too');
  assert.match(exportBody, /setReviewPages\(null\); setJob\(null\);/, 'the preview job holding the PDF as base64 is dropped');
  assert.match(fill, /function dropVaultBytes\(\) \{ vaultBytes\.current\?\.fill\(0\); vaultBytes\.current = null; \}/);
  assert.match(fill, /onPress=\{\(\) => \{ dropVaultBytes\(\); setVaultOffer\('none'\);[^\n]*setPhase\(\{ kind: 'fill' \}\)/, '다시 수정해서 만들기 drops it');
  const save = fill.slice(fill.indexOf('async function saveToVault()'), fill.indexOf("const title = '서류 작성 도우미';"));
  assert.ok(save.indexOf('vaultBytes.current = null;') < save.indexOf('await saveFilledForm('), 'the save takes the buffer before awaiting, so re-edit or exit cannot zero it mid-save');
  assert.match(save, /if \(e instanceof VaultSaveError && e\.problem === 'failed'\) vaultBytes\.current = bytes;/, 'only a retryable failure hands it back');
  assert.match(fill, /<Pressable accessibilityRole="button" disabled=\{busy\} onPress=\{\(\) => \{ dropVaultBytes\(\);/, 're-edit is disabled while saving');
  // Only the exported PDF reaches the vault: never the form state.
  assert.doesNotMatch(fill, /saveFilledForm\([^)]*state/);
  assert.match(fill, /vaultOffer === 'ready' \? '작성본 파일은 보관함에 저장하거나 이 화면을 나가면 앱 메모리에서 지워져요\.'/, 'the done copy stays true while the PDF is kept');
  assert.match(fill, /phase\.cleared\.length \? `입력한 \$\{phase\.cleared\.join\('·'\)\}는 지웠어요\.`/, 'it names only the sensitive values the form had');
});

test('detail card: refetches the programme first, offers 공식 양식 받기 on a refusal, uses only existing Figma components', () => {
  const card = read('components/ApplicationResourcesCard.tsx');
  const store = card.slice(card.indexOf('async function store('), card.indexOf('// Only what the person can act on'));
  assert.ok(store.indexOf('api.getProgram(programId)') < store.indexOf('currentResource(latest, resource)'));
  assert.ok(store.indexOf('currentResource(latest, resource)') < store.indexOf('saveOfficialForm('), 'the fresh resource is what gets downloaded');
  assert.match(store, /confirmAction\('보관함에 넣지 않았어요', e\.message, '공식 양식 받기', \(\) => void open\(official\)\)/);
  assert.match(store, /notify\('이미 보관함에 있어요'/);
  assert.match(store, /requireLogin\(router, /);
  assert.doesNotMatch(store, /Alert\.alert|console\.|logger/);
  const button = card.match(/\{canStore && (<Pressable[^\n]*<\/Pressable>)\}/)[1];
  assert.match(button, /style=\{ui\.outlineButton\}><Text style=\{ui\.outlineButtonText\}>/, 'the shared Figma outline button, unchanged');
  assert.doesNotMatch(button, /#[0-9a-f]{3,8}|fontSize|borderColor|backgroundColor/i);
});

test('vault screen: one save path, re-read on focus, and every change is a read-modify-write', () => {
  const vault = read('app/document-vault.tsx');
  assert.match(vault, /import \{ addVaultEntry, decryptVaultFile, loadVaultDocuments, storeVaultFile, updateVault, vaultGeneration, VaultFileMissingError, type DocumentType, type StoredDocument, type VaultEntry \} from "@\/lib\/vault";/);
  assert.doesNotMatch(vault, /persistFile|persistBytes|async function saveDocuments\(|encryptFile|interface StoredDocument|copyAsync/, 'no second copy of the save path or the type');
  // One write path: the picked file goes through storeVaultFile with the generation taken at the start of the save.
  assert.match(vault, /const started=vaultGeneration\(owner\);if\(file\)await storeVaultFile\(owner,\{sourceUri:file\.sourceUri\},\{\.\.\.entry,mimeType:file\.mimeType\},\{extension:file\.extension,started\}\);else await addVaultEntry\(owner,entry\);/);
  const lib = read('lib/vault.ts');
  assert.equal((lib.match(/encryptBytesToFile\(|encryptFileTo\(/g) ?? []).length, 2, 'storeVaultFile is the only place lib/vault.ts encrypts');
  assert.doesNotMatch(read('lib/vaultSave.ts'), /encryptBytesToFile|encryptFileTo|updateVault\(|persist(File|Bytes)|AsyncStorage|writeAsStringAsync/, 'vaultSave writes only through storeVaultFile');
  assert.match(vault, /useFocusEffect\(useCallback\(/);
  assert.equal((vault.match(/updateVault\(owner,/g) ?? []).length, 1, 'delete');
  assert.equal((vault.match(/if\(useAuthStore\.getState\(\)\.user\?\.id===owner\)setDocuments\(next\)/g) ?? []).length, 2, 'a late save never shows another account the old list');
  const tabs = vault.match(/const tabs=\[(.*?)\];/)[1];
  assert.equal(tabs, '["","전체"],["resident_register","주민등록"],["enrollment_cert","재학증명"],["income_proof","소득증빙"],["other","기타"]', 'the Figma tabs are unchanged');
});

test('no upload, no new route, no logging in the new path', () => {
  const lib = read('lib/vault.ts') + read('lib/vaultSave.ts');
  for (const pattern of [/fetch\(/, /uploadAsync/, /\bapi\./, /console\./, /logger/, /Linking/]) assert.doesNotMatch(lib, pattern, String(pattern));
  for (const file of ['components/ApplicationResourcesCard.tsx', 'app/form-fill.tsx']) assert.doesNotMatch(read(file), /console\.|logger\.|uploadAsync/, file);
  assert.doesNotMatch(read('lib/api.ts'), /vault/i, 'the api client has no vault route');
});

test('clearing the account while a save is in flight does not write it back', async () => {
  reset();
  const { resource } = official('pdf');
  let release; download.gate = new Promise(r => { release = r; });
  try {
    const saving = V.saveOfficialForm(USER, PROGRAM, resource);
    await new Promise(r => setTimeout(r, 10));
    await V.clearAccountVault(USER);
    release();
    await assert.rejects(saving, e => e instanceof V.VaultSaveError && e.problem === 'failed');
  } finally { download.gate = null; }
  assert.deepEqual(await docs(), []);
  assert.deepEqual(files('docs'), [], 'no ciphertext recreated for the cleared account');
});

test('an unreadable vault list is never overwritten by a save; names lose control and bidi characters', async () => {
  reset();
  storage.set(V.vaultStorageKey(USER), '{corrupt');
  const { resource } = official('pdf');
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, resource), 'failed');
  assert.equal(storage.get(V.vaultStorageKey(USER)), '{corrupt', 'the stored list is left as it was');
  assert.deepEqual(files('docs'), []);
  storage.set(V.vaultStorageKey(USER), '{"not":"a list"}');
  await rejectsWith(V.saveFilledForm(USER, new Uint8Array(MAGIC.pdf), { programId: 'p', programTitle: 't', resourceId: 'r', resourceLabel: 'l', templateHash: 'a' }), 'failed');
  assert.equal(storage.get(V.vaultStorageKey(USER)), '{"not":"a list"}');
  assert.equal(V.officialFormName('청년\u202E fdp.exe\n지원', '신청서\u2066'), '청년 fdp.exe 지원 – 신청서');
  await assert.rejects(V.storeVaultFile('../other', { bytes: new Uint8Array(MAGIC.pdf) }, { type: 'other', name: 'x' }, { extension: 'pdf' }), /Invalid account/);
});

test('vault list screens do not load the save code (download, pdf-lib)', () => {
  const vault = read('lib/vault.ts').replace(/\/\/[^\n]*|\/\*[\s\S]*?\*\//g, '');
  const statics = [...vault.matchAll(/^import[^;]*?from\s+["']([^"']+)["']/gm)].map(m => m[1]);
  assert.deepEqual(statics.filter(m => /formFill|pdf-lib|vaultSave/.test(m)), [], 'lib/vault.ts imports nothing heavy at load time');
  assert.match(vault, /await import\("\.\/formFill\/device"\)/, 'the form-fill cleanup loads only when an account is cleared');
  for (const file of ['app/(tabs)/profile.tsx', 'app/apply-assistant.tsx', 'components/AgentJourney.tsx', 'lib/documentReadiness.ts', 'app/document-vault.tsx']) {
    assert.doesNotMatch(read(file), /lib\/vaultSave|formFill\/pdf/, file);
  }
  for (const file of ['components/ApplicationResourcesCard.tsx', 'app/form-fill.tsx']) assert.match(read(file), /from '@\/lib\/vaultSave'/, file);
});

test('a stuck vault write times out, releases the queue and leaves nothing behind', async () => {
  reset();
  // A change that never finishes: the next write must still go through.
  V.vaultWriteTimeout.ms = 30;
  let finish; const stuck = V.updateVault(USER, () => new Promise(r => { finish = r; }));
  const next = V.updateVault(USER, async docs => ({ docs: [...docs, { id: 'doc_next' }], result: 'ok' }));
  await assert.rejects(stuck, e => e instanceof V.VaultTimeoutError);
  assert.equal(await next, 'ok', 'the queue moved on');
  finish({ docs: [{ id: 'doc_late' }], result: 'late' });
  await new Promise(r => setTimeout(r, 20));
  assert.deepEqual((await docs()).map(d => d.id), ['doc_next'], 'the late change never writes');

  // Through the real save: a hung list write times out with the fixed copy, and the queue is free again.
  reset();
  V.vaultWriteTimeout.ms = 40;
  storage.hang = true;
  const { resource } = official('pdf');
  await assert.rejects(V.saveOfficialForm(USER, PROGRAM, resource), e => e instanceof V.VaultSaveError && e.problem === 'failed' && e.message === '보관함에 저장하지 못했어요. 잠시 후 다시 시도해 주세요.');
  storage.hang = false;
  await new Promise(r => setTimeout(r, 150));
  V.vaultWriteTimeout.ms = 90_000;
  const other = official('hwpx');
  const doc = await V.saveOfficialForm(USER, PROGRAM, other.resource);
  assert.ok(doc.fileUri, 'a later save is not blocked');
  // The timed-out write had started, so it may land: its entry keeps its file, never an entry without one.
  for (const d of await docs()) assert.ok(d.fileUri && fs.existsSync(local(d.fileUri)), d.id);
  assert.equal(files('docs').length, (await docs()).length, 'no orphaned ciphertext');
});

test('L2: the 8 MiB cap holds while downloading: the task is cancelled early, nothing is kept', async () => {
  reset();
  const big = Buffer.concat([MAGIC.pdf, Buffer.alloc(40 * CHUNK)]);
  const url = 'https://official.example.org/forms/huge.pdf';
  downloads.set(url, { body: big });
  const { resource } = official('pdf');
  // Declared length over the cap: cancelled on the first chunk.
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, url, byte_size: null, content_hash: sha(big) }), 'too_large');
  assert.ok(download.cancelled && download.chunks <= 2, `stopped after ${download.chunks} chunks`);
  // No Content-Length: cancelled once the bytes written pass the cap.
  reset();
  download.hideLength = true;
  downloads.set(url, { body: big });
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, { ...resource, url, byte_size: null, content_hash: sha(big) }), 'too_large');
  assert.ok(download.cancelled && download.chunks <= 10, `stopped after ${download.chunks} chunks (of 41)`);
  assert.deepEqual(files('cache'), [], 'the partial download is deleted');
  assert.deepEqual(files('docs'), []);
  assert.deepEqual(await docs(), []);
  // A small file is unaffected.
  reset();
  assert.ok((await V.saveOfficialForm(USER, PROGRAM, official('pdf').resource)).fileUri);
  assert.equal(download.cancelled, false);
});

test('L3: one cleaner strips C0/C1, bidi, zero-width and line separators wherever a name is made', () => {
  const { cleanDocumentName } = require('../lib/documentName.ts');
  const hidden = ['\u0000', '\u0007', '\u001F', '\u007F', '\u0085', '\u009F', '\u061C', '\u200B', '\u200C', '\u200D', '\u200E', '\u200F', '\u2028', '\u2029', '\u202A', '\u202E', '\u2060', '\u2066', '\u2069', '\uFEFF'];
  for (const ch of hidden) assert.equal(cleanDocumentName(`청년${ch}지원`), '청년 지원', `U+${ch.charCodeAt(0).toString(16).padStart(4, '0')}`);
  assert.equal(cleanDocumentName('  a\n\n b\t'), 'a b');
  assert.equal(cleanDocumentName('가'.repeat(300)).length, 200);
  assert.equal(V.officialFormName('월세\u202Efdp.exe', '신청서\u200B'), '월세 fdp.exe – 신청서');
  // Every place that makes a document or file name uses it, and no ad-hoc cleaner is left.
  const device = read('lib/formFill/device.ts');
  assert.match(device, /const safe = cleanDocumentName\(title\.replace\(/, 'the filled-PDF file name');
  assert.match(device, /program_title: cleanDocumentName\(record\.program_title\), resource_label: cleanDocumentName\(record\.resource_label\)/, '작성 기록 labels');
  assert.match(read('lib/vault.ts'), /name: cleanDocumentName\(entry\.name\)/, 'every vault entry');
  for (const file of ['lib/vaultSave.ts', 'lib/vault.ts', 'lib/formFill/device.ts']) assert.doesNotMatch(read(file), /\\u0000-\\u001F|\\u202A/, `${file}: no second cleaner`);
});

test('L4: a picked file is encrypted from memory into the vault (never copied in plaintext)', async () => {
  reset();
  const picked = path.join(dir, 'picked-scan.jpg');
  const photo = Buffer.concat([Buffer.from([0xff, 0xd8, 0xff]), Buffer.from('주민등록번호 900101-1234567'), nodeCrypto.randomBytes(1000)]);
  fs.writeFileSync(picked, photo);
  const doc = await V.storeVaultFile(USER, { sourceUri: `file://${picked}` }, { type: 'resident_register', name: '주민등록등본\u202E', mimeType: 'image/jpeg', issuedAt: '2026-10-01T00:00:00.000Z' }, { extension: 'jpg' });
  assert.equal(doc.name, '주민등록등본');
  assert.equal(doc.encrypted, true);
  assert.equal(doc.issuedAt, '2026-10-01T00:00:00.000Z');
  assert.deepEqual(files('docs').map(f => path.basename(f)), [`${doc.id}.jpg.enc`], 'only ciphertext; copyAsync would have thrown');
  assert.equal(fs.readFileSync(files('docs')[0]).includes(Buffer.from('900101-1234567')), false);
  const out = await C.decryptFile(doc.fileUri, await C.generateEncryptionKey(USER));
  assert.equal(Buffer.compare(fs.readFileSync(local(out)), photo), 0);
  // A metadata-only entry has no file and is not marked encrypted.
  const bare = await V.addVaultEntry(USER, { type: 'income_proof', name: '소득증명원' });
  assert.deepEqual([bare.fileUri, bare.encrypted, bare.mimeType], [null, false, null]);
});

test('L4: the startup sweep removes decrypted copies and plaintext leftovers, never a file an entry points to', async () => {
  reset();
  const doc = await V.saveOfficialForm(USER, PROGRAM, official('pdf').resource);
  const vaultDir = path.join(dir, 'docs/vault', USER);
  fs.writeFileSync(path.join(vaultDir, 'doc_1_aa.pdf'), 'plaintext leftover');                // interrupted old copy-then-encrypt
  fs.writeFileSync(path.join(vaultDir, 'doc_2_bb.jpg'), 'legacy file the list still uses');
  const list = JSON.parse(storage.get(V.vaultStorageKey(USER)));
  list.push({ id: 'doc_2_bb', type: 'other', name: 'legacy', fileUri: `file://${vaultDir}/doc_2_bb.jpg`, mimeType: 'image/jpeg', issuedAt: '2020-01-01', expiresAt: null, addedAt: '2020-01-01', note: null, encrypted: false });
  storage.set(V.vaultStorageKey(USER), JSON.stringify(list));
  fs.mkdirSync(path.join(dir, 'cache/vault_tmp'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'cache/vault_tmp/opened.pdf'), 'decrypted copy');
  fs.mkdirSync(path.join(dir, 'docs/vault/not-an-account'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'docs/vault/not-an-account/x.pdf'), 'left alone');
  await V.sweepVaultLeftovers();
  assert.deepEqual(fs.readdirSync(vaultDir).sort(), [path.basename(local(doc.fileUri)), 'doc_2_bb.jpg'].sort());
  assert.equal(fs.existsSync(path.join(dir, 'cache/vault_tmp')), false);
  assert.ok(fs.existsSync(path.join(dir, 'docs/vault/not-an-account/x.pdf')), 'only account folders are swept');
  assert.match(read('app/_layout.tsx'), /void sweepVaultLeftovers\(\)\.catch\(\(\) => \{\}\);/, 'runs at startup');
  platform.OS = 'web';
  await V.sweepVaultLeftovers();
});

test('L5: a save that began before an account clear creates no folder, salt or file', async () => {
  reset();
  secure.clear();
  const started = V.vaultGeneration(USER);
  await V.clearAccountVault(USER);
  await assert.rejects(V.storeVaultFile(USER, { bytes: new Uint8Array(MAGIC.pdf) }, { type: 'other', name: 'late' }, { extension: 'pdf', started }), /Account cleared|no longer current/);
  assert.equal(fs.existsSync(path.join(dir, 'docs/vault', USER)), false, 'no folder');
  assert.deepEqual([...secure.keys()].filter(k => k.includes('salt')), [], 'no new salt');
  assert.deepEqual(files('docs'), []);
  // Cleared while the save is already running (after the queue's own check): still no folder, salt or file.
  reset(); secure.clear();
  await assert.rejects(V.storeVaultFile(USER, { bytes: new Uint8Array(MAGIC.pdf) }, { type: 'other', name: 'racing' }, {
    extension: 'pdf', refuse: () => { void V.clearAccountVault(USER).catch(() => {}); },
  }), /no longer current/);
  await V.updateVault(USER, async d => ({ docs: d, result: null }));
  assert.equal(fs.existsSync(path.join(dir, 'docs/vault', USER)), false, 'no folder (in-run)');
  assert.deepEqual([...secure.keys()].filter(k => k.includes('salt')), [], 'no new salt (in-run)');
  // The liveness check also sits inside the run, before the folder and the key.
  const lib = read('lib/vault.ts');
  const body = lib.slice(lib.indexOf('export async function storeVaultFile('), lib.indexOf('export function addVaultEntry('));
  const order = ['live();\n      const dir = await ensureVaultDir', 'live();\n      const key = await generateEncryptionKey', 'try { live(); } catch (e) { await FileSystem.deleteAsync(encUri'].map(x => body.indexOf(x));
  assert.ok(order.every(i => i > 0) && order[0] < order[1] && order[1] < order[2], 'live() before folder, before key, after encryption');
});

test('queue: a timed-out list write that had started holds the queue until it settles; clearing has the same deadline', async () => {
  reset();
  V.vaultWriteTimeout.ms = 30;
  storage.hang = true;
  const events = [];
  const first = V.updateVault(USER, async docs => ({ docs: [...docs, { id: 'doc_first' }], result: 'first' }));
  const second = V.updateVault(USER, async docs => { events.push(['second reads', docs.map(d => d.id)]); return { docs, result: 'second' }; });
  await assert.rejects(first, e => e instanceof V.VaultTimeoutError && e.writeStarted);
  storage.hang = false;
  assert.equal(await second, 'second');
  assert.deepEqual(events, [['second reads', ['doc_first']]], 'the next write ran after the stalled write landed, and saw it');
  // clearAccountVault: rejects at the deadline, and later work waits for its deletes.
  reset();
  await V.addVaultEntry(USER, { type: 'other', name: 'kept until clear' });
  V.vaultWriteTimeout.ms = 30;
  storage.hangRemove = true;
  const t0 = Date.now();
  await assert.rejects(V.clearAccountVault(USER), e => e instanceof V.VaultTimeoutError);
  assert.ok(Date.now() - t0 < 110, 'the caller is released at the deadline');
  storage.hangRemove = false;
  V.vaultWriteTimeout.ms = 90_000;
  const after = await V.updateVault(USER, async docs => ({ docs, result: docs.length }));
  assert.equal(after, 0, 'the next operation ran after the clear finished');
});

test('M1: the vault folder is kept out of iOS and Android backups (config plugin, new native build)', () => {
  const app = JSON.parse(read('app.json')).expo;
  assert.ok(app.plugins.includes('./plugins/withVaultBackupExclusion'), 'plugin registered');
  const secureStore = app.plugins.find(p => Array.isArray(p) && p[0] === 'expo-secure-store');
  assert.deepEqual(secureStore?.[1], { configureAndroidBackup: false }, 'the plugin owns the Android backup rules');
  const plugin = require('../plugins/withVaultBackupExclusion.js');
  // Android backs up only what an <include> names, so the vault (domain "file") stays
  // out because nothing outside shared preferences is ever included. Every exclude
  // must also sit inside an included path, or lintVitalRelease fails the release build.
  const sections = [plugin.backupXml(), ...plugin.extractionXml().match(/<(cloud-backup|device-transfer)>[\s\S]*?<\/\1>/g)];
  assert.equal(sections.length, 3, 'Android 11-, cloud backup and device transfer');
  for (const xml of sections) {
    const includes = [...xml.matchAll(/<include domain="([^"]+)" path="([^"]+)"\/>/g)].map(m => `${m[1]}:${m[2]}`);
    assert.deepEqual(includes, ['sharedpref:.'], 'only shared preferences are backed up, never files/vault');
    const excludes = [...xml.matchAll(/<exclude domain="([^"]+)" path="([^"]+)"\/>/g)].map(m => `${m[1]}:${m[2]}`);
    assert.deepEqual(excludes, ['sharedpref:SecureStore'], 'expo-secure-store’s own exclusion is kept');
    for (const e of excludes) assert.ok(includes.some(i => i.split(':')[0] === e.split(':')[0]), `${e} is inside an included domain (lint FullBackupContent)`);
  }
  assert.match(plugin.SWIFT, /appendingPathComponent\("vault", isDirectory: true\)/);
  assert.match(plugin.SWIFT, /values\.isExcludedFromBackup = true/);
  // Keychain items: device-only, unlocked-only.
  const crypto = read('lib/crypto.ts');
  assert.match(crypto, /const DEVICE_ONLY = \{ keychainAccessible: SecureStore\.WHEN_UNLOCKED_THIS_DEVICE_ONLY \};/);
  assert.equal((crypto.match(/setItemAsync\([^)]*DEVICE_ONLY\)/g) ?? []).length, 3, 'master key, salt and the migrated copy');
  assert.doesNotMatch(crypto, /setItemAsync\([^,]+,[^,)]+\)/, 'no keychain write without the option');
});

test('a save that times out while encrypting leaves no .enc behind, even when the write lands late', async () => {
  reset();
  V.vaultWriteTimeout.ms = 30;
  failures.slowWrite = 80;   // the encrypted file is written after the deadline
  const { resource } = official('pdf');
  await rejectsWith(V.saveOfficialForm(USER, PROGRAM, resource), 'failed');
  await new Promise(r => setTimeout(r, 150));
  failures.slowWrite = 0;
  V.vaultWriteTimeout.ms = 90_000;
  assert.deepEqual(files('docs'), [], 'the task removed the file it wrote after the deadline');
  assert.deepEqual(await docs(), []);
});

test('restored on another device: an entry whose file is gone gets its own message, and no key is created for it', async () => {
  reset(); secure.clear();
  const doc = await V.saveOfficialForm(USER, PROGRAM, official('pdf').resource);
  const opened = await V.decryptVaultFile(USER, doc);
  assert.ok(fs.existsSync(local(opened)), 'a present file opens');
  // The list came back from an iOS backup; the vault folder and the device-only key did not.
  fs.rmSync(path.join(dir, 'docs/vault'), { recursive: true, force: true });
  secure.clear();
  await assert.rejects(V.decryptVaultFile(USER, doc), e => e instanceof V.VaultFileMissingError && e.message === '다른 기기에서 복원된 항목이라 파일이 없어요. 다시 등록해 주세요.');
  assert.equal(secure.size, 0, 'no key material is made for a missing file');
  const screen = read('app/document-vault.tsx');
  assert.match(screen, /uri=await decryptVaultFile\(user\.id,doc\)/, 'the vault screen opens through the helper');
  assert.match(screen, /catch\(e\)\{setError\(e instanceof VaultFileMissingError\?e\.message:"서류를 열지 못했어요\. 다시 등록해 주세요\."\)\}/, 'same error line, specific copy');
});
