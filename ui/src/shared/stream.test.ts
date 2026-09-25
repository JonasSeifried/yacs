import { describe, expect, it } from "vitest";
import { chunkCount, chunkLen, chunkSizeFor, fileChunks, fileSlice, inlineFileLimit, sealedLen, streamTotal } from "./stream";
import type { ServerConfig, StreamInfo } from "./types";

const C = 64 * 1024;

function stream(sizes: number[]): StreamInfo {
  return {
    salt: new Uint8Array(32),
    chunk_size: C,
    files: sizes.map((size, i) => ({ name: `f${i}`, mime: "application/octet-stream", size })),
  };
}

// The same cases as `yacs_core::stream::tests::chunk_arithmetic`.
describe("chunk arithmetic", () => {
  it("matches yacs-core", () => {
    const s = stream([C, C / 2, 0, C + 1]);
    expect(streamTotal(s)).toBe(2.5 * C + 1);
    expect(chunkCount(s)).toBe(3);
    expect(chunkLen(s, 0)).toBe(C);
    expect(chunkLen(s, 2)).toBe(C / 2 + 1);
    expect(sealedLen(s)).toBe(streamTotal(s) + 3 * 16);
    expect(fileChunks(s, 0)).toEqual([0, 1]);
    expect(fileChunks(s, 1)).toEqual([1, 2]);
    expect(fileChunks(s, 2)).toEqual([1, 2]);
    expect(fileChunks(s, 3)).toEqual([1, 3]);

    const empty = stream([0]);
    expect([chunkCount(empty), chunkLen(empty, 0), sealedLen(empty)]).toEqual([1, 0, 16]);
    expect(fileChunks(empty, 0)).toEqual([0, 1]);
  });

  it("slices each chunk into its files", () => {
    const s = stream([C, C / 2, 0, C + 1]);
    expect(fileSlice(s, 0, 0)).toEqual([0, C]);
    expect(fileSlice(s, 1, 1)).toEqual([0, C / 2]);
    expect(fileSlice(s, 2, 1)).toEqual([C / 2, C / 2]);
    expect(fileSlice(s, 3, 1)).toEqual([C / 2, C]);
    expect(fileSlice(s, 3, 2)).toEqual([0, C / 2 + 1]);
  });
});

describe("limits", () => {
  const config = (max: number, chunked: boolean): ServerConfig => ({
    default_ttl_secs: 900,
    max_ttl_secs: 86400,
    max_size_bytes: max,
    max_clips: 50,
    chunked: chunked ? { max_chunk_bytes: 16 * 1024 * 1024 } : undefined,
  });

  it("keeps small files in the clip", () => {
    expect(inlineFileLimit(config(20_000_000, true))).toBe(8 * 1024 * 1024);
    expect(inlineFileLimit(config(20_000_000, false))).toBe(20_000_000 - 64 * 1024);
    expect(inlineFileLimit(config(1_000_000, true))).toBe(1_000_000 - 64 * 1024);
  });

  it("picks the chunk size", () => {
    expect(chunkSizeFor({ max_chunk_bytes: 16 * 1024 * 1024 })).toBe(4 * 1024 * 1024);
    expect(chunkSizeFor({ max_chunk_bytes: 100_000 })).toBe(100_000);
    expect(chunkSizeFor({ max_chunk_bytes: 10 })).toBe(C);
  });
});
