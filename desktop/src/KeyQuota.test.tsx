// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api } from "./bridge";
import { accountOperation } from "./accountOperations";
import KeyQuotaView, { KeyQuotaSummary } from "./KeyQuota";
import {
  KeyQuotaProvider,
  KeyQuotaRefreshSettings,
  KeyQuotaSettingsDialog,
} from "./KeyQuotaSettings";
import UsageCell, { usageRefreshEvent } from "./UsageCell";
import type { Account, KeyQuota, QuotaPlan, UsageSummary } from "./types";
vi.mock("./bridge", () => ({ api: vi.fn() }));
vi.mock("./accountOperations", () => ({ accountOperation: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: Root, container: HTMLDivElement;
const plan = (values: Partial<QuotaPlan> = {}): QuotaPlan => ({
  name: "钱包余额",
  unit: "USD",
  remaining: 0,
  used: null,
  total: null,
  unlimited: false,
  reset_at: null,
  ...values,
});
const quota = (values: Partial<KeyQuota> = {}): KeyQuota => ({
  state: "ok",
  source: "sub2api",
  checked_at: Date.now() / 1000,
  success_at: Date.now() / 1000,
  retry_at: null,
  next_refresh_at: null,
  stale: false,
  error: null,
  key_status: null,
  plans: [plan()],
  subscriptions: [],
  expires_at: null,
  expires_at_unix: null,
  today: null,
  total_usage: null,
  ...values,
});
const usage = (q?: KeyQuota): UsageSummary => ({
  branch: "apikey",
  native_read: true,
  windows: [],
  actions: ["query_usage"],
  reset_credits: null,
  today: { requests: 3, tokens: 0, cost: 7, standard_cost: 0, user_cost: 0 },
  key_quota: q,
});
const account = (values: Partial<Account> = {}) =>
  ({
    id: 1,
    name: "Fixture",
    type: "apikey",
    version: "fixture-version",
    operation_versions: { usage: "fixture-identity" },
    usage: usage(),
    ...values,
  }) as Account;
const button = (label: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => (b.getAttribute("aria-label") || b.textContent)?.trim() === label,
  )!;
const click = async (label: string) => act(async () => button(label).click());
const change = async (input: HTMLInputElement, value: string) =>
  act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
const visibility = async (state: "visible" | "hidden") =>
  act(async () => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: state,
    });
    document.dispatchEvent(new Event("visibilitychange"));
  });
const advance = async (ms: number) =>
  act(async () => vi.advanceTimersByTimeAsync(ms));
const readCalls = () =>
  vi.mocked(api).mock.calls.filter(([, path]) => path === "/accounts/1/usage");
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-24T00:00:00Z"));
  vi.clearAllMocks();
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "visible",
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

it("shows all upstream plans, resets, expiry and separate local statistics inside the usage cell", async () => {
  const q = quota({
    plans: [
      plan(),
      plan({
        name: "滚动 5h",
        remaining: null,
        total: 20,
        reset_at: "2026-10-24T05:00:00Z",
      }),
    ],
    subscriptions: [
      {
        name: "First",
        status: "active",
        active: true,
        expires_at: "2026-11-01T00:00:00Z",
        plans: [plan({ name: "每日", remaining: 12, used: 8, total: 20 })],
      },
      {
        name: "Second",
        status: "expired",
        active: false,
        expires_at: null,
        plans: [plan({ name: "每周", unlimited: true })],
      },
    ],
    today: { requests: 0, tokens: 1200, cost: 4 },
    total_usage: { requests: 20, tokens: null, cost: null },
  });
  await act(async () =>
    root.render(
      <UsageCell
        account={account({ usage: usage(q) })}
        active={false}
        online
        report={vi.fn()}
        refresh={async () => {}}
      />,
    ),
  );
  const section = container.querySelector('[aria-label="上游余额"]')!;
  for (const text of [
    "剩余 0 USD",
    "First",
    "Second",
    "每日",
    "每周",
    "滚动 5h",
    "重置 10-24 13:00:00",
    "到期 11-01 08:00:00",
    "已到期",
    "无限制",
    "上游用量统计",
  ])
    expect(section.textContent).toContain(text);
  expect(section.querySelector("details")?.open).toBe(false);
  expect(container.querySelector(".usage-stats")?.textContent).toContain(
    "A $7.00",
  );
  expect(container.querySelector('[role="dialog"]')).toBeNull();
  expect(api).not.toHaveBeenCalled();
});
it("distinguishes unsupported, unknown and unlimited quota without duplicating a subscription summary", async () => {
  const render = async (value: KeyQuota) =>
    act(async () =>
      root.render(
        <KeyQuotaView
          quota={value}
          online
          busy={false}
          refresh={vi.fn()}
          settings={vi.fn()}
        />,
      ),
    );
  await render(quota({ state: "unsupported", plans: [] }));
  expect(container.textContent).toContain("不可查询");
  await render(quota({ plans: [] }));
  expect(container.textContent).toContain("余额未知");
  await render(
    quota({
      source: "newapi",
      plans: [plan({ name: "Key 额度", unlimited: true })],
    }),
  );
  expect(container.textContent).toContain("Key 配额无限制");
  expect(container.textContent).toContain("账户余额未开放");
  const daily = plan({ name: "每日", remaining: 2, total: 5 });
  await render(
    quota({
      plans: [plan({ name: "Fixture plan", remaining: 2 }), daily],
      subscriptions: [
        {
          name: "Fixture plan",
          status: null,
          active: null,
          expires_at: null,
          plans: [daily],
        },
      ],
    }),
  );
  expect(container.querySelectorAll("tbody tr")).toHaveLength(1);
  expect(container.textContent?.match(/每日/g)).toHaveLength(1);
});
it.each([
  [quota({ plans: [plan({ remaining: 0 })] }), "剩余 0 USD"],
  [quota({ plans: [], state: "unsupported" }), "不可查询"],
  [quota({ plans: [] }), "余额未知"],
  [quota({ source: "newapi", plans: [plan({ name: "Key 额度", unlimited: true })] }), "Key 配额无限制"],
] as const)("keeps the compact balance distinct from zero, unknown and unlimited", async (value, expected) => {
  await act(async () => root.render(<KeyQuotaSummary quota={value} busy={false}/>));
  expect(container.textContent).toBe(expected);
  expect(container.querySelector("button, table, details")).toBeNull();
});
it("refreshes due results only while visible and preserves the last balance through a failed read", async () => {
  let fail = false;
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/config/key-quota")
      return { seconds: 60, revision: "a".repeat(64) } as never;
    if (fail) throw new Error("fixture offline");
    return usage(quota({ plans: [plan({ remaining: 8 })] })) as never;
  });
  const render = async (active = true) =>
    act(async () =>
      root.render(
        <KeyQuotaProvider enabled online active={active}>
          <UsageCell
            account={account()}
            online
            active={active}
            report={vi.fn()}
            refresh={async () => {}}
          />
        </KeyQuotaProvider>,
      ),
    );
  await render();
  expect(readCalls()).toHaveLength(1);
  await advance(59000);
  expect(readCalls()).toHaveLength(1);
  await visibility("hidden");
  await advance(121000);
  expect(readCalls()).toHaveLength(1);
  await visibility("visible");
  expect(readCalls()).toHaveLength(2);
  fail = true;
  await advance(60000);
  expect(readCalls()).toHaveLength(3);
  expect(container.textContent).toContain("剩余 8 USD");
  expect(container.textContent).toContain("余额读取失败");
  await render(false);
  await advance(60000);
  expect(readCalls()).toHaveLength(3);
});
it("disables automatic refresh from the toolbar while allowing a manual query", async () => {
  let seconds = 60;
  vi.mocked(api).mockImplementation(async (method, path, body) => {
    if (path === "/config/key-quota") {
      if (method === "PUT") seconds = (body as { seconds: number }).seconds;
      return { seconds, revision: "a".repeat(64) } as never;
    }
    return usage(quota()) as never;
  });
  vi.mocked(accountOperation).mockResolvedValue({
    account: { usage: usage(quota()) },
    message: "已刷新",
  });
  await act(async () =>
    root.render(
      <KeyQuotaProvider enabled online active>
        <KeyQuotaRefreshSettings online />
        <UsageCell
          account={account()}
          online
          report={vi.fn()}
          refresh={async () => {}}
        />
      </KeyQuotaProvider>,
    ),
  );
  await click("余额刷新");
  await act(async () =>
    container
      .querySelector<HTMLInputElement>('input[type="checkbox"]')!
      .click(),
  );
  await click("保存");
  expect(container.querySelector('[role="dialog"]')).toBeNull();
  await advance(120000);
  expect(readCalls()).toHaveLength(1);
  await click("刷新余额");
  expect(accountOperation).toHaveBeenCalledOnce();
  expect(readCalls()).toHaveLength(2);
});
it("honors retry time for manual and automatic refresh and accepts a batch result", async () => {
  vi.mocked(api).mockImplementation(async (_method, path) =>
    path === "/config/key-quota"
      ? ({ seconds: 10, revision: "a".repeat(64) } as never)
      : (usage(
          quota({
            retry_at: Date.now() / 1000 + 120,
            error: "限流",
            stale: true,
            plans: [plan({ remaining: 6 })],
          }),
        ) as never),
  );
  await act(async () =>
    root.render(
      <KeyQuotaProvider enabled online active>
        <UsageCell
          account={account()}
          online
          connectionKey="fixture"
          report={vi.fn()}
          refresh={async () => {}}
        />
      </KeyQuotaProvider>,
    ),
  );
  expect(button("刷新余额").disabled).toBe(true);
  await advance(119000);
  expect(readCalls()).toHaveLength(1);
  await advance(1000);
  expect(readCalls()).toHaveLength(2);
  await act(async () =>
    document.dispatchEvent(
      new CustomEvent(usageRefreshEvent, {
        detail: {
          connectionKey: "fixture",
          batch: {
            items: [
              {
                account_id: 1,
                usage: usage(quota({ plans: [plan({ remaining: 9 })] })),
              },
            ],
          },
        },
      }),
    ),
  );
  expect(container.textContent).toContain("剩余 9 USD");
  expect(button("刷新余额").disabled).toBe(false);
});
it("discards an old credential response and balance after the account identity changes", async () => {
  let resolve!: (v: UsageSummary) => void;
  vi.mocked(api).mockReturnValueOnce(
    new Promise<UsageSummary>((done) => {
      resolve = done;
    }),
  );
  const render = async (version: string) =>
    act(async () =>
      root.render(
        <UsageCell
          account={account({ operation_versions: { usage: version } })}
          online
          report={vi.fn()}
          refresh={async () => {}}
        />,
      ),
    );
  await render("old");
  vi.mocked(api).mockResolvedValue(
    usage(quota({ plans: [plan({ remaining: 2 })] })),
  );
  await render("new");
  await act(async () =>
    resolve(usage(quota({ plans: [plan({ remaining: 99 })] }))),
  );
  expect(container.textContent).toContain("剩余 2 USD");
  expect(container.textContent).not.toContain("99 USD");
});
it("keeps token drafts on failure, omits blank tokens, replaces and explicitly clears them", async () => {
  const close = vi.fn(),
    saved = vi.fn();
  let fail = true;
  vi.mocked(api).mockImplementation(async (method, _path, body) => {
    if (method === "PUT" && fail) throw new Error("409 fixture conflict");
    return {
      account_id: 1,
      has_access_token:
        (body as { access_token?: string })?.access_token !== "",
      revision: "a".repeat(64),
    } as never;
  });
  const render = async () => {
    await act(async () => root.render(null));
    await act(async () =>
      root.render(
        <KeyQuotaSettingsDialog
          account={account()}
          online
          close={close}
          saved={saved}
        />,
      ),
    );
  };
  await render();
  const input = container.querySelector<HTMLInputElement>(
    'input[type="password"]',
  )!;
  expect(input.placeholder).toContain("已配置");
  await change(input, "fixture-token");
  await click("保存");
  expect(close).not.toHaveBeenCalled();
  expect(input.value).toBe("fixture-token");
  expect(container.textContent).toContain("409");
  fail = false;
  await click("保存");
  expect(close).toHaveBeenCalledTimes(1);
  expect(api).toHaveBeenLastCalledWith(
    "PUT",
    "/accounts/1/key-quota-settings",
    { expected_revision: "a".repeat(64), access_token: "fixture-token" },
  );
  await render();
  await click("保存");
  expect(api).toHaveBeenLastCalledWith(
    "PUT",
    "/accounts/1/key-quota-settings",
    { expected_revision: "a".repeat(64) },
  );
  await render();
  await click("清除账户访问令牌");
  await click("保存");
  expect(api).toHaveBeenLastCalledWith(
    "PUT",
    "/accounts/1/key-quota-settings",
    { expected_revision: "a".repeat(64), access_token: "" },
  );
  expect(close).toHaveBeenCalledTimes(3);
});
it("validates refresh intervals before saving and keeps a conflict open", async () => {
  const close = vi.fn();
  vi.mocked(api).mockImplementation(async (method) => {
    if (method === "PUT") throw new Error("409 fixture conflict");
    return { seconds: 60, revision: "a".repeat(64) } as never;
  });
  await act(async () =>
    root.render(
      <KeyQuotaSettingsDialog online close={close} saved={vi.fn()} />,
    ),
  );
  const input = container.querySelector<HTMLInputElement>(
    'input[type="number"]',
  )!;
  for (const value of ["9", "86401", "1.5"]) {
    await change(input, value);
    await act(async () =>
      container
        .querySelector("form")!
        .dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        ),
    );
  }
  expect(
    vi.mocked(api).mock.calls.filter(([method]) => method === "PUT"),
  ).toHaveLength(0);
  await change(input, "10");
  await click("保存");
  expect(close).not.toHaveBeenCalled();
  expect(input.value).toBe("10");
});
it("invalidates a child balance when its inherited parent credentials change", async () => {
  vi.mocked(api).mockImplementation(async (_method, path) =>
    path === "/config/key-quota"
      ? ({ seconds: 60, revision: "a".repeat(64) } as never)
      : (usage(quota({ plans: [plan({ remaining: 4 })] })) as never),
  );
  const child = account({ parent_account_id: 2 });
  const render = async (version: string) =>
    act(async () =>
      root.render(
        <KeyQuotaProvider
          accounts={[
            child,
            account({ id: 2, version, operation_versions: { usage: version } }),
          ]}
          enabled
          online
          active
        >
          <UsageCell
            account={child}
            online
            report={vi.fn()}
            refresh={async () => {}}
          />
        </KeyQuotaProvider>,
      ),
    );
  await render("old");
  expect(container.textContent).toContain("剩余 4 USD");
  vi.mocked(api).mockRejectedValue(new Error("fixture unavailable"));
  await render("new");
  expect(container.textContent).not.toContain("剩余 4 USD");
  expect(container.textContent).toContain("查询失败");
});
