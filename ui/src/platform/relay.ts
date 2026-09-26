// Requests to the relay that served the page, shared by the PWA and its
// upload and download workers.

export const API = "/api/v1";

export class RelayError extends Error {
  /** Worth trying again: the network or the relay had a moment. */
  constructor(
    message: string,
    readonly transient: boolean,
    readonly status?: number,
  ) {
    super(message);
  }
}

/** `fetch` with the access token; throws a `RelayError` with a readable message unless it's ok or in `okStatuses`. */
export async function relayRequest(
  url: string,
  token: string | null,
  init: RequestInit = {},
  okStatuses: number[] = [],
): Promise<Response> {
  const headers = new Headers(init.headers);
  if (token) headers.set("authorization", `Bearer ${token}`);
  let res: Response;
  try {
    res = await fetch(url, { ...init, headers, cache: "no-store" });
  } catch (e) {
    if (init.signal?.aborted) throw e;
    throw new RelayError("Can't reach the relay. Check your connection.", true);
  }
  if (res.ok || okStatuses.includes(res.status)) return res;
  throw relayError(res.status, await res.text(), res.statusText, res.headers.has("retry-after"));
}

/** The relay's reason, from its JSON error body; null for anything else (a proxy's page, say). */
function reason(body: string): string | null {
  try {
    const error = JSON.parse(body).error;
    return typeof error === "string" ? error : null;
  } catch {
    return null;
  }
}

function sentence(text: string): string {
  const s = text.charAt(0).toUpperCase() + text.slice(1);
  return /[.!?]$/.test(s) ? s : `${s}.`;
}

function relayError(status: number, body: string, statusText = "", retryAfter = false): RelayError {
  switch (status) {
    case 401:
      return new RelayError("The relay needs its account key to create a space, or didn't take the one given.", false, 401);
    case 413:
      return new RelayError(sentence(reason(body) ?? "this clip is too large for the relay"), false, 413);
    case 429: {
      // Only a rate limit passes by waiting; quotas say why they're reached.
      const why = retryAfter ? null : reason(body);
      return why ? new RelayError(sentence(why), false, 429) : new RelayError("The relay is busy. Trying again…", true, 429);
    }
    case 507:
      return new RelayError("The relay's storage is full.", false, 507);
  }
  const message = reason(body) ?? body;
  const transient = status >= 500 || status === 408;
  return new RelayError(`Relay error ${status}: ${message || statusText}`, transient, status);
}

/**
 * POSTs `body` like `relayRequest`, reporting how much of it went out:
 * `fetch` can't, `XMLHttpRequest` can. Resolves to the response text.
 */
export function relayUpload(
  url: string,
  token: string | null,
  body: Uint8Array<ArrayBuffer>,
  onProgress: (sent: number, total: number) => void,
  signal: AbortSignal,
): Promise<string> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", url);
    xhr.setRequestHeader("content-type", "application/octet-stream");
    if (token) xhr.setRequestHeader("authorization", `Bearer ${token}`);
    xhr.upload.onprogress = (e) => onProgress(e.loaded, e.total || body.length);
    xhr.onload = () =>
      xhr.status >= 200 && xhr.status < 300
        ? resolve(xhr.responseText)
        : reject(relayError(xhr.status, xhr.responseText, xhr.statusText, xhr.getResponseHeader("retry-after") !== null));
    xhr.onerror = () => reject(new RelayError("Can't reach the relay. Check your connection.", true));
    xhr.onabort = () => reject(new DOMException("Sending cancelled.", "AbortError"));
    const abort = () => xhr.abort();
    signal.addEventListener("abort", abort, { once: true });
    xhr.onloadend = () => signal.removeEventListener("abort", abort);
    xhr.send(body);
  });
}

/** Attempts per chunk. With the backoff below, about four minutes of trying. */
const ATTEMPTS = 10;
const BACKOFF_MAX_MS = 30_000;

/** Runs `attempt` again after transient failures, backing off. */
export async function retrying<T>(attempt: () => Promise<T>, signal: AbortSignal): Promise<T> {
  let delay = 1000;
  for (let i = 1; ; i++) {
    try {
      return await attempt();
    } catch (e) {
      if (signal.aborted || !(e instanceof RelayError && e.transient) || i >= ATTEMPTS) throw e;
      await new Promise((resolve) => setTimeout(resolve, delay));
      delay = Math.min(delay * 2, BACKOFF_MAX_MS);
    }
  }
}

export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
