// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { clipTitle, previewDocument, previewKind } from "./clip";
import type { ClipView } from "./types";

function clip(fields: Partial<ClipView>): ClipView {
  return {
    meta: { id: "01J", created_at_ms: 0, expires_at_ms: 0, size: 10 },
    deviceName: "MacBook",
    text: null,
    textTruncated: false,
    html: null,
    rtf: false,
    image: null,
    files: [],
    ...fields,
  };
}

const image = { mime: "image/png", size: 100, width: 1280, height: 720 };

describe("clipTitle", () => {
  it("collapses whitespace and caps long text", () => {
    expect(clipTitle(clip({ text: "  Meeting\n\n notes\tfor Thursday  " }))).toBe("Meeting notes for Thursday");
    const title = clipTitle(clip({ text: "a".repeat(500) }));
    expect(title).toHaveLength(121);
    expect(title.endsWith("…")).toBe(true);
  });

  it("describes clips without text", () => {
    expect(clipTitle(clip({ image }))).toBe("Image · 1280×720");
    expect(clipTitle(clip({ image: { ...image, width: null, height: null } }))).toBe("Image");
    expect(clipTitle(clip({ rtf: true, text: "  " }))).toBe("Formatted text");
    expect(clipTitle(clip({}))).toBe("Empty clip");
  });

  it("names the files", () => {
    const pdf = { name: "report.pdf", mime: "application/pdf", size: 1 };
    const zip = { name: "photos.zip", mime: "application/zip", size: 1 };
    expect(clipTitle(clip({ files: [pdf], text: "see attached" }))).toBe("report.pdf");
    expect(clipTitle(clip({ files: [pdf, zip] }))).toBe("2 files: report.pdf, photos.zip");
  });
});

describe("previewKind", () => {
  it("prefers formatted text, then text, then the image", () => {
    expect(previewKind(clip({ text: "hi", html: "<b>hi</b>", image }))).toBe("html");
    expect(previewKind(clip({ text: "hi", image }))).toBe("text");
    expect(previewKind(clip({ html: '<img src="https://example.com/cat.png">', image }))).toBe("image");
    expect(previewKind(clip({ html: "<b>only html</b>" }))).toBe("html");
    expect(previewKind(clip({ rtf: true }))).toBe("none");
    expect(previewKind(clip({ text: "hi", files: [{ name: "a.png", mime: "image/png", size: 1 }] }))).toBe("files");
  });
});

describe("previewDocument", () => {
  it("strips scripts, handlers and javascript: links", () => {
    const doc = previewDocument(
      `<b>hi</b><script>alert(1)</script><img src="x" onerror="alert(2)"><a href="javascript:alert(3)">x</a><form><input></form>`,
    );
    const body = doc.slice(doc.indexOf("<body>"));
    expect(body).toContain("<b>hi</b>");
    expect(body).not.toMatch(/script|onerror|javascript:|<form|<input/i);
  });

  it("forbids all network requests", () => {
    const doc = new DOMParser().parseFromString(previewDocument("<p>x</p>"), "text/html");
    const csp = doc.querySelector('meta[http-equiv="Content-Security-Policy"]')?.getAttribute("content");
    expect(csp).toBe("default-src 'none'; style-src 'unsafe-inline'; img-src data:");
  });

  it("keeps a meta refresh or CSP override out of the body", () => {
    const body = previewDocument(`<meta http-equiv="refresh" content="0;url=https://evil.example"><base href="https://evil.example/">`);
    expect(body.slice(body.indexOf("<body>"))).not.toMatch(/<meta|<base/i);
  });
});
