import { describe, expect, it } from "vitest";
import { pairLinkFromCode, parsePairLink } from "./link";

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

describe("pairLinkFromCode", () => {
  const origin = "https://clip.example.com";

  it("accepts links for this relay", () => {
    expect(pairLinkFromCode(`${origin}/#pair=v1.abc.def&token=t`, origin)).toEqual({ secret: "v1.abc.def", token: "t" });
  });

  it("rejects other codes and other relays", () => {
    expect(pairLinkFromCode("hello", origin)).toEqual({ error: "That's not a YACS pairing code." });
    expect(pairLinkFromCode(`${origin}/#settings`, origin)).toEqual({ error: "That's not a YACS pairing code." });
    expect(pairLinkFromCode("https://other.example.com/#pair=v1.abc.def", origin)).toEqual({
      error: "That code is for other.example.com. Open YACS there to pair with it.",
    });
  });
});
