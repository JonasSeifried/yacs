/** Compares `major.minor.patch` versions; anything after `-` or `+` is ignored. */
export function compareVersions(a: string, b: string): number {
  const parts = (v: string) => v.split(/[-+]/)[0].split(".").map((n) => Number.parseInt(n, 10) || 0);
  const [pa, pb] = [parts(a), parts(b)];
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return Math.sign(diff);
  }
  return 0;
}

/**
 * Whether the relay is older than the newest release. `relay` is undefined
 * for relays before 0.2.0, which didn't report their version.
 */
export function relayBehind(relay: string | undefined, latest: string): boolean {
  return relay === undefined || compareVersions(relay, latest) < 0;
}
