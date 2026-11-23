import { describe, expect, it } from "vitest";
import { joinTexts, markCopied, pickAfterClick, pruneCopied, stepPick } from "./picks";

describe("copied marks", () => {
  it("marks and prunes", () => {
    const copied = markCopied({ a: 1 }, ["b", "c"], 5);
    expect(copied).toEqual({ a: 1, b: 5, c: 5 });
    expect(pruneCopied(copied, ["b", "x"])).toEqual({ b: 5 });
  });
});

describe("pickAfterClick", () => {
  const order = ["a", "b", "c", "d"];

  it("a plain click picks one", () => {
    expect(pickAfterClick(["a", "b"], order, "b", "c", "only")).toEqual(["c"]);
  });

  it("toggling keeps the selected clip and click order", () => {
    expect(pickAfterClick([], order, "c", "a", "toggle")).toEqual(["c", "a"]);
    expect(pickAfterClick(["c", "a"], order, "a", "d", "toggle")).toEqual(["c", "a", "d"]);
    expect(pickAfterClick(["c", "a", "d"], order, "d", "a", "toggle")).toEqual(["c", "d"]);
  });

  it("toggling the selected clip alone picks nothing extra", () => {
    expect(pickAfterClick([], order, "a", "a", "toggle")).toEqual(["a"]);
  });

  it("shift picks the range from the anchor", () => {
    expect(pickAfterClick([], order, "a", "c", "range")).toEqual(["a", "b", "c"]);
    expect(pickAfterClick([], order, "d", "b", "range")).toEqual(["d", "c", "b"]);
  });
});

describe("joinTexts", () => {
  it("puts each on its own line", () => {
    expect(joinTexts(["one", "two\n", "three"])).toBe("one\ntwo\nthree");
    expect(joinTexts([])).toBe("");
  });
});

describe("stepPick", () => {
  it("picks going one way and unpicks going back", () => {
    expect(stepPick([], "d", "c")).toEqual(["d", "c"]);
    expect(stepPick(["d", "c"], "c", "b")).toEqual(["d", "c", "b"]);
    expect(stepPick(["d", "c", "b"], "b", "c")).toEqual(["d", "c"]);
  });

  it("keeps earlier picks when moving onto one", () => {
    // ⌘-clicked a and d, then Shift+Up from d to b: on to a keeps b.
    expect(stepPick(["a", "d", "c", "b"], "b", "a")).toEqual(["a", "d", "c", "b"]);
  });
});
