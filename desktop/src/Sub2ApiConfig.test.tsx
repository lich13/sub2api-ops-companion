// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Sub2ApiConfig, { type ConfigDocument } from "./Sub2ApiConfig";
import { configPreview } from "./sub2apiConfigPreview";
import { api } from "./bridge";
vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root, fixture: ConfigDocument;
const button = (text: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  )!;
const input = (path: string) =>
  container.querySelector<HTMLInputElement>(`[aria-label="${path}"]`)!;
async function fill(el: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function select(label: string, value: string) {
  await act(async () => {
    const el = container.querySelector<HTMLSelectElement>(
      `[aria-label="${label}"]`,
    )!;
    el.value = value;
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
async function yaml(path: string, value: string) {
  await act(async () => {
    const el = container.querySelector<HTMLTextAreaElement>(
      `textarea[aria-label="${path}"]`,
    )!;
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const changesSent = () =>
  (
    vi.mocked(api).mock.calls.find((c) => c[1].endsWith("/validate"))?.[2] as
      | { changes?: unknown }
      | undefined
  )?.changes;
beforeEach(() => {
  fixture = structuredClone(
    configPreview("/sub2api-config", {}) as ConfigDocument,
  );
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/sub2api-config") return fixture as never;
    if (path.endsWith("/validate")) return { valid: true, errors: [] } as never;
    if (path.endsWith("/apply"))
      return { status: "restarting", message: "正在重启" } as never;
    if (path.endsWith("/operation"))
      return { status: "restored", message: "已恢复上次配置" } as never;
    throw new Error(path);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.clearAllMocks();
});
it("shows runtime version, explains overrides and keeps them read only", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain("运行 0.2.18");
  expect(input("server.port").disabled).toBe(true);
  expect(container.textContent).toContain("SERVER_PORT");
  await fill(input("搜索配置字段"), "jwt.expire_hour");
  expect(input("jwt.expire_hour").value).toBe("168");
  expect(input("jwt.expire_hour").disabled).toBe(true);
});
it("keeps edits across tab changes and preserves draft after automatic restoration", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "load_batch_cache_ttl_ms");
  await fill(input("gateway.scheduling.load_batch_cache_ttl_ms"), "300");
  await act(async () => root.render(<Sub2ApiConfig active={false} online />));
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("300");
  await act(async () => button("应用并重启").click());
  expect(container.querySelector('[role="dialog"]')?.textContent).toContain(
    "300",
  );
  await act(async () => button("确认应用并重启").click());
  expect(container.textContent).toContain("已恢复上次配置");
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("300");
  expect(
    vi.mocked(api).mock.calls.find((c) => c[1].endsWith("/apply"))?.[2],
  ).toMatchObject({
    changes: [
      {
        path: "gateway.scheduling.load_batch_cache_ttl_ms",
        remove: false,
        value: 300,
      },
    ],
  });
});
it("blocks writes for an unmatched version", async () => {
  fixture.editable = false;
  fixture.reason = "运行版本没有匹配的配置目录";
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain(fixture.reason);
  expect(button("应用并重启").disabled).toBe(true);
});
it("refreshes runtime identity on reopening and stops stale drafts", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "load_batch_cache_ttl_ms");
  await fill(input("gateway.scheduling.load_batch_cache_ttl_ms"), "300");
  await act(async () => root.render(<Sub2ApiConfig active={false} online />));
  fixture = { ...fixture, runtime_revision: "b".repeat(64) };
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain("文件或运行进程已变更");
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("300");
  expect(button("应用并重启").disabled).toBe(true);
});
it("locates field validation errors and keeps the draft", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation(async (...args) =>
    args[1].endsWith("/validate")
      ? ({
          valid: false,
          errors: [
            {
              path: "gateway.scheduling.load_batch_cache_ttl_ms",
              message: "必须大于 0",
            },
          ],
        } as never)
      : original(...args),
  );
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "load_batch_cache_ttl_ms");
  await fill(input("gateway.scheduling.load_batch_cache_ttl_ms"), "-1");
  await act(async () => button("应用并重启").click());
  expect(container.textContent).toContain("必须大于 0");
  expect(container.querySelector('[role="dialog"]')).toBeNull();
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("-1");
});
it("can recover an ongoing operation after reopening while the upstream is offline", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  let reads = 0;
  vi.mocked(api).mockImplementation(async (...args) =>
    args[1].endsWith("/operation")
      ? ({
          status: ++reads === 1 ? "restarting" : "restored",
          message: reads === 1 ? "正在重启" : "已恢复上次配置",
        } as never)
      : original(...args),
  );
  await act(async () => root.render(<Sub2ApiConfig active online={false} />));
  expect(reads).toBe(2);
  expect(container.textContent).toContain("已恢复上次配置");
});

it("edits default values directly and removes an unchanged draft without writing defaults", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(input("server.host").disabled).toBe(false);
  expect(button("应用并重启").disabled).toBe(true);
  await fill(input("server.host"), "127.0.0.1");
  await act(async () => button("检查配置").click());
  expect(changesSent()).toEqual([
    { path: "server.host", remove: false, value: "127.0.0.1" },
  ]);
  await fill(input("server.host"), "0.0.0.0");
  expect(button("应用并重启").disabled).toBe(true);
  expect(container.querySelectorAll(".config-field.changed")).toHaveLength(0);
});

it("supports explicit defaults and restoring YAML without turning an unset list into an empty list", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "server.host");
  await act(async () =>
    container
      .querySelector<HTMLElement>(".config-field-details summary")!
      .click(),
  );
  await act(async () => button("写入默认值").click());
  await act(async () => button("检查配置").click());
  expect(changesSent()).toEqual([
    { path: "server.host", remove: false, value: "0.0.0.0" },
  ]);
  await act(async () => button("恢复默认").click());
  expect(button("应用并重启").disabled).toBe(true);
  await fill(input("搜索配置字段"), "server.trusted_proxies");
  expect(input("server.trusted_proxies").value).toBe("");
  await yaml("server.trusted_proxies", "[]");
  expect(button("应用并重启").disabled).toBe(false);
  await act(async () => button("撤销全部").click());
  await fill(input("搜索配置字段"), "log.level");
  await act(async () => button("恢复默认").click());
  vi.mocked(api).mockClear();
  await act(async () => button("应用并重启").click());
  expect(changesSent()).toEqual([{ path: "log.level", remove: true }]);
  expect(container.querySelector('[role="dialog"]')?.textContent).toContain(
    "删除 YAML 项",
  );
});

it("shows global matches and lets a group narrow them without losing the original group", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), ".port");
  for (const path of ["server.port", "database.port", "redis.port"])
    expect(input(path)).not.toBeNull();
  await select("选择配置分组", "database");
  expect(input("database.port")).not.toBeNull();
  expect(input("server.port")).toBeNull();
  await fill(input("搜索配置字段"), "");
  expect(input("server.host")).not.toBeNull();
});

it("collects changed fields from multiple groups and preserves their values", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("server.host"), "127.0.0.1");
  await fill(input("搜索配置字段"), "log.level");
  await select("log.level", "warn");
  await act(async () => button("2 项修改").click());
  expect(input("server.host").value).toBe("127.0.0.1");
  expect(input("log.level").value).toBe("warn");
  expect(container.querySelectorAll(".config-field")).toHaveLength(2);
});

it("searches internal fields and opens their parent explanation", async () => {
  const child = fixture.catalog.fields.find((f) => f.internal)!;
  const parent = fixture.catalog.fields.find(
    (f) => !f.internal && child.path.startsWith(f.path),
  )!;
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), child.path);
  expect(input(parent.path)).not.toBeNull();
  expect(
    container.querySelector<HTMLDetailsElement>(".config-field-details")!.open,
  ).toBe(true);
  expect(container.textContent).toContain(child.description);
});

it("retains invalid YAML through navigation and blocks validation until corrected", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "server.trusted_proxies");
  await yaml("server.trusted_proxies", "[unterminated");
  await fill(input("搜索配置字段"), "log.level");
  await fill(input("搜索配置字段"), "server.trusted_proxies");
  expect(input("server.trusted_proxies").value).toBe("[unterminated");
  await act(async () => button("应用并重启").click());
  expect(
    vi.mocked(api).mock.calls.some((c) => c[1].endsWith("/validate")),
  ).toBe(false);
  expect(container.querySelector('[role="dialog"]')).toBeNull();
  expect(document.activeElement).toBe(input("server.trusted_proxies"));
  await yaml("server.trusted_proxies", "- 127.0.0.1");
  await act(async () => button("检查配置").click());
  expect(changesSent()).toEqual([
    { path: "server.trusted_proxies", remove: false, value: ["127.0.0.1"] },
  ]);
});

it("focuses validation failures even when another group is visible", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation(async (...args) =>
    args[1].endsWith("/validate")
      ? ({
          valid: false,
          errors: [{ path: "server.host", message: "地址无效" }],
        } as never)
      : original(...args),
  );
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("server.host"), "invalid");
  await fill(input("搜索配置字段"), "log.level");
  await act(async () => button("检查配置").click());
  expect(document.activeElement).toBe(input("server.host"));
  expect(input("server.host").getAttribute("aria-invalid")).toBe("true");
  expect(
    container.querySelector(".config-field-errors")?.textContent,
  ).toContain("地址无效");
});

it("keeps all catalog entries reachable through their groups", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  const seen = new Set<string>();
  const internal = new Set<string>();
  for (const group of fixture.catalog.groups) {
    await select("选择配置分组", group.id);
    for (const element of container.querySelectorAll<HTMLElement>(
      "[data-config-path]",
    ))
      seen.add(element.dataset.configPath!);
    for (const element of container.querySelectorAll(".config-child > code"))
      internal.add(element.textContent!);
    const expected = fixture.catalog.fields.filter(
      (f) => f.group === group.id && !f.internal,
    );
    expect(container.querySelectorAll(".config-field")).toHaveLength(
      expected.length,
    );
  }
  expect([...seen].sort()).toEqual(
    fixture.catalog.fields
      .filter((f) => !f.internal)
      .map((f) => f.path)
      .sort(),
  );
  expect([...internal].sort()).toEqual(
    fixture.catalog.fields
      .filter((f) => f.internal)
      .map((f) => f.path)
      .sort(),
  );
});

it("uses boolean controls without creating a draft when returned to the default", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "server.enable_server_timing");
  const toggle = input("server.enable_server_timing");
  expect(toggle.getAttribute("role")).toBe("switch");
  await act(async () => toggle.click());
  expect(toggle.getAttribute("aria-checked")).toBe("true");
  await act(async () => toggle.click());
  expect(button("应用并重启").disabled).toBe(true);
});

it("clears confirmed successful changes but retains drafts while offline", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  let applied = false;
  vi.mocked(api).mockImplementation(async (...args) => {
    if (args[1].endsWith("/operation"))
      return {
        status: applied ? "succeeded" : "idle",
        message: applied ? "配置已应用" : "",
      } as never;
    if (args[1].endsWith("/apply")) {
      applied = true;
      fixture.values["server.host"] = {
        ...fixture.values["server.host"],
        configured: true,
        yaml: "127.0.0.1",
        effective: "127.0.0.1",
        source: "yaml",
      };
    }
    return original(...args);
  });
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("server.host"), "127.0.0.1");
  await act(async () => root.render(<Sub2ApiConfig active online={false} />));
  expect(input("server.host").value).toBe("127.0.0.1");
  expect(button("应用并重启").disabled).toBe(true);
  expect(container.textContent).toContain("连接已断开");
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await act(async () => button("应用并重启").click());
  await act(async () => button("确认应用并重启").click());
  expect(container.querySelectorAll(".config-field.changed")).toHaveLength(0);
  expect(input("server.host").value).toBe("127.0.0.1");
  expect(container.textContent).toContain("配置已应用");
});

it("keeps a failed application draft and displays the full failure", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  let applied = false;
  const failure = `恢复失败\n${"fixture failure details ".repeat(30)}`;
  vi.mocked(api).mockImplementation(async (...args) => {
    if (args[1].endsWith("/apply")) applied = true;
    if (args[1].endsWith("/operation"))
      return {
        status: applied ? "failed" : "idle",
        message: applied ? failure : "",
      } as never;
    return original(...args);
  });
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("server.host"), "127.0.0.1");
  await act(async () => button("应用并重启").click());
  await act(async () => button("确认应用并重启").click());
  expect(container.textContent).toContain(failure);
  expect(input("server.host").value).toBe("127.0.0.1");
  expect(container.querySelectorAll(".config-field.changed")).toHaveLength(1);
});

it("keeps secret values hidden in the editor and confirmation until explicitly revealed", async () => {
  const path = "jwt.secret";
  fixture.values[path] = {
    ...fixture.values[path],
    configured: true,
    yaml: "fixture-secret",
    effective: "fixture-secret",
    source: "yaml",
    editable: true,
  };
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), path);
  expect(input(path).type).toBe("password");
  expect(container.textContent).not.toContain("fixture-secret");
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="显示字段值"]')!
      .click(),
  );
  expect(input(path).type).toBe("text");
  await fill(input(path), "fixture-new-secret");
  await act(async () => button("应用并重启").click());
  const dialog = container.querySelector('[role="dialog"]')!;
  expect(dialog.textContent).not.toContain("fixture-secret");
  expect(dialog.textContent).not.toContain("fixture-new-secret");
  expect(dialog.textContent).toContain("替换为新值（已遮蔽）");
});

it("shows unavailable and empty states with a way back to the fields", async () => {
  fixture = {
    ...fixture,
    available: false,
    editable: false,
    reason: "运行版本无法确认",
  };
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain("运行版本无法确认");
  expect(container.querySelectorAll(".config-field")).toHaveLength(0);
  fixture = { ...fixture, available: true, editable: true, reason: "" };
  await act(async () => button("刷新").click());
  await fill(input("搜索配置字段"), "fixture-no-match");
  expect(container.textContent).toContain("没有匹配的配置字段");
  await act(async () => button("清除搜索与筛选").click());
  expect(input("server.host")).not.toBeNull();
});
