// Chunk arithmetic of `yacs_core::Stream`, for the PWA's upload and download
// workers, and when a relay takes files as chunks at all.

import type { ServerConfig, StreamInfo } from "./types";

/** Poly1305 tag at the end of every sealed chunk. */
export const CHUNK_TAG_LEN = 16;
/** `yacs_core::DEFAULT_CHUNK_SIZE` and `MIN_CHUNK_SIZE`. */
const DEFAULT_CHUNK_SIZE = 4 * 1024 * 1024;
const MIN_CHUNK_SIZE = 64 * 1024;
/** `yacs_core::api::INLINE_FILE_BYTES`: smaller files go into the clip itself. */
const INLINE_FILE_BYTES = 8 * 1024 * 1024;
const ENVELOPE_SLACK = 64 * 1024;

/** `ServerConfig::inline_file_limit`: the most file bytes to put into one clip. */
export function inlineFileLimit(config: ServerConfig): number {
  const fits = Math.max(0, config.max_size_bytes - ENVELOPE_SLACK);
  return config.chunked ? Math.min(fits, INLINE_FILE_BYTES) : fits;
}

/** `ChunkedConfig::chunk_size`. */
export function chunkSizeFor(config: NonNullable<ServerConfig["chunked"]>): number {
  return Math.max(MIN_CHUNK_SIZE, Math.min(DEFAULT_CHUNK_SIZE, config.max_chunk_bytes));
}

export function streamTotal(stream: StreamInfo): number {
  return stream.files.reduce((sum, f) => sum + f.size, 0);
}

/** At least one: an empty stream is one empty chunk. */
export function chunkCount(stream: StreamInfo): number {
  return Math.max(1, Math.ceil(streamTotal(stream) / stream.chunk_size));
}

/** Plaintext bytes in chunk `index`. */
export function chunkLen(stream: StreamInfo, index: number): number {
  const start = index * stream.chunk_size;
  return Math.max(0, Math.min(stream.chunk_size, streamTotal(stream) - start));
}

/** What the relay stores for all chunks together. */
export function sealedLen(stream: StreamInfo): number {
  return streamTotal(stream) + chunkCount(stream) * CHUNK_TAG_LEN;
}

/** Where file `index` starts in the stream. */
export function fileOffset(stream: StreamInfo, index: number): number {
  return stream.files.slice(0, index).reduce((sum, f) => sum + f.size, 0);
}

/** The chunks holding any of file `index`: `[first, end)`. */
export function fileChunks(stream: StreamInfo, index: number): [number, number] {
  const size = stream.chunk_size;
  const start = fileOffset(stream, index);
  const len = stream.files[index].size;
  if (len === 0) {
    const at = Math.min(Math.floor(start / size), chunkCount(stream) - 1);
    return [at, at + 1];
  }
  return [Math.floor(start / size), Math.ceil((start + len) / size)];
}

/** The part of chunk `chunk`'s plaintext that belongs to file `index`, as `[from, to)`. */
export function fileSlice(stream: StreamInfo, index: number, chunk: number): [number, number] {
  const chunkStart = chunk * stream.chunk_size;
  const start = fileOffset(stream, index);
  const end = start + stream.files[index].size;
  const from = Math.max(start, chunkStart) - chunkStart;
  const to = Math.min(end, chunkStart + chunkLen(stream, chunk)) - chunkStart;
  return [from, Math.max(from, to)];
}
