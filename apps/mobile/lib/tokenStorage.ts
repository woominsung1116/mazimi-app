import { Platform } from "react-native";
import * as SecureStore from "expo-secure-store";

// Serialize credential writes so a pending refresh cannot finish writing an
// old credential after a later logout has deleted it.
let writes: Promise<void> = Promise.resolve();
function mutate(operation: () => Promise<void>) {
  const next = writes.then(operation, operation);
  writes = next.catch(() => {});
  return next;
}
// Web sessions last for the browser tab. Native credentials stay in the OS key store.
export const tokenStorage = {
  getItemAsync: async (key: string) => {
    await writes;
    return Platform.OS === "web" ? (typeof sessionStorage === "undefined" ? null : sessionStorage.getItem(key)) : SecureStore.getItemAsync(key);
  },
  setItemAsync: (key: string, value: string) => mutate(async () => {
    if (Platform.OS === "web") sessionStorage.setItem(key, value);
    else await SecureStore.setItemAsync(key, value);
  }),
  deleteItemAsync: (key: string) => mutate(async () => {
    if (Platform.OS === "web") { if (typeof sessionStorage !== "undefined") sessionStorage.removeItem(key); }
    else await SecureStore.deleteItemAsync(key);
  }),
};
