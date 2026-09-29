// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { onFocusLeft } from "./focus";

afterEach(() => {
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("onFocusLeft", () => {
  it("fires when the window loses focus", () => {
    vi.useFakeTimers();
    const left = vi.fn();
    const stop = onFocusLeft(left);
    window.dispatchEvent(new Event("blur"));
    vi.runAllTimers();
    expect(left).toHaveBeenCalledOnce();

    stop();
    window.dispatchEvent(new Event("blur"));
    vi.runAllTimers();
    expect(left).toHaveBeenCalledOnce();
  });

  // Or a delete waiting for ⌘Z goes to every device on a click into the preview.
  it("takes focus back from the preview instead", () => {
    vi.useFakeTimers();
    const preview = document.createElement("iframe");
    document.body.append(preview);
    const left = vi.fn();
    const stop = onFocusLeft(left);
    preview.focus();
    expect(document.activeElement).toBe(preview);
    const blur = vi.spyOn(preview, "blur");
    window.dispatchEvent(new Event("blur"));
    vi.runAllTimers();
    expect(left).not.toHaveBeenCalled();
    expect(blur).toHaveBeenCalledOnce();
    stop();
  });
});
