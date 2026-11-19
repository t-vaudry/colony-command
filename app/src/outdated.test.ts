import { describe, expect, it } from "vitest";
import { isProtocolError, outdatedBanner, remoteMismatch } from "./outdated";

describe("outdated service banner", () => {
  it("stays away when the versions match", () => {
    expect(outdatedBanner("0.1.7", "0.1.7")).toBeNull();
  });

  it("shows for a different version, older or newer", () => {
    expect(outdatedBanner("0.1.7", "0.1.6")).toBe("Colony's background service is out of date (0.1.6). Restart Colony to update it.");
    expect(outdatedBanner("0.1.7", "0.2.0")).not.toBeNull();
  });

  it("shows for a snapshot with no version", () => {
    expect(outdatedBanner("0.1.7", undefined)).toBe("Colony's background service is out of date (unknown version). Restart Colony to update it.");
  });

  it("can't compare without the app's version, but a protocol error still shows it", () => {
    expect(outdatedBanner(null, "0.1.6")).toBeNull();
    expect(outdatedBanner(null, "0.1.6", true)).not.toBeNull();
  });

  it("recognizes the daemon's unknown-command error", () => {
    expect(isProtocolError("unknown command (unknown variant `folder_crew`, expected one of `ack`, `spawn`)")).toBe(true);
    expect(isProtocolError("not connected to colonyd")).toBe(false);
  });
});

describe("a phone paired with a PC", () => {
  it("accepts a different patch release", () => {
    expect(remoteMismatch("0.2.4", "0.2.1")).toBeNull();
    expect(remoteMismatch("0.2.1", "0.2.9")).toBeNull();
  });

  it("says to update the app when the PC is newer, and the PC when it is older", () => {
    expect(remoteMismatch("0.2.0", "0.3.0")).toBe("This app (0.2.0) is older than Colony on your PC (0.3.0). Install the latest app from the release page.");
    expect(remoteMismatch("0.3.0", "0.2.5")).toBe("Colony on your PC (0.2.5) is older than this app (0.3.0). Update Colony on your PC.");
    expect(remoteMismatch("1.0.0", "0.9.0")).toContain("older than this app");
  });

  it("flags a daemon that predates versions, and a protocol error on matching versions", () => {
    expect(remoteMismatch("0.2.0", undefined)).toContain("too old");
    expect(remoteMismatch("0.2.0", "0.2.0", true)).toContain("older than this app");
  });

  it("can't judge without the app's version, unless a command was refused", () => {
    expect(remoteMismatch(null, "0.2.0")).toBeNull();
    expect(remoteMismatch(null, "0.2.0", true)).toContain("don't match");
  });
});
