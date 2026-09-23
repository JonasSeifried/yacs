import type { Os } from "./types";

const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock", "Fn"]);

/**
 * Turns a key press into an accelerator the Rust side can register, e.g.
 * `Super+Shift+Space`. Uses `event.code` (the physical key), so the shortcut
 * stays the same across keyboard layouts.
 *
 * Returns `null` for presses that can't be a global shortcut: a bare modifier,
 * or a key without Ctrl/Alt/Cmd (that would swallow normal typing).
 */
export function acceleratorFromEvent(e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): string | null {
  if (MODIFIER_KEYS.has(e.key) || !e.code) return null;
  if (!e.ctrlKey && !e.altKey && !e.metaKey) return null;
  const parts: string[] = [];
  if (e.metaKey) parts.push("Super");
  if (e.ctrlKey) parts.push("Control");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  parts.push(e.code);
  return parts.join("+");
}

const MAC_SYMBOLS: Record<string, string> = {
  commandorcontrol: "⌘",
  cmdorctrl: "⌘",
  super: "⌘",
  command: "⌘",
  cmd: "⌘",
  control: "⌃",
  ctrl: "⌃",
  alt: "⌥",
  option: "⌥",
  shift: "⇧",
};

const WINDOWS_NAMES: Record<string, string> = {
  commandorcontrol: "Ctrl",
  cmdorctrl: "Ctrl",
  super: "Win",
  command: "Win",
  cmd: "Win",
  control: "Ctrl",
  ctrl: "Ctrl",
  alt: "Alt",
  option: "Alt",
  shift: "Shift",
};

const KEY_NAMES: Record<string, string> = {
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Escape: "Esc",
};

function keyLabel(code: string): string {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  return KEY_NAMES[code] ?? code;
}

/** Human form: `⌘⇧Space` on macOS, `Ctrl+Shift+Space` elsewhere. */
export function formatAccelerator(accelerator: string, os: Os): string {
  const parts = accelerator.split("+").filter(Boolean);
  const names = os === "macos" ? MAC_SYMBOLS : WINDOWS_NAMES;
  const labels = parts.map((p, i) =>
    i === parts.length - 1 && !(p.toLowerCase() in names) ? keyLabel(p) : (names[p.toLowerCase()] ?? p),
  );
  return os === "macos" ? labels.join("") : labels.join("+");
}

/** How to write the platform's main modifier in hints, e.g. "⌘," vs "Ctrl+,". */
export function modKey(os: Os, key: string): string {
  return os === "macos" ? `⌘${key}` : `Ctrl+${key}`;
}

export function guessOs(): Os {
  const ua = navigator.userAgent;
  if (ua.includes("Mac")) return "macos";
  if (ua.includes("Windows")) return "windows";
  return "linux";
}
