// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import App from "./main";
import QualityDialog from "./AccountQuality";
import type { Account, ViewState } from "./types";
vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}),
  preview: true,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root;
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
const account = (id: number, score: number | null, grade: string) =>
  ({
    id,
    name: `Quality ${id}`,
    priority: id,
    platform: "openai",
    type: "oauth",
    status: "active",
    schedulable: true,
    available: true,
    blockers: [],
    group_ids: [],
    usage_windows: [],
    managed: false,
    version: "a".repeat(64),
    last_success_at: null,
    last_error_at: null,
    last_error_id: null,
    last_error_code: null,
    last_error_status: null,
    error_message: "",
    success_after_error: false,
    quality: {
      score,
      grade,
      reasons: [
        score == null ? "样本不足" : score > 85 ? "表现良好" : "错误偏多",
      ],
      sample_status: score == null ? "insufficient" : "complete",
      computed_at: "2026-09-26T08:00:00Z",
      data_status: "fresh",
    },
  }) as Account;
it("keeps priority default, sorts both ways with unknowns last, filters grades and opens read-only details", async () => {
  const state = {
    connected: true,
    online: true,
    error: "",
    preferences: {
      base_url: "https://qa.invalid",
      favorites: [],
      pinned: false,
      launch_at_login: false,
    },
    snapshot: {
      observed_at: "2026-09-26T08:00:00Z",
      accounts: [
        account(1, 35, "red"),
        account(2, 93, "green"),
        account(3, null, "yellow"),
      ],
      groups: [],
      errors: [],
      recoveries: [],
    },
  } as ViewState;
  vi.mocked(subscribe).mockResolvedValue(() => {});
  vi.mocked(command).mockResolvedValue(state);
  vi.mocked(api).mockImplementation(async (_method, path) =>
    path.endsWith("/quality")
      ? { ...state.snapshot!.accounts[1].quality, account_id: 2 }
      : { status: "idle", items: [] },
  );
  await act(async () => root.render(<App />));
  const names = () =>
    [...container.querySelectorAll("tbody .account-name strong")].map(
      (x) => x.textContent,
    );
  expect(names()).toEqual(["Quality 1", "Quality 2", "Quality 3"]);
  const sort = () =>
    [...container.querySelectorAll("th button")].find((x) =>
      x.textContent?.startsWith("质量"),
    ) as HTMLButtonElement;
  await act(async () => sort().click());
  expect(names()).toEqual(["Quality 2", "Quality 1", "Quality 3"]);
  await act(async () => sort().click());
  expect(names()).toEqual(["Quality 1", "Quality 2", "Quality 3"]);
  const filter = container.querySelector(
    '[aria-label="质量筛选"]',
  ) as HTMLSelectElement;
  await act(async () => {
    filter.value = "green";
    filter.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(names()).toEqual(["Quality 2"]);
  await act(async () =>
    (container.querySelector(".quality-badge") as HTMLButtonElement).click(),
  );
  expect(api).toHaveBeenCalledWith("GET", "/accounts/2/quality");
  expect(container.querySelector('[role="dialog"]')?.textContent).toContain(
    "09-26 16:00:00",
  );
  expect(container.querySelector('[role="dialog"]')?.textContent).not.toContain(
    "2026",
  );
  expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(
    true,
  );
});
it("discards old detail responses after account changes and exposes errors without writing", async () => {
  let finish: (value: unknown) => void = () => {};
  vi.mocked(api)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockRejectedValueOnce(new Error("账号已删除"));
  await act(async () =>
    root.render(
      <QualityDialog account={account(1, 35, "red")} onClose={vi.fn()} />,
    ),
  );
  await act(async () =>
    root.render(
      <QualityDialog account={account(2, 93, "green")} onClose={vi.fn()} />,
    ),
  );
  await act(async () =>
    finish({ ...account(1, 35, "red").quality, reasons: ["OLD_RESPONSE"] }),
  );
  expect(container.textContent).toContain("账号已删除");
  expect(container.textContent).not.toContain("OLD_RESPONSE");
});
