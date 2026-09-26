import { formatDuration, formatSize } from "./time";
import type { ServerConfig, SpaceLimits } from "./types";

/** The relay YACS's author runs, free for anyone (`yacs_client::PUBLIC_RELAY`). */
export const PUBLIC_RELAY = "https://yacs-relay.jonasseifried.com";

export function isPublicRelay(url: string): boolean {
  return url.trim().replace(/\/+$/, "") === PUBLIC_RELAY;
}

/** Longest TTL the space takes: its plan's, or the relay's before 0.5.0. */
export function maxTtlSecs(config: ServerConfig | null, limits: SpaceLimits | null): number | undefined {
  return limits?.max_ttl_secs ?? config?.max_ttl_secs;
}

/** "clips up to 10.0 MB, kept up to 1 h, 500.0 MB a day", or null without limits worth saying. */
export function describeLimits(limits: SpaceLimits | null): string | null {
  if (!limits || limits.plan !== "free") return null;
  const parts = [
    limits.max_clip_bytes !== undefined && `clips up to ${formatSize(limits.max_clip_bytes)}`,
    `kept up to ${formatDuration(limits.max_ttl_secs * 1000)}`,
    limits.daily_transfer_bytes !== undefined && `${formatSize(limits.daily_transfer_bytes)} a day`,
  ].filter(Boolean);
  return parts.join(", ");
}
