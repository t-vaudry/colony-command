import { describe, expect, it } from "vitest";
import { toolProp } from "./props";

describe("toolProp", () => {
  it("maps each tool family to its prop", () => {
    expect(toolProp("Read")).toBe("look");
    expect(toolProp("MultiEdit")).toBe("write");
    expect(toolProp("PowerShell")).toBe("run");
    expect(toolProp("WebSearch")).toBe("web");
    expect(toolProp("Agent")).toBe("delegate");
  });

  it("treats MCP tools as reaching outside", () => {
    expect(toolProp("mcp__github__create_issue")).toBe("web");
  });

  it("holds nothing for unknown or missing tools", () => {
    expect(toolProp("TodoWrite")).toBeNull();
    expect(toolProp(null)).toBeNull();
    expect(toolProp(undefined)).toBeNull();
  });
});
