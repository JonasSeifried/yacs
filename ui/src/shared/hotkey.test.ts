import { describe, expect, it } from "vitest";
import { acceleratorFromEvent, formatAccelerator } from "./hotkey";

const press = (code: string, key: string, mods: Partial<Record<"ctrlKey" | "altKey" | "shiftKey" | "metaKey", boolean>> = {}) => ({
  code,
  key,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

describe("acceleratorFromEvent", () => {
  it("builds modifier+code accelerators", () => {
    expect(acceleratorFromEvent(press("Space", " ", { metaKey: true, shiftKey: true }))).toBe("Super+Shift+Space");
    expect(acceleratorFromEvent(press("KeyV", "v", { ctrlKey: true, altKey: true }))).toBe("Control+Alt+KeyV");
  });

  it("uses the physical key, not the layout's character", () => {
    // Option+V on a Mac types "√"; the shortcut is still KeyV.
    expect(acceleratorFromEvent(press("KeyV", "√", { altKey: true }))).toBe("Alt+KeyV");
  });

  it("ignores bare modifiers and shortcuts that would swallow typing", () => {
    expect(acceleratorFromEvent(press("ShiftLeft", "Shift", { shiftKey: true }))).toBeNull();
    expect(acceleratorFromEvent(press("KeyA", "A", { shiftKey: true }))).toBeNull();
    expect(acceleratorFromEvent(press("KeyA", "a"))).toBeNull();
  });
});

describe("formatAccelerator", () => {
  it("uses symbols on macOS", () => {
    expect(formatAccelerator("CommandOrControl+Shift+Space", "macos")).toBe("⌘⇧Space");
    expect(formatAccelerator("Control+Alt+KeyV", "macos")).toBe("⌃⌥V");
  });

  it("uses names on Windows", () => {
    expect(formatAccelerator("CommandOrControl+Shift+Space", "windows")).toBe("Ctrl+Shift+Space");
    expect(formatAccelerator("Super+Digit1", "windows")).toBe("Win+1");
  });
});
