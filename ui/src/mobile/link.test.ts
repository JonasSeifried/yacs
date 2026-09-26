import { describe, expect, it } from "vitest";
import { inviteLinkFromCode, inviteUrl, looksLikeCode, parseInviteLink } from "./link";

describe("parseInviteLink", () => {
  it("reads one-time invites", () => {
    expect(parseInviteLink("#join=v2.abc")).toEqual({ kind: "invite", secret: "v2.abc" });
  });

  it("reads the links YACS 0.3 made, with their token and name", () => {
    expect(parseInviteLink("#pair=v1.abc.def")).toEqual({ kind: "space", secret: "v1.abc.def", token: null, name: null });
    expect(parseInviteLink("#pair=v1.abc.def&token=s%203cret%26x&name=Anna+%26+me")).toEqual({
      kind: "space",
      secret: "v1.abc.def",
      token: "s 3cret&x",
      name: "Anna & me",
    });
  });

  it("ignores other fragments", () => {
    expect(parseInviteLink("")).toBeNull();
    expect(parseInviteLink("#settings")).toBeNull();
    expect(parseInviteLink("#pair=")).toBeNull();
    expect(parseInviteLink("#join=")).toBeNull();
  });
});

describe("inviteLinkFromCode", () => {
  const origin = "https://clip.example.com";

  it("accepts links for this relay", () => {
    expect(inviteLinkFromCode(` ${origin}/#join=v2.abc\n`, origin)).toEqual({ kind: "invite", secret: "v2.abc" });
  });

  it("rejects other codes and other relays", () => {
    expect(inviteLinkFromCode("hello", origin)).toEqual({ error: "That's not a YACS invite." });
    expect(inviteLinkFromCode(`${origin}/#settings`, origin)).toEqual({ error: "That's not a YACS invite." });
    expect(inviteLinkFromCode("https://other.example.com/#join=v2.abc", origin)).toEqual({
      error: "That invite is for other.example.com. Open YACS there to join with it.",
    });
  });
});

describe("inviteUrl", () => {
  it("makes links the apps read", () => {
    // The same as `invite_url` in yacs-client.
    const url = inviteUrl("v2.abc", "https://clip.example.com/");
    expect(url).toBe("https://clip.example.com/#join=v2.abc");
    expect(inviteLinkFromCode(url, "https://clip.example.com")).toEqual({ kind: "invite", secret: "v2.abc" });
    expect(inviteUrl("v2.abc", "https://c.example/yacs/")).toBe("https://c.example/yacs/#join=v2.abc");
  });
});

describe("looksLikeCode", () => {
  it("tells codes from links", () => {
    expect(looksLikeCode(" 7-tulip-apple")).toBe(true);
    expect(looksLikeCode("12 tulip apple")).toBe(true);
    expect(looksLikeCode("https://clip.example.com/#join=v2.abc")).toBe(false);
    expect(looksLikeCode("tulip")).toBe(false);
  });
});
