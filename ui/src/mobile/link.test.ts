import { describe, expect, it } from "vitest";
import { desktopOs, guessDeviceName, inviteLinkFromCode, inviteUrl, looksLikeCode, parseInviteLink } from "./link";

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

describe("desktopOs", () => {
  const ua = {
    mac: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
    windows: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
    linux: "Mozilla/5.0 (X11; Linux x86_64; rv:143.0) Gecko/20100101 Firefox/143.0",
    android: "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36",
    iphone: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
    chromebook: "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
  };

  it("tells computers from phones", () => {
    expect(desktopOs(ua.mac)).toBe("macos");
    expect(desktopOs(ua.windows)).toBe("windows");
    expect(desktopOs(ua.linux)).toBe("linux");
    expect(desktopOs(ua.android)).toBeNull();
    expect(desktopOs(ua.iphone)).toBeNull();
    expect(desktopOs(ua.chromebook)).toBeNull();
  });

  it("names a computer's browser after its system", () => {
    expect(guessDeviceName(ua.mac)).toBe("Mac (browser)");
    expect(guessDeviceName(ua.windows)).toBe("Windows (browser)");
    expect(guessDeviceName(ua.android)).toBe("Android");
  });
});
