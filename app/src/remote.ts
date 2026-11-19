// The remote daemon this device is paired with: where it is and the credential it gave us.
// Kept on the device (localStorage in the app's own webview or the browser's origin), never sent anywhere but that daemon.

export interface RemoteDaemon {
  host: string;
  port: number;
  credential: string;
  /** What this device is called on the desktop's list. */
  name: string;
  deviceId?: string;
}

const KEY = "colony.remote";
export const DEFAULT_PORT = 7878;

interface Store {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
  removeItem(k: string): void;
}

function store(): Store | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function loadRemote(s: Store | null = store()): RemoteDaemon | null {
  try {
    const v = JSON.parse(s?.getItem(KEY) ?? "null");
    if (v && typeof v.host === "string" && typeof v.credential === "string" && Number.isInteger(v.port)) return v as RemoteDaemon;
  } catch {
    /* damaged: treat as unpaired */
  }
  return null;
}

export function saveRemote(r: RemoteDaemon, s: Store | null = store()): void {
  s?.setItem(KEY, JSON.stringify(r));
}

export function clearRemote(s: Store | null = store()): void {
  s?.removeItem(KEY);
}

/** A host is an IPv4 or IPv6 address or a name (a Tailscale MagicDNS one); nothing with a scheme, path or port. */
export function validHost(h: string): boolean {
  return /^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$/.test(h) || /^[0-9A-Fa-f:]+$/.test(h) || /^\[[0-9A-Fa-f:]+\]$/.test(h);
}

/** `host`, `host:port` or `[v6]:port`, as people type it. */
export function parseAddress(text: string): { host: string; port: number } | null {
  const t = text.trim();
  let host = t;
  let port = DEFAULT_PORT;
  const m = /^(\[[0-9A-Fa-f:]+\]|[^:]+):(\d{1,5})$/.exec(t);
  if (m) {
    host = m[1];
    port = Number(m[2]);
  }
  if (!host || port < 1 || port > 65535 || !validHost(host)) return null;
  return { host, port };
}

/**
 * What a pairing QR code holds: `colony://pair?host=100.101.102.103&port=7878&code=12345678`.
 * Returns null for anything else, so a stray QR code is just ignored.
 */
export function parsePairingLink(text: string): { host: string; port: number; code: string } | null {
  let u: URL;
  try {
    u = new URL(text.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "colony:" || u.hostname !== "pair") return null;
  const host = u.searchParams.get("host") ?? "";
  const code = u.searchParams.get("code") ?? "";
  const port = Number(u.searchParams.get("port") ?? DEFAULT_PORT);
  if (!validHost(host) || !/^[0-9 -]{8,12}$/.test(code) || !Number.isInteger(port) || port < 1 || port > 65535) return null;
  return { host, port, code: code.replace(/\D/g, "") };
}

export type Scope = "view" | "control";

export type PairResult = { ok: true; remote: RemoteDaemon; scope: Scope } | { ok: false; message: string };

/** Trade a pairing code for this device's own credential. The messages are for the person at the tablet. */
export async function pair(host: string, port: number, code: string, name: string, f: typeof fetch = fetch): Promise<PairResult> {
  const digits = code.replace(/\D/g, "");
  if (digits.length !== 8) return { ok: false, message: "The code is 8 digits, as shown on your PC." };
  let r: Response;
  try {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), 8000);
    try {
      const h = host.startsWith("[") || !host.includes(":") ? host : `[${host}]`;
      r = await f(`http://${h}:${port}/api/pair`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ code: digits, name }),
        signal: ctl.signal,
      });
    } finally {
      clearTimeout(t);
    }
  } catch {
    return { ok: false, message: "Can't reach your PC. Check that the VPN is on and Colony's remote access is turned on." };
  }
  if (r.status === 403) return { ok: false, message: "That code is wrong or has expired. Ask the PC for a new one." };
  if (r.status === 429) return { ok: false, message: "Too many wrong codes. Wait a minute, then ask the PC for a new code." };
  if (!r.ok) return { ok: false, message: `Your PC answered with an error (HTTP ${r.status}).` };
  const body = (await r.json().catch(() => null)) as { device_id?: string; credential?: string; scope?: Scope } | null;
  if (!body?.credential) return { ok: false, message: "Your PC's reply wasn't what Colony expected. Is it the same version?" };
  return { ok: true, scope: body.scope ?? "view", remote: { host, port, credential: body.credential, name, deviceId: body.device_id } };
}

/**
 * What to tell someone whose paired PC can't be reached. A web view can't read the VPN's state, but it
 * can tell whether this device has any network at all, which splits the cause into "this device" and
 * "the PC or the VPN between".
 */
export function unreachableText(why: string, deviceOnline: boolean, host?: string): string {
  if (!deviceOnline) return `${why}. This device has no network connection: turn on Wi-Fi or mobile data, then check its VPN is connected.`;
  const at = host ? ` at ${host}` : "";
  return `${why}. This device is online, but your PC${at} didn't answer: check the VPN is connected on this device and on your PC, and that the PC is awake.`;
}
