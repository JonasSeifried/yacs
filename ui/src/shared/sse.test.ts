import { describe, expect, it } from "vitest";
import { SseParser } from "./sse";

describe("SseParser", () => {
  it("collects data and skips the rest", () => {
    const p = new SseParser();
    expect(p.feed(": keep-alive\n\n")).toEqual([]);
    expect(p.feed('event: x\ndata: {"a":1}\n\n')).toEqual(['{"a":1}']);
    expect(p.feed("data:one\r\ndata: two\r\n\r\ndata: 3\n\n")).toEqual(["one\ntwo", "3"]);
    expect(p.feed("database: no\n\n")).toEqual([]);
  });

  it("handles messages split across chunks", () => {
    const p = new SseParser();
    expect(p.feed("da")).toEqual([]);
    expect(p.feed("ta: hel")).toEqual([]);
    expect(p.feed("lo\n")).toEqual([]);
    expect(p.feed("\ndata: next")).toEqual(["hello"]);
    expect(p.feed("\n\n")).toEqual(["next"]);
  });
});
