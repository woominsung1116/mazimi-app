/**
 * The one cleaner for any name the app makes for a document or file (vault
 * entries, the filled-PDF file name, 작성 기록 labels). Removes characters that
 * can disguise a name or break a line: C0/C1 controls and DEL, bidi controls
 * (U+061C, U+200E/F, U+202A–202E, U+2066–2069), zero-width characters
 * (U+200B–200D, U+2060, U+FEFF) and the line/paragraph separators U+2028/2029.
 * Whitespace runs collapse to one space.
 */
const HIDDEN = /[\u0000-\u001F\u007F-\u009F\u061C\u200B-\u200F\u2028\u2029\u202A-\u202E\u2060\u2066-\u2069\uFEFF]/g;

export function cleanDocumentName(value: string, maxLength = 200): string {
  return value.replace(HIDDEN, " ").replace(/\s+/g, " ").trim().slice(0, maxLength).trim();
}

/**
 * What the app calls a form the user fills in (screen title, exported file,
 * 작성 기록, vault entry): the programme's name and 신청서. The agency's file
 * name ("…(서식).hwp") names a download, not the form.
 */
export function applicationFormName(programTitle: string): string {
  const title = cleanDocumentName(programTitle, 120);
  if (!title) return "신청서";
  return /신청서$/.test(title) ? title : `${title} 신청서`;
}

/**
 * The form's name for a programme, wherever the form-fill screen shows or stores it (header, shared
 * file, 작성 기록, vault copy): from the stored title, which records and vault copies already carry,
 * so one form has one name and earlier records keep theirs.
 */
export function programFormName(program: { title: string }): string {
  return applicationFormName(program.title);
}
