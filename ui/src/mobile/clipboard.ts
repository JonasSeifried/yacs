// The browser clipboard. Browsers only offer text/plain, text/html and
// image/png, so RTF from a desktop clip is left out when copying here.

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
  const image = pick(clip, "Image");

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
  if (Object.keys(record).length === 0) throw new Error("This clip only has rich text (RTF), which browsers can't copy.");
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

export async function imageItem(blob: Blob): Promise<ClipItem> {
  return { Image: { mime: blob.type || "image/png", data: new Uint8Array(await blob.arrayBuffer()) } };
}

/** Share sheet where available (save to Photos, send to an app), else a download. */
export async function shareImage(clip: Clip, name: string) {
  const image = pick(clip, "Image");
  if (!image) return;
  const ext = image.mime.split("/")[1]?.replace("jpeg", "jpg") ?? "png";
  const file = new File([image.data as Uint8Array<ArrayBuffer>], `${name}.${ext}`, { type: image.mime });
  if (navigator.canShare?.({ files: [file] })) {
    try {
      await navigator.share({ files: [file] });
    } catch (e) {
      if (!(e instanceof DOMException && e.name === "AbortError")) throw e;
    }
    return;
  }
  const url = URL.createObjectURL(file);
  const a = Object.assign(document.createElement("a"), { href: url, download: file.name });
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

type Kind = "Text" | "Html" | "Rtf" | "Image";
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
