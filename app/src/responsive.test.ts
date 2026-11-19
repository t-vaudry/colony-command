import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

// The layout is CSS, so this only guards the pieces a refactor could quietly drop; the look is checked in a browser.
const css = readFileSync(new URL("./style.css", import.meta.url), "utf8");

describe("touch and small-screen layout", () => {
  it("hands touch gestures on the map to the app instead of the browser", () => {
    expect(css).toMatch(/#map canvas\s*\{[^}]*touch-action:\s*none/);
  });
  it("sizes touch targets for fingers on coarse pointers", () => {
    expect(css).toMatch(/@media \(pointer: coarse\)[\s\S]*min-height:\s*44px/);
  });
  it("has a tablet and a phone breakpoint", () => {
    expect(css).toContain("@media (max-width: 1100px)");
    expect(css).toContain("@media (max-width: 600px)");
  });
  it("keeps focused fields at 16px so the page doesn't zoom on focus", () => {
    expect(css).toMatch(/font-size:\s*16px/);
  });
});
