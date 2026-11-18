/**
 * The form helper's cache folder (verified template downloads, the filled PDF
 * during the share sheet). Its own small module so the root layout can clear
 * it at startup without loading the form helper's PDF code.
 */
import { Platform } from 'react-native';
import * as FileSystem from 'expo-file-system/legacy';

export const CACHE_DIR = () => `${FileSystem.cacheDirectory}form-fill/`;

/** Leftovers from an interrupted session (app killed mid-share) are removed on entry and exit. */
export async function clearFormFillCache(): Promise<void> {
  if (Platform.OS === 'web' || !FileSystem.cacheDirectory) return;
  await FileSystem.deleteAsync(CACHE_DIR(), { idempotent: true }).catch(() => {});
}
