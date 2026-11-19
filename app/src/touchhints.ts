// A touchscreen has no hover, so every `title` explanation is out of reach. A long press on a control
// that has one shows it in the toast, and the tap that follows the press is swallowed.

const HOLD_MS = 500;

export function initTouchHints(show: (text: string) => void): void {
  if (!matchMedia("(hover: none)").matches) return;
  let timer: number | undefined;
  let fired = false;
  const cancel = () => clearTimeout(timer);
  document.addEventListener(
    "pointerdown",
    (e) => {
      fired = false;
      const el = (e.target as HTMLElement).closest<HTMLElement>("[title]");
      const text = el?.getAttribute("title");
      if (!text || e.target instanceof HTMLCanvasElement) return;
      timer = window.setTimeout(() => {
        fired = true;
        show(text);
      }, HOLD_MS);
    },
    true,
  );
  for (const t of ["pointerup", "pointercancel", "pointermove"]) document.addEventListener(t, cancel, true);
  // The release after a long press would otherwise click the button.
  document.addEventListener(
    "click",
    (e) => {
      if (!fired) return;
      fired = false;
      e.preventDefault();
      e.stopPropagation();
    },
    true,
  );
  document.addEventListener("contextmenu", (e) => fired && e.preventDefault(), true);
}
