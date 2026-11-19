import { describe, expect, it } from "vitest";
import { ctrlify, keyData } from "./termkeys";

describe("termkeys", () => {
  it("knows the keys a touch keyboard lacks", () => {
    expect(keyData("esc")).toBe("\x1b");
    expect(keyData("tab")).toBe("\t");
    expect(keyData("ctrl-c")).toBe("\x03");
    expect(keyData("up")).toBe("\x1b[A");
    expect(keyData("left")).toBe("\x1b[D");
    expect(keyData("nope")).toBeNull();
  });

  it("turns a letter into its control code", () => {
    expect(ctrlify("c")).toBe("\x03");
    expect(ctrlify("D")).toBe("\x04");
    expect(ctrlify("[")).toBe("\x1b");
  });

  it("leaves what is not a control character alone", () => {
    expect(ctrlify("1")).toBe("1");
    expect(ctrlify("é")).toBe("é");
    expect(ctrlify("ab")).toBe("ab");
    expect(ctrlify("")).toBe("");
  });
});
