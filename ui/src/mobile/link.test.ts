import { describe, expect, it } from "vitest";
import { inviteLinkFromCode, inviteUrl, parseInviteLink } from "./link";

describe("parseInviteLink", () => {
  it("reads the secret, optional token and name", () => {
    expect(parseInviteLink("#pair=v1.abc.def")).toEqual({ secret: "v1.abc.def", token: null, name: null });
    expect(parseInviteLink("#pair=v1.abc.def&token=s%203cret%26x&name=Anna+%26+me")).toEqual({
      secret: "v1.abc.def",
      token: "s 3cret&x",
      name: "Anna & me",
    });
  });

  it("ignores other fragments", () => {
    expect(parseInviteLink("")).toBeNull();
    expect(parseInviteLink("#settings")).toBeNull();
    expect(parseInviteLink("#pair=")).toBeNull();
  });
});

describe("inviteLinkFromCode", () => {
  const origin = "https://clip.example.com";

  it("accepts links for this relay", () => {
    expect(inviteLinkFromCode(` ${origin}/#pair=v1.abc.def&token=t\n`, origin)).toEqual({
      secret: "v1.abc.def",
      token: "t",
      name: null,
    });
  });

  it("rejects other codes and other relays", () => {
    expect(inviteLinkFromCode("hello", origin)).toEqual({ error: "That's not a YACS invite." });
    expect(inviteLinkFromCode(`${origin}/#settings`, origin)).toEqual({ error: "That's not a YACS invite." });
    expect(inviteLinkFromCode("https://other.example.com/#pair=v1.abc.def", origin)).toEqual({
      error: "That invite is for other.example.com. Open YACS there to join with it.",
    });
  });
});

describe("inviteUrl", () => {
  it("makes links the apps read", () => {
    const link = { secret: "v1.abc.def", token: "s 3cret&x", name: "Anna & me" };
    const url = inviteUrl(link, "https://clip.example.com/");
    // The same as `InviteLink::to_url` in yacs-client.
    expect(url).toBe("https://clip.example.com/#pair=v1.abc.def&token=s+3cret%26x&name=Anna+%26+me");
    expect(inviteLinkFromCode(url, "https://clip.example.com")).toEqual(link);
    expect(inviteUrl({ secret: "v1.a.b", token: null, name: null }, "https://c.example/yacs/")).toBe(
      "https://c.example/yacs/#pair=v1.a.b",
    );
  });
});
