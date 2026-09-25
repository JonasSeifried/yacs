// Downloads one file of a chunked clip, off the page's thread: chunks are
// fetched a few ahead, opened in WASM and handed on in order to
// - "port": the service worker, which streams them to the browser's downloads
//   (a pull at a time, so nothing piles up in memory),
// - "opfs": a file in the origin private file system (iOS: the page shares it
//   from there, since a multi-GB Blob would get the tab killed),
// - "memory": a Blob, when neither is available.

import init, { Pairing } from "../wasm/yacs";
import { API, errorText, relayRequest, retrying } from "../platform/relay";
import { fileChunks, fileSlice } from "../shared/stream";
import type { StreamInfo } from "../shared/types";

export type DownloadMode = "port" | "opfs" | "memory";

export interface DownloadRequest {
  type: "start";
  secret: string;
  token: string | null;
  clipId: string;
  stream: StreamInfo;
  file: number;
  mode: DownloadMode;
  /** "port": to the service worker. */
  port?: MessagePort;
}

export type DownloadMessage =
  | { type: "progress"; done: number; total: number }
  /** "port": the browser has it. "opfs": in `OPFS_DIR` under `name`. "memory": `blob`. */
  | { type: "done"; name?: string; blob?: Blob }
  | { type: "error"; message: string };

/** Where downloads wait in the origin private file system until they're shared. */
export const OPFS_DIR = "yacs-downloads";
const AHEAD = 3;

const scope = self as unknown as {
  onmessage: ((e: MessageEvent<DownloadRequest | { type: "cancel" }>) => void) | null;
  postMessage(message: DownloadMessage): void;
};
const controller = new AbortController();

scope.onmessage = (e) => {
  if (e.data.type === "cancel") return controller.abort();
  const req = e.data;
  download(req, controller.signal).then(
    (done) => scope.postMessage(done),
    (err) => {
      const message = controller.signal.aborted ? "Download cancelled." : errorText(err);
      req.port?.postMessage({ type: "error", message });
      scope.postMessage({ type: "error", message });
    },
  );
};

interface Sink {
  write(bytes: Uint8Array): Promise<void>;
  close(): Promise<DownloadMessage>;
}

async function download(req: DownloadRequest, signal: AbortSignal): Promise<DownloadMessage> {
  await init();
  const pairing = Pairing.fromSecret(req.secret);
  const cipher = pairing.streamCipher(req.stream);
  const url = `${API}/channels/${pairing.channelId}/clips/${encodeURIComponent(req.clipId)}/chunks`;
  pairing.free();

  const { stream, file } = req;
  const sink = await openSink(req, signal);
  const [first, end] = fileChunks(stream, file);
  const total = stream.files[file].size;
  const fetchChunk = (index: number) =>
    retrying(async () => {
      const res = await relayRequest(`${url}/${index}`, req.token, { signal });
      return new Uint8Array(await res.arrayBuffer());
    }, signal).then((sealed) => cipher.open(index, sealed));

  const ahead: Promise<Uint8Array>[] = [];
  let next = first;
  let done = 0;
  scope.postMessage({ type: "progress", done, total });
  try {
    for (let index = first; index < end; index++) {
      while (ahead.length < AHEAD && next < end) {
        const chunk = fetchChunk(next++);
        chunk.catch(() => {}); // awaited below; don't report it twice
        ahead.push(chunk);
      }
      const plain = await ahead.shift()!;
      const [from, to] = fileSlice(stream, file, index);
      await sink.write(plain.subarray(from, to));
      done += to - from;
      scope.postMessage({ type: "progress", done, total });
    }
    return await sink.close();
  } finally {
    cipher.free();
  }
}

async function openSink(req: DownloadRequest, signal: AbortSignal): Promise<Sink> {
  if (req.mode === "port" && req.port) return portSink(req.port, signal);
  if (req.mode === "opfs") {
    const sink = await opfsSink(req).catch(() => null);
    if (sink) return sink;
  }
  return memorySink(req);
}

/** One piece per pull from the service worker's stream. */
function portSink(port: MessagePort, signal: AbortSignal): Sink {
  let credits = 0;
  let wake: (() => void) | null = null;
  port.onmessage = ({ data }) => {
    if (data?.type === "pull") credits++;
    // The browser's download was cancelled.
    if (data?.type === "cancel") controller.abort();
    wake?.();
    wake = null;
  };
  const credit = async () => {
    while (credits === 0) {
      if (signal.aborted) throw new Error("Download cancelled.");
      await new Promise<void>((resolve) => (wake = resolve));
    }
    credits--;
  };
  return {
    async write(bytes) {
      if (bytes.length === 0) return;
      await credit();
      const copy = bytes.slice();
      port.postMessage({ type: "data", bytes: copy }, [copy.buffer]);
    },
    async close() {
      port.postMessage({ type: "end" });
      return { type: "done" };
    },
  };
}

interface SyncAccessHandle {
  write(bytes: Uint8Array, options: { at: number }): number;
  truncate(size: number): void;
  flush(): void;
  close(): void;
}

async function opfsSink(req: DownloadRequest): Promise<Sink> {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(OPFS_DIR, { create: true });
  const name = `${req.clipId}-${req.file}`;
  const handle = await dir.getFileHandle(name, { create: true });
  const access = await (handle as unknown as { createSyncAccessHandle(): Promise<SyncAccessHandle> }).createSyncAccessHandle();
  access.truncate(0);
  let at = 0;
  return {
    async write(bytes) {
      at += access.write(bytes, { at });
    },
    async close() {
      access.flush();
      access.close();
      return { type: "done", name };
    },
  };
}

/** Beyond this, a Blob in memory would likely get the tab killed. */
const MEMORY_MAX = 1024 * 1024 * 1024;

function memorySink(req: DownloadRequest): Sink {
  if (req.stream.files[req.file].size > MEMORY_MAX) {
    throw new Error("This browser can't save a file this big. Try Chrome, or YACS on a computer.");
  }
  const parts: Uint8Array<ArrayBuffer>[] = [];
  return {
    async write(bytes) {
      parts.push(bytes.slice());
    },
    async close() {
      return { type: "done", blob: new Blob(parts, { type: req.stream.files[req.file].mime }) };
    },
  };
}
