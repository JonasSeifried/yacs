import { describe, expect, it } from "vitest";
import { parsePairLink } from "./link";

describe("parsePairLink", () => {
  it("reads the secret and optional token", () => {
    expect(parsePairLink("#pair=v1.abc.def")).toEqual({ secret: "v1.abc.def", token: null });
    expect(parsePairLink("#pair=v1.abc.def&token=s%203cret%26x")).toEqual({ secret: "v1.abc.def", token: "s 3cret&x" });
  });

  it("ignores other fragments", () => {
    expect(parsePairLink("")).toBeNull();
    expect(parsePairLink("#settings")).toBeNull();
    expect(parsePairLink("#pair=")).toBeNull();
  });
});
