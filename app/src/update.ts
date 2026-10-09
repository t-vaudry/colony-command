// A small "update available" notice over the map's bottom-right corner. It only
// checks (at start and every few hours) and installs when the user clicks Install;
// "Later" hides it until the next check. Installing never happens on its own.
type Invoke = <T>(cmd: string, args?: object) => Promise<T>;
interface Available {
  version: string;
  notes: string | null;
}

const RECHECK_MS = 6 * 3600 * 1000;

export function initUpdateNotice(): void {
  const invoke = (window as unknown as { __TAURI__?: { core?: { invoke: Invoke } } }).__TAURI__?.core?.invoke;
  const el = document.getElementById("update-notice");
  if (!invoke || !el) return;
  const text = el.querySelector<HTMLElement>(".msg")!;
  const install = el.querySelector<HTMLButtonElement>(".install")!;
  const later = el.querySelector<HTMLButtonElement>(".later")!;
  let dismissed: string | null = null;

  const check = async (): Promise<void> => {
    try {
      const u = await invoke<Available | null>("update_check");
      if (!u || u.version === dismissed) return;
      text.textContent = `Colony ${u.version} is available`;
      el.title = u.notes ?? "";
      install.disabled = false;
      el.hidden = false;
      later.onclick = () => {
        dismissed = u.version;
        el.hidden = true;
      };
    } catch {
      // Offline or no release published yet: try again later.
    }
  };

  install.addEventListener("click", () => {
    install.disabled = true;
    text.textContent = "Installing the update…";
    invoke("update_install").catch((e) => {
      text.textContent = `Update failed: ${e}`;
      install.disabled = false;
    });
  });

  void check();
  setInterval(() => {
    if (el.hidden) void check();
  }, RECHECK_MS);
}
