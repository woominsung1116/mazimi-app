/**
 * Returns the URL only when it is safe to put in an external `href`: HTTPS,
 * no embedded credentials, default port and a public dotted host name (no IP
 * literal, no localhost or internal suffix). Anything else, including
 * `javascript:` and relative values, yields null so the link is not rendered.
 */
export function safeHttpsUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value.trim());
    const host = url.hostname.toLowerCase();
    const publicHost =
      host.includes(".") &&
      !host.endsWith(".") &&
      !host.includes(":") &&
      !host.startsWith("[") &&
      !/^[\d.]+$/.test(host) &&
      host !== "localhost" &&
      !/\.(localhost|local|internal|home\.arpa)$/.test(host);
    return url.protocol === "https:" && !url.username && !url.password && url.port === "" && publicHost
      ? url.href
      : null;
  } catch {
    return null;
  }
}
