// Uploads a clip's big files as chunks, off the page's thread: each chunk is
// read from the picked files, sealed in WASM and PUT on its own (Safari can't
// stream request bodies). Only a few chunks are in memory at once.

import init, { Pairing } from "../wasm/yacs";
import { API, errorText, relayRequest, retrying } from "../platform/relay";
import { CHUNK_TAG_LEN, chunkCount, chunkLen, sealedLen, streamTotal } from "../shared/stream";
import type { Clip, ClipMeta } from "../shared/types";

export interface UploadRequest {
  type: "start";
  secret: string;
  token: string | null;
  /** The header: carries the `Stream` item describing `files`. */
  clip: Clip;
  files: File[];
  ttlSecs: number;
}

export type UploadMessage =
  | { type: "progress"; done: number; total: number }
  | { type: "done"; meta: ClipMeta }
  | { type: "error"; message: string };

const IN_FLIGHT = 3;

const scope = self as unknown as {
  onmessage: ((e: MessageEvent<UploadRequest | { type: "cancel" }>) => void) | null;
  postMessage(message: UploadMessage): void;
};
const controller = new AbortController();

scope.onmessage = (e) => {
  if (e.data.type === "cancel") return controller.abort();
  upload(e.data, controller.signal).then(
    (meta) => scope.postMessage({ type: "done", meta }),
    (err) => scope.postMessage({ type: "error", message: controller.signal.aborted ? "Upload cancelled." : errorText(err) }),
  );
};

async function upload(req: UploadRequest, signal: AbortSignal): Promise<ClipMeta> {
  await init();
  const pairing = Pairing.fromSecret(req.secret);
  const stream = req.clip.items.flatMap((i) => ("Stream" in i ? [i.Stream] : []))[0];
  if (!stream) throw new Error("This clip has no files to upload.");
  const cipher = pairing.streamCipher(stream);
  const base = `${API}/channels/${pairing.channelId}/uploads`;
  const header = pairing.seal(req.clip) as Uint8Array<ArrayBuffer>;
  pairing.free();

  const query = new URLSearchParams({
    ttl: String(Math.max(1, Math.round(req.ttlSecs))),
    length: String(sealedLen(stream)),
    chunk_size: String(stream.chunk_size + CHUNK_TAG_LEN),
  });
  const start = { method: "POST", headers: { "content-type": "application/octet-stream" }, body: header, signal };
  let created: Response;
  try {
    created = await relayRequest(`${base}?${query}`, req.token, start);
  } catch (e) {
    if ((e as { status?: number }).status === 429) throw new Error("Too many uploads are running; try again when one is done.");
    throw e;
  }
  const { id } = (await created.json()) as { id: string };

  try {
    const total = streamTotal(stream);
    const count = chunkCount(stream);
    let next = 0;
    let done = 0;
    scope.postMessage({ type: "progress", done, total });
    const lane = async () => {
      while (next < count && !signal.aborted) {
        const index = next++;
        const len = chunkLen(stream, index);
        const plain = await read(req.files, index * stream.chunk_size, len);
        // Sealed once and kept for retries: sealing again could reuse a nonce.
        const sealed = cipher.seal(index, plain) as Uint8Array<ArrayBuffer>;
        await retrying(
          () => relayRequest(`${base}/${id}/chunks/${index}`, req.token, { method: "PUT", body: sealed, signal }),
          signal,
        );
        done += len;
        scope.postMessage({ type: "progress", done, total });
      }
    };
    await Promise.all(Array.from({ length: IN_FLIGHT }, lane));
    if (signal.aborted) throw new Error("Upload cancelled.");
    const res = await relayRequest(`${base}/${id}/complete`, req.token, { method: "POST", signal });
    return (await res.json()) as ClipMeta;
  } catch (e) {
    // Frees the relay's quota now rather than in a day. Awaited: the page
    // ends this worker once it hears about the error.
    await relayRequest(`${base}/${id}`, req.token, { method: "DELETE", signal: AbortSignal.timeout(5000) }).catch(() => {});
    throw e;
  } finally {
    cipher.free();
  }
}

/** `len` bytes of the files one after another, from `offset`. */
async function read(files: File[], offset: number, len: number): Promise<Uint8Array> {
  const parts: Blob[] = [];
  let left = len;
  for (const file of files) {
    if (left === 0) break;
    if (offset >= file.size) {
      offset -= file.size;
      continue;
    }
    const take = Math.min(left, file.size - offset);
    parts.push(file.slice(offset, offset + take));
    left -= take;
    offset = 0;
  }
  const bytes = new Uint8Array(await new Blob(parts).arrayBuffer());
  if (bytes.length !== len) throw new Error("A file changed while it was being sent.");
  return bytes;
}
