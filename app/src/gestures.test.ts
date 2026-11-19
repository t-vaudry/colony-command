import { describe, expect, it } from "vitest";
import { dragSlop, MAX_ZOOM, MIN_ZOOM, pinch, zoomAt } from "./gestures";

describe("zoomAt", () => {
  it("keeps the point under the cursor still", () => {
    const c = zoomAt({ x: 10, y: 20, s: 1 }, 100, 100, 2);
    expect(c.s).toBe(2);
    // the map point that was at (100,100) is still there
    expect((100 - c.x) / c.s).toBeCloseTo((100 - 10) / 1);
    expect((100 - c.y) / c.s).toBeCloseTo((100 - 20) / 1);
  });
  it("clamps the zoom", () => {
    expect(zoomAt({ x: 0, y: 0, s: 1 }, 0, 0, 100).s).toBe(MAX_ZOOM);
    expect(zoomAt({ x: 0, y: 0, s: 1 }, 0, 0, 0.0001).s).toBe(MIN_ZOOM);
  });
});

describe("pinch", () => {
  const cam = { x: 0, y: 0, s: 1 };
  it("spreading the fingers zooms in around their midpoint", () => {
    const c = pinch(cam, { x: 90, y: 100 }, { x: 110, y: 100 }, { x: 80, y: 100 }, { x: 120, y: 100 });
    expect(c.s).toBeCloseTo(2);
    expect((100 - c.x) / c.s).toBeCloseTo(100); // the point at x=100 is still under x=100
  });
  it("moving both fingers together pans without zooming", () => {
    const c = pinch(cam, { x: 90, y: 100 }, { x: 110, y: 100 }, { x: 120, y: 130 }, { x: 140, y: 130 });
    expect(c.s).toBeCloseTo(1);
    expect(c.x).toBeCloseTo(30);
    expect(c.y).toBeCloseTo(30);
  });
  it("two fingers on the same spot do not blow up", () => {
    const c = pinch(cam, { x: 5, y: 5 }, { x: 5, y: 5 }, { x: 50, y: 50 }, { x: 60, y: 50 });
    expect(Number.isFinite(c.x) && Number.isFinite(c.s)).toBe(true);
  });
});

describe("dragSlop", () => {
  it("is larger for a finger", () => {
    expect(dragSlop("touch")).toBeGreaterThan(dragSlop("mouse"));
  });
});
