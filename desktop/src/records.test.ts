import { describe, expect, it } from "vitest";
import {
  cacheTokenCount,
  defaultRecordColumns,
  latency,
  latencyTone,
  modelRoute,
  money,
  tokenCount,
  tokensPerSecond,
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

  const speed = (fields: Partial<UsageRecord>) =>
    tokensPerSecond({
      request_type: "stream",
      output_tokens: 1000,
      duration_ms: 60000,
      first_token_ms: 10000,
      ...fields,
    } as UsageRecord);

  it("measures output generation after the first token for stream, WebSocket, and sync", () => {
    expect(speed({})).toBe(20);
    expect(
      speed({
        request_type: "ws_v2",
        output_tokens: 450,
        duration_ms: 30000,
        first_token_ms: 0,
      }),
    ).toBe(15);
    expect(
      speed({
        request_type: "sync",
        output_tokens: 120,
        duration_ms: 70000,
        first_token_ms: 10000,
      }),
    ).toBe(2);
  });

  it.each([
    ["stream", 600, 30000, null, 20],
    ["ws_v2", 300, 30000, null, 10],
    ["sync", 120, 60000, null, 2],
    ["stream", 600, 30000, undefined, 20],
    ["ws_v2", 300, 30000, undefined, 10],
    ["sync", 120, 60000, undefined, 2],
  ] as const)(
    "uses total duration for %s when first-token timing is absent",
    (request_type, output_tokens, duration_ms, first_token_ms, expected) => {
      expect(
        speed({ request_type, output_tokens, duration_ms, first_token_ms }),
      ).toBe(expected);
    },
  );

  it("continues subtracting a valid first-token duration", () => {
    expect(
      speed({ request_type: "stream", output_tokens: 120, duration_ms: 70000, first_token_ms: 10000 }),
    ).toBe(2);
    expect(
      speed({ request_type: "ws_v2", output_tokens: 300, duration_ms: 30000, first_token_ms: 0 }),
    ).toBe(10);
  });

  it.each([
    { output_tokens: null },
    { output_tokens: -1 },
    { output_tokens: Number.NaN },
    { output_tokens: Number.POSITIVE_INFINITY },
    { duration_ms: null },
    { duration_ms: 0 },
    { duration_ms: -1 },
    { duration_ms: Number.NaN },
    { duration_ms: Number.POSITIVE_INFINITY },
    { first_token_ms: -1 },
    { first_token_ms: Number.NaN },
    { first_token_ms: Number.POSITIVE_INFINITY },
    { first_token_ms: 60000 },
    { first_token_ms: 60001 },
    { request_type: "sync" as const, first_token_ms: -1 },
  ])("does not fabricate TPS from invalid timing or output: %o", (fields) => {
    expect(speed(fields)).toBeNull();
  });

  it("preserves a legitimate zero output rate without hiding invalid denominators", () => {
    expect(speed({ output_tokens: 0 })).toBe(0);
    expect(speed({ output_tokens: 0, first_token_ms: 0 })).toBe(0);
    expect(
      speed({ request_type: "sync", output_tokens: 0, first_token_ms: null }),
    ).toBe(0);
    expect(speed({ output_tokens: 0, first_token_ms: 60000 })).toBeNull();
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
