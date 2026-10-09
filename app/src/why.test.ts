import { describe, expect, it } from "vitest";
import { whyText, WRONG_STATE_CHOICES, type Agent } from "./types";

const agent = (over: Partial<Agent>): Agent => ({ state: "working", current_tool: null, ...over }) as Agent;

describe("whyText", () => {
  it("prefers the rule or classifier the daemon recorded", () => {
    expect(whyText(agent({ state: "awaiting_reply", basis: "Claude Haiku read the last message and found it is waiting for an answer." }))).toMatch(/Haiku/);
  });

  it("describes the state when the daemon gave no basis", () => {
    expect(whyText(agent({ state: "ready_to_review" }))).toMatch(/without a question/);
    expect(whyText(agent({ state: "working", current_tool: { name: "Bash" } as Agent["current_tool"] }))).toMatch(/Bash/);
  });
});

describe("wrong-state choices", () => {
  it("only names states the daemon accepts feedback for", () => {
    const accepted = ["working", "needs_input", "awaiting_reply", "blocked", "ready_to_review", "idle"];
    expect(WRONG_STATE_CHOICES.map(([s]) => s).sort()).toEqual([...accepted].sort());
  });
});
