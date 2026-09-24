/**
 * Just enough of a server-sent events parser for the relay's event stream:
 * collects `data:` lines into messages and skips everything else (comments,
 * `event:`, `id:`, `retry:`). Same rules as `yacs-client`'s parser.
 */
export class SseParser {
  private partial = "";
  private data: string | null = null;

  /** Feed received text; returns the `data` of every message it completes. */
  feed(text: string): string[] {
    this.partial += text;
    const messages: string[] = [];
    let end: number;
    while ((end = this.partial.indexOf("\n")) !== -1) {
      const line = this.partial.slice(0, end).replace(/\r$/, "");
      this.partial = this.partial.slice(end + 1);
      if (line === "") {
        if (this.data !== null) messages.push(this.data);
        this.data = null;
      } else if (line === "data" || line.startsWith("data:")) {
        const value = line.slice(5).replace(/^ /, "");
        this.data = this.data === null ? value : `${this.data}\n${value}`;
      }
    }
    return messages;
  }
}
