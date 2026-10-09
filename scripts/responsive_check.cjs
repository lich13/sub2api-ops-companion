#!/usr/bin/env node
"use strict";

// CI-only browser checks against the Vite development transport and its synthetic data.
// Playwright is supplied through NODE_PATH; no package or browser is installed here.
const { spawn } = require("node:child_process");
const { once } = require("node:events");
const net = require("node:net");
const path = require("node:path");
const { isDeepStrictEqual } = require("node:util");

const ORIGIN = "http://127.0.0.1:4173";
const PORT = 4173;
const TIMEOUT = 10000;
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const failures = [];
let server;
let browser;
let serverExit;
let serverError;
let cleanupPromise;

class CheckFailure extends Error {
  constructor(reason, evidence = {}) {
    super(reason);
    this.evidence = evidence;
  }
}

function requireCondition(condition, reason, evidence) {
  if (!condition) throw new CheckFailure(reason, evidence);
}

function localUrl(value, websocket = false) {
  try {
    const url = new URL(value);
    return url.hostname === "127.0.0.1" && url.port === String(PORT)
      && url.protocol === (websocket ? "ws:" : "http:");
  } catch {
    return false;
  }
}

async function startServer() {
  requireCondition(process.platform !== "win32", "posix-runner-required");
  const probe = net.createServer();
  await new Promise((resolve, reject) => {
    probe.once("error", () => reject(new CheckFailure("preview-port-unavailable")));
    probe.listen(PORT, "127.0.0.1", resolve);
  });
  await new Promise((resolve) => probe.close(resolve));
  server = spawn("pnpm", ["dev", "--host", "127.0.0.1", "--port", String(PORT), "--strictPort"], {
    cwd: path.resolve(__dirname, "../desktop"),
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  // Drain Vite output without exposing paths or arbitrary application logs.
  server.stdout.resume();
  server.stderr.resume();
  server.once("error", (error) => { serverError = error.code || "spawn-error"; });
  server.once("exit", (code, signal) => { serverExit = { code, signal }; });
  const deadline = Date.now() + 45000;
  while (Date.now() < deadline) {
    requireCondition(!serverError && !serverExit, "preview-process-failed", {
      processCode: serverError || serverExit?.code,
      signal: serverExit?.signal,
    });
    try {
      const response = await fetch(ORIGIN, { redirect: "error", signal: AbortSignal.timeout(1000) });
      if (response.ok) {
        const html = await response.text();
        if (html.includes("/@vite/client")) return;
      }
    } catch {
      // Only the bounded startup wait is retried; a test failure is never rerun.
    }
    await delay(200);
  }
  throw new CheckFailure("preview-startup-timeout");
}

function signalServer(signal) {
  if (!server?.pid) return;
  try {
    process.kill(-server.pid, signal);
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  cleanupPromise = (async () => {
    const cleanupErrors = [];
    try {
      if (browser) await browser.close();
    } catch {
      cleanupErrors.push("browser-close-failed");
    } finally {
      if (server?.pid) {
        try {
          const exited = serverExit ? Promise.resolve() : once(server, "exit").catch(() => {});
          signalServer("SIGTERM");
          await Promise.race([exited, delay(5000)]);
          // The detached process group also contains pnpm's Vite child.
          try {
            process.kill(-server.pid, 0);
            signalServer("SIGKILL");
            await Promise.race([exited, delay(1000)]);
          } catch (error) {
            if (error.code !== "ESRCH") throw error;
          }
        } catch {
          cleanupErrors.push("preview-process-cleanup-failed");
        }
      }
    }
    if (cleanupErrors.length) throw new CheckFailure("cleanup-failed", { cleanupErrors });
  })();
  return cleanupPromise;
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    process.exitCode = signal === "SIGINT" ? 130 : 143;
    void cleanup().catch(() => {
      console.error(JSON.stringify({ result: "FAIL", phase: "cleanup", reason: "signal-cleanup-failed" }));
    }).finally(() => process.exit(process.exitCode));
  });
}

async function geometry(locator) {
  if (await locator.count() !== 1) return { matches: await locator.count() };
  return locator.evaluate((element) => {
    const round = (n) => Math.round(n * 100) / 100;
    const rect = (node) => {
      const r = node.getBoundingClientRect();
      return { x: round(r.x), y: round(r.y), width: round(r.width), height: round(r.height), right: round(r.right), bottom: round(r.bottom) };
    };
    const box = element.getBoundingClientRect();
    const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
    const createsFixedContainingBlock = (style) => {
      const changed = style.willChange.split(",").map((value) => value.trim());
      return ["transform", "translate", "rotate", "scale", "perspective", "filter", "backdropFilter"]
        .some((property) => style[property] && style[property] !== "none")
        || /\b(layout|paint|strict|content)\b/.test(style.contain)
        || style.contentVisibility === "auto"
        || changed.some((property) => ["transform", "translate", "rotate", "scale", "perspective", "filter", "backdrop-filter", "contain", "content-visibility"].includes(property));
    };
    let fixedRoot = null;
    for (let node = element; node; node = node.parentElement) {
      if (getComputedStyle(node).position === "fixed") {
        fixedRoot = node;
        break;
      }
    }
    let viewportFixed = !!fixedRoot;
    for (let node = fixedRoot?.parentElement; node; node = node.parentElement) {
      if (createsFixedContainingBlock(getComputedStyle(node))) {
        viewportFixed = false;
        break;
      }
    }
    // A viewport-fixed surface escapes ordinary outer overflow containers.
    // Its own scrolling/clipping ancestors still apply to its descendants.
    const clipStop = viewportFixed ? fixedRoot.parentElement : null;
    const clips = [];
    for (let node = element.parentElement; node && node !== clipStop; node = node.parentElement) {
      const style = getComputedStyle(node);
      const r = node.getBoundingClientRect();
      const horizontal = /auto|scroll|hidden|clip/.test(style.overflowX)
        && (box.left < r.left - 1 || box.right > r.right + 1);
      const vertical = /auto|scroll|hidden|clip/.test(style.overflowY)
        && (box.top < r.top - 1 || box.bottom > r.bottom + 1);
      if (horizontal || vertical) clips.push({ element: node.tagName.toLowerCase(), class: node.className, horizontal, vertical, ...rect(node) });
    }
    return {
      element: element.tagName.toLowerCase(), class: element.className,
      ...rect(element), viewport: { width: innerWidth, height: innerHeight },
      hit: !!hit && (hit === element || element.contains(hit)), viewportFixed, clips,
    };
  });
}

class Harness {
  constructor(page, scenario) {
    this.page = page;
    this.scenario = scenario;
    this.phase = "initialization";
    this.kind = "page";
    this.target = null;
    this.checks = 0;
  }

  async state(condition, reason, locator = this.target) {
    this.checks++;
    if (!condition) throw new CheckFailure(reason, locator ? await geometry(locator).catch(() => ({})) : {});
  }

  async visible(locator, kind) {
    this.kind = kind;
    this.target = locator;
    await locator.waitFor({ state: "visible" });
    return locator;
  }

  async within(locator, kind, { touch = this.scenario.mobile, vertical = true, scroll = true, hit = false } = {}) {
    await this.visible(locator, kind);
    if (scroll) await locator.scrollIntoViewIfNeeded();
    const bounds = await geometry(locator);
    this.checks++;
    const fail = (reason) => { throw new CheckFailure(reason, bounds); };
    if (!bounds.width || !bounds.height) fail("empty-bounds");
    if (bounds.x < -1 || bounds.right > bounds.viewport.width + 1) fail("outside-horizontal-viewport");
    if (vertical && (bounds.y < -1 || bounds.bottom > bounds.viewport.height + 1)) fail("outside-vertical-viewport");
    if (bounds.clips.some((clip) => clip.horizontal || vertical && clip.vertical)) fail("clipped-by-ancestor");
    // The surrounding label is the checkbox touch target, not the 20px glyph.
    if (touch && (bounds.width < 47.5 || bounds.height < 47.5)) fail("touch-target-under-48px");
    if (hit && !bounds.hit) fail("target-obscured");
    return bounds;
  }

  async click(locator, kind, options = {}) {
    await this.within(locator, kind, { ...options, hit: true });
    await locator.click();
  }

  async allWithin(locators, kind, options = {}) {
    const count = await locators.count();
    await this.state(count > 0, "missing-control-collection", locators);
    for (let index = 0; index < count; index++) await this.within(locators.nth(index), kind, options);
  }

  async layout() {
    this.kind = "document/body";
    this.target = null;
    const sizes = await this.page.evaluate(() => ({
      viewport: { width: innerWidth, height: innerHeight },
      document: { clientWidth: document.documentElement.clientWidth, scrollWidth: document.documentElement.scrollWidth },
      body: { clientWidth: document.body.clientWidth, scrollWidth: document.body.scrollWidth },
    }));
    this.checks++;
    requireCondition(sizes.document.scrollWidth <= sizes.document.clientWidth + 1
      && sizes.body.scrollWidth <= sizes.body.clientWidth + 1
      && sizes.body.scrollWidth <= sizes.viewport.width + 1,
    "horizontal-document-overflow", sizes);
  }

  async navigate(label, id) {
    this.phase = id;
    await this.click(this.page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: label, exact: true }), "bottom-navigation");
    await this.visible(this.page.locator(`.page-surface[data-page="${id}"]`), "page-surface");
  }
}

async function fixtureData(page) {
  return page.evaluate(async () => {
    if ("__TAURI_INTERNALS__" in window) throw new Error("native-transport-present");
    const preview = await import("/src/preview.ts");
    const state = await preview.run("get_state", {});
    const config = await preview.run("api_request", { method: "GET", path: "/config" });
    return { accounts: state.snapshot.accounts, recoveries: state.snapshot.recoveries, config, platform: state.platform };
  });
}

function configForm(page, heading) {
  return page.locator('form.settings-card').filter({ has: page.getByRole("heading", { name: heading, exact: true }) });
}

function recoveryLabel(page, form, mode, id) {
  return form.locator(".recovery-columns > fieldset")
    .filter({ has: page.locator("legend", { hasText: new RegExp(`^${mode}$`) }) })
    .locator("label.check-account").filter({ hasText: new RegExp(`#${id}$`) });
}

function fallbackRow(form, id) {
  return form.locator(".fallback-account").filter({ hasText: new RegExp(`#${id}\\s*·`) });
}

async function unchangedConfig(h, baseline) {
  const current = (await fixtureData(h.page)).config;
  await h.state(isDeepStrictEqual(current, baseline), "draft-wrote-synthetic-config");
}

function fixtureAccountActions(account) {
  const openai = account.platform === "openai" && ["oauth", "apikey"].includes(account.type);
  return {
    primary: openai ? ["模型测试", account.degradation_mark?.marked ? "取消标记" : "标记降智", "定时检测"] : [],
    more: ["测试连接", ...(account.recoverable ? ["恢复状态"] : []), ...(openai ? ["应用模板"] : []), "删除"],
  };
}

async function accountActionButtons(h, container, expected, kind) {
  const buttons = container.getByRole("button");
  await h.state(isDeepStrictEqual((await buttons.allTextContents()).map((label) => label.trim()), expected), "account-action-set-mismatch", container);
  for (const label of expected) {
    const button = container.getByRole("button", { name: label, exact: true });
    await h.state(await button.isEnabled(), "available-account-action-disabled", button);
    await h.within(button, kind, { hit: true });
  }
}

async function cancelDeleteAction(h, source, account) {
  await h.click(source.getByRole("button", { name: "删除", exact: true }), "account-delete-confirmation-entry");
  await source.waitFor({ state: "hidden" });
  const dialog = h.page.getByRole("dialog", { name: "删除 1 个账号？", exact: true });
  await h.within(dialog, "account-delete-confirmation", { touch: false, scroll: false });
  await h.state(await dialog.locator(".delete-list strong").textContent() === account.name, "account-action-target-mismatch", dialog);
  // Opening and cancelling this confirmation exercises action dismissal without submitting an operation.
  await h.click(dialog.getByRole("button", { name: "取消", exact: true }), "account-delete-cancel");
  await dialog.waitFor({ state: "hidden" });
}

async function groupMenuChecks(h, fixture, manager) {
  const { page } = h;
  const tabs = manager.getByRole("tablist", { name: "分组平台" });
  const search = manager.getByRole("textbox", { name: "搜索分组账号", exact: true });
  const menus = page.locator(".account-action-popup");
  const triggerFor = (account) => manager.getByRole("button", { name: `${account.name}操作`, exact: true });

  async function open(account, keyboard = false) {
    const trigger = triggerFor(account);
    if (keyboard) {
      await h.visible(trigger, "group-account-actions-trigger");
      await trigger.press("Enter");
    } else {
      await h.click(trigger, "group-account-actions-trigger");
    }
    const popup = page.getByRole("group", { name: `${account.name}操作`, exact: true });
    await h.within(popup, "group-account-actions-popup", { touch: false, scroll: false });
    await h.state(await menus.count() === 1, "multiple-account-menus-open", popup);
    await h.state(await popup.evaluate((element) => element.parentElement === document.body), "group-popup-inside-clipped-card", popup);
    await h.state(await trigger.getAttribute("aria-expanded") === "true"
      && await trigger.getAttribute("aria-controls") === await popup.getAttribute("id"), "group-menu-trigger-link", trigger);
    return { trigger, popup };
  }

  for (const [platform, label, otherLabel] of [["openai", "Codex", "Grok"], ["grok", "Grok", "Codex"]]) {
    h.phase = `groups/${platform}/actions`;
    const tab = tabs.getByRole("tab", { name: label, exact: true });
    await h.click(tab, "group-platform-tab");
    await h.state(await tab.getAttribute("aria-selected") === "true", "group-tab-not-selected", tab);
    const accounts = fixture.accounts.filter((account) => account.platform === platform);
    await h.state(accounts.length >= 2 && accounts.some((account) => account.type === "oauth")
      && accounts.some((account) => account.type === "apikey"), "missing-group-action-fixtures");
    if (platform === "openai") await h.state(accounts.some((account) => account.recoverable), "missing-recoverable-openai-fixture");
    for (const account of accounts) {
      await search.fill(String(account.id));
      const card = manager.locator(".group-account");
      await h.state(await card.count() === 1, "group-menu-search-not-specific", card);
      await h.within(card.locator(".group-account-select strong"), "group-account-name", { touch: false, vertical: false });
      const { trigger, popup } = await open(account);
      const actions = fixtureAccountActions(account);
      await accountActionButtons(h, popup, [...actions.primary, ...actions.more], "group-account-action");
      await h.layout();
      await page.keyboard.press("Escape");
      await popup.waitFor({ state: "hidden" });
      await h.state(await trigger.getAttribute("aria-expanded") === "false", "group-menu-escape-still-expanded", trigger);
      await h.state(await trigger.evaluate((element) => document.activeElement === element), "group-menu-focus-not-restored", trigger);
    }

    h.phase = `groups/${platform}/single-menu`;
    await search.fill("");
    const first = await open(accounts[0]);
    // Keyboard activation remains reachable when the first popup covers the neighbouring trigger.
    const second = await open(accounts[1], true);
    await first.popup.waitFor({ state: "hidden" });
    await h.state(await first.trigger.getAttribute("aria-expanded") === "false", "previous-group-menu-still-expanded", first.trigger);

    h.phase = `groups/${platform}/outside-dismissal`;
    // The positioned popup has an 8px viewport inset; this neutral corner remains outside it.
    await page.mouse.click(1, 1);
    await second.popup.waitFor({ state: "hidden" });
    await h.state(await menus.count() === 0 && await second.trigger.getAttribute("aria-expanded") === "false", "group-menu-outside-still-open", second.trigger);

    h.phase = `groups/${platform}/platform-dismissal`;
    await search.fill(String(accounts[0].id));
    const switching = await open(accounts[0]);
    const otherTab = tabs.getByRole("tab", { name: otherLabel, exact: true });
    await h.visible(otherTab, "group-platform-tab");
    await otherTab.press("Enter");
    await switching.popup.waitFor({ state: "hidden" });
    await h.state(await menus.count() === 0 && await otherTab.getAttribute("aria-selected") === "true", "group-menu-survived-platform-switch", otherTab);
    await h.click(tab, "group-platform-tab");
    await h.state(await menus.count() === 0 && await triggerFor(accounts[0]).getAttribute("aria-expanded") === "false", "group-menu-reopened-after-platform-switch", triggerFor(accounts[0]));

    h.phase = `groups/${platform}/action-dismissal`;
    await search.fill(String(accounts[0].id));
    const action = await open(accounts[0]);
    await cancelDeleteAction(h, action.popup, accounts[0]);
    await h.state(await menus.count() === 0 && await action.trigger.getAttribute("aria-expanded") === "false", "group-menu-action-still-open", action.trigger);
    await h.state(await manager.locator('.group-account-select[aria-pressed="true"]').count() === 0
      && await manager.locator(".group-draft-panel").count() === 0, "group-menu-changed-membership-draft", manager);
    await search.fill("");
    await h.layout();
  }
  await h.state(isDeepStrictEqual((await fixtureData(page)).accounts, fixture.accounts), "group-menu-mutated-preview-accounts");
}

async function accountChecks(h, fixture) {
  const { page } = h;
  h.phase = "accounts";
  const surface = page.locator('.page-surface[data-page="accounts"]');
  await h.allWithin(page.locator(".bottom-nav button"), "bottom-navigation", { scroll: false, hit: true });
  const search = surface.getByPlaceholder("搜索名称或账号 ID", { exact: true });
  await h.within(search, "account-search");
  const account = fixture.accounts.find((item) => item.id === 102);
  await h.state(!!account, "missing-long-name-fixture");
  await search.fill(String(account.id));
  const card = surface.locator(".mobile-account");
  await h.visible(card, "mobile-account");
  await h.state(await card.count() === 1, "account-search-not-specific", card);
  await h.within(card.locator(".mobile-identity strong"), "long-account-name", { touch: false, vertical: false });
  const actions = fixtureAccountActions(account);
  await accountActionButtons(h, card.locator(".mobile-primary-actions"), actions.primary, "mobile-primary-action");
  await h.layout();

  h.phase = "accounts/quality-detail";
  await h.click(card.locator(".quality-badge"), "quality-badge");
  const quality = page.getByRole("dialog", { name: "账号质量明细", exact: true });
  await h.within(quality, "quality-dialog", { touch: false, scroll: false });
  await h.layout();
  await h.click(quality.getByRole("button", { name: "关闭评分", exact: true }), "quality-close");
  await quality.waitFor({ state: "hidden" });

  h.phase = "accounts/action-sheet";
  await h.click(card.getByRole("button", { name: `${account.name}操作`, exact: true }), "account-actions-trigger");
  const sheet = page.getByRole("dialog", { name: "账号操作", exact: true });
  await h.within(sheet, "account-actions-sheet", { touch: false, scroll: false });
  const more = sheet.locator("div.mobile-more-actions");
  await h.visible(more, "mobile-more-actions");
  await accountActionButtons(h, more, actions.more, "mobile-more-action");
  await h.layout();
  await h.click(sheet.getByRole("button", { name: "关闭账号操作", exact: true }), "account-actions-close");
  await sheet.waitFor({ state: "hidden" });
  await h.click(card.getByRole("button", { name: `${account.name}操作`, exact: true }), "account-actions-trigger");
  await cancelDeleteAction(h, sheet, account);
  await h.state(isDeepStrictEqual((await fixtureData(page)).accounts, fixture.accounts), "account-sheet-mutated-preview-accounts");
  await search.fill("");

  h.phase = "accounts/filters";
  await h.click(surface.getByRole("button", { name: "筛选账号", exact: true }), "account-filters-trigger");
  const filters = page.getByRole("dialog", { name: "账号筛选", exact: true });
  await h.allWithin(filters.getByRole("combobox"), "account-filter-select");
  await h.layout();
  await h.click(filters.getByRole("button", { name: "关闭筛选", exact: true }), "account-filters-close");
  await filters.waitFor({ state: "hidden" });

  h.phase = "groups/activity";
  await h.click(surface.getByRole("button", { name: "分组动态", exact: true }), "group-activity-trigger");
  const activity = page.getByRole("dialog", { name: "分组动态", exact: true });
  await h.within(activity, "group-activity-surface", { touch: false, scroll: false });
  await h.allWithin(activity.locator(".compact-identity > strong"), "compact-group-account", { touch: false, vertical: false });
  await h.layout();
  await h.click(activity.getByRole("button", { name: "关闭分组动态", exact: true }), "group-activity-close");
  await activity.waitFor({ state: "hidden" });

  h.phase = "groups/management";
  await h.click(surface.getByRole("button", { name: "分组管理", exact: true }), "group-management-entry");
  const manager = page.locator(".group-manager");
  await h.visible(manager, "group-manager");
  const tabs = manager.getByRole("tablist", { name: "分组平台" });
  await h.allWithin(tabs.getByRole("tab"), "group-platform-tab");
  const groupSearch = manager.getByRole("textbox", { name: "搜索分组账号" });
  await h.within(groupSearch, "group-account-search");
  await groupSearch.fill("101");
  const choice = manager.locator(".group-account-select");
  await h.state(await choice.count() === 1, "group-search-not-specific", choice);
  await h.click(choice, "group-account-select");
  await h.allWithin(manager.locator(".group-destination-bar button"), "group-draft-destination");
  await h.layout();
  await h.click(manager.getByRole("button", { name: "取消选择", exact: true }), "group-selection-cancel");
  await groupSearch.fill("");
  await groupMenuChecks(h, fixture, manager);
  await h.click(manager.getByRole("button", { name: "返回账号", exact: true }), "group-back");
  await manager.waitFor({ state: "hidden" });
}

async function recordChecks(h) {
  const { page } = h;
  await h.navigate("记录", "records");
  const records = page.getByRole("region", { name: "调用记录", exact: true });
  await h.visible(records.locator(".mobile-record").first(), "mobile-record");
  await h.layout();
  h.phase = "records/date-picker";
  await h.click(records.getByRole("button", { name: "时间范围", exact: true }), "record-date-trigger");
  const date = page.getByRole("dialog", { name: "时间范围", exact: true });
  await h.within(date, "record-date-dialog", { touch: false, scroll: false });
  await h.allWithin(date.locator(".records-date-presets button"), "record-date-preset");
  await h.allWithin(date.locator("input"), "record-date-input");
  await h.layout();
  await h.click(date.getByRole("button", { name: "关闭时间范围", exact: true }), "record-date-close");
  await date.waitFor({ state: "hidden" });

  h.phase = "records/filter-sheet";
  await h.click(records.getByRole("button", { name: "筛选记录", exact: true }), "record-filters-trigger");
  const filters = page.getByRole("dialog", { name: "筛选记录", exact: true });
  await h.allWithin(filters.locator(".record-filter-trigger"), "record-filter-trigger");
  await h.within(filters.getByRole("textbox", { name: "模型筛选" }), "record-model-filter");
  await h.within(filters.locator(".records-mismatch-filter"), "record-mismatch-label");
  await h.layout();
  await h.click(filters.getByRole("button", { name: "关闭记录筛选", exact: true }), "record-filters-close");
  await filters.waitFor({ state: "hidden" });

  h.phase = "records/columns";
  const columns = records.locator("details.records-column-menu");
  await h.click(columns.locator("summary"), "record-columns-summary");
  await h.state(await columns.getAttribute("open") !== null, "columns-did-not-open", columns);
  await h.allWithin(columns.locator(".records-column-options > label"), "record-column-label");
  await h.layout();
  await h.click(columns.getByRole("button", { name: "关闭显示字段", exact: true }), "record-columns-close");
  await h.state(await columns.getAttribute("open") === null, "columns-did-not-close", columns);

  h.phase = "records/detail";
  const missingFirstToken = records.locator(".mobile-record").filter({
    has: page.locator('.mobile-record-open[aria-label="查看记录 #196"]'),
  });
  await h.visible(missingFirstToken, "record-without-first-token");
  const compactTps = missingFirstToken.locator(".record-tps");
  await h.state(/4\.98\s*tok\/s/.test((await compactTps.textContent()) || ""), "tps-requires-first-token", compactTps);
  await h.click(missingFirstToken.locator(".mobile-record-open"), "record-missing-first-detail-trigger");
  const detail = page.getByRole("dialog", { name: "调用记录详情", exact: true });
  await h.within(detail, "record-detail", { touch: false, scroll: false });
  await h.visible(detail.locator(".record-detail-section").first(), "record-detail-content");
  const tpsValue = detail.locator("dt").filter({ hasText: /^TPS$/ }).locator("xpath=following-sibling::dd[1]");
  await h.state((await tpsValue.textContent())?.includes("4.98 tok/s"), "detail-tps-requires-first-token", tpsValue);
  await h.layout();
  await h.click(detail.getByRole("button", { name: "关闭详情", exact: true }), "record-detail-close");
  await detail.waitFor({ state: "hidden" });
}

async function eventChecks(h, fixture) {
  const { page } = h;
  await h.navigate("事件", "events");
  const events = page.locator(".events-view");
  const tabs = events.getByRole("tablist", { name: "事件类型" });
  await h.state(await tabs.getByRole("tab").count() === 3, "event-tab-count", tabs);
  for (const [id, label] of [["degradation", "降智错误"], ["other", "其他错误"], ["recoveries", "恢复历史"]]) {
    h.phase = `events/${id}`;
    const tab = tabs.getByRole("tab", { name: label, exact: true });
    await h.allWithin(tabs.getByRole("tab"), "event-tab", { scroll: false, hit: true });
    await h.click(tab, "event-tab");
    await h.state(await tab.getAttribute("aria-selected") === "true", "event-tab-not-selected", tab);
    await h.state(await tab.getAttribute("aria-controls") === `${id}-panel`, "event-tab-panel-link", tab);
    const panel = events.locator(`#${id}-panel`);
    await h.visible(panel, "event-panel");
    await h.state(await events.locator('[role="tabpanel"]:visible').count() === 1, "multiple-visible-event-panels", panel);
    if (id === "degradation") {
      // The current fixture has no degradation error; its empty state is deliberate.
      await h.visible(panel.getByText("暂无错误记录", { exact: true }), "degradation-empty-state");
    } else if (id === "other") {
      await h.click(panel.locator(".error-row").first(), "error-detail-trigger");
      const drawer = page.locator(".drawer-backdrop > .drawer");
      await h.within(drawer, "error-detail", { touch: false, scroll: false });
      await h.layout();
      await h.click(drawer.getByRole("button", { name: "关闭错误详情", exact: true }), "error-detail-close");
      await drawer.waitFor({ state: "hidden" });
    } else {
      const rows = panel.locator(".recovery-table tbody tr");
      await h.visible(rows.first(), "recovery-history-row");
      const columns = ["账号", "恢复类型", "验证方式", "测试模型", "用卡时间", "验证通过时间", "恢复确认时间"];
      await h.state(isDeepStrictEqual(await panel.locator(".recovery-table th").allTextContents(), columns), "recovery-history-column-labels", panel);
      const recoveries = [...fixture.recoveries].sort((a, b) => b.id - a.id);
      await h.state(await rows.count() === recoveries.length && recoveries.some((item) => item.kind === "reset_credit")
        && recoveries.some((item) => item.kind !== "reset_credit"), "missing-recovery-kind-fixtures", rows);
      for (let index = 0; index < recoveries.length; index++) {
        const recovery = recoveries[index];
        const row = rows.nth(index);
        const labels = await row.locator("td").evaluateAll((cells) => cells.map((cell) => ({
          label: cell.dataset.label, rendered: getComputedStyle(cell, "::before").content,
        })));
        await h.state(isDeepStrictEqual(labels.map((item) => item.label), columns)
          && labels.every((item) => item.rendered.includes(item.label) || item.rendered === "attr(data-label)"),
        "recovery-history-mobile-labels", row);
        const cellText = async (label) => (await row.locator(`td[data-label="${label}"]`).textContent())?.trim();
        const card = recovery.kind === "reset_credit";
        await h.state(await cellText("账号") === recovery.account_name && await cellText("测试模型") === recovery.model_id,
          "recovery-history-fixture-mismatch", row);
        await h.state(await cellText("恢复类型") === (card ? "用卡恢复" : "额度恢复"), "recovery-kind-label", row);
        if (card) {
          await h.state(!!recovery.reset_credit?.completed_at && ["model", "connection"].includes(recovery.reset_credit.verification_method), "missing-reset-credit-metadata", row);
          await h.state(await cellText("验证方式") === (recovery.reset_credit.verification_method === "model" ? "模型测试" : "测试连接")
            && !["", "—", "时间未知"].includes(await cellText("用卡时间")), "reset-credit-metadata-labels", row);
        } else {
          await h.state(await cellText("验证方式") === "—" && await cellText("用卡时间") === "—", "quota-recovery-has-card-metadata", row);
        }
        await h.state(!["", "—", "时间未知"].includes(await cellText("验证通过时间"))
          && !["", "—", "时间未知"].includes(await cellText("恢复确认时间")), "recovery-history-timestamps-missing", row);
      }
      await h.allWithin(panel.locator(".recovery-table td"), "recovery-history-cell", { touch: false, vertical: false });
    }
    await h.layout();
  }
}

async function featureChecks(h, baseline) {
  const { page } = h;
  await h.navigate("功能", "features");
  const oauth = configForm(page, "OAuth 恢复与测活");
  const fallback = configForm(page, "Key 调度回退");
  await h.visible(oauth, "oauth-config-form");
  await h.visible(fallback, "fallback-config-form");
  await h.state(await oauth.getByRole("button", { name: "保存", exact: true }).isDisabled(), "oauth-initial-draft-dirty");
  await h.state(await fallback.getByRole("button", { name: "保存", exact: true }).isDisabled(), "key-initial-draft-dirty");

  h.phase = "features/recovery-search-and-exclusion";
  const search = oauth.getByRole("textbox", { name: "搜索恢复账号", exact: true });
  await h.within(search, "recovery-search");
  await search.fill("101");
  await h.state(await oauth.locator(".recovery-account-list label").count() === 2, "recovery-search-not-shared", search);
  const connection = recoveryLabel(page, oauth, "测试连接", 101);
  const model = recoveryLabel(page, oauth, "模型测试", 101);
  await h.within(connection, "recovery-connection-label", { hit: true });
  await h.within(model, "recovery-model-label", { hit: true });
  await h.state(await connection.getByRole("checkbox").isChecked(), "unexpected-connection-fixture", connection);
  await h.state(!(await model.getByRole("checkbox").isChecked()), "unexpected-model-fixture", model);
  await model.getByRole("checkbox").check();
  await h.state(!(await connection.getByRole("checkbox").isChecked()) && await model.getByRole("checkbox").isChecked(), "recovery-model-not-exclusive", model);
  await search.fill("102");
  await h.state(await oauth.locator(".recovery-account-list label").count() === 2, "recovery-search-id-mismatch", search);
  await search.fill("Codex");
  await h.state(await oauth.locator(".recovery-account-list label").count() === 4, "recovery-search-name-mismatch", search);
  await search.fill("no-matching-fixture");
  await h.state(await oauth.locator(".recovery-account-list label").count() === 0, "recovery-empty-search-mismatch", search);
  await search.fill("101");
  await h.state(await model.getByRole("checkbox").isChecked(), "search-lost-recovery-draft", model);
  await connection.getByRole("checkbox").check();
  await h.state(await connection.getByRole("checkbox").isChecked() && !(await model.getByRole("checkbox").isChecked()), "recovery-connection-not-exclusive", connection);
  await model.getByRole("checkbox").check();
  await search.fill("");
  await h.allWithin(oauth.locator(".recovery-account-list label"), "recovery-checkbox-label");
  await h.within(oauth.getByRole("button", { name: "保存", exact: true }), "recovery-draft-save");
  await h.state(await oauth.getByRole("button", { name: "保存", exact: true }).isEnabled(), "recovery-draft-save-disabled");
  await unchangedConfig(h, baseline);
  await h.layout();

  h.phase = "features/key-coexist-draft";
  for (const [id, initiallyManaged] of [[389, true], [390, false]]) {
    const row = fallbackRow(fallback, id);
    const managedLabel = row.locator("label.check-account:not(.coexist-option)");
    const coexistLabel = row.locator("label.coexist-option");
    const managed = managedLabel.getByRole("checkbox");
    const coexist = coexistLabel.getByRole("checkbox");
    await h.within(managedLabel, "key-managed-label", { hit: true });
    await h.within(coexistLabel, "key-coexist-label", { hit: true });
    await h.within(coexist, "key-coexist-checkbox", { touch: false });
    await h.state(await managed.isChecked() === initiallyManaged, "unexpected-key-management-fixture", managedLabel);
    await h.state(await coexist.isDisabled() === !initiallyManaged, "key-coexist-disabled-state", coexistLabel);
    await h.state(!(await coexist.isChecked()), "unexpected-key-coexist-fixture", coexistLabel);
    if (!initiallyManaged) await managed.check();
    await h.state(await coexist.isEnabled(), "managed-key-coexist-disabled", coexistLabel);
    await coexist.check();
    await h.state(await coexist.isChecked(), "coexist-checkbox-did-not-check", coexistLabel);
    await unchangedConfig(h, baseline);
    await managed.uncheck();
    await h.state(!(await coexist.isChecked()) && await coexist.isDisabled(), "unmanaged-key-retained-coexist", coexistLabel);
    await managed.check();
    await coexist.check();
  }
  await h.within(fallback.getByRole("button", { name: "保存", exact: true }), "key-draft-save");
  await h.state(await fallback.getByRole("button", { name: "保存", exact: true }).isEnabled(), "key-draft-save-disabled");
  await unchangedConfig(h, baseline);
  await h.layout();

  await h.navigate("设置", "settings");
  const settings = page.locator('.page-surface[data-page="settings"]');
  await h.visible(settings.getByRole("heading", { name: "客户端", exact: true }), "client-settings");
  await h.visible(settings.locator('.connection-diagnostic [role="status"]'), "synthetic-connection-status");
  const bark = configForm(page, "Bark 事件推送");
  await h.within(bark.getByRole("switch", { name: "启用推送", exact: true }), "settings-switch");
  await h.within(bark.getByLabel(/^Device Key/), "settings-key-input");
  await h.layout();

  await h.navigate("功能", "features");
  h.phase = "features/draft-not-saved";
  await h.visible(fallback, "fallback-config-form");
  await unchangedConfig(h, baseline);
  await h.state(await oauth.getByRole("button", { name: "保存", exact: true }).isDisabled(), "recovery-draft-survived-remount");
  await h.state(await fallback.getByRole("button", { name: "保存", exact: true }).isDisabled(), "key-draft-survived-remount");
  await h.state(await connection.getByRole("checkbox").isChecked() && !(await model.getByRole("checkbox").isChecked()), "recovery-unsaved-selection-persisted", connection);
  for (const [id, initiallyManaged] of [[389, true], [390, false]]) {
    const row = fallbackRow(fallback, id);
    await h.state(await row.locator("label:not(.coexist-option) input").isChecked() === initiallyManaged, "key-unsaved-management-persisted", row);
    await h.state(!(await row.locator(".coexist-option input").isChecked()), "key-unsaved-coexist-persisted", row);
  }
  await h.layout();
}

async function templateChecks(h) {
  const { page } = h;
  await h.navigate("账号", "accounts");
  const surface = page.locator('.page-surface[data-page="accounts"]');
  await h.click(surface.getByRole("button", { name: "账号模板", exact: true }), "account-templates-entry");
  const dialog = page.getByRole("dialog", { name: "账号模板", exact: true });
  await h.within(dialog, "account-templates-dialog", { touch: false, scroll: false });
  const cards = dialog.locator(".account-template-grid > .account-template-card");
  await h.visible(cards.first(), "first-editable-template-card");
  await h.state(await cards.count() === 3, "template-card-count", cards);
  for (let index = 0; index < await cards.count(); index++) {
    const card = cards.nth(index);
    await h.within(card, "editable-template-card", { touch: false, vertical: false });
    await h.state(await card.getByRole("heading", { name: "白名单", exact: true }).count() === 1
      && await card.getByRole("heading", { name: "模型映射", exact: true }).count() === 1,
    "template-editor-sections-missing", card);
    await h.allWithin(card.locator(".account-template-tools .icon-button"), "template-save-delete-icon", { hit: true });
    await h.allWithin(card.locator(".account-template-row .icon-button"), "template-row-delete-icon", { hit: true });
  }

  const full = cards.nth(0);
  const longName = "响应式账号模板长名称在窄屏与深浅主题下仍可编辑保存";
  const nameInput = full.locator(".account-template-name");
  await h.within(nameInput, "template-long-name-input", { touch: false, vertical: false });
  await nameInput.fill(longName);
  const saveRenamed = full.getByRole("button", { name: `保存${longName}模板`, exact: true });
  await h.click(saveRenamed, "template-save-icon");
  const deleteRenamed = full.locator('button[title="删除模板"]');
  await page.waitForFunction((button) => !button.disabled, await deleteRenamed.elementHandle());
  await h.state(await saveRenamed.isDisabled(), "template-save-did-not-settle", saveRenamed);
  await h.state((await nameInput.inputValue()) === longName, "template-long-name-lost", nameInput);
  await h.layout();

  // Deleting every profile row must leave an intentionally empty profile, without auto-filling a row.
  for (let index = 0; index < 3; index++) {
    const remove = full.getByRole("button", { name: new RegExp(`^删除${longName}白名单 1$`) });
    await h.click(remove, "template-whitelist-row-delete");
  }
  await h.state(await full.locator(".account-template-row").count() === 0, "empty-template-auto-filled-row", full);
  await h.state((await full.locator(".account-template-fields > .muted").textContent()) === "不限制模型", "empty-template-label", full);
  await h.layout();

  await h.click(dialog.getByRole("button", { name: "新增模板", exact: true }), "template-add");
  const creating = dialog.locator(".account-template-card").filter({
    has: page.getByRole("textbox", { name: "新模板名称", exact: true }),
  });
  await h.state(await creating.count() === 1, "new-template-card-not-unique", creating);
  await h.within(creating, "new-empty-template-card", { touch: false, vertical: false });
  await h.state((await creating.locator(".account-template-fields > .muted").textContent()) === "不限制模型"
    && await creating.locator(".account-template-row").count() === 0,
  "new-template-not-empty", creating);
  const customName = "响应式空模板批量预览";
  await creating.getByRole("textbox", { name: "新模板名称" }).fill(customName);
  await h.click(creating.getByRole("button", { name: "保存新模板", exact: true }), "new-template-save-icon");
  await creating.waitFor({ state: "detached" });
  const created = dialog.locator(".account-template-grid > .account-template-card").last();
  await h.state((await created.locator(".account-template-name").inputValue()) === customName, "new-template-not-saved", created);
  await h.state((await created.locator(".account-template-fields > .muted").textContent()) === "不限制模型", "saved-empty-template-label", created);
  await h.layout();
  await h.click(dialog.getByRole("button", { name: "关闭账号模板", exact: true }), "template-close");
  await dialog.waitFor({ state: "hidden" });

  const account101 = surface.locator(".mobile-account").filter({ has: page.getByRole("checkbox", { name: "选择 Codex · 主力", exact: true }) });
  const account102 = surface.locator(".mobile-account").filter({ has: page.getByRole("checkbox", { name: "选择 Codex · 备用账号 · 用于验证长名称的显示与调度开关", exact: true }) });
  await h.click(account101.locator("label.account-check"), "batch-select-account");
  await h.click(account102.locator("label.account-check"), "batch-select-long-name-account");
  await h.layout();
  const selectionBar = surface.locator(".selection-bar");
  const selectionSummary = selectionBar.locator(".selection-summary");
  const selectionActions = selectionBar.locator(".selection-actions");
  const applyTemplateAction = selectionActions.getByRole("button", { name: "应用模板", exact: true });
  const deleteSelectedAction = selectionActions.getByRole("button", { name: "删除所选", exact: true });
  await h.state(await selectionActions.getAttribute("role") === "group"
    && await selectionActions.getAttribute("aria-label") === "批量账号操作",
  "batch-actions-group-semantics", selectionActions);
  await h.state(await selectionBar.evaluate((element) => {
    const summary = element.querySelector(".selection-summary");
    const actions = element.querySelector(".selection-actions");
    return !!summary && !!actions
      && !!(summary.compareDocumentPosition(actions) & Node.DOCUMENT_POSITION_FOLLOWING);
  }), "batch-summary-not-before-actions", selectionBar);
  await h.state((await selectionSummary.textContent())?.includes("已选 2 个账号"),
    "batch-selection-summary-count", selectionSummary);
  const actionLayout = await selectionActions.evaluate((element) => {
    const buttons = Array.from(element.querySelectorAll("button"));
    const first = buttons[0]?.getBoundingClientRect();
    const second = buttons[1]?.getBoundingClientRect();
    return {
      count: buttons.length,
      adjacent: buttons[0]?.nextElementSibling === buttons[1],
      sameRow: !!first && !!second && Math.abs(first.top - second.top) < 1,
      gap: getComputedStyle(element).columnGap,
      wrap: getComputedStyle(element).flexWrap,
      actualGap: first && second ? second.left - first.right : null,
    };
  });
  const selectionWrap = await selectionBar.evaluate((element) => getComputedStyle(element).flexWrap);
  await h.state(selectionWrap === "wrap" && actionLayout.count === 2 && actionLayout.adjacent
    && actionLayout.sameRow && actionLayout.wrap === "nowrap" && actionLayout.gap === "8px"
    && actionLayout.actualGap !== null && Math.abs(actionLayout.actualGap - 8) < 1,
  "batch-actions-not-adjacent-at-eight-pixels", selectionActions);
  await h.within(applyTemplateAction, "batch-template-entry", { vertical: false });
  await h.within(deleteSelectedAction, "batch-delete-entry", { vertical: false });
  await h.layout();
  await h.click(applyTemplateAction, "batch-template-entry");
  const batchDialog = page.getByRole("dialog", { name: "账号模板", exact: true });
  await h.within(batchDialog, "batch-template-dialog", { touch: false, scroll: false });
  const templateSelect = batchDialog.getByRole("combobox", { name: "选择账号模板", exact: true });
  await h.within(templateSelect, "batch-template-select", { hit: true });
  await templateSelect.selectOption({ label: customName });
  const apply = batchDialog.getByRole("button", { name: "应用模板", exact: true });
  await h.within(apply, "batch-template-apply", { hit: true });
  await page.waitForFunction((button) => !button.disabled, await apply.elementHandle());
  await h.state(await apply.isEnabled(), "batch-template-apply-disabled", apply);
  await h.click(apply, "batch-template-preview-submit");
  const previews = batchDialog.locator(".account-template-preview");
  await h.visible(previews.first(), "batch-template-first-preview");
  await h.state(await previews.count() === 2, "batch-template-preview-count", previews);
  const longPreview = previews.filter({ hasText: "Codex · 备用账号 · 用于验证长名称的显示与调度开关" });
  await h.within(longPreview.locator("strong"), "batch-preview-long-account-name", { touch: false, vertical: false });
  const longNameStyle = await longPreview.locator("strong").evaluate((element) => ({
    overflowWrap: getComputedStyle(element).overflowWrap,
    width: element.getBoundingClientRect().width,
    scrollWidth: element.scrollWidth,
  }));
  await h.state(longNameStyle.overflowWrap === "anywhere" && longNameStyle.width > 0, "batch-preview-long-name-not-wrappable", longPreview.locator("strong"));
  await h.allWithin(previews, "batch-template-preview-card", { touch: false, vertical: false });
  await h.layout();
  await h.click(batchDialog.getByRole("button", { name: "关闭账号模板", exact: true }), "batch-template-close");
  await batchDialog.waitFor({ state: "hidden" });

  await h.click(deleteSelectedAction, "batch-delete-entry");
  const deleteAccountsDialog = page.getByRole("dialog", { name: "删除 2 个账号？", exact: true });
  await h.within(deleteAccountsDialog, "batch-delete-confirmation", { touch: false, scroll: false });
  const deleteConfirmation = await deleteAccountsDialog.textContent() || "";
  await h.state(deleteConfirmation.includes("删除后无法撤销")
    && deleteConfirmation.includes("Codex · 主力")
    && deleteConfirmation.includes("Codex · 备用账号"),
  "batch-delete-confirmation-missing-selected-accounts", deleteAccountsDialog);
  await h.click(deleteAccountsDialog.getByRole("button", { name: "取消", exact: true }), "batch-delete-cancel");
  await deleteAccountsDialog.waitFor({ state: "hidden" });
  await h.layout();

  await h.click(surface.getByRole("button", { name: "账号模板", exact: true }), "account-templates-reopen");
  const deleteDialog = page.getByRole("dialog", { name: "账号模板", exact: true });
  await h.within(deleteDialog, "delete-template-dialog", { touch: false, scroll: false });
  const deleteCard = deleteDialog.locator(".account-template-grid > .account-template-card").filter({
    has: page.getByRole("button", { name: `删除${customName}模板`, exact: true }),
  });
  await h.visible(deleteCard, "deletable-template-card");
  await h.state(await deleteCard.count() === 1, "deletable-template-card-not-unique", deleteCard);
  await h.within(deleteCard.locator('.account-template-tools button[title="删除模板"]'), "template-delete-icon", { hit: true });
  await h.click(deleteCard.locator('.account-template-tools button[title="删除模板"]'), "template-delete-icon");
  await h.click(deleteCard.getByRole("button", { name: "确认删除", exact: true }), "template-delete-confirm");
  await deleteCard.waitFor({ state: "detached" });
  await h.state(await deleteDialog.locator(".account-template-grid > .account-template-card").count() === 3, "deleted-template-remained-or-was-refilled", deleteDialog.locator(".account-template-grid"));
  await h.layout();
  await h.click(deleteDialog.getByRole("button", { name: "关闭账号模板", exact: true }), "delete-template-close");
  await deleteDialog.waitFor({ state: "hidden" });

  await page.goto(`${ORIGIN}/?mobile=1&scenario=empty`, { waitUntil: "domcontentloaded" });
  h.phase = "templates/empty-account-set";
  await h.visible(page.locator('.page-surface[data-page="accounts"]'), "empty-account-surface");
  const emptySurface = page.locator('.page-surface[data-page="accounts"]');
  await h.click(emptySurface.getByRole("button", { name: "账号模板", exact: true }), "empty-account-templates-entry");
  const emptyDialog = page.getByRole("dialog", { name: "账号模板", exact: true });
  await h.within(emptyDialog, "empty-account-template-dialog", { touch: false, scroll: false });
  const accountSelect = emptyDialog.getByRole("combobox", { name: "选择应用账号", exact: true });
  await h.visible(accountSelect, "empty-account-selection");
  await h.state(await accountSelect.count() === 1 && await accountSelect.locator("option").count() === 1
    && await accountSelect.inputValue() === "0", "empty-account-set-selected-account", accountSelect);
  await h.state(await emptyDialog.getByRole("button", { name: "应用模板", exact: true }).isDisabled(), "empty-account-set-allows-apply");
  await h.state(await emptyDialog.locator(".account-template-preview").count() === 0, "empty-account-set-rendered-preview", emptyDialog);
  await h.layout();
}

async function desktopMenuChecks(h, fixture) {
  const { page } = h;
  h.phase = "desktop/compact-more";
  const table = page.locator(".accounts-table");
  await h.visible(table, "desktop-account-table");
  await h.layout();
  for (const id of [102, 391]) {
    const account = fixture.accounts.find((item) => item.id === id);
    await h.state(!!account, "missing-desktop-fixture");
    const label = `${account.name}更多操作`;
    const trigger = table.getByRole("button", { name: label, exact: true });
    await h.click(trigger, "compact-more-trigger", { touch: false });
    const popup = page.getByRole("group", { name: label, exact: true });
    await h.within(popup, "compact-more-popup", { touch: false, scroll: false });
    await h.state(await popup.evaluate((element) => element.parentElement === document.body), "more-popup-inside-clipped-table", popup);
    await h.allWithin(popup.getByRole("button"), "compact-more-action", { touch: false, scroll: false, hit: true });
    await h.layout();
    await page.keyboard.press("Escape");
    await popup.waitFor({ state: "hidden" });
    await h.state(await trigger.getAttribute("aria-expanded") === "false", "more-trigger-still-expanded", trigger);
    await h.state(await trigger.evaluate((element) => document.activeElement === element), "more-focus-not-restored", trigger);
  }
  await h.click(page.locator(".sidebar nav").getByRole("button", { name: "分组", exact: true }), "desktop-group-entry", { touch: false });
  const manager = page.locator(".group-manager");
  await h.visible(manager, "group-manager");
  await groupMenuChecks(h, fixture, manager);
}

async function runScenario(scenario) {
  const context = await browser.newContext({
    viewport: scenario.viewport, colorScheme: scenario.theme,
    hasTouch: scenario.mobile, isMobile: scenario.mobile,
    reducedMotion: "reduce", serviceWorkers: "block", locale: "zh-CN",
  });
  const blocked = [];
  const pageErrors = [];
  let h;
  try {
    await context.route("**/*", (route) => {
      if (localUrl(route.request().url())) return route.continue();
      blocked.push({ type: route.request().resourceType() });
      return route.abort("blockedbyclient");
    });
    await context.routeWebSocket("**/*", (socket) => {
      if (localUrl(socket.url(), true)) socket.connectToServer();
      else {
        blocked.push({ type: "websocket" });
        socket.close();
      }
    });
    const page = await context.newPage();
    page.setDefaultTimeout(TIMEOUT);
    page.setDefaultNavigationTimeout(30000);
    page.on("pageerror", (error) => pageErrors.push(error.name));
    h = new Harness(page, scenario);
    await page.goto(`${ORIGIN}/${scenario.mobile ? "?mobile=1" : ""}`, { waitUntil: "domcontentloaded" });
    await h.visible(page.locator('.page-surface[data-page="accounts"]'), "initial-account-surface");
    await h.visible(page.locator(".preview-label"), "synthetic-preview-indicator");
    const initialFont = await page.locator("html").evaluate((element) => parseFloat(getComputedStyle(element).fontSize));
    if (scenario.largeFont) await page.addStyleTag({ content: "html { font-size: 125% !important; }" });
    const presentation = await page.locator("html").evaluate((element) => ({
      fontSize: parseFloat(getComputedStyle(element).fontSize),
      colorScheme: getComputedStyle(element).colorScheme,
      dark: matchMedia("(prefers-color-scheme: dark)").matches,
      width: innerWidth, height: innerHeight,
    }));
    await h.state(presentation.width === scenario.viewport.width && presentation.height === scenario.viewport.height, "viewport-mismatch");
    await h.state(presentation.colorScheme === scenario.theme && presentation.dark === (scenario.theme === "dark"), "theme-not-applied");
    if (scenario.largeFont) await h.state(presentation.fontSize > initialFont, "font-enlargement-not-applied");
    const fixture = await fixtureData(page);
    await h.state(fixture.platform === (scenario.mobile ? "android" : "macos"), "unexpected-preview-platform");
    if (scenario.mobile) {
      await accountChecks(h, fixture);
      await recordChecks(h);
      await eventChecks(h, fixture);
      await featureChecks(h, fixture.config);
      await templateChecks(h);
    } else {
      await desktopMenuChecks(h, fixture);
    }
    requireCondition(!blocked.length, "non-loopback-request-blocked", { blocked });
    requireCondition(!pageErrors.length, "browser-page-error", { errorTypes: pageErrors });
    requireCondition(!serverExit && !serverError, "preview-exited-during-check", { processCode: serverExit?.code || serverError });
    console.log(JSON.stringify({ result: "PASS", scenario: scenario.name, checks: h.checks, fontSize: presentation.fontSize }));
  } catch (error) {
    const evidence = error instanceof CheckFailure ? error.evidence
      : h?.target ? await geometry(h.target).catch(() => ({})) : {};
    const failure = {
      result: "FAIL", scenario: scenario.name, viewport: scenario.viewport,
      phase: h?.phase || "initialization", kind: h?.kind || "browser",
      reason: error instanceof CheckFailure ? error.message : error.name === "TimeoutError" ? "browser-action-timeout" : "browser-action-failed",
      ...evidence, blocked, pageErrorCount: pageErrors.length,
    };
    failures.push(failure);
    console.error(JSON.stringify(failure));
  } finally {
    await context.close();
  }
}

async function main() {
  const { chromium } = require("playwright");
  await startServer();
  browser = await chromium.launch({ headless: true });
  const viewports = [
    { width: 320, height: 640 }, { width: 360, height: 800 },
    { width: 393, height: 852 }, { width: 430, height: 932 },
    { width: 844, height: 390 },
  ];
  let total = 0;
  for (const mobile of [true, false]) {
    for (const viewport of mobile ? viewports : [{ width: 960, height: 540 }]) {
      for (const theme of ["light", "dark"]) {
        for (const largeFont of [false, true]) {
          const name = `${mobile ? "mobile" : "desktop"}-${viewport.width}x${viewport.height}-${theme}-font${largeFont ? "125" : "default"}`;
          await runScenario({ name, viewport, theme, largeFont, mobile });
          total++;
        }
      }
    }
  }
  console.log(JSON.stringify({ result: failures.length ? "FAIL" : "PASS", scenarios: total, passed: total - failures.length, failed: failures.length }));
  if (failures.length) process.exitCode = 1;
}

void main().catch((error) => {
  process.exitCode = 1;
  console.error(JSON.stringify({ result: "FAIL", phase: "runner", reason: error instanceof CheckFailure ? error.message : "runner-failed", ...(error.evidence || {}) }));
}).finally(async () => {
  try {
    await cleanup();
  } catch (error) {
    process.exitCode = 1;
    console.error(JSON.stringify({ result: "FAIL", phase: "cleanup", reason: error instanceof CheckFailure ? error.message : "cleanup-failed", ...(error.evidence || {}) }));
  }
});
