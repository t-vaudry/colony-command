// The one place that knows where the app is running: the desktop shell, a phone, or a plain browser.
// Everything else asks this module instead of looking for window.__TAURI__ itself.
export type PlatformKind = "desktop" | "mobile" | "browser";
export type Invoke = <T>(cmd: string, args?: object) => Promise<T>;

interface TauriGlobal {
  core?: { invoke: Invoke };
  app?: { getVersion: () => Promise<string> };
}

export interface Platform {
  kind: PlatformKind;
  /** The shell's command bridge; null outside a Tauri shell. */
  invoke: Invoke | null;
  /** The map's own version, or null outside a shell. */
  appVersion(): Promise<string | null>;
  /** Hand a link to the default browser. */
  openLink(uri: string): void;
  /** Which shell-only features exist here; hide the UI for the ones that don't. */
  has: { updater: boolean; gating: boolean; setup: boolean; daemonControl: boolean; patience: boolean };
}

const MOBILE_UA = /Android/i;

type Win = Pick<Window, "open"> & { __TAURI__?: TauriGlobal; navigator?: { userAgent?: string } };

/** Classify a window. Pure, so tests can pass a fake one. */
export function detect(win: Win): Platform {
  const tauri = win.__TAURI__;
  const invoke = tauri?.core?.invoke ?? null;
  const kind: PlatformKind = !tauri ? "browser" : MOBILE_UA.test(win.navigator?.userAgent ?? "") ? "mobile" : "desktop";
  const desktop = kind === "desktop";
  return {
    kind,
    invoke,
    async appVersion() {
      try {
        return (await tauri?.app?.getVersion()) ?? null;
      } catch {
        return null;
      }
    },
    openLink(uri) {
      if (invoke) void invoke("open_link", { url: uri });
      else win.open(uri, "_blank", "noopener");
    },
    has: { updater: desktop && !!invoke, gating: desktop && !!invoke, setup: desktop && !!invoke, daemonControl: desktop && !!invoke, patience: desktop && !!invoke },
  };
}

let cached: Platform | null = null;

export function platform(): Platform {
  return (cached ??= detect(window as unknown as Win));
}

/** For tests that change window.__TAURI__ between cases. */
export function resetPlatform(): void {
  cached = null;
}
