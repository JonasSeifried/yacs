import { afterEach, describe, expect, it, vi } from "vitest";
import { relayRequest, retrying } from "./relay";

/** A fetch that answers with `statuses` in turn, counting the calls. */
function answering(...statuses: number[]) {
  const fetch = vi.fn(async () => new Response(null, { status: statuses.shift() ?? 200 }));
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("retrying", () => {
  const post = (signal: AbortSignal) => () => relayRequest("/x", null, { method: "POST", signal });

  it("tries again after a server error", async () => {
    vi.useFakeTimers();
    const fetch = answering(502, 503, 201);
    const done = retrying(post(new AbortController().signal), new AbortController().signal);
    await vi.runAllTimersAsync();
    expect((await done).status).toBe(201);
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it("gives up at once when the relay refuses", async () => {
    const fetch = answering(409);
    const signal = new AbortController().signal;
    await expect(retrying(post(signal), signal)).rejects.toMatchObject({ status: 409, transient: false });
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("stops once cancelled", async () => {
    vi.useFakeTimers();
    const fetch = answering(502, 201);
    const cancel = new AbortController();
    const done = retrying(post(cancel.signal), cancel.signal);
    const failed = expect(done).rejects.toMatchObject({ status: 502 });
    cancel.abort();
    await vi.runAllTimersAsync();
    await failed;
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("stops waiting once cancelled between tries", async () => {
    vi.useFakeTimers();
    const fetch = answering(502, 201);
    const cancel = new AbortController();
    const done = retrying(post(cancel.signal), cancel.signal);
    const failed = expect(done).rejects.toMatchObject({ status: 502 });
    // The first try has failed; the next one is a second away.
    await vi.advanceTimersByTimeAsync(10);
    cancel.abort();
    await failed;
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
