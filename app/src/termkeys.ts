// Keys a touchscreen keyboard lacks, for the terminal pane's key bar.

/** What each key sends to the session. */
export const KEYS: Record<string, string> = {
  esc: "\x1b",
  tab: "\t",
  enter: "\r",
  "ctrl-c": "\x03",
  up: "\x1b[A",
  down: "\x1b[B",
  right: "\x1b[C",
  left: "\x1b[D",
  "shift-tab": "\x1b[Z",
  home: "\x1b[H",
  end: "\x1b[F",
  pgup: "\x1b[5~",
  pgdn: "\x1b[6~",
  slash: "/",
  pipe: "|",
  tilde: "~",
};

/** Bytes for a key-bar key, or null for a name it does not know. */
export function keyData(name: string): string | null {
  return KEYS[name] ?? null;
}

/**
 * What a typed character becomes while Ctrl is armed: a letter or one of `@[\]^_`
 * turns into its control code (Ctrl-C is 0x03). Anything else is left as typed.
 */
export function ctrlify(data: string): string {
  if (data.length !== 1) return data;
  const c = data.toUpperCase().charCodeAt(0);
  return c >= 0x40 && c <= 0x5f ? String.fromCharCode(c - 0x40) : data;
}
