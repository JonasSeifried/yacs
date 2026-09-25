// The browser clipboard. Browsers only offer text/plain, text/html and
// image/png, so RTF from a desktop clip is left out when copying here, and
// files are saved or shared instead.

import { isImageMime } from "../shared/clip";
import { isIos } from "./link";
import type { Clip, ClipItem } from "../shared/types";

const CLIPBOARD_NEEDS_HTTPS =
  "The clipboard only works on an https:// address. Open YACS through your relay's HTTPS URL.";

/**
 * Put every format the browser supports on the clipboard. Must be called
 * straight from a tap: Safari only allows clipboard writes in the handler of
 * a user gesture, so nothing may be awaited before `clipboard.write`.
 */
export async function copyClip(clip: Clip): Promise<void> {
  const text = pick(clip, "Text");
  const html = pick(clip, "Html");
  const image = pick(clip, "Image") ?? imageFile(clip);

  if (!navigator.clipboard) throw new Error(CLIPBOARD_NEEDS_HTTPS);
  if (!navigator.clipboard.write || typeof ClipboardItem === "undefined") {
    if (text === undefined) throw new Error("This browser can only copy plain text.");
    return navigator.clipboard.writeText(text);
  }

  const record: Record<string, Blob | Promise<Blob>> = {};
  if (text !== undefined) record["text/plain"] = new Blob([text], { type: "text/plain" });
  if (html !== undefined) record["text/html"] = new Blob([html], { type: "text/html" });
  if (image) {
    const blob = new Blob([image.data as Uint8Array<ArrayBuffer>], { type: image.mime });
    record["image/png"] = image.mime === "image/png" ? blob : toPng(blob);
  }
  if (Object.keys(record).length === 0) {
    if (pick(clip, "File")) throw new Error("Files can't go on this clipboard. Use Save / Share.");
    throw new Error("This clip only has rich text (RTF), which browsers can't copy.");
  }
  await navigator.clipboard.write([new ClipboardItem(record)]);
}

/** Everything useful on the clipboard. iOS asks for permission with its own "Paste" button. */
export async function readClipboard(): Promise<ClipItem[]> {
  if (!navigator.clipboard) throw new Error(CLIPBOARD_NEEDS_HTTPS);
  const items: ClipItem[] = [];
  try {
    if (!navigator.clipboard.read) {
      const text = await navigator.clipboard.readText();
      return text ? [{ Text: text }] : [];
    }
    for (const entry of await navigator.clipboard.read()) {
      const types = entry.types;
      if (types.includes("text/plain")) items.push({ Text: await (await entry.getType("text/plain")).text() });
      if (types.includes("text/html")) items.push({ Html: await (await entry.getType("text/html")).text() });
      const imageType = types.find((t) => t.startsWith("image/"));
      if (imageType) items.push(await imageItem(await entry.getType(imageType)));
      if (items.length) break; // one copy action; more entries are rare and not meant together
    }
  } catch (e) {
    if (e instanceof DOMException && e.name === "NotAllowedError") {
      throw new Error("Clipboard access was denied. Allow it, or type or pick what to send.");
    }
    throw e;
  }
  return items.filter((i) => !("Text" in i) || i.Text.length > 0);
}

/** Whether Copy can put anything on a browser's clipboard. */
export function canCopy(clip: Clip): boolean {
  return clip.items.some((i) => "Text" in i || "Html" in i || "Image" in i) || imageFile(clip) !== undefined;
}

/** A single image file can be copied as the picture. */
function imageFile(clip: Clip) {
  const files = clip.items.filter((i) => "File" in i);
  return files.length === 1 && isImageMime(files[0].File.mime) ? files[0].File : undefined;
}

export async function imageItem(blob: Blob): Promise<ClipItem> {
  return { Image: { mime: blob.type || "image/png", data: new Uint8Array(await blob.arrayBuffer()) } };
}

/** Picked or shared files: images as pictures (to paste anywhere), the rest as files. */
export async function fileItem(file: File): Promise<ClipItem> {
  if (isImageMime(file.type)) return imageItem(file);
  const data = new Uint8Array(await file.arrayBuffer());
  return { File: { name: file.name || "file", mime: file.type || "application/octet-stream", data } };
}

export function canSave(clip: Clip): boolean {
  return clip.items.some((i) => "File" in i || "Image" in i || "Stream" in i);
}

/** The clip's files, or its image, as files to save. Copies the bytes: build them on demand. */
export function savable(clip: Clip, name: string): File[] {
  const files = clip.items.flatMap((i) =>
    "File" in i ? [new File([i.File.data as Uint8Array<ArrayBuffer>], i.File.name, { type: i.File.mime })] : [],
  );
  if (files.length) return files;
  const image = pick(clip, "Image");
  if (!image) return [];
  const ext = image.mime.split("/")[1]?.replace("jpeg", "jpg") ?? "png";
  return [new File([image.data as Uint8Array<ArrayBuffer>], `${name}.${ext}`, { type: image.mime })];
}

/**
 * On iOS the share sheet (it's how you save to Photos or Files), elsewhere a
 * download, which is what Android users expect; Chrome on Android also
 * refuses to share many file types ("Permission denied"), and then it's a
 * download too. Resolves to whether the share sheet is done with them.
 */
export async function shareFiles(files: File[]): Promise<boolean> {
  if (files.length === 0) return true;
  if (isIos() && navigator.canShare?.({ files })) {
    try {
      await navigator.share({ files });
      return true;
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return true;
      if (!(e instanceof DOMException && e.name === "NotAllowedError")) throw e;
    }
  }
  for (const file of files) {
    const url = URL.createObjectURL(file);
    const a = Object.assign(document.createElement("a"), { href: url, download: file.name });
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  }
  return false;
}

type Kind = "Text" | "Html" | "Rtf" | "Image" | "File" | "Stream";
type Value<K extends Kind> = Extract<ClipItem, Record<K, unknown>>[K];

export function pick<K extends Kind>(clip: Clip, kind: K): Value<K> | undefined {
  for (const item of clip.items) if (kind in item) return (item as unknown as Record<K, Value<K>>)[kind];
  return undefined;
}

async function toPng(blob: Blob): Promise<Blob> {
  const bitmap = await createImageBitmap(blob);
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext("2d")!.drawImage(bitmap, 0, 0);
  return new Promise((resolve, reject) =>
    canvas.toBlob((png) => (png ? resolve(png) : reject(new Error("can't convert the image"))), "image/png"),
  );
}
