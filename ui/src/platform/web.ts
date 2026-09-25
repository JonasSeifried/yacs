// The PWA's backend: crypto in WASM (`yacs-wasm`, the same Rust code the
// desktop runs), network via fetch to the relay that served the page.
// The pairing is kept in localStorage; the page's CSP allows no third-party
// scripts that could read it.

import init, { Pairing, generatePhrase as wasmGeneratePhrase } from "../wasm/yacs";
import { SseParser } from "../shared/sse";
import type { ChannelEvent, Clip, ClipItem, ClipMeta, ClipView, ServerConfig } from "../shared/types";

const STORAGE_KEY = "yacs.pairing";
const API = "/api/v1";
/** The relay sends a keep-alive every 20 s; this much silence means the connection is dead. */
const LIVE_IDLE_MS = 60_000;

/** The relay predates live updates (0.2.0). */
export class LiveUnsupported extends Error {}

export interface StoredPairing {
  /** `v1.<channel id>.<key>`, see `Pairing::to_secret` in yacs-core. */
  secret: string;
  token: string | null;
  deviceName: string;
}

/** A decrypted clip: `view` for display, `clip` with every format for copying. */
export interface Decrypted {
  view: ClipView;
  clip: Clip;
}

let wasm: Promise<unknown> | null = null;
const ready = () => (wasm ??= init());

export function storedPairing(): StoredPairing | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as StoredPairing) : null;
  } catch {
    return null;
  }
}

export async function generatePhrase(): Promise<string> {
  await ready();
  return wasmGeneratePhrase();
}

/**
 * Derive (or restore) the pairing, prove the relay accepts it, then store it.
 * Nothing is stored if any step fails.
 */
export async function pair(source: { phrase: string } | { secret: string }, token: string | null, deviceName: string) {
  await ready();
  // Argon2id takes a moment and blocks the thread: let the UI paint "Pairing…" first.
  await new Promise((r) => setTimeout(r, 50));
  const pairing = "phrase" in source ? Pairing.fromPhrase(source.phrase) : Pairing.fromSecret(source.secret);
  const stored: StoredPairing = { secret: pairing.secret(), token: token?.trim() || null, deviceName: deviceName.trim() || "Phone" };
  pairing.free();
  await new WebClient(stored).config();
  localStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
  return stored;
}

export function unpair() {
  localStorage.removeItem(STORAGE_KEY);
}

export function saveDeviceName(stored: StoredPairing, deviceName: string): StoredPairing {
  const next = { ...stored, deviceName: deviceName.trim() || stored.deviceName };
  localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  return next;
}

export class WebClient {
  private pairing: Promise<Pairing>;
  private cache = new Map<string, Decrypted>();

  constructor(private stored: StoredPairing) {
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
      size: envelope.length,
    };
    const clip = (await this.pairing).open(envelope) as Clip;
    return this.remember(meta, clip);
  }

  async send(items: ClipItem[], ttlSecs: number): Promise<Decrypted> {
    const clip: Clip = { created_at_ms: Date.now(), device_name: this.stored.deviceName, items };
    const envelope = (await this.pairing).seal(clip);
    const res = await this.request(`${await this.clipsUrl()}?ttl=${Math.max(1, Math.round(ttlSecs))}`, {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body: envelope as Uint8Array<ArrayBuffer>,
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

  private async request(url: string, init: RequestInit = {}, okStatuses: number[] = []): Promise<Response> {
    const headers = new Headers(init.headers);
    if (this.stored.token) headers.set("authorization", `Bearer ${this.stored.token}`);
    let res: Response;
    try {
      res = await fetch(url, { ...init, headers, cache: "no-store" });
    } catch {
      throw new Error("Can't reach the relay. Check your connection.");
    }
    if (res.ok || okStatuses.includes(res.status)) return res;
    switch (res.status) {
      case 401:
        throw new Error("The relay rejected the access token.");
      case 413:
        throw new Error("This clip is too large for the relay.");
      case 507:
        throw new Error("The relay's storage is full.");
    }
    const body = await res.text();
    let message = body;
    try {
      message = JSON.parse(body).error ?? body;
    } catch {
      // not JSON
    }
    throw new Error(`Relay error ${res.status}: ${message || res.statusText}`);
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
  };
  for (const item of clip.items) {
    if ("Text" in item) view.text ??= item.Text;
    else if ("Html" in item) view.html ??= item.Html;
    else if ("Rtf" in item) view.rtf = true;
    else view.image ??= { mime: item.Image.mime, size: item.Image.data.length, width: null, height: null };
  }
  return view;
}
