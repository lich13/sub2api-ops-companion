import { afterEach, describe, expect, it, vi } from "vitest";
import { detectionPreview } from "./detectionPreview";
import { modelTestCandidates } from "./modelTestPreview";
import { operationPreview } from "./operationPreview";
import type { Account } from "./types";

const account = (id: number, patch: Partial<Account> = {}): Account => ({
  id,
  name: `Account ${id}`,
  platform: "openai",
  type: "apikey",
  version: `version-${id}`,
  priority: id,
  group_ids: [],
  ...patch,
} as Account);

afterEach(() => vi.useRealTimers());

describe("account operation preview batches", () => {
  it("previews a confirmed delete batch and removes the selected parent with its child", async () => {
    vi.useFakeTimers();
    const accounts = [
      account(1, { }),
      account(2, { parent_account_id: 1 }),
    ];
    const body = {
      action: "delete",
      payload: { confirmed: true },
      accounts: [{ account_id: 1, expected_version: "version-1", payload: {} }],
      request_id: "preview-delete-request",
      client_id: "preview-client",
    };

    const initial = await operationPreview("POST", "/account-operations/batch", body, accounts, vi.fn());
    if (!initial) throw new Error("expected preview batch");
    expect(initial).toMatchObject({ pending: 1, items: [{ action: "delete", status: "queued", execution_mode: "native" }] });
    await vi.advanceTimersByTimeAsync(1200);
    const done = await operationPreview("GET", `/account-operations?batch_id=${initial.batch_id}`, {}, accounts, vi.fn());

    expect(done).toMatchObject({ pending: 0, items: [{ status: "completed", result: { verified: true, deleted: true } }] });
    expect(accounts).toEqual([]);
  });

  it("applies a groups batch using the scoped payload and returns sorted group IDs", async () => {
    vi.useFakeTimers();
    const accounts = [account(3, { group_ids: [2, 9] })];
    const body = {
      action: "groups",
      payload: {},
      accounts: [{ account_id: 3, expected_version: "version-3", payload: { scope_group_ids: [1, 2, 9], group_ids: [9, 1] } }],
      request_id: "preview-groups-request",
      client_id: "preview-client",
    };

    const initial = await operationPreview("POST", "/account-operations/batch", body, accounts, vi.fn());
    if (!initial) throw new Error("expected preview batch");
    expect(initial).toMatchObject({ pending: 1, items: [{ action: "groups", requested: body.accounts[0].payload }] });
    await vi.advanceTimersByTimeAsync(1200);
    const done = await operationPreview("GET", `/account-operations?batch_id=${initial.batch_id}`, {}, accounts, vi.fn());

    expect(done).toMatchObject({ pending: 0, items: [{ status: "completed", result: { verified: true, group_ids: [1, 9] } }] });
    expect(accounts[0].group_ids).toEqual([1, 9]);
  });

  it("rejects an unconfirmed delete batch before creating any jobs", async () => {
    const body = {
      action: "delete",
      payload: {},
      accounts: [{ account_id: 1, expected_version: "version-1", payload: {} }],
      request_id: "preview-unconfirmed-delete",
      client_id: "preview-client",
    };
    await expect(operationPreview("POST", "/account-operations/batch", body, [account(1)], vi.fn()))
      .rejects.toThrow("请先确认删除");
  });
});


it("recovers mixed preview accounts once while preserving scheduling and memberships", async () => {
  vi.useFakeTimers();
  const accounts = [account(8, { status: "error", schedulable: false, recoverable: true, group_ids: [4], blockers: [{ code: "disabled", label: "调度已关闭" }, { code: "status", label: "异常" }] }), account(9, { recoverable: false })];
  const emit = vi.fn();
  const body = { action: "recover", payload: {}, accounts: accounts.map(a => ({ account_id: a.id, expected_version: a.version })), request_id: "preview-recovery", client_id: "fixture-client" };
  const initial = await operationPreview("POST", "/account-operations/batch", body, accounts, emit);
  const replay = await operationPreview("POST", "/account-operations/batch", body, accounts, emit);
  expect(replay?.batch_id).toBe(initial?.batch_id);
  if (!initial || !("items" in initial)) throw new Error("expected recovery batch");
  expect(initial.items[1]).toMatchObject({ status: "completed", result: { verified: true, unchanged: true } });
  await vi.advanceTimersByTimeAsync(1200);
  const done = await operationPreview("GET", `/account-operations?batch_id=${initial?.batch_id}`, {}, accounts, emit);
  if (!done || !("items" in done)) throw new Error("expected recovery batch result");
  expect(done.items[0]).toMatchObject({ action: "recover", status: "completed", result: { verified: true } });
  expect(accounts[0]).toMatchObject({ status: "active", schedulable: false, recoverable: false, available: false, group_ids: [4], blockers: [{ code: "disabled", label: "调度已关闭" }] });
  expect(accounts).toHaveLength(2);
  expect(emit).toHaveBeenCalledOnce();
});


it("shares preview model candidates and keeps a Key override when the global model changes", () => {
  const accounts = [account(11)];
  const initial = detectionPreview("GET", "/accounts/11/model-detection", {}, accounts) as Record<string, unknown>;
  const custom = modelTestCandidates(accounts[0])[0].id;
  const saved = detectionPreview("PUT", "/accounts/11/model-detection", { expected_version: initial.version, model_override: custom, enabled: true, interval_minutes: 15 }, accounts) as Record<string, unknown>;
  expect(saved).toMatchObject({ model_source: "account", model_override: custom, model_id: custom });
  const updated = detectionPreview("GET", "/accounts/11/model-detection", {}, accounts, "fixture-new-global") as Record<string, unknown>;
  expect(updated.version).toBe(saved.version);
  expect(updated.model_id).toBe(custom);
  expect(() => detectionPreview("PUT", "/accounts/11/model-detection", { expected_version: updated.version, model_override: "missing-model", enabled: true, interval_minutes: 15 }, accounts)).toThrow("上游模型目录");
  const follow = detectionPreview("PUT", "/accounts/11/model-detection", { expected_version: updated.version, model_override: null, enabled: false, interval_minutes: 15 }, accounts, "fixture-new-global") as Record<string, unknown>;
  expect(follow).toMatchObject({ model_source: "global", model_override: null, model_id: "fixture-new-global" });
});

it("previews smart floors, idle pause and fixed compatibility", () => {
  const accounts = [account(102),account(103)];
  const path = '/accounts/102/model-detection';
  const initial = detectionPreview('GET',path,{},accounts) as Record<string,unknown>;
  const paused = detectionPreview('PUT',path,{ expected_version:initial.version, enabled:true, interval_minutes:37, schedule_mode:'smart', min_interval_minutes:17 },accounts) as Record<string,unknown>;
  expect(paused).toMatchObject({ status:'paused', recent_request_count:0, effective_interval_minutes:null, next_at:null });
  const fixed = detectionPreview('PUT',path,{ expected_version:paused.version, enabled:true, interval_minutes:37, schedule_mode:'fixed' },accounts) as Record<string,unknown>;
  expect(fixed).toMatchObject({ status:'waiting', effective_interval_minutes:37, min_interval_minutes:17 });
  const active = detectionPreview('GET','/accounts/103/model-detection',{},accounts) as Record<string,unknown>;
  const smart = detectionPreview('PUT','/accounts/103/model-detection',{ expected_version:active.version, enabled:true, interval_minutes:15, min_interval_minutes:17 },accounts) as Record<string,unknown>;
  expect(smart).toMatchObject({ schedule_mode:'smart', recent_request_count:60, effective_interval_minutes:17 });
  expect(() => detectionPreview('PUT','/accounts/103/model-detection',{ expected_version:smart.version, enabled:true, interval_minutes:15, min_interval_minutes:31 },accounts)).toThrow('5–30');
});
