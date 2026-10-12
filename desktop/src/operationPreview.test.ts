import { afterEach, describe, expect, it, vi } from "vitest";
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
  managed: false,
  ...patch,
} as Account);

afterEach(() => vi.useRealTimers());

describe("account operation preview batches", () => {
  it("previews a confirmed delete batch and removes the selected parent with its child", async () => {
    vi.useFakeTimers();
    const accounts = [
      account(1, { managed: true }),
      account(2, { parent_account_id: 1 }),
    ];
    const body = {
      action: "delete",
      payload: { confirmed: true },
      accounts: [{ account_id: 1, expected_version: "version-1", payload: { detach_managed: true } }],
      request_id: "preview-delete-request",
      client_id: "preview-client",
    };

    const initial = await operationPreview("POST", "/account-operations/batch", body, accounts, vi.fn());
    if (!initial) throw new Error("expected preview batch");
    expect(initial).toMatchObject({ pending: 1, items: [{ action: "delete", status: "queued", execution_mode: "native" }] });
    await vi.advanceTimersByTimeAsync(1200);
    const done = await operationPreview("GET", `/account-operations?batch_id=${initial.batch_id}`, {}, accounts, vi.fn());

    expect(done).toMatchObject({ pending: 0, items: [{ status: "completed", result: { verified: true, deleted: true, detached: true } }] });
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
      accounts: [{ account_id: 1, expected_version: "version-1", payload: { detach_managed: false } }],
      request_id: "preview-unconfirmed-delete",
      client_id: "preview-client",
    };
    await expect(operationPreview("POST", "/account-operations/batch", body, [account(1)], vi.fn()))
      .rejects.toThrow("请先确认删除");
  });
});
