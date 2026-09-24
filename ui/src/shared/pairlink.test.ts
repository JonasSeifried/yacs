import { describe, expect, it } from "vitest";
import { readPairLink } from "./pairlink";

describe("readPairLink", () => {
  it("splits a pairing link into relay, secret and token", () => {
    expect(readPairLink(" https://clip.example.com/#pair=v1.abc.def&token=s3cret+%26x\n")).toEqual({
      serverUrl: "https://clip.example.com",
      secret: "v1.abc.def",
      token: "s3cret &x",
    });
    expect(readPairLink("http://10.0.0.2:8080/yacs/#pair=v1.abc.def")).toEqual({
      serverUrl: "http://10.0.0.2:8080/yacs",
      secret: "v1.abc.def",
      token: null,
    });
  });

  it("ignores anything else", () => {
    expect(readPairLink("https://clip.example.com")).toBeNull();
    expect(readPairLink("tundra velvet anchor")).toBeNull();
    expect(readPairLink("ftp://clip.example.com/#pair=v1.abc.def")).toBeNull();
  });
});
