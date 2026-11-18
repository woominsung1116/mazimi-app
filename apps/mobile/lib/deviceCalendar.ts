// The expo-calendar adapter for lib/calendarEvent.ts. Writes only after an
// approved add_to_calendar card.
//
// API: expo-calendar 15 (SDK 54, pinned). Its function API is the current one
// in this version; the class API that deprecates `createEventAsync` and adds
// write-only access ships with a later SDK and needs a native build, so the
// switch belongs to that upgrade and this adapter is the one file to change.
//
// Permission: on iOS 17+ the OS's add-event form (`createEventInCalendarAsync`)
// runs without any calendar permission in this module, so the app does not ask
// to read every calendar just to add one event. An existing grant, Android and
// iOS < 17 write directly after the permission prompt.
import * as Calendar from 'expo-calendar';
import { Platform } from 'react-native';
import { addToDeviceCalendar, type CalendarEvent, type CalendarAdapter, type CalendarWriteResult } from './calendarEvent';

const iosMajor = Platform.OS === 'ios' ? Number.parseInt(String(Platform.Version), 10) : 0;

export const expoCalendarAdapter: CalendarAdapter = {
  async permissionGranted() {
    try { return (await Calendar.getCalendarPermissionsAsync()).status === 'granted'; } catch { return false; }
  },
  async requestPermission() {
    const { status } = await Calendar.requestCalendarPermissionsAsync();
    return status === 'granted';
  },
  async defaultCalendarId() {
    if (Platform.OS === 'ios') {
      try { return (await Calendar.getDefaultCalendarAsync()).id; } catch { /* fall back to the list */ }
    }
    const calendars = await Calendar.getCalendarsAsync(Calendar.EntityTypes.EVENT);
    const writable = calendars.filter(c => c.allowsModifications);
    return (writable.find(c => c.isPrimary) ?? writable[0])?.id ?? null;
  },
  createEvent(calendarId, event) {
    return Calendar.createEventAsync(calendarId, event);
  },
  addWithForm: iosMajor >= 17
    ? async (event) => {
      const { action } = await Calendar.createEventInCalendarAsync(event);
      return action === Calendar.CalendarDialogResultActions.saved ? 'saved' : 'canceled';
    }
    : null,
};

/** Write an approved event on this device. */
export function addEventOnThisDevice(event: CalendarEvent): Promise<CalendarWriteResult> {
  return addToDeviceCalendar(event, expoCalendarAdapter, Platform.OS);
}
