import { describe, expect, it } from "vitest";
import { isProtocolError, outdatedBanner } from "./outdated";

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
