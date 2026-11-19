// Remote access, for the whole app rather than one session: the VPN address, who is paired,
// pairing a new device, phone alerts and keeping the PC awake. Desktop only (a paired phone
// has nothing to manage here).
import type { Daemon } from "./daemon";
import { platform } from "./platform";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

export class RemotePanel {
  private dlg = document.getElementById("remote-panel") as HTMLDialogElement;
  private btn = document.getElementById("remote-btn") as HTMLButtonElement;
  private addrForm = document.getElementById("rp-addr-form") as HTMLFormElement;
  private ntfyForm = document.getElementById("rp-ntfy-form") as HTMLFormElement;
  private addr = this.addrForm.elements.namedItem("addr") as HTMLInputElement;
  private ntfy = this.ntfyForm.elements.namedItem("url") as HTMLInputElement;
  private error = document.getElementById("rp-error")!;

  constructor(private daemon: Daemon) {
    this.btn.addEventListener("click", () => this.open());
    document.getElementById("rp-close")!.addEventListener("click", () => this.dlg.close());
    this.addrForm.addEventListener("submit", (e) => {
      e.preventDefault();
      this.send(this.daemon.setRemoteListen(this.addr.value));
    });
    this.ntfyForm.addEventListener("submit", (e) => {
      e.preventDefault();
      this.send(this.daemon.setNtfyUrl(this.ntfy.value));
      this.ntfy.value = "";
    });
    document.getElementById("rp-restart-btn")!.addEventListener("click", () => void this.daemon.restart());
    this.dlg.addEventListener("click", (e) => this.click(e.target as HTMLElement));
    this.daemon.onError((m) => {
      if (this.dlg.open) this.error.textContent = m;
    });
    this.daemon.onChange(() => this.sync());
    this.sync();
  }

  private send(ok: boolean): void {
    this.error.textContent = ok ? "" : "Not connected to colonyd.";
  }

  open(): void {
    this.error.textContent = "";
    this.daemon.listDevices();
    this.sync(true);
    if (!this.dlg.open) this.dlg.showModal();
  }

  private click(t: HTMLElement): void {
    const el = t.closest("button");
    if (!el) return;
    const d = this.daemon;
    if (el.id === "rp-sleep") this.send(d.setPreventSleep(el.dataset.to === "on"));
    else if (el.id === "rp-pair") this.send(d.createPairing());
    else if (el.id === "rp-audit") this.send(d.showAudit());
    else if (el.hasAttribute("data-hide-audit")) d.hideAudit();
    else if (el.hasAttribute("data-hide-pairing")) d.hidePairing();
    else if (el.dataset.deviceScope && el.dataset.device) this.send(d.setDeviceScope(el.dataset.device, el.dataset.deviceScope as "view" | "control"));
    else if (el.dataset.revokeDevice) this.send(d.revokeDevice(el.dataset.revokeDevice));
  }

  /** Bring the dialog up to date. Fields being typed in are left alone. */
  private sync(force = false): void {
    // A paired phone has nothing to manage here; its own pairing is the pairing screen.
    this.btn.hidden = this.daemon.remote;
    if (!this.dlg.open && !force) return;
    const s = this.daemon.settings;
    const on = !!s.remote_listening;
    const saved = s.remote_listen ?? "";
    if (document.activeElement !== this.addr) this.addr.value = saved;
    document.getElementById("rp-status")!.textContent = on ? `On at ${s.remote_listening}` : "Off";
    // Saved but not what is running: the service has to restart to listen there.
    const pending = saved !== (s.remote_listening ? s.remote_listening.replace(/:\d+$/, "") : "");
    document.getElementById("rp-restart")!.hidden = !pending;
    (document.getElementById("rp-restart-btn") as HTMLButtonElement).hidden = !platform().has.daemonControl;
    document.getElementById("rp-restart-text")!.textContent = saved
      ? `Saved. Colony's background service has to restart to listen on ${saved}.`
      : "Saved. Colony's background service has to restart to stop listening.";
    document.getElementById("rp-on")!.hidden = !on;
    document.getElementById("rp-ntfy-status")!.textContent = s.push_on ? "On (ntfy). Paste a new address to change it, or clear it to turn alerts off." : "Off.";
    const sleep = document.getElementById("rp-sleep")!;
    sleep.textContent = s.prevent_sleep ? "On" : "Off";
    sleep.setAttribute("aria-pressed", String(s.prevent_sleep === true));
    sleep.dataset.to = s.prevent_sleep ? "off" : "on";
    document.getElementById("rp-devices")!.innerHTML = this.devices();
    this.codeAndLog();
  }

  private devices(): string {
    const list = this.daemon.devices
      .map(
        (d) =>
          `<div class="row"><span>${esc(d.name)}</span><span class="muted small">paired ${new Date(d.created_ms).toLocaleDateString()}${d.last_seen_ms ? `, seen ${new Date(d.last_seen_ms).toLocaleTimeString()}` : ""}</span><button type="button" data-device-scope="${d.scope === "control" ? "view" : "control"}" data-device="${esc(d.id)}" aria-pressed="${d.scope === "control"}" title="${d.scope === "control" ? "Can answer approvals, type into and end sessions. Click to make it view-only." : "View-only: can watch, not act. Click to let it control."}">${d.scope === "control" ? "Control" : "View only"}</button><button type="button" data-revoke-device="${esc(d.id)}" title="Unpair this device. It stops working at once.">Revoke</button></div>`,
      )
      .join("");
    return list || `<p class="muted small">No devices paired yet.</p>`;
  }

  private codeAndLog(): void {
    const p = this.daemon.pairing;
    const code = document.getElementById("rp-code")!;
    code.hidden = !p;
    if (p) {
      code.innerHTML = `<p>Enter this code on the device (valid 5 minutes, once): <b class="mono">${esc(p.code.slice(0, 4))} ${esc(p.code.slice(4))}</b></p>${p.remote ? `<p class="muted small">Connect to ${esc(p.remote)}</p>` : ""}<div class="actions"><button type="button" data-hide-pairing>Done</button></div>`;
    }
    const a = this.daemon.audit;
    const log = document.getElementById("rp-log")!;
    log.hidden = !a;
    if (a) {
      log.innerHTML = `<p><b>Remote activity</b> <span class="muted small">device, kind of command and time only; never what was typed</span></p>${
        a.length
          ? a.map((e) => `<div class="row small"><span>${new Date(e.ts).toLocaleString()}</span><span>${esc(e.device_name)}</span><span>${esc(e.command.replace(/_/g, " "))}${e.allowed ? "" : " (refused)"}</span></div>`).join("")
          : `<p class="muted small">Nothing yet.</p>`
      }<div class="actions"><button type="button" data-hide-audit>Close</button></div>`;
    }
  }
}
