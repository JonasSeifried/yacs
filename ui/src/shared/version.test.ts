import { describe, expect, it } from "vitest";
import { compareVersions, relayBehind } from "./version";

describe("compareVersions", () => {
  it("compares numerically", () => {
    expect(compareVersions("0.10.0", "0.9.9")).toBe(1);
    expect(compareVersions("1.2.3", "1.2.3")).toBe(0);
    expect(compareVersions("1.2", "1.2.1")).toBe(-1);
    expect(compareVersions("1.2.3-beta.1", "1.2.3")).toBe(0);
  });
});

describe("relayBehind", () => {
  it("flags older relays, including ones that don't say", () => {
    expect(relayBehind("0.2.0", "0.2.1")).toBe(true);
    expect(relayBehind(undefined, "0.2.0")).toBe(true);
    expect(relayBehind("0.2.1", "0.2.1")).toBe(false);
    expect(relayBehind("0.3.0", "0.2.1")).toBe(false);
  });
});
