// Camera math for touch: pinch to zoom, two fingers to pan. Pure, so it is tested without a canvas.
export interface Cam {
  x: number;
  y: number;
  s: number;
}
export interface Pt {
  x: number;
  y: number;
}

export const MIN_ZOOM = 0.25;
export const MAX_ZOOM = 3;

const clamp = (s: number) => Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, s));
const mid = (a: Pt, b: Pt): Pt => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
const gap = (a: Pt, b: Pt) => Math.hypot(a.x - b.x, a.y - b.y);

/** Zoom by `factor` keeping the point (mx, my) still under the finger or cursor. */
export function zoomAt(cam: Cam, mx: number, my: number, factor: number): Cam {
  const s = clamp(cam.s * factor);
  return { s, x: mx - ((mx - cam.x) * s) / cam.s, y: my - ((my - cam.y) * s) / cam.s };
}

/**
 * The camera for two fingers now at `a`/`b`, given where they were (`a0`/`b0`) when the gesture
 * started and the camera then. The map point under the starting midpoint ends up under the
 * current midpoint, so moving both fingers pans and spreading them zooms.
 */
export function pinch(cam0: Cam, a0: Pt, b0: Pt, a: Pt, b: Pt): Cam {
  const d0 = gap(a0, b0);
  const s = clamp(d0 < 1 ? cam0.s : (cam0.s * gap(a, b)) / d0);
  const m0 = mid(a0, b0);
  const m = mid(a, b);
  const wx = (m0.x - cam0.x) / cam0.s;
  const wy = (m0.y - cam0.y) / cam0.s;
  return { s, x: m.x - wx * s, y: m.y - wy * s };
}

/** A finger needs a bigger slop than a mouse before a press counts as a drag rather than a tap. */
export function dragSlop(pointerType: string): number {
  return pointerType === "touch" ? 10 : 4;
}
