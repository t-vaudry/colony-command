// Dictation for the message boxes. A mic button floats over whichever box has
// focus (they come and go as panels re-render, so one shared button follows
// focus); Ctrl+Shift+M does the same from the keyboard. Audio is recorded here
// and transcribed on this machine by the desktop app (whisper.cpp); the text is
// inserted at the cursor and never sent for you.

type Invoke = <T>(cmd: string, args?: unknown) => Promise<T>;
type Listen = <T>(event: string, cb: (e: { payload: T }) => void) => Promise<() => void>;
interface Tauri {
  core?: { invoke: Invoke };
  event?: { listen: Listen };
}

/** Boxes you type messages into: reply boxes, the first message, "type your own answer". */
const FIELDS = "textarea, input.other";
const SAMPLE_RATE = 16000;
/** A forgotten recording stops itself. */
const MAX_SECONDS = 180;

type Mode = "idle" | "downloading" | "recording" | "transcribing";

/** Collects 16 kHz mono samples off the audio thread. */
const WORKLET = `class Tap extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch) this.port.postMessage(ch.slice(0));
    return true;
  }
}
registerProcessor("tap", Tap);`;

export function initSpeech(): void {
  const tauri = (window as unknown as { __TAURI__?: Tauri }).__TAURI__;
  const invoke = tauri?.core?.invoke;
  // Plain browser (dev server): no speech engine behind it, so no button.
  if (!invoke || !navigator.mediaDevices?.getUserMedia) return;
  new Dictation(invoke, tauri?.event?.listen);
}

class Dictation {
  private btn = document.createElement("button");
  private mode: Mode = "idle";
  private field: HTMLTextAreaElement | HTMLInputElement | null = null;
  /** How to find the field again if its panel re-renders mid-recording. */
  private again: string | null = null;
  private ready: boolean | null = null;

  private stream: MediaStream | null = null;
  private ctx: AudioContext | null = null;
  private chunks: Float32Array[] = [];
  private samples = 0;
  private timer = 0;
  private cancelled = false;

  constructor(
    private invoke: Invoke,
    listen: Listen | undefined,
  ) {
    const b = this.btn;
    b.type = "button";
    b.id = "mic";
    b.hidden = true;
    b.setAttribute("aria-label", "Dictate");
    // Clicking must not take focus from the box being dictated into.
    b.addEventListener("mousedown", (e) => e.preventDefault());
    b.addEventListener("click", () => void this.toggle());
    document.body.append(b);
    this.paint();

    document.addEventListener("focusin", (e) => {
      const t = e.target as HTMLElement;
      if (t.matches?.(FIELDS) && this.mode === "idle") this.attach(t as HTMLTextAreaElement);
    });
    document.addEventListener("focusout", () => {
      // Focus moves through document.body between boxes; check where it landed.
      setTimeout(() => {
        if (this.mode === "idle" && !document.activeElement?.matches?.(FIELDS)) this.btn.hidden = true;
      }, 0);
    });
    const place = () => this.place();
    window.addEventListener("resize", place);
    document.addEventListener("scroll", place, true);
    document.addEventListener("input", place, true);
    window.addEventListener("keydown", (e) => {
      if (e.ctrlKey && e.shiftKey && e.code === "KeyM") {
        e.preventDefault();
        const f = document.activeElement as HTMLElement | null;
        if (this.mode === "idle" && f?.matches?.(FIELDS)) this.attach(f as HTMLTextAreaElement);
        void this.toggle();
      } else if (e.key === "Escape" && this.mode === "recording") {
        e.preventDefault();
        e.stopPropagation();
        this.cancelled = true;
        void this.stop();
      }
    }, true);
    void listen?.<number>("speech-download", (e) => {
      this.btn.textContent = `${e.payload}%`;
    });
  }

  private attach(f: HTMLTextAreaElement | HTMLInputElement): void {
    this.field = f;
    this.again = selectorFor(f);
    // A modal dialog sits in the top layer, above anything in the body.
    const host = f.closest("dialog[open]") ?? document.body;
    if (this.btn.parentElement !== host) host.append(this.btn);
    this.btn.hidden = false;
    this.place();
  }

  private place(): void {
    const f = this.field;
    if (this.btn.hidden || !f?.isConnected) return;
    const r = f.getBoundingClientRect();
    const size = 30;
    const clipped = r.bottom < 0 || r.top > innerHeight || r.width === 0;
    this.btn.style.visibility = clipped ? "hidden" : "visible";
    this.btn.style.left = `${Math.max(0, r.right - size - 6)}px`;
    this.btn.style.top = `${Math.max(0, r.top + 6)}px`;
  }

  private paint(): void {
    const b = this.btn;
    b.dataset.mode = this.mode;
    b.disabled = this.mode === "transcribing";
    if (this.mode === "downloading") return;
    b.textContent = this.mode === "transcribing" ? "…" : "";
    b.title =
      this.mode === "recording"
        ? "Stop and insert the text (Ctrl+Shift+M). Esc cancels."
        : this.mode === "transcribing"
          ? "Transcribing…"
          : "Dictate into this box (Ctrl+Shift+M)";
    b.setAttribute("aria-pressed", String(this.mode === "recording"));
  }

  private set(mode: Mode): void {
    this.mode = mode;
    this.paint();
  }

  private toast(message: string): void {
    const t = document.getElementById("toast");
    if (!t) return;
    t.textContent = message;
    t.hidden = false;
    clearTimeout(Number(t.dataset.timer));
    t.dataset.timer = String(setTimeout(() => (t.hidden = true), 6000));
  }

  private async toggle(): Promise<void> {
    if (this.mode === "recording") return this.stop();
    if (this.mode !== "idle" || !this.field) return;
    try {
      if (this.ready === null) this.ready = (await this.invoke<{ ready: boolean }>("speech_status")).ready;
      if (!this.ready && !(await this.download())) return;
      await this.start();
    } catch (err) {
      this.cleanup();
      this.set("idle");
      this.toast(err instanceof Error ? err.message : String(err));
    }
  }

  /** The speech model is fetched once, with the user's say-so. */
  private async download(): Promise<boolean> {
    if (!confirm("Dictation runs on this computer. It needs to download a speech model once (about 150 MB). Download it now?")) return false;
    this.set("downloading");
    this.btn.textContent = "0%";
    try {
      await this.invoke("speech_download");
      this.ready = true;
      this.toast("Speech model ready. Click the mic to dictate.");
    } finally {
      this.set("idle");
    }
    return false;
  }

  private async start(): Promise<void> {
    this.stream = await navigator.mediaDevices
      .getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } })
      .catch(() => {
        throw new Error("Can't use the microphone. Allow it in Windows Settings > Privacy > Microphone.");
      });
    this.ctx = new AudioContext({ sampleRate: SAMPLE_RATE });
    const url = URL.createObjectURL(new Blob([WORKLET], { type: "text/javascript" }));
    try {
      await this.ctx.audioWorklet.addModule(url);
    } finally {
      URL.revokeObjectURL(url);
    }
    const tap = new AudioWorkletNode(this.ctx, "tap");
    this.chunks = [];
    this.samples = 0;
    this.cancelled = false;
    tap.port.onmessage = (e: MessageEvent<Float32Array>) => {
      this.chunks.push(e.data);
      this.samples += e.data.length;
    };
    this.ctx.createMediaStreamSource(this.stream).connect(tap);
    this.timer = window.setTimeout(() => void this.stop(), MAX_SECONDS * 1000);
    this.set("recording");
  }

  private cleanup(): void {
    clearTimeout(this.timer);
    this.stream?.getTracks().forEach((t) => t.stop());
    void this.ctx?.close();
    this.stream = this.ctx = null;
  }

  private async stop(): Promise<void> {
    if (this.mode !== "recording") return;
    const chunks = this.chunks;
    this.chunks = [];
    this.cleanup();
    if (this.cancelled || this.samples === 0) {
      this.set("idle");
      return;
    }
    this.set("transcribing");
    try {
      const pcm = new Int16Array(this.samples);
      let at = 0;
      for (const c of chunks) {
        for (let i = 0; i < c.length; i++) pcm[at++] = Math.max(-1, Math.min(1, c[i]!)) * 0x7fff;
      }
      const text = await this.invoke<string>("speech_transcribe", pcm.buffer);
      if (text) this.insert(text);
      else this.toast("Didn't catch anything.");
    } catch (err) {
      this.toast(err instanceof Error ? err.message : String(err));
    } finally {
      this.set("idle");
    }
  }

  /** At the cursor (or replacing the selection), spaced from the words around it. */
  private insert(text: string): void {
    let f = this.field;
    // The panel may have re-rendered while transcribing; the box is then a new element.
    if (!f?.isConnected && this.again) f = document.querySelector<HTMLTextAreaElement>(this.again);
    if (!f) {
      this.toast("The box is gone; the text was dropped.");
      return;
    }
    this.field = f;
    const v = f.value;
    const start = f.selectionStart ?? v.length;
    const end = f.selectionEnd ?? v.length;
    const before = v.slice(0, start);
    const after = v.slice(end);
    const lead = before && !/\s$/.test(before) ? " " : "";
    const tail = after && !/^\s/.test(after) ? " " : "";
    const ins = lead + text + tail;
    f.value = before + ins + after;
    const caret = before.length + lead.length + text.length;
    f.setSelectionRange(caret, caret);
    f.dispatchEvent(new Event("input", { bubbles: true }));
    f.focus();
    this.place();
  }
}

/** A selector that finds the same box after its panel is rebuilt, or null. */
function selectorFor(f: HTMLElement): string | null {
  if (f.id) return `#${CSS.escape(f.id)}`;
  const d = f.dataset;
  if (d.draft) return `[data-draft="${CSS.escape(d.draft)}"]`;
  if (d.other && d.req) return `[data-other="${CSS.escape(d.other)}"][data-req="${CSS.escape(d.req)}"]`;
  return null;
}
