import { describe, expect, it } from "vitest";
import { modelLine, modelName, modelSplit, opusShare } from "./modelcost";
import { mergeModels, type ModelCost } from "./types";

const m = (model: string, cost_usd: number, extra: Partial<ModelCost> = {}): ModelCost => ({ model, tokens: 1000, cost_usd, partial: false, ...extra });

describe("model split", () => {
  const crew = [m("haiku", 0.05), m("sonnet", 0.42), m("advisor:opus", 0.18, { consults: 3 })];

  it("names models and advisors", () => {
    expect(modelName("advisor:opus")).toBe("Opus advisor");
    expect(modelName("haiku")).toBe("Haiku");
    expect(modelName("weird")).toBe("weird");
  });

  it("lists the biggest spender first and counts consults", () => {
    expect(modelSplit(crew)).toBe("Sonnet ~$0.42 · Opus advisor ~$0.18 (3 consults) · Haiku ~$0.05");
    expect(modelSplit([m("advisor:opus", 0.01, { consults: 1 })])).toBe("Opus advisor ~$0.01 (1 consult)");
    expect(modelSplit(undefined)).toBe("");
  });

  it("marks a model without a known price as partial", () => {
    expect(modelSplit([m("other", 0, { partial: true })])).toBe("Other ~$0.00 partial");
  });

  it("shares the cost to Opus as main model and advisor", () => {
    expect(opusShare(crew)).toBeCloseTo(0.18 / 0.65);
    expect(opusShare([m("opus", 1), m("sonnet", 3)])).toBeCloseTo(0.25);
    expect(opusShare([])).toBeNull();
    expect(modelLine(crew)).toContain("Opus 28% of cost");
  });

  it("stays quiet for a single plain model", () => {
    expect(modelLine([m("sonnet", 1)])).toBe("");
    expect(modelLine([m("advisor:opus", 1, { consults: 2 })])).toContain("2 consults");
  });

  it("merges ledger buckets", () => {
    const into = new Map<string, ModelCost>();
    mergeModels(into, [m("sonnet", 1), m("advisor:opus", 2, { consults: 1 })]);
    mergeModels(into, [m("sonnet", 0.5), m("advisor:opus", 1, { consults: 2 })]);
    expect(into.get("sonnet")?.cost_usd).toBeCloseTo(1.5);
    expect(into.get("advisor:opus")).toMatchObject({ cost_usd: 3, consults: 3, tokens: 2000 });
  });
});
