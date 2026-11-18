// agent-tools-v3 on the device: the calendar card (validation, permission,
// write through an adapter) and the official-page and issue-link cards'
// readers. Runs the shipped TypeScript; no network, no native modules.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

const C = require('../lib/calendarEvent.ts');
const T = require('../lib/agentTools.ts');


const executed = (result, extra = {}) => ({
  id: '00000000-0000-4000-8000-000000000001', tool: 'add_to_calendar', program_id: '00000000-0000-4000-8000-000000000002',
  title: '청년 면접 지원', action: '이 기기 캘린더에 ‘신청 마감’ 일정 추가 (2026. 11. 27.)', before: '캘린더에 추가 전', after: '2026. 11. 27. 하루 종일 일정',
  reversible: true, payload_hash: 'a'.repeat(64), expires_at: '2099-01-01T00:00:00Z', status: 'executed', result, ...extra,
});
const event = { title: '[신청 마감] 청년 면접 지원', date: '2026-11-27', all_day: true, time_zone: 'Asia/Seoul', notes: '공식 공고에서 다시 확인해 주세요.' };

function fakeCalendar({ granted = true, calendarId = 'cal-1', fail = false } = {}) {
  const calls = { permission: 0, created: [] };
  return {
    calls,
    adapter: {
      async requestPermission() { calls.permission++; return granted; },
      async defaultCalendarId() { return calendarId; },
      async createEvent(id, input) { if (fail) throw new Error('native'); calls.created.push({ id, input }); return 'event-1'; },
    },
  };
}

test('calendar card: only an executed card with a valid event becomes a device write', () => {
  const ok = C.calendarEventOf(executed({ device_write: 'calendar', event }));
  assert.deepEqual(ok, { title: event.title, date: '2026-11-27', notes: event.notes, timeZone: 'Asia/Seoul' });
  assert.equal(C.calendarEventOf(executed({ device_write: 'calendar', event }, { status: 'pending' })), null, 'not before approval');
  assert.equal(C.calendarEventOf(executed({ device_write: 'calendar', event }, { tool: 'set_alert' })), null);
  for (const bad of [
    { ...event, date: '2026-02-30' }, { ...event, date: '27/11/2026' }, { ...event, all_day: false },
    { ...event, title: '' }, { ...event, title: 'x'.repeat(81) }, { ...event, title: 'a\u202Eb' }, { ...event, title: 'line\nbreak' },
  ]) assert.equal(C.calendarEventOf(executed({ device_write: 'calendar', event: bad })), null, JSON.stringify(bad));
  assert.equal(C.calendarEventOf(executed({ event })), null, 'the server marks the device write');
});

test('calendar write: permission first, all-day on the notice date, every failure named', async () => {
  const yes = fakeCalendar();
  const ev = C.calendarEventOf(executed({ device_write: 'calendar', event }));
  assert.equal(await C.addToDeviceCalendar(ev, yes.adapter), 'added');
  assert.equal(yes.calls.permission, 1);
  const created = yes.calls.created[0];
  assert.equal(created.id, 'cal-1');
  assert.equal(created.input.allDay, true);
  // iOS: the all-day event is the notice's calendar date in the device's
  // own zone (review LOW), so it never drifts to the 26th or 28th abroad.
  const local = (d) => [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes(), d.getSeconds()];
  assert.deepEqual(local(created.input.startDate), [2026, 11, 27, 0, 0, 0], 'local midnight');
  assert.deepEqual(local(created.input.endDate), [2026, 11, 27, 23, 59, 59]);
  assert.equal(created.input.timeZone, undefined, 'device time zone, not a fixed one');
  for (const zone of ['America/Los_Angeles', 'Asia/Seoul', 'Pacific/Kiritimati']) {
    const out = require('node:child_process').execFileSync(process.execPath, ['-e', `
      const ts = require('typescript');
      require.extensions['.ts'] = (m, f) => m._compile(ts.transpileModule(require('node:fs').readFileSync(f, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, f);
      const C = require(${JSON.stringify(require.resolve('../lib/calendarEvent.ts'))});
      const r = C.eventDates('2026-11-27', 'ios');
      process.stdout.write(JSON.stringify([r.startDate.getDate(), r.endDate.getDate(), r.startDate.getHours(), r.timeZone ?? null]));
    `], { env: { ...process.env, TZ: zone }, cwd: require('node:path').join(__dirname, '..') }).toString();
    assert.equal(out, '[27,27,0,null]', zone);
  }
  // Android keeps all-day events at UTC midnight (code review M5).
  const android = fakeCalendar();
  assert.equal(await C.addToDeviceCalendar(ev, android.adapter, 'android'), 'added');
  const a = android.calls.created[0].input;
  assert.equal(a.startDate.toISOString(), '2026-11-27T00:00:00.000Z');
  assert.equal(a.endDate.toISOString(), '2026-11-28T00:00:00.000Z');
  assert.equal(a.timeZone, 'UTC');
  // An executed card can be written again (permission denied the first time).
  assert.equal(C.canRetryCalendar(executed({ device_write: 'calendar', event })), true);
  assert.equal(C.canRetryCalendar(executed({ device_write: 'calendar', event }, { status: 'pending' })), false);

  const no = fakeCalendar({ granted: false });
  assert.equal(await C.addToDeviceCalendar(ev, no.adapter), 'denied');
  assert.equal(no.calls.created.length, 0, 'nothing written without permission');
  assert.equal(await C.addToDeviceCalendar(ev, fakeCalendar({ calendarId: null }).adapter), 'no_calendar');
  assert.equal(await C.addToDeviceCalendar(ev, fakeCalendar({ fail: true }).adapter), 'failed');
  for (const key of ['added', 'denied', 'no_calendar', 'failed']) assert.ok(C.calendarResultMessages[key]);
});

test('calendar alarms: D-3 and D-1 at 09:00 on every write path', async () => {
  const ev = C.calendarEventOf(executed({ device_write: 'calendar', event }));
  // relativeOffset is minutes from the all-day start (local midnight):
  // 3 days before at 09:00 and 1 day before at 09:00.
  const want = [{ relativeOffset: -3 * 24 * 60 + 9 * 60 }, { relativeOffset: -24 * 60 + 9 * 60 }];
  for (const platform of ['ios', 'android']) {
    const cal = fakeCalendar();
    assert.equal(await C.addToDeviceCalendar(ev, cal.adapter, platform), 'added');
    assert.deepEqual(cal.calls.created[0].input.alarms, want, platform);
  }
  const form = formCalendar('saved');
  assert.equal(await C.addToDeviceCalendar(ev, form.adapter), 'added');
  assert.deepEqual(form.calls.form[0].alarms, want, 'the system form carries the alarms too');
});

// The OS's add-event form (iOS 17+): no calendar permission prompt.
function formCalendar(action, { granted = false } = {}) {
  const base = fakeCalendar({ granted: true });
  const calls = { ...base.calls, form: [], asked: 0 };
  return {
    calls,
    adapter: {
      ...base.adapter,
      async permissionGranted() { return granted; },
      async requestPermission() { calls.asked++; return true; },
      async createEvent(id, input) { calls.created.push({ id, input }); return 'event-1'; },
      addWithForm: async (input) => { calls.form.push(input); return action; },
    },
  };
}

test('calendar permission: the system form needs none; an earlier grant writes directly', async () => {
  const ev = C.calendarEventOf(executed({ device_write: 'calendar', event }));
  const saved = formCalendar('saved');
  assert.equal(await C.addToDeviceCalendar(ev, saved.adapter), 'added');
  assert.equal(saved.calls.asked, 0, 'no full-access prompt for the form');
  assert.equal(saved.calls.created.length, 0, 'the OS form writes, not the app');
  assert.equal(saved.calls.form[0].allDay, true);
  // The user closes the form: nothing written, nothing to tell them.
  const closed = formCalendar('canceled');
  assert.equal(await C.addToDeviceCalendar(ev, closed.adapter), 'canceled');
  assert.equal(C.calendarNotice('canceled'), '');
  assert.equal(C.calendarNotice('added'), C.calendarResultMessages.added);
  // Access granted before (an earlier build asked for it): one tap, no form.
  const granted = formCalendar('saved', { granted: true });
  assert.equal(await C.addToDeviceCalendar(ev, granted.adapter), 'added');
  assert.equal(granted.calls.form.length, 0);
  assert.equal(granted.calls.created.length, 1);
  // A form that fails is a failure, never a silent success.
  const broken = { ...formCalendar('saved').adapter, addWithForm: async () => { throw new Error('native'); } };
  assert.equal(await C.addToDeviceCalendar(ev, broken), 'failed');
});

const result = (tool, data, ok = true) => ({ seq: 0, tool, permission: 'read', ok, status: 'ok', data, evidence: [], executed_at: '2026-10-16T05:03:00Z' });

test('official page card: status, Korea time, quotes, changes and an HTTPS link only', () => {
  const view = T.officialPageView(result('check_official_page', {
    live_status: 'closed', checked_at: '2026-10-16T05:03:00Z', cached: false,
    source_url: 'https://www.city.go.kr/youth/1', official_url: 'https://www.city.go.kr/youth/1',
    quotes: [{ label: '접수 상태', text: '접수가 마감되었습니다' }],
    changes: [{ field: 'status', ours: '신청 기간', live: '마감' }, { field: 'deadline', ours: '2026. 11. 27.', live: '2026. 9. 30.' }, { field: 'attachments_added', live: ['a.hwp', 'b.pdf'] }],
  }));
  assert.equal(view.status, 'closed');
  assert.equal(view.label, '공식 페이지: 접수 마감');
  assert.equal(view.checkedAt, '10월 16일 14:03 확인');
  assert.deepEqual(view.quotes, [{ label: '접수 상태', text: '접수가 마감되었습니다' }]);
  assert.deepEqual(view.changes, ['접수 상태: 마지미 ‘신청 기간’ → 공식 ‘마감’', '마감일: 마지미 2026. 11. 27. → 공식 2026. 9. 30.', '마지미에 없는 첨부: 2개']);
  assert.equal(view.link, 'https://www.city.go.kr/youth/1');
  const unchecked = T.officialPageView(result('check_official_page', { live_status: 'not_checked', reason: 'timeout', official_url: 'javascript:alert(1)' }));
  assert.equal(unchecked.status, 'not_checked');
  assert.equal(unchecked.label, '공식 페이지를 확인하지 못했어요');
  assert.equal(unchecked.link, null, 'never a non-HTTPS link');
  assert.equal(T.officialPageView(result('check_official_page', { live_status: 'whatever' })).status, 'not_checked');
  assert.equal(T.officialPageView(result('search_benefits', {})), null);
});

test('issue link card: links only to catalog hosts over HTTPS; unknown documents get none', () => {
  const rows = T.issueLinkRows(result('document_issue_link', {
    official_url: 'https://www.city.go.kr/notice/1',
    documents: [
      { name: '주민등록등본', known: true, issuer: '정부24', url: 'https://www.gov.kr/mw/AA020InfoCappView.do?CappBizCD=13100000015', guidance: '정부24에 로그인', verified_on: '2026-10-16' },
      { name: '재학증명서', known: true, issuer: '다니는 학교', url: null, guidance: '학교 포털' },
      { name: '가짜', known: true, issuer: '피싱', url: 'https://gov.kr.evil.example/login', guidance: 'x' },
      { name: '평문', known: true, issuer: '정부24', url: 'http://www.gov.kr/', guidance: 'x' },
      { name: '임대차계약서', known: false },
    ],
  }));
  assert.equal(rows[0].url, 'https://www.gov.kr/mw/AA020InfoCappView.do?CappBizCD=13100000015');
  assert.equal(rows[0].verifiedOn, '2026-10-16');
  assert.equal(rows[1].url, null);
  assert.equal(rows[2].url, null, 'a host outside the catalog');
  assert.equal(rows[3].url, null, 'not HTTPS');
  assert.equal(rows[4].url, null);
  assert.match(rows[4].guidance, /^발급처를 안내할 수 없어요/);
  assert.equal(T.noticeLink(result('document_issue_link', { official_url: 'https://www.city.go.kr/notice/1' })), 'https://www.city.go.kr/notice/1');
  assert.deepEqual(T.issueLinkRows(result('document_issue_link', {}, false)), []);
  // The client list mirrors the server's.
  const server = fs.readFileSync(require('node:path').join(__dirname, '../../../crates/api/src/agent/documents.rs'), 'utf8');
  const hosts = server.match(/pub const ISSUER_HOSTS: &\[&str\] = &\[([^\]]*)\]/)[1].match(/"([^"]+)"/g).map(h => h.slice(1, -1));
  assert.deepEqual([...T.ISSUER_HOSTS], hosts);
});
