// The first screen on a phone or tablet: type the address and the 8-digit code your PC shows.
import type { Daemon } from "./daemon";
import { pair, parseAddress, parsePairingLink, saveRemote, type Scope } from "./remote";
import { platform } from "./platform";

/** What a scope means, in words for the person holding the device. */
export function scopeText(scope: Scope | null): string {
  return scope === "control" ? "can answer approvals and send replies" : scope === "view" ? "view only" : "";
}

function defaultName(): string {
  const ua = navigator.userAgent;
  if (/Android/i.test(ua)) return /Mobile/i.test(ua) ? "Android phone" : "Android tablet";
  return platform().kind === "browser" ? "Browser" : "My device";
}

export class PairScreen {
  private dlg = document.getElementById("pair-screen") as HTMLDialogElement;
  private form = document.getElementById("pair-form") as HTMLFormElement;
  private error = document.getElementById("pair-error")!;
  private busy = false;

  constructor(
    private daemon: Daemon,
    private paired: (scope: Scope) => void,
  ) {
    this.form.addEventListener("submit", (e) => {
      e.preventDefault();
      void this.submit();
    });
    // Pasting a whole pairing link into the address box fills in the rest.
    this.field("address").addEventListener("input", () => this.fromLink());
    this.daemon.onChange(() => this.sync());
    this.sync();
  }

  private field(name: string): HTMLInputElement {
    return this.form.elements.namedItem(name) as HTMLInputElement;
  }

  /** Open while there is nothing to connect to; closed otherwise. */
  private sync(): void {
    const unpaired = this.daemon.status === "unpaired";
    if (unpaired && !this.dlg.open) {
      if (!this.field("name").value) this.field("name").value = defaultName();
      this.dlg.showModal();
    } else if (!unpaired && this.dlg.open && !this.busy) {
      this.dlg.close();
    }
    if (unpaired && this.daemon.error && !this.error.textContent) this.error.textContent = this.daemon.error;
  }

  private fromLink(): void {
    const l = parsePairingLink(this.field("address").value);
    if (!l) return;
    this.field("address").value = l.port === 7878 ? l.host : `${l.host}:${l.port}`;
    this.field("code").value = l.code;
  }

  private async submit(): Promise<void> {
    if (this.busy) return;
    const addr = parseAddress(this.field("address").value);
    if (!addr) {
      this.error.textContent = "Enter your PC's VPN address, like 100.101.102.103 or pc.tailnet.ts.net.";
      return;
    }
    this.busy = true;
    this.error.textContent = "";
    (this.form.querySelector("button[type=submit]") as HTMLButtonElement).disabled = true;
    try {
      const name = this.field("name").value.trim() || defaultName();
      const r = await pair(addr.host, addr.port, this.field("code").value, name);
      if (!r.ok) {
        this.error.textContent = r.message;
        return;
      }
      saveRemote(r.remote);
      this.field("code").value = "";
      this.busy = false;
      this.daemon.connectNow();
      this.dlg.close();
      this.paired(r.scope);
    } finally {
      this.busy = false;
      (this.form.querySelector("button[type=submit]") as HTMLButtonElement).disabled = false;
    }
  }
}
