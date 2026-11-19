import { platform } from "./platform";
// Whether the background service (colonyd) is the version this map was built for.
// The app replaces a mismatched daemon on start; this is the fallback for when it
// couldn't, so the user sees one clear banner rather than raw protocol errors.

/** The map's own version, or null outside the desktop app (a browser dev session). */
export async function appVersion(): Promise<string | null> {
  return platform().appVersion();
}

/** A command the daemon's protocol doesn't know: the daemon is older than the map. */
export function isProtocolError(message: string): boolean {
  return /unknown variant/i.test(message);
}

/** `major.minor` of a plain `x.y.z` version, or null when it isn't one. */
function minor(v: string): [number, number] | null {
  const m = /^(\d+)\.(\d+)\.\d+/.exec(v);
  return m ? [Number(m[1]), Number(m[2])] : null;
}

/**
 * The banner text for a phone or tablet paired with a PC, or null when they get along. They
 * update separately (the PC by its updater, the phone by a new APK), so a different patch
 * release is fine; a different major or minor, or a protocol error, is not. The text says
 * which side to update, because neither can restart the other.
 */
export function remoteMismatch(app: string | null, daemon: string | undefined, protocolError = false): string | null {
  if (daemon === undefined) return "Colony on your PC is too old for this app. Update Colony on your PC.";
  const a = app ? minor(app) : null;
  const d = minor(daemon);
  if (!a || !d) return protocolError ? `This app and Colony on your PC (${daemon}) don't match. Update both to the latest release.` : null;
  const sameLine = a[0] === d[0] && a[1] === d[1];
  if (sameLine && !protocolError) return null;
  const phoneOlder = d[0] > a[0] || (d[0] === a[0] && d[1] > a[1]);
  return phoneOlder
    ? `This app (${app}) is older than Colony on your PC (${daemon}). Install the latest app from the release page.`
    : `Colony on your PC (${daemon}) is older than this app (${app}). Update Colony on your PC.`;
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
