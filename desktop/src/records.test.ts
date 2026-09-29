import { describe, expect, it } from "vitest";
import {
  latency,
  modelRoute,
  money,
  tokenCount,
  type UsageRecord,
} from "./records";

const route = (fields: Partial<UsageRecord>) =>
  modelRoute(fields as UsageRecord);

describe("record model route", () => {
  it("preserves distinct requested, mapped, forwarded, and returned models", () => {
    expect(
      route({
        requested_model: " request-model ",
        model: "legacy-model",
        model_mapping_chain: "request-model → mapped-model -> forward-model",
        upstream_model: "forward-model",
        upstream_response_model: "returned-model",
      }),
    ).toEqual([
      { model: "request-model", labels: ["请求", "映射"] },
      { model: "mapped-model", labels: ["映射"] },
      { model: "forward-model", labels: ["映射", "转发"] },
      { model: "returned-model", labels: ["返回"] },
    ]);
  });

  it("merges only consecutive equal models and retains every stage label", () => {
    expect(
      route({
        requested_model: "same-model",
        model_mapping_chain: "same-model → same-model",
        upstream_model: "same-model",
        upstream_response_model: "same-model",
      }),
    ).toEqual([
      { model: "same-model", labels: ["请求", "映射", "转发", "返回"] },
    ]);
    expect(
      route({
        requested_model: "model-a",
        model_mapping_chain: "model-a -> model-b -> model-a",
        upstream_model: "model-a",
      }),
    ).toEqual([
      { model: "model-a", labels: ["请求", "映射"] },
      { model: "model-b", labels: ["映射"] },
      { model: "model-a", labels: ["映射", "转发"] },
    ]);
  });

  it("uses a legacy model without inventing an upstream response", () => {
    expect(route({ model: "legacy-model" })).toEqual([
      { model: "legacy-model", labels: ["请求", "转发"] },
    ]);
    expect(route({})).toEqual([]);
    expect(route({ upstream_response_model: "actual-only" })).toEqual([
      { model: "actual-only", labels: ["返回"] },
    ]);
  });
});

describe("record values", () => {
  it.each([
    ["0", "$0.000000"],
    ["0.0000000000", "$0.000000"],
    ["0.0000004", "$0.000000"],
    ["0.0000005", "$0.000001"],
    ["1.9999995", "$2.000000"],
    ["9007199254740993.1234567890", "$9007199254740993.123457"],
    ["-12345678901234567890.9999995", "$-12345678901234567891.000000"],
    ["1e-7", "$0.000000"],
  ])(
    "rounds %s only for the table without Number precision loss",
    (value, formatted) => {
      expect(money(value)).toBe(formatted);
    },
  );

  it.each([null, undefined, "", "not-a-number", "Infinity"])(
    "renders absent or invalid money %s as missing",
    (value) => {
      expect(money(value)).toBe("—");
    },
  );

  it("distinguishes zero tokens and latency from missing values", () => {
    expect(tokenCount(0)).toBe("0");
    expect(tokenCount(127100)).toBe("127,100");
    expect(tokenCount(null)).toBe("—");
    expect(tokenCount(undefined)).toBe("—");
    expect(latency(0)).toBe("0.00s");
    expect(latency(4210)).toBe("4.21s");
    expect(latency(null)).toBe("—");
    expect(latency(undefined)).toBe("—");
  });
});
