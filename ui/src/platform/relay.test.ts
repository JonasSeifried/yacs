import { afterEach, describe, expect, it, vi } from "vitest";
import { readSlot, relayRequest, retrying } from "./relay";

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

  it("tries at most as often as asked", async () => {
    vi.useFakeTimers();
    const fetch = answering(502, 502, 502, 201);
    const signal = new AbortController().signal;
    const done = retrying(post(signal), signal, 3);
    const failed = expect(done).rejects.toMatchObject({ status: 502 });
    await vi.runAllTimersAsync();
    await failed;
    expect(fetch).toHaveBeenCalledTimes(3);
  });
});

describe("readSlot", () => {
  it("rides out a bad gateway", async () => {
    vi.useFakeTimers();
    const fetch = answering(502, 204, 200);
    const done = readSlot("/rendezvous/7/a/0", false);
    await vi.runAllTimersAsync();
    expect(await done).toEqual(new Uint8Array());
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  // Reading a/0 closes nothing, so it's gone because the code is wrong.
  it("calls a code gone after a bad gateway gone", async () => {
    vi.useFakeTimers();
    answering(502, 404);
    const done = readSlot("/rendezvous/9/a/0", false);
    await vi.runAllTimersAsync();
    expect(await done).toBeNull();
  });

  // The relay closed the rendezvous as it handed a/1 over; the answer got lost.
  it("blames the network for a/1 gone after a bad gateway", async () => {
    vi.useFakeTimers();
    answering(502, 404);
    const done = readSlot("/rendezvous/7/a/1", true);
    const failed = expect(done).rejects.toMatchObject({ status: 502 });
    await vi.runAllTimersAsync();
    await failed;
  });
});
