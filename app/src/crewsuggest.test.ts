import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { chooseCrew, folderDefaultAction, suggest, type CrewInputs } from "./crewsuggest";

interface Case {
  prompt: string;
  crew: string | null;
  why: string | null;
}

// The table colony-core's tests read too.
const cases: Case[] = JSON.parse(readFileSync(new URL("../../crates/colony-core/tests/crew-suggest.json", import.meta.url), "utf8"));

describe("suggest", () => {
  it("has a table worth trusting", () => {
    expect(cases.length).toBeGreaterThanOrEqual(25);
  });

  it.each(cases.map((c) => [c.prompt.slice(0, 50), c] as const))("%s", (_name, c) => {
    const got = suggest(c.prompt);
    expect(got?.crew ?? null).toBe(c.crew);
    expect(got?.why ?? null).toBe(c.why);
  });
});

describe("folderDefaultAction", () => {
  it("leaves the default alone when the dialog ticked the box itself", () => {
    expect(folderDefaultAction({ touched: false, checked: true, hadDefault: true })).toBe("none");
  });

  it("leaves it alone when a one-off pick unticked the auto-ticked box", () => {
    expect(folderDefaultAction({ touched: false, checked: false, hadDefault: true })).toBe("none");
  });

  it("sets it when the user ticks the box", () => {
    expect(folderDefaultAction({ touched: true, checked: true, hadDefault: false })).toBe("set");
    expect(folderDefaultAction({ touched: true, checked: true, hadDefault: true })).toBe("set");
  });

  it("clears an existing default the user unticks, and does nothing without one", () => {
    expect(folderDefaultAction({ touched: true, checked: false, hadDefault: true })).toBe("clear");
    expect(folderDefaultAction({ touched: true, checked: false, hadDefault: false })).toBe("none");
  });
});

describe("chooseCrew", () => {
  const base: CrewInputs = { picked: null, folderDefault: null, suggestion: null, remembered: null, available: ["default", "light", "opus-solo", "mine"] };
  const light = { crew: "light", why: "x" } as const;

  it("lets the user's own pick win over everything", () => {
    expect(chooseCrew({ ...base, picked: "opus-solo", folderDefault: "mine", suggestion: light, remembered: "default" })).toEqual({ crew: "opus-solo", source: "picked" });
  });

  it("uses the folder's default before the suggestion", () => {
    expect(chooseCrew({ ...base, folderDefault: "mine", suggestion: light, remembered: "default" })).toEqual({ crew: "mine", source: "folder" });
  });

  it("uses the suggestion before the remembered pick", () => {
    expect(chooseCrew({ ...base, suggestion: light, remembered: "opus-solo" })).toEqual({ crew: "light", source: "suggested" });
  });

  it("falls back to the remembered pick, then the default crew", () => {
    expect(chooseCrew({ ...base, remembered: "opus-solo" })).toEqual({ crew: "opus-solo", source: "remembered" });
    expect(chooseCrew(base)).toEqual({ crew: "default", source: "default" });
  });

  it("skips crews that no longer exist", () => {
    expect(chooseCrew({ ...base, picked: "gone", folderDefault: "gone", remembered: "gone", suggestion: light })).toEqual({ crew: "light", source: "suggested" });
    expect(chooseCrew({ ...base, folderDefault: "gone", remembered: "gone" })).toEqual({ crew: "default", source: "default" });
  });
});
