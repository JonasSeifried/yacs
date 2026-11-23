import DOMPurify from "dompurify";
import type { ClipView, FileInfo } from "./types";

const TITLE_CHARS = 120;

/** Images every receiver can show; `yacs_core::File::is_image`. */
export function isImageMime(mime: string): boolean {
  return ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(mime);
}

/** One line for the history list. */
export function clipTitle(clip: ClipView): string {
  // Formatted text's plain version is often Markdown (chat apps): the title
  // shows what the formatting shows, without the syntax. Not code, though,
  // where `__init__` and `# comment` mean what they say. Only the start
  // matters, and long text would make the patterns slow.
  const head = clip.text?.slice(0, TITLE_CHARS * 4);
  const markdown = head && clip.html !== null && !CODE_HTML.test(clip.html);
  const plain = markdown ? stripMarkdown(head) : head;
  const text = plain?.replace(/\s+/g, " ").trim();
  if (clip.files.length) return capped(filesTitle(clip.files));
  if (text) return capped(text);
  if (clip.image) {
    const { width, height } = clip.image;
    return width && height ? `Image · ${width}×${height}` : "Image";
  }
  if (clip.hasHtml || clip.rtf) return "Formatted text";
  return "Empty clip";
}

/** HTML from an editor or a code block: monospace, kept as it is. */
const CODE_HTML = /<pre[\s>]|<code[\s>]|white-space:\s*pre/i;

/**
 * The Markdown marks a title doesn't need: emphasis, code, links, and
 * headings, quotes and bullets at the start of a line. Marks have to hug
 * their text and not sit inside a word, so `**kwargs`, `x**2` and `2 * 3 * 4`
 * stay as they are. Underscores always stay (`__init__.py`), and so do
 * numbered lists ("10. Oktober").
 * No lookbehind: Safari before 16.4 can't parse it.
 */
export function stripMarkdown(text: string): string {
  return text
    .replace(/^[ \t]*(#{1,6}[ \t]+|>[ \t]?|[-*+][ \t]+(\[[ xX]\][ \t]+)?)/gm, "")
    .replace(/!?\[([^\]\n]+)\]\((?:[^()\s]|\([^()\s]*\))+\)/g, "$1")
    .replace(/(^|[^\w*])\*\*(?=\S)([^\n]*?\S)\*\*(?![\w*])/gm, "$1$2")
    .replace(/(^|[^\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])/gm, "$1$2")
    .replace(/(^|[^\w~])~~(?=\S)([^\n]*?\S)~~(?![\w~])/gm, "$1$2")
    .replace(/`([^`\n]+)`/g, "$1");
}

function filesTitle(files: FileInfo[]): string {
  if (files.length === 1) return files[0].name;
  return `${files.length} files: ${files.map((f) => f.name).join(", ")}`;
}

function capped(text: string): string {
  return text.length > TITLE_CHARS ? `${text.slice(0, TITLE_CHARS)}…` : text;
}

export type ClipIcon = "text" | "link" | "formatted" | "image" | "file";

/** The small icon in front of a clip in the history list. */
export function clipIcon(clip: ClipView): ClipIcon {
  if (clip.files.length) return clip.files.length === 1 && isImageMime(clip.files[0].mime) ? "image" : "file";
  const text = clip.text?.trim();
  if (text) {
    if (/^https?:\/\/\S+$/i.test(text)) return "link";
    return clip.hasHtml || clip.rtf ? "formatted" : "text";
  }
  if (clip.image) return "image";
  return clip.hasHtml || clip.rtf ? "formatted" : "text";
}

export type PreviewKind = "files" | "html" | "text" | "image" | "none";

/** As in the desktop's previews (`clips.rs`): text beyond this isn't needed for one. */
export const PREVIEW_TEXT_CHARS = 20_000;
/** Rendering huge HTML is slow; plain text is shown instead. */
export const PREVIEW_HTML_BYTES = 512 * 1024;

/** `text` cut to `max` characters (code points, like Rust's `chars`), and whether it was. */
export function cutText(text: string, max = PREVIEW_TEXT_CHARS): { text: string; cut: boolean } {
  let units = 0;
  let chars = 0;
  for (const ch of text) {
    if (chars === max) return { text: text.slice(0, units), cut: true };
    units += ch.length;
    chars++;
  }
  return { text, cut: false };
}

/** At most `PREVIEW_HTML_BYTES` in UTF-8, as the desktop counts it. */
export function htmlFitsPreview(html: string): boolean {
  // Every UTF-16 unit is at least one byte, so a longer string can't fit.
  return html.length <= PREVIEW_HTML_BYTES && new TextEncoder().encode(html).length <= PREVIEW_HTML_BYTES;
}

/**
 * What the preview shows. Files win: someone attached them on purpose.
 * Text wins over the image: an image next to text is
 * usually a rendering of it (Word), while a copied picture from a browser
 * comes with an `<img>` tag pointing at a URL that is never loaded.
 */
export function previewKind(clip: ClipView): PreviewKind {
  // Without a working sanitizer, HTML is never rendered.
  const html = clip.html !== null && DOMPurify.isSupported;
  if (clip.files.length) return "files";
  if (clip.text?.trim()) return html ? "html" : "text";
  if (clip.image) return "image";
  if (html) return "html";
  return "none";
}

/**
 * A complete document for a sandboxed `<iframe srcdoc>`. HTML comes from
 * another device, so it's sanitized, and its own CSP blocks every network
 * request: remote images or fonts would reveal to their host that the clip
 * was viewed.
 */
export function previewDocument(html: string, fontSize = 13): string {
  if (!DOMPurify.isSupported) throw new Error("HTML sanitizer unavailable");
  const body = DOMPurify.sanitize(html, { FORBID_TAGS: ["form", "input", "button", "textarea", "select"] });
  return `<!doctype html>
<html><head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">
<style>
html { color-scheme: light; }
body { margin: 14px 16px; font: ${fontSize}px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; color: #1c1b22; background: #fff; overflow-wrap: anywhere; display: flow-root; }
img, video { max-width: 100%; height: auto; }
img:not([src^="data:"]) { display: none; } /* blocked remote images */
table { border-collapse: collapse; }
pre { white-space: pre-wrap; }
</style>
</head><body>${body}</body></html>`;
}
