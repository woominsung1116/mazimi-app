// A confirmed write (비서 card or a direct button) marks every cached read of
// the changed rows stale, in one place: lib/writeEffects.ts. Runs the shipped
// TypeScript against a real TanStack QueryClient; no network, no React.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);
const { QueryClient } = require('@tanstack/query-core');

const W = require('../lib/writeEffects.ts');
const root = path.join(__dirname, '..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');

const USER = 'u-1', PROGRAM = 'p-1', OTHER = 'p-2';
// The keys the screens read with (their literals are checked below).
const screenKeys = {
  program: ['program', PROGRAM],
  saved: ['saved', USER],
  alert: ['alert-preference', USER, PROGRAM],
  otherAlert: ['alert-preference', USER, OTHER],
  preparation: ['preparation', USER, PROGRAM],
  journey: ['application-journey', USER, PROGRAM],
  application: ['application', USER, PROGRAM],
  applications: ['applications', USER],
  guidance: ['benefit-assistant', USER, PROGRAM],
  briefing: ['assistant-briefing', USER],
  profile: ['profile', 'me', USER],
  recommend: ['recommend', { region: '26' }, USER],
  bundle: ['assistant-bundle', USER, 'housing'],
  otherGuidance: ['benefit-assistant', USER, OTHER],
  compare: ['assistant-compare', USER, 'p-1,p-2'],
  // Another account's rows on this device: never touched by this user's write.
  otherSaved: ['saved', 'u-2'],
  otherApplications: ['applications', 'u-2'],
  otherProfile: ['profile', 'me', 'u-2'],
  // Public lists the server gates on the viewer's profile (viewer_gate).
  programs: ['programs', { category: '' }, null],
  corporate: ['corporate-categories', USER, {}],
  imminent: ['home-imminent', '26'],
  browse: ['home-browse', '', '26', true],
};

function seeded() {
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } });
  for (const key of Object.values(screenKeys)) client.setQueryData(key, { cached: true });
  return client;
}
const stale = (client) => Object.entries(screenKeys).filter(([, key]) => client.getQueryState(key)?.isInvalidated).map(([name]) => name).sort();

const confirmation = (tool, extra = {}) => ({
  id: 'c-1', tool, program_id: PROGRAM, title: '', action: '', before: '', after: '', reversible: true,
  payload_hash: 'a'.repeat(64), expires_at: '2099-01-01T00:00:00Z', status: 'executed', ...extra,
});

// The approve handler's input: the run re-read after the tap, and the approve
// answer (null when it never arrived).
const settle = (client, card, { lostAnswer = false, runRead = true } = {}) => W.invalidateAfterApproval(client, {
  run: runRead ? { confirmations: [card] } : null, decided: lostAnswer ? null : card, confirmationId: card.id,
}, USER);

test('an executed 비서 write invalidates exactly the reads of the rows it changed', async () => {
  const expected = {
    set_alert: ['alert', 'briefing', 'guidance'],
    bookmark: ['briefing', 'guidance', 'saved'],
    toggle_checklist_item: ['briefing', 'guidance', 'preparation'],
    record_application_status: ['application', 'applications', 'briefing', 'guidance', 'journey'],
    // The server sends a program_id on every card, profile ones included; the profile is not per programme.
    profile_patch: ['briefing', 'browse', 'bundle', 'compare', 'corporate', 'guidance', 'imminent', 'otherGuidance', 'profile', 'programs', 'recommend'],
    add_to_calendar: [],
  };
  for (const [tool, names] of Object.entries(expected)) {
    const client = seeded();
    await settle(client, confirmation(tool));
    assert.deepEqual(stale(client), names, tool);
  }
});

test('the demo bug: an approved alert card refreshes the detail page bell', async () => {
  const client = seeded();
  await settle(client, confirmation('set_alert'));
  assert.equal(client.getQueryState(screenKeys.alert).isInvalidated, true, 'detail page alert');
  assert.equal(client.getQueryState(screenKeys.otherAlert).isInvalidated, false, 'another programme is untouched');
});

test('the server record decides, not the approve answer', async () => {
  // The network dropped after the server committed: no answer, but the re-read run says executed.
  const lost = seeded();
  await settle(lost, confirmation('set_alert'), { lostAnswer: true });
  assert.deepEqual(stale(lost), ['alert', 'briefing', 'guidance'], 'lost answer, executed on the server');
  // The re-read failed: the approve answer is the record.
  const unread = seeded();
  await settle(unread, confirmation('set_alert'), { runRead: false });
  assert.deepEqual(stale(unread), ['alert', 'briefing', 'guidance'], 'answer only');
  // A refused card (409: the server state moved): nothing written, but this device's copy is the stale one.
  const refused = seeded();
  await settle(refused, confirmation('set_alert', { status: 'stale' }), { lostAnswer: true });
  assert.deepEqual(stale(refused), ['alert', 'briefing', 'guidance'], 'stale');
  // Nothing at all to go on.
  const none = seeded();
  await settle(none, confirmation('set_alert'), { lostAnswer: true, runRead: false });
  assert.deepEqual(stale(none), [], 'no record');
});

test('a card that changed nothing invalidates nothing', async () => {
  for (const status of ['pending', 'rejected', 'expired', 'failed', 'approved']) {
    const client = seeded();
    await settle(client, confirmation('set_alert', { status }), { lostAnswer: true });
    assert.deepEqual(stale(client), [], status);
  }
  const readOnly = seeded();
  await settle(readOnly, confirmation('search_benefits'));
  assert.deepEqual(stale(readOnly), [], 'a read tool');
});

test('the approve handler settles on the re-read run, after any outcome of the request', () => {
  const agent = read('app/(tabs)/agent.tsx');
  const finallyBlock = agent.slice(agent.indexOf('async function decide'), agent.indexOf('async function writeCalendar'));
  assert.match(finallyBlock, /finally \{\s*const run = await refresh\(entry\.runId\);[\s\S]{0,200}if \(approve\) void invalidateAfterApproval\(client, \{ run, decided, confirmationId \}, userId\)/);
  assert.match(finallyBlock, /decided = decision\.confirmation;/);
  assert.equal((agent.match(/invalidateAfter/g) ?? []).length, 2, 'one import, one call: no other path');
});

test('a profile save waits only for the profile and 추천; the lists refresh behind it', async () => {
  const pending = [];
  const client = { invalidateQueries: ({ queryKey }) => {
    if (queryKey[0] === 'profile' || queryKey[0] === 'recommend') return Promise.resolve();
    return new Promise(() => pending.push(queryKey)); // a list refetch that never ends
  } };
  const wait = (key) => key[0] === 'profile' || key[0] === 'recommend';
  const done = await Promise.race([
    W.invalidateAfterWrite(client, 'profile_patch', { userId: USER, programId: null }, wait).then(() => 'resolved'),
    new Promise((r) => setTimeout(() => r('blocked'), 200)),
  ]);
  assert.equal(done, 'resolved');
  assert.ok(pending.some((k) => k[0] === 'programs'), 'the lists were still started');
  const src = read('lib/conditions.ts').replace(/\s+/g, '');
  assert.ok(src.includes("invalidateAfterWrite(client,'profile_patch',{userId,programId:null},key=>key[0]==='profile'||key[0]==='recommend')"), 'saveConditions waits for those two only');
});

test('every write tool the client knows has an entry; read tools have none', () => {
  const types = read('lib/agentTypes.ts');
  const tools = [...types.match(/export type ToolName =([^;]+);/)[1].matchAll(/'([a-z_]+)'/g)].map((m) => m[1]);
  for (const tool of tools) assert.ok(Object.hasOwn(W.WRITE_EFFECTS, tool) || W.READ_TOOLS.includes(tool), `${tool} is neither mapped nor a read`);
  for (const tool of Object.keys(W.WRITE_EFFECTS)) assert.ok(tools.includes(tool), `${tool} is not a ToolName`);
});

test('screens read with the keys the map names', () => {
  const literals = {
    'app/programs/[id].tsx': ["['saved', user?.id]", "['alert-preference', user?.id, id]"],
    'app/apply-assistant.tsx': ["['preparation', user?.id, programId]", "['application-journey', user?.id, programId]"],
    'app/benefit-assistant.tsx': ["['benefit-assistant', userId, programId]"],
    'components/AssistantBriefingCard.tsx': ["['assistant-briefing', user?.id]"],
    'app/application/[id].tsx': ['["application",user?.id,id]'],
    'app/applications.tsx': ['["applications", user?.id]'],
    'app/(tabs)/manage.tsx': ['["saved", user?.id]'],
    'app/(tabs)/profile.tsx': ['["profile", "me", user?.id]', '["saved", user?.id]', '["applications", user?.id]'],
    'lib/catalogue.ts': ['["recommend", profile, user?.id]'],
    'app/assistant.tsx': ["['assistant-bundle', userId, scenario]"],
    'app/compare.tsx': ["['assistant-compare', userId, selection]"],
    'app/(tabs)/explore.tsx': ['["programs", filters', '["corporate-categories", user?.id, chipFilters]'],
    'app/(tabs)/index.tsx': ['["home-imminent", region]', '["home-browse", activeCategory'],
  };
  for (const [file, keys] of Object.entries(literals)) for (const key of keys) assert.ok(read(file).includes(key), `${file} reads ${key}`);
});

test('the confirm handler and the direct writes go through the map, not their own key lists', () => {
  const agent = read('app/(tabs)/agent.tsx');
  assert.doesNotMatch(agent, /confirmation\.tool === '(bookmark|set_alert|record_application_status|profile_patch|toggle_checklist_item)'/, 'no per-tool branches');
  const direct = {
    'app/programs/[id].tsx': ["'bookmark'", "'set_alert'"],
    'app/benefit-assistant.tsx': ["'set_alert'"],
    'app/apply-assistant.tsx': ["'toggle_checklist_item'", "'record_application_status'"],
    'app/application/[id].tsx': ["'record_application_status'"],
    'components/ApplicationJourneyCard.tsx': ["'record_application_status'"],
    'app/(tabs)/manage.tsx': ["'bookmark'"],
    'lib/conditions.ts': ["'profile_patch'"],
  };
  for (const [file, writes] of Object.entries(direct)) {
    const src = read(file).replace(/\s+/g, '');
    for (const write of writes) assert.ok(src.includes(`invalidateAfterWrite(client,${write}`), `${file}: ${write} through the map`);
  }
});
