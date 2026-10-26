// Whether the background service (colonyd) is the version this map was built for.
// The app replaces a mismatched daemon on start; this is the fallback for when it
// couldn't, so the user sees one clear banner rather than raw protocol errors.

/** The map's own version, or null outside the desktop app (a browser dev session). */
export async function appVersion(): Promise<string | null> {
  const tauri = (window as unknown as { __TAURI__?: { app?: { getVersion: () => Promise<string> } } }).__TAURI__;
  try {
    return (await tauri?.app?.getVersion()) ?? null;
  } catch {
    return null;
  }
}

/** A command the daemon's protocol doesn't know: the daemon is older than the map. */
export function isProtocolError(message: string): boolean {
  return /unknown variant/i.test(message);
}

/**
 * The banner text, or null when the daemon is current. A snapshot without a
 * version comes from a daemon that predates versions; a different version is out
 * of date either way. Without the app's own version (a browser) only the former counts.
 */
export function outdatedBanner(app: string | null, daemon: string | undefined, protocolError = false): string | null {
  const stale = daemon === undefined || (app !== null && daemon !== app) || protocolError;
  if (!stale) return null;
  return `Colony's background service is out of date (${daemon ?? "unknown version"}). Restart Colony to update it.`;
}
