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
  const transient = res.status >= 500 || res.status === 408;
  switch (res.status) {
    case 401:
      throw new RelayError("The relay rejected the access token.", false, 401);
    case 413:
      throw new RelayError("This clip is too large for the relay.", false, 413);
    case 507:
      throw new RelayError("The relay's storage is full.", false, 507);
  }
  const body = await res.text();
  let message = body;
  try {
    message = JSON.parse(body).error ?? body;
  } catch {
    // not JSON
  }
  throw new RelayError(`Relay error ${res.status}: ${message || res.statusText}`, transient, res.status);
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
