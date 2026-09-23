/** Durations and sizes for list rows: "2 min", "3 h", "85 B". */

export const TTL_OPTIONS = [
  { secs: 5 * 60, label: "5 minutes" },
  { secs: 15 * 60, label: "15 minutes" },
  { secs: 60 * 60, label: "1 hour" },
  { secs: 8 * 60 * 60, label: "8 hours" },
  { secs: 24 * 60 * 60, label: "24 hours" },
  { secs: 7 * 24 * 60 * 60, label: "7 days" },
];

/** Options the server accepts. Before the server is known, assume its default max (24 h). */
export function ttlOptions(maxTtlSecs = 24 * 60 * 60) {
  return TTL_OPTIONS.filter((o) => o.secs <= maxTtlSecs);
}

/**
 * Options for an expiry dropdown. A saved value that isn't a preset (or is
 * above the server's max, which the server clamps) is kept as its own entry
 * rather than silently changed.
 */
export function ttlChoices(current: number, maxTtlSecs?: number) {
  const options = ttlOptions(maxTtlSecs);
  if (!options.some((o) => o.secs === current)) {
    options.push({ secs: current, label: formatDuration(current * 1000) });
    options.sort((a, b) => a.secs - b.secs);
  }
  return options;
}

export function formatDuration(ms: number): string {
  const secs = Math.max(0, Math.round(ms / 1000));
  if (secs < 60) return `${secs} s`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min`;
  if (secs < 86_400) return `${Math.floor(secs / 3600)} h`;
  return `${Math.floor(secs / 86_400)} d`;
}

export function formatSize(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1_000_000) return `${(bytes / 1000).toFixed(1)} KB`;
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}
