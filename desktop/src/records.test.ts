import { describe, expect, it } from "vitest";
import {
  cacheTokenCount,
  defaultRecordColumns,
  latency,
  latencyTone,
  modelRoute,
  money,
  tokenCount,
  formatUsageOutputRate,
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

describe("record performance metrics", () => {
  it.each([
    [0, "0"],
    [1, "1"],
    [999, "999"],
    [1000, "1.0K"],
    [1250, "1.3K"],
    [127100, "127.1K"],
    [999999, "1000.0K"],
    [1000000, "1.0M"],
    [1250000, "1.3M"],
    [127100000, "127.1M"],
    [null, "—"],
    [undefined, "—"],
    [Number.NaN, "—"],
    [Number.POSITIVE_INFINITY, "—"],
  ] as const)("formats cache count %s as %s", (value, expected) => {
    expect(cacheTokenCount(value)).toBe(expected);
  });

  it.each([
    ["first", 0, "good"],
    ["first", 9999, "good"],
    ["first", 10000, "warn"],
    ["first", 29999, "warn"],
    ["first", 30000, "slow"],
    ["first", 59999, "slow"],
    ["first", 60000, "critical"],
    ["total", 0, "good"],
    ["total", 59999, "good"],
    ["total", 60000, "warn"],
    ["total", 179999, "warn"],
    ["total", 180000, "slow"],
    ["total", 299999, "slow"],
    ["total", 300000, "critical"],
  ] as const)(
    "classifies %s latency %s at its boundary",
    (metric, value, expected) => {
      expect(latencyTone(value, metric)).toBe(expected);
    },
  );

  it.each([
    null,
    undefined,
    -1,
    Number.NaN,
    Number.POSITIVE_INFINITY,
    Number.NEGATIVE_INFINITY,
  ])("keeps invalid latency %s unknown for both metrics", (value) => {
    expect(latencyTone(value, "first")).toBe("unknown");
    expect(latencyTone(value, "total")).toBe("unknown");
  });

  type OutputRateFields = Omit<
    Partial<UsageRecord>,
    "request_type" | "native_request_type"
  > & {
    request_type?: string | null;
    native_request_type?: string | null;
  };
  const outputRate = (fields: OutputRateFields) =>
    formatUsageOutputRate({
      output_tokens: 1000,
      duration_ms: 60000,
      image_count: 0,
      image_output_tokens: 0,
      billing_mode: "token",
      ...fields,
    } as UsageRecord);

  it.each([
    [undefined, "16.7 tok/s"],
    [null, "16.7 tok/s"],
    ["sync", "16.7 tok/s"],
    ["stream", "16.7 tok/s"],
    ["ws_v2", "16.7 tok/s"],
    ["cyber", "16.7 tok/s"],
    ["", "16.7 tok/s"],
  ] as const)(
    "formats output rate for missing or supported request type %s",
    (request_type, expected) => {
      expect(outputRate({ request_type })).toBe(expected);
    },
  );

  it("uses the native request type when the legacy projection would be misleading", () => {
    expect(outputRate({ request_type: "sync", native_request_type: "cyber" })).toBe(
      "16.7 tok/s",
    );
    expect(outputRate({ request_type: "sync", native_request_type: "live" })).toBe(
      "—",
    );
  });

  it("uses total duration and never subtracts first-token timing", () => {
    expect(
      outputRate({
        request_type: "stream",
        output_tokens: 120,
        duration_ms: 70000,
        first_token_ms: 10000,
      }),
    ).toBe("1.7 tok/s");
  });

  it.each([
    { output_tokens: null },
    { output_tokens: 0 },
    { output_tokens: -1 },
    { output_tokens: Number.NaN },
    { output_tokens: Number.POSITIVE_INFINITY },
    { duration_ms: null },
    { duration_ms: 0 },
    { duration_ms: -1 },
    { duration_ms: Number.NaN },
    { duration_ms: Number.POSITIVE_INFINITY },
    { request_type: "async" },
  ])("renders invalid output rate inputs as missing: %o", (fields) => {
    expect(outputRate(fields)).toBe("—");
  });

  it.each([
    { image_count: 1 },
    { image_output_tokens: 1 },
    { billing_mode: "image" },
    { request_type: "live" },
    { native_request_type: "live" },
  ])("does not show output rate for image/live rows: %o", (fields) => {
    expect(outputRate(fields)).toBe("—");
  });

  it("uses the nine explicit default columns without the removed type column", () => {
    expect(defaultRecordColumns).toEqual([
      "api_key",
      "account",
      "model",
      "reasoning",
      "tokens",
      "cost",
      "latency",
      "user_agent",
      "ip",
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
