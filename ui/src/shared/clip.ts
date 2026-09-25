import DOMPurify from "dompurify";
import type { ClipView, FileInfo } from "./types";

const TITLE_CHARS = 120;

/** Images every receiver can show; `yacs_core::File::is_image`. */
export function isImageMime(mime: string): boolean {
  return ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(mime);
}

/** One line for the history list. */
export function clipTitle(clip: ClipView): string {
  const text = clip.text?.replace(/\s+/g, " ").trim();
  if (clip.files.length) return capped(filesTitle(clip.files));
  if (text) return capped(text);
  if (clip.image) {
    const { width, height } = clip.image;
    return width && height ? `Image · ${width}×${height}` : "Image";
  }
  if (clip.html || clip.rtf) return "Formatted text";
  return "Empty clip";
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
    return clip.html !== null || clip.rtf ? "formatted" : "text";
  }
  if (clip.image) return "image";
  return clip.html !== null || clip.rtf ? "formatted" : "text";
}

export type PreviewKind = "files" | "html" | "text" | "image" | "none";

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
export function previewDocument(html: string): string {
  if (!DOMPurify.isSupported) throw new Error("HTML sanitizer unavailable");
  const body = DOMPurify.sanitize(html, { FORBID_TAGS: ["form", "input", "button", "textarea", "select"] });
  return `<!doctype html>
<html><head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">
<style>
html { color-scheme: light; }
body { margin: 14px 16px; font: 13px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; color: #1c1b22; background: #fff; overflow-wrap: anywhere; }
img, video { max-width: 100%; height: auto; }
img:not([src^="data:"]) { display: none; } /* blocked remote images */
table { border-collapse: collapse; }
pre { white-space: pre-wrap; }
</style>
</head><body>${body}</body></html>`;
}
