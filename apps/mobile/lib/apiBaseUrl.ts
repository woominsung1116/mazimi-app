// The API origin. Expo inlines EXPO_PUBLIC_API_URL at build time (eas.json
// profiles, CI, apps/mobile/.env). There is no built-in production host: a
// release bundle built without the variable stops at start with this error
// instead of quietly calling a domain that does not resolve. Development
// builds fall back to a local API.
const configured = process.env.EXPO_PUBLIC_API_URL?.trim();
const development = typeof __DEV__ !== "undefined" && __DEV__;

if (!configured && !development) {
  throw new Error("EXPO_PUBLIC_API_URL is not set for this build");
}

export const API_BASE_URL = (configured || "http://localhost:8080").replace(/\/+$/, "");
