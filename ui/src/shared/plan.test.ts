import { describe, expect, it } from "vitest";
import { describeLimits, isPublicRelay, maxTtlSecs } from "./plan";
import type { ServerConfig, SpaceLimits } from "./types";

const free: SpaceLimits = {
  plan: "free",
  default_ttl_secs: 900,
  max_ttl_secs: 3600,
  max_clip_bytes: 10_000_000,
  daily_transfer_bytes: 500_000_000,
  transfer_used_bytes: 0,
  max_clips: 50,
};

describe("plan", () => {
  it("knows the free relay however it's written", () => {
    expect(isPublicRelay("https://yacs-relay.jonasseifried.com/")).toBe(true);
    expect(isPublicRelay(" https://yacs-relay.jonasseifried.com ")).toBe(true);
    expect(isPublicRelay("https://clip.example.com")).toBe(false);
  });

  it("takes the space's TTL over the relay's", () => {
    const config = { max_ttl_secs: 86_400 } as ServerConfig;
    expect(maxTtlSecs(config, free)).toBe(3600);
    expect(maxTtlSecs(config, null)).toBe(86_400);
    expect(maxTtlSecs(null, null)).toBeUndefined();
  });

  it("describes only limits worth saying", () => {
    expect(describeLimits(free)).toBe("clips up to 10.0 MB, kept up to 1 h, 500.0 MB a day");
    expect(describeLimits({ ...free, plan: "unlimited" })).toBeNull();
    expect(describeLimits(null)).toBeNull();
  });
});
