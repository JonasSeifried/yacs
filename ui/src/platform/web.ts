// The PWA's backend: crypto in WASM (`yacs-wasm`, the same Rust code the
// desktop runs), network via fetch to the relay that served the page.
// Spaces are kept in localStorage; the page's CSP allows no third-party
// scripts that could read them.

import init, { Pairing, newStream } from "../wasm/yacs";
import type { DownloadMessage, DownloadMode, DownloadRequest } from "../mobile/download.worker";
import { OPFS_DIR } from "../mobile/download.worker";
import { isIos } from "../mobile/link";
import type { UploadMessage, UploadRequest } from "../mobile/upload.worker";
import { SseParser } from "../shared/sse";
import { chunkSizeFor } from "../shared/stream";
import type { ChannelEvent, Clip, ClipItem, ClipMeta, ClipView, ServerConfig, StreamInfo } from "../shared/types";
import { API, relayRequest, relayUpload } from "./relay";

const STORAGE_KEY = "yacs.spaces";
/** Before spaces: one pairing, `{ secret, token, deviceName }`. */
const LEGACY_KEY = "yacs.pairing";
export const DEFAULT_SPACE_NAME = "My devices";
/** As in the apps (`yacs_client::spaces`). */
const MAX_NAME_CHARS = 64;
/** The relay sends a keep-alive every 20 s; this much silence means the connection is dead. */
const LIVE_IDLE_MS = 60_000;

/** The relay predates live updates (0.2.0). */
export class LiveUnsupported extends Error {}

/**
 * What this browser keeps. The app only talks to the relay that served it, so
 * unlike the apps' lists (`yacs_client::spaces`) spaces carry no relay, and
 * there's one token. The first space is the one in use.
 */
export interface Stored {
  spaces: StoredSpace[];
  token: string | null;
  deviceName: string;
}

export interface StoredSpace {
  /** This device's own name for it. */
  name: string;
  /** `v1.<channel id>.<key>`, see `Pairing::to_secret` in yacs-core. */
  secret: string;
}

/** The space a `WebClient` works with. */
export interface Session {
  secret: string;
  token: string | null;
  deviceName: string;
}

export function session(stored: Stored | null): Session | null {
  const space = stored?.spaces[0];
  return space ? { secret: space.secret, token: stored!.token, deviceName: stored!.deviceName } : null;
}

/** A decrypted clip: `view` for display, `clip` with every format for copying. */
export interface Decrypted {
  view: ClipView;
  clip: Clip;
}

export type OnProgress = (done: number, total: number) => void;

let wasm: Promise<unknown> | null = null;
const ready = () => (wasm ??= init());

/** Null if this browser was never in a space. A pairing from before spaces becomes "My devices". */
export function loadStored(): Stored | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as Stored;
    const legacy = localStorage.getItem(LEGACY_KEY);
    if (!legacy) return null;
    const { secret, token, deviceName } = JSON.parse(legacy) as Session;
    const stored = save({ spaces: [{ name: DEFAULT_SPACE_NAME, secret }], token, deviceName });
    localStorage.removeItem(LEGACY_KEY);
    return stored;
  } catch {
    return null;
  }
}

function save(stored: Stored): Stored {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
  return stored;
}

/** Trimmed, whitespace collapsed, at most 64 characters; empty if blank. */
export function cleanName(name: string): string {
  return Array.from(name.trim().replace(/\s+/g, " ")).slice(0, MAX_NAME_CHARS).join("").trimEnd();
}

/**
 * Join the space in an invite link (or start one: `secret` null), once the
 * relay accepts it. Replaces the space in use; nothing is stored if the relay
 * says no.
 */
export async function enterSpace(secret: string | null, name: string, token: string | null, deviceName: string): Promise<Stored> {
  await ready();
  if (secret === null) {
    const pairing = Pairing.generate();
    secret = pairing.secret();
    pairing.free();
  } else {
    Pairing.fromSecret(secret).free(); // throws if the link is damaged
  }
  const next: Stored = {
    spaces: [{ name: cleanName(name) || DEFAULT_SPACE_NAME, secret }],
    token: token?.trim() || null,
    deviceName: deviceName.trim() || "Phone",
  };
  await new WebClient(session(next)!).config();
  return save(next);
}

export function renameSpace(stored: Stored, name: string): Stored {
  const [space, ...rest] = stored.spaces;
  const cleaned = cleanName(name);
  if (!space || !cleaned) return stored;
  return save({ ...stored, spaces: [{ ...space, name: cleaned }, ...rest] });
}

/** Forgets the space in use. The device name stays. */
export function leaveSpace(stored: Stored): Stored {
  const spaces = stored.spaces.slice(1);
  return save({ ...stored, spaces, token: spaces.length ? stored.token : null });
}

export function saveDeviceName(stored: Stored, deviceName: string): Stored {
  return save({ ...stored, deviceName: deviceName.trim() || stored.deviceName });
}

export class WebClient {
  private pairing: Promise<Pairing>;
  private cache = new Map<string, Decrypted>();

  constructor(private stored: Session) {
    this.pairing = ready().then(() => Pairing.fromSecret(stored.secret));
  }

  async config(): Promise<ServerConfig> {
    return (await this.request(`${API}/config`)).json();
  }

  /** Newest first. Also forgets cached clips that are gone. */
  async list(): Promise<ClipMeta[]> {
    const listed: ClipMeta[] = await (await this.request(await this.clipsUrl())).json();
    const ids = new Set(listed.map((m) => m.id));
    for (const id of this.cache.keys()) if (!ids.has(id)) this.cache.delete(id);
    return listed;
  }

  /** Fetched once and decrypted: clips never change. Null if it's gone. */
  async get(id: string): Promise<Decrypted | null> {
    const cached = this.cache.get(id);
    if (cached) return cached;
    const res = await this.request(`${await this.clipsUrl()}/${encodeURIComponent(id)}`, {}, [404]);
    if (res.status === 404) return null;
    const envelope = new Uint8Array(await res.arrayBuffer());
    const meta: ClipMeta = {
      id: res.headers.get("x-yacs-clip-id") ?? id,
      created_at_ms: Number(res.headers.get("x-yacs-created-at")),
      expires_at_ms: Number(res.headers.get("x-yacs-expires-at")),
      // Chunks included; relays before 0.3.0 don't say.
      size: Number(res.headers.get("x-yacs-size") ?? envelope.length),
      chunked: res.headers.get("x-yacs-chunked") === "1",
    };
    let clip: Clip;
    try {
      clip = (await this.pairing).open(envelope) as Clip;
    } catch (e) {
      // A kind of clip this build doesn't know yet.
      if (String(e).includes("malformed")) throw new Error("This clip needs a newer YACS. Close and reopen the app to update it.");
      throw e;
    }
    return this.remember(meta, clip);
  }

  /**
   * `items` with `files` too big for the clip: the files upload in a worker,
   * as chunks, and the clip is listed once they're all in.
   */
  async sendBig(
    items: ClipItem[],
    files: File[],
    ttlSecs: number,
    config: ServerConfig,
    onProgress: OnProgress,
    signal: AbortSignal,
  ): Promise<Decrypted> {
    if (!config.chunked) throw new Error("This relay only takes smaller files. Update it to send big ones.");
    await ready();
    const described = files.map((f) => ({ name: f.name || "file", mime: f.type || "application/octet-stream", size: f.size }));
    const stream = newStream(described, chunkSizeFor(config.chunked)) as StreamInfo;
    const clip: Clip = {
      created_at_ms: Date.now(),
      device_name: this.stored.deviceName,
      items: [...items, { Stream: stream }],
    };
    const request: UploadRequest = { type: "start", secret: this.stored.secret, token: this.stored.token, clip, files, ttlSecs };
    const worker = new Worker(new URL("../mobile/upload.worker.ts", import.meta.url), { type: "module" });
    try {
      const meta = await new Promise<ClipMeta>((resolve, reject) => {
        signal.addEventListener("abort", () => worker.postMessage({ type: "cancel" }), { once: true });
        worker.onerror = () => reject(new Error("The upload stopped unexpectedly."));
        worker.onmessage = ({ data }: MessageEvent<UploadMessage>) => {
          if (data.type === "progress") onProgress(data.done, data.total);
          else if (data.type === "done") resolve(data.meta);
          else reject(new Error(data.message));
        };
        worker.postMessage(request);
      });
      return this.remember(meta, clip);
    } finally {
      worker.terminate();
    }
  }

  /**
   * One file of a chunked clip, decrypted in a worker. Resolves to the file
   * for the page to share or save (iOS, and browsers without the service
   * worker), or to null when the browser is saving it already.
   */
  async download(id: string, stream: StreamInfo, file: number, onProgress: OnProgress, signal: AbortSignal): Promise<File | null> {
    const { name, size } = stream.files[file];
    const mode = downloadMode();
    const request: DownloadRequest = {
      type: "start",
      secret: this.stored.secret,
      token: this.stored.token,
      clipId: id,
      stream,
      file,
      mode,
    };
    const transfer: Transferable[] = [];
    let saving: (() => void) | undefined;
    if (mode === "port") {
      const started = await streamThroughServiceWorker(name, size);
      if (started) {
        request.port = started.port;
        transfer.push(started.port);
        saving = started.save;
      } else {
        request.mode = hasOpfs() ? "opfs" : "memory";
      }
    }

    const worker = new Worker(new URL("../mobile/download.worker.ts", import.meta.url), { type: "module" });
    // A service worker streaming a response may be stopped when it looks idle.
    const keepAlive = setInterval(() => navigator.serviceWorker?.controller?.postMessage({ type: "keepalive" }), 20_000);
    try {
      const done = await new Promise<DownloadMessage & { type: "done" }>((resolve, reject) => {
        signal.addEventListener("abort", () => worker.postMessage({ type: "cancel" }), { once: true });
        worker.onerror = () => reject(new Error("The download stopped unexpectedly."));
        worker.onmessage = ({ data }: MessageEvent<DownloadMessage>) => {
          if (data.type === "progress") onProgress(data.done, data.total);
          else if (data.type === "done") resolve(data);
          else reject(new Error(data.message));
        };
        worker.postMessage(request, transfer);
        saving?.();
      });
      if (done.name) {
        const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle(OPFS_DIR);
        const stored = await (await dir.getFileHandle(done.name)).getFile();
        return new File([stored], name, { type: stream.files[file].mime });
      }
      if (done.blob) return new File([done.blob], name, { type: stream.files[file].mime });
      return null;
    } finally {
      clearInterval(keepAlive);
      worker.terminate();
    }
  }

  /** In one request; `onProgress` hears how much of it went out. */
  async send(items: ClipItem[], ttlSecs: number, onProgress?: OnProgress, signal?: AbortSignal): Promise<Decrypted> {
    const clip: Clip = { created_at_ms: Date.now(), device_name: this.stored.deviceName, items };
    const envelope = (await this.pairing).seal(clip) as Uint8Array<ArrayBuffer>;
    const url = `${await this.clipsUrl()}?ttl=${Math.max(1, Math.round(ttlSecs))}`;
    if (onProgress) {
      const body = await relayUpload(url, this.stored.token, envelope, onProgress, signal ?? new AbortController().signal);
      return this.remember(JSON.parse(body), clip);
    }
    const res = await this.request(url, {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body: envelope,
    });
    return this.remember(await res.json(), clip);
  }

  async delete(id: string): Promise<void> {
    await this.request(`${await this.clipsUrl()}/${encodeURIComponent(id)}`, { method: "DELETE" }, [404]);
    this.cache.delete(id);
  }

  /**
   * Streams live changes until `signal` aborts or the connection drops.
   * `onOpen` runs once connected: re-list then, since nothing is missed from
   * that point on. Throws `LiveUnsupported` for relays before 0.2.0.
   */
  async listen(signal: AbortSignal, onOpen: () => void, onEvent: (event: ChannelEvent) => void): Promise<void> {
    const url = `${API}/channels/${(await this.pairing).channelId}/events`;
    const res = await this.request(url, { signal, headers: { accept: "text/event-stream" } }, [404]);
    if (res.status === 404) throw new LiveUnsupported("The relay has no live updates.");
    if (!res.body) throw new Error("The relay sent no event stream.");
    onOpen();
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    const parser = new SseParser();
    try {
      for (;;) {
        const { done, value } = await within(reader.read(), LIVE_IDLE_MS);
        if (done) return;
        for (const data of parser.feed(decoder.decode(value, { stream: true }))) {
          let event: ChannelEvent = { type: "other" };
          try {
            event = JSON.parse(data);
          } catch {
            // Unknown or broken: still a sign that something changed.
          }
          onEvent(event);
        }
      }
    } finally {
      reader.cancel().catch(() => {});
    }
  }

  private remember(meta: ClipMeta, clip: Clip): Decrypted {
    const decrypted = { view: clipView(meta, clip), clip };
    this.cache.set(meta.id, decrypted);
    return decrypted;
  }

  private async clipsUrl() {
    return `${API}/channels/${(await this.pairing).channelId}/clips`;
  }

  private request(url: string, init: RequestInit = {}, okStatuses: number[] = []): Promise<Response> {
    return relayRequest(url, this.stored.token, init, okStatuses);
  }
}

/**
 * The service worker streams downloads straight into the browser's
 * downloads, except on iOS, where that's unreliable (and in a home screen
 * app, unknown); there the file goes through OPFS and the share sheet.
 */
function downloadMode(): DownloadMode {
  if (!isIos() && navigator.serviceWorker?.controller) return "port";
  return hasOpfs() ? "opfs" : "memory";
}

function hasOpfs(): boolean {
  return typeof navigator.storage?.getDirectory === "function";
}

/**
 * Registers a download with the service worker. Returns the port the
 * download worker feeds and `save`, which starts the browser's download; or
 * null if the service worker doesn't answer (an older one is still active).
 */
async function streamThroughServiceWorker(name: string, size: number): Promise<{ port: MessagePort; save: () => void } | null> {
  const sw = navigator.serviceWorker?.controller;
  if (!sw) return null;
  const data = new MessageChannel();
  const ack = new MessageChannel();
  const token = crypto.randomUUID();
  const ready = new Promise<boolean>((resolve) => {
    ack.port1.onmessage = () => resolve(true);
    setTimeout(() => resolve(false), 3000);
  });
  sw.postMessage({ type: "download", token, name, size }, [data.port1, ack.port2]);
  if (!(await ready)) return null;
  const save = () => {
    const frame = Object.assign(document.createElement("iframe"), { hidden: true, src: `/download/${token}` });
    document.body.append(frame);
    // Long enough for the browser to take over the download.
    setTimeout(() => frame.remove(), 60_000);
  };
  return { port: data.port2, save };
}

/**
 * Removes downloaded files from OPFS once they're shared: those of one clip,
 * or all of them (left over when the app was closed before sharing).
 */
export async function forgetDownloads(clipId?: string) {
  try {
    const root = await navigator.storage.getDirectory();
    if (clipId === undefined) return await root.removeEntry(OPFS_DIR, { recursive: true });
    const dir = await root.getDirectoryHandle(OPFS_DIR);
    for await (const name of (dir as unknown as { keys(): AsyncIterable<string> }).keys()) {
      if (name.startsWith(`${clipId}-`)) await dir.removeEntry(name);
    }
  } catch {
    // none, or no OPFS
  }
}

function within<T>(promise: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error("The live update connection went quiet.")), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/** The same shape the desktop's Rust `ClipView` has, so both UIs share helpers. */
export function clipView(meta: ClipMeta, clip: Clip): ClipView {
  const view: ClipView = {
    meta,
    deviceName: clip.device_name,
    text: null,
    textTruncated: false,
    html: null,
    rtf: false,
    image: null,
    files: [],
  };
  for (const item of clip.items) {
    if ("Text" in item) view.text ??= item.Text;
    else if ("Html" in item) view.html ??= item.Html;
    else if ("Rtf" in item) view.rtf = true;
    else if ("Image" in item) view.image ??= { mime: item.Image.mime, size: item.Image.data.length, width: null, height: null };
    else if ("File" in item) view.files.push({ name: item.File.name, mime: item.File.mime, size: item.File.data.length });
    else view.files.push(...item.Stream.files);
  }
  return view;
}
