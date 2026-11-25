import { describe, expect, it } from "vitest";
import { adaptivePollDelaysMs, nextAdaptiveDelay } from "./pollBackoff";

describe("nextAdaptiveDelay", () => {
  it("steps through adaptive intervals and clamps negative or saturated steps", () => {
    expect(nextAdaptiveDelay(-3)).toBe(adaptivePollDelaysMs[0]);
    expect(nextAdaptiveDelay(0)).toBe(35);
    expect(nextAdaptiveDelay(1)).toBe(80);
    expect(nextAdaptiveDelay(2)).toBe(180);
    expect(nextAdaptiveDelay(3)).toBe(350);
    expect(nextAdaptiveDelay(4)).toBe(500);
    expect(nextAdaptiveDelay(99)).toBe(500);
  });
});
