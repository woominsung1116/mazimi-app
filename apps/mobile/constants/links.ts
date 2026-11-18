import { API_BASE_URL } from "@/lib/apiBaseUrl";

/**
 * Where a web visitor goes to get the 마지미 app. Until store listings exist
 * this is the 마지미 site (same origin as the API, see infra/caddy/Caddyfile);
 * set EXPO_PUBLIC_APP_INSTALL_URL to the store or landing page once one does.
 */
export const APP_INSTALL_URL = process.env.EXPO_PUBLIC_APP_INSTALL_URL?.trim() || API_BASE_URL;
