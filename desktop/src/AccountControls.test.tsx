// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RecoveryHistory } from "./AccountControls";
import { api } from "./bridge";
import { fullTime, type Account, type Recovery } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account: Account = {
  id: 1,
  name: "Fixture OAuth",
  priority: 1,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  version: "account-version",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

const completedAt = "2026-10-01T01:10:00Z";
const verifiedAt = "2026-10-01T01:11:00Z";
const recoveredAt = "2026-10-01T01:12:00Z";
const recovery = (overrides: Partial<Recovery> = {}): Recovery => ({
  id: 1,
  account_id: account.id,
  account_name: account.name,
  model_id: "fixture-model",
  test_completed_at: verifiedAt,
  recovered_at: recoveredAt,
  legacy: false,
  ...overrides,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

async function render(records: Recovery[]) {
  await act(async () => root.render(
    <RecoveryHistory latest={records} accounts={[account]} online={false} report={vi.fn()} />,
  ));
}

const cell = (row: Element, label: string) => row.querySelector(`[data-label="${label}"]`)?.textContent;

describe("recovery history provenance", () => {
  it("keeps legacy and ordinary recoveries as quota recovery without inventing card-use evidence", async () => {
    await render([
      recovery({ id: 1 }),
      recovery({ id: 2, legacy: true, test_completed_at: null, recovered_at: null }),
    ]);

    const rows = [...container.querySelectorAll("tbody tr")];
    expect(rows).toHaveLength(2);
    for (const row of rows) {
      expect(row.textContent).toContain("额度恢复");
      expect(row.textContent).not.toContain("用卡恢复");
      expect(cell(row, "用卡时间")).not.toContain(fullTime(completedAt));
    }
    expect(rows[0].textContent).toContain("时间未知");
    expect(rows[1].textContent).toContain(fullTime(verifiedAt));
    expect(rows[1].textContent).toContain(fullTime(recoveredAt));
    expect(api).not.toHaveBeenCalled();
  });

  it.each([
    ["connection", "测试连接"],
    ["model", "模型测试"],
  ] as const)("shows completed %s card recovery with separate card-use and verification times", async (method, label) => {
    await render([recovery({
      kind: "reset_credit",
      reset_credit: { completed_at: completedAt, verification_method: method },
    })]);

    const row = container.querySelector("tbody tr")!;
    expect(row.textContent).toContain("用卡恢复");
    expect(cell(row, "用卡时间")).toBe(fullTime(completedAt));
    expect(cell(row, "验证方式")).toBe(label);
    expect(cell(row, "验证通过时间")).toBe(fullTime(verifiedAt));
    expect(cell(row, "恢复确认时间")).toBe(fullTime(recoveredAt));
  });

  it.each([
    ["missing kind", { kind: undefined }],
    ["missing metadata", { reset_credit: undefined }],
    ["legacy record", { legacy: true }],
    ["missing card-use time", { reset_credit: { completed_at: "", verification_method: "connection" } }],
    ["missing verification time", { test_completed_at: null }],
    ["missing recovery confirmation", { recovered_at: null }],
    ["unknown verification method", { reset_credit: { completed_at: completedAt, verification_method: "unknown" } }],
  ] as const)("does not label %s as a completed card recovery", async (_reason, overrides) => {
    const record = {
      ...recovery(),
      kind: "reset_credit",
      reset_credit: { completed_at: completedAt, verification_method: "connection" },
      ...overrides,
    } as Recovery;
    await render([record]);

    const row = container.querySelector("tbody tr")!;
    expect(row.textContent).toContain("额度恢复");
    expect(row.textContent).not.toContain("用卡恢复");
    expect(cell(row, "用卡时间")).not.toContain(fullTime(completedAt));
    expect(cell(row, "验证方式")).not.toBe("测试连接");
  });
});
