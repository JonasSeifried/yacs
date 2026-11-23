// Picking several clips, and which clips this device copied (and when), so
// the list can mark them. Marks last while the relay lists the clip.

const KEY = "yacs.copied";

export type Copied = Record<string, number>;

export function loadCopied(): Copied {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return parsed && typeof parsed === "object" ? (parsed as Copied) : {};
  } catch {
    return {};
  }
}

export function saveCopied(copied: Copied) {
  try {
    localStorage.setItem(KEY, JSON.stringify(copied));
  } catch {
    // Not remembered past this session; the marks still show.
  }
}

/** `copied` with `ids` marked at `now`. */
export function markCopied(copied: Copied, ids: string[], now: number): Copied {
  return { ...copied, ...Object.fromEntries(ids.map((id) => [id, now])) };
}

/** Only clips the relay still lists: the rest expired or were deleted. */
export function pruneCopied(copied: Copied, listed: string[]): Copied {
  const keep = new Set(listed);
  return Object.fromEntries(Object.entries(copied).filter(([id]) => keep.has(id)));
}

/**
 * The clips picked after a click: ⌘/Ctrl toggles one, Shift adds the range
 * from `anchor`, a plain click picks just that one. In click order.
 */
export function pickAfterClick(
  picked: string[],
  order: string[],
  anchor: string | null,
  id: string,
  how: "toggle" | "range" | "only",
): string[] {
  if (how === "only") return [id];
  // Picking a second clip keeps the one that was selected.
  const base = picked.length > 0 ? picked : anchor && anchor !== id ? [anchor] : [];
  if (how === "toggle") return base.includes(id) ? base.filter((x) => x !== id) : [...base, id];
  const from = anchor ? order.indexOf(anchor) : -1;
  const to = order.indexOf(id);
  if (from < 0 || to < 0) return [...base.filter((x) => x !== id), id];
  const range = order.slice(Math.min(from, to), Math.max(from, to) + 1);
  if (from > to) range.reverse();
  return [...base.filter((x) => !range.includes(x)), ...range];
}

/** Texts a line apart, as the desktop app joins them (`clipboard::combine`). */
export function joinTexts(texts: string[]): string {
  let joined = "";
  for (const text of texts) {
    if (joined && !joined.endsWith("\n")) joined += "\n";
    joined += text;
  }
  return joined;
}

/**
 * Shift+arrow from `from` to `to`: picks `to`, or, going straight back over
 * the last pick, unpicks `from`. Moving onto a clip picked earlier keeps
 * every pick.
 */
export function stepPick(picked: string[], from: string, to: string): string[] {
  const base = picked.length > 0 ? picked : [from];
  if (base.at(-1) === from && base.at(-2) === to) return base.slice(0, -1);
  return base.includes(to) ? base : [...base, to];
}
