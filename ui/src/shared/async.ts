/** Run async tasks with at most `limit` in flight. Failures are ignored. */
export function runLimited(tasks: (() => Promise<unknown>)[], limit: number) {
  const queue = [...tasks];
  const worker = async (): Promise<void> => {
    const task = queue.shift();
    if (!task) return;
    await task().catch(() => {});
    return worker();
  };
  for (let i = 0; i < limit; i++) worker();
}

/** Clips up to this size are decrypted right away so their rows show a title; bigger ones on demand. */
export const EAGER_BYTES = 256 * 1024;

/** Whether to decrypt a clip right away: small ones, and chunked ones (only their small header is fetched). */
export function loadsEagerly(meta: { size: number; chunked?: boolean }): boolean {
  return meta.size <= EAGER_BYTES || meta.chunked === true;
}
export const EAGER_CONCURRENCY = 4;
