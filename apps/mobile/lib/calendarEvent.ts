/**
 * add_to_calendar on the device. The server proposes the event on a
 * confirmation card (title, all-day date, source quote) and returns it only
 * after the user approves; this module validates that event and writes it
 * through a calendar adapter (expo-calendar in the app, a fake in tests).
 * Nothing about the user is in the event, and the write result is not sent
 * back to the server.
 *
 * The event is all-day on the notice's date (the server's contract, which the
 * shipped app checks: `all_day: true`); a notice gives a date, rarely a time.
 * It carries two alarms, D-3 and D-1 at 09:00, inside the window the server's
 * own D-7/D-3/D-1 deadline pushes use (09–21).
 *
 * Pure logic, no React Native imports: `npm run test:agent-tools` loads it.
 */
import type { AgentConfirmation } from './agentTypes';

export interface CalendarEvent { title: string; date: string; notes: string; timeZone: 'Asia/Seoul' }

/** An alarm `relativeOffset` minutes from the event's start (negative: before). */
export interface CalendarAlarm { relativeOffset: number }

export interface CalendarEventInput {
  title: string; startDate: Date; endDate: Date; allDay: true; timeZone?: string; notes: string;
  alarms: CalendarAlarm[];
}

export interface CalendarAdapter {
  /** The calendar permission as it stands, without asking. */
  permissionGranted(): Promise<boolean>;
  /** Ask (or read) the calendar permission; true when granted. */
  requestPermission(): Promise<boolean>;
  /** A calendar this app may write to, or null. */
  defaultCalendarId(): Promise<string | null>;
  createEvent(calendarId: string, event: CalendarEventInput): Promise<string>;
  /** The OS's own add-event form where it needs no calendar permission
   * (iOS 17+), or null. The user saves or closes it there. */
  addWithForm: ((event: CalendarEventInput) => Promise<'saved' | 'canceled'>) | null;
}

export type CalendarWriteResult = 'added' | 'denied' | 'no_calendar' | 'failed' | 'canceled';

/** Days before the event, each alarm at ALARM_HOUR local time. */
export const ALARM_DAYS_BEFORE = [3, 1] as const;
export const ALARM_HOUR = 9;

/** The event's alarms, from its all-day start (local midnight). */
export function eventAlarms(): CalendarAlarm[] {
  return ALARM_DAYS_BEFORE.map(days => ({ relativeOffset: -days * 24 * 60 + ALARM_HOUR * 60 }));
}

/** The notice after a write: nothing when the user closed the OS form. */
export function calendarNotice(result: CalendarWriteResult): string {
  return result === 'canceled' ? '' : calendarResultMessages[result];
}

export const calendarResultMessages: Record<Exclude<CalendarWriteResult, 'canceled'>, string> = {
  added: '이 기기 캘린더에 일정을 넣었어요.',
  denied: '캘린더 권한이 없어 일정을 넣지 못했어요. 설정에서 캘린더 접근을 허용한 뒤 다시 요청해 주세요.',
  no_calendar: '일정을 넣을 수 있는 캘린더를 찾지 못했어요.',
  failed: '캘린더에 일정을 넣지 못했어요. 잠시 후 다시 시도해 주세요.',
};

const DATE = /^(\d{4})-(\d{2})-(\d{2})$/;
// Control and bidi characters never reach the device calendar.
const UNSAFE = /[\u0000-\u001f\u007f\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/;

function validDate(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  const m = value.match(DATE);
  if (!m) return false;
  const d = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
  return d.getUTCFullYear() === Number(m[1]) && d.getUTCMonth() === Number(m[2]) - 1 && d.getUTCDate() === Number(m[3]);
}

const text = (value: unknown, max: number): string | null =>
  typeof value === 'string' && value.trim() && value.length <= max && !UNSAFE.test(value) ? value.trim() : null;

/** The event an executed add_to_calendar card carries, or null. */
export function calendarEventOf(confirmation: AgentConfirmation): CalendarEvent | null {
  if (confirmation.tool !== 'add_to_calendar' || confirmation.status !== 'executed') return null;
  const result = confirmation.result as Record<string, unknown> | undefined;
  if (!result || result.device_write !== 'calendar') return null;
  const event = result.event as Record<string, unknown> | undefined;
  if (!event || event.all_day !== true || !validDate(event.date)) return null;
  const title = text(event.title, 80);
  const notes = text(event.notes, 300) ?? '';
  if (!title) return null;
  return { title, date: event.date, notes, timeZone: 'Asia/Seoul' };
}

/** An all-day event on the notice's calendar date. Android stores all-day
 * events at UTC midnight (CalendarContract). iOS reads an all-day event's
 * date in the device's own zone, so the dates are device-local and no
 * fixed zone is set: a phone abroad still shows the 27th, not the 26th. */
export function eventDates(date: string, platform: string = 'ios'): { startDate: Date; endDate: Date; timeZone?: string } {
  const [y, m, d] = date.split('-').map(Number);
  if (platform === 'android') {
    return { startDate: new Date(Date.UTC(y, m - 1, d)), endDate: new Date(Date.UTC(y, m - 1, d + 1)), timeZone: 'UTC' };
  }
  return { startDate: new Date(y, m - 1, d), endDate: new Date(y, m - 1, d, 23, 59, 59) };
}

/** Write after approval. Calendar access granted already: written
 * directly. Otherwise the OS form where it needs no permission (iOS 17+:
 * the app never asks to read the user's calendars); else permission first,
 * then a writable calendar. */
export async function addToDeviceCalendar(event: CalendarEvent, adapter: CalendarAdapter, platform: string = 'ios'): Promise<CalendarWriteResult> {
  try {
    const input: CalendarEventInput = { title: event.title, ...eventDates(event.date, platform), allDay: true, notes: event.notes, alarms: eventAlarms() };
    const granted = await adapter.permissionGranted?.() ?? false;
    if (!granted && adapter.addWithForm) return (await adapter.addWithForm(input)) === 'saved' ? 'added' : 'canceled';
    if (!granted && !(await adapter.requestPermission())) return 'denied';
    const calendarId = await adapter.defaultCalendarId();
    if (!calendarId) return 'no_calendar';
    await adapter.createEvent(calendarId, input);
    return 'added';
  } catch {
    return 'failed';
  }
}

/** An executed calendar card whose event this device can (re)write: after
 * a denied permission or on another device of the same account. */
export function canRetryCalendar(confirmation: AgentConfirmation): boolean {
  return calendarEventOf(confirmation) !== null;
}

