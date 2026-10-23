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
  const desktop = path.resolve(__dirname, "../desktop");
  const vite = path.join(path.dirname(require.resolve("vite/package.json", { paths: [desktop] })), "bin/vite.js");
  server = spawn(process.execPath, [vite, "--host", "127.0.0.1", "--port", String(PORT), "--strictPort"], {
    cwd: desktop,
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
          // Vite and its children share this owned process group.
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
    return { accounts: state.snapshot.accounts, recoveries: state.snapshot.recoveries, config, platform: state.platform, baseUrl: state.preferences.base_url };
  });
}

async function measureTypography(page, selectors) {
  return page.evaluate((entries) => {
    const measure = (node) => {
      if (!node) return null;
      const computed = getComputedStyle(node);
      const rect = node.getBoundingClientRect();
      const nativeSelect = node.tagName === "SELECT";
      // Native selects can report normal despite the declared 20px line height.
      const allowsNormalLineHeight = nativeSelect && ["auto", "menulist"].includes(computed.appearance);
      return {
        fontSize: computed.fontSize,
        lineHeight: computed.lineHeight,
        lineHeightOk: computed.lineHeight === "20px"
          || allowsNormalLineHeight && computed.lineHeight === "normal",
        appearance: computed.appearance,
        height: Math.round(rect.height * 100) / 100,
      };
    };
    return Object.fromEntries(Object.entries(entries).map(([key, selector]) => [
      key,
      [...document.querySelectorAll(selector)].map(measure),
    ]));
  }, selectors);
}

async function accountSettingsData(page, accountId) {
  return page.evaluate(async (id) => {
    const preview = await import("/src/preview.ts");
    return preview.run("api_request", { method: "GET", path: `/accounts/${id}/settings` });
  }, accountId);
}

function configForm(page, heading) {
  return page.locator('form.settings-card').filter({ has: page.getByRole("heading", { name: heading, exact: true }) });
}

function recoveryLabel(page, form, mode, id) {
  return form.locator(".recovery-columns > fieldset")
    .filter({ has: page.locator("legend", { hasText: new RegExp(`^${mode}$`) }) })
    .locator("label.check-account").filter({ hasText: new RegExp(`#${id}$`) });
}

async function unchangedConfig(h, baseline) {
  const current = (await fixtureData(h.page)).config;
  await h.state(isDeepStrictEqual(current, baseline), "draft-wrote-synthetic-config");
}

function fixtureAccountActions(account) {
  const openai = account.platform === "openai" && ["oauth", "apikey"].includes(account.type);
  return {
    primary: openai ? ["模型测试", account.degradation_mark?.marked ? "取消标记" : "标记降智", "定时检测"] : [],
    more: ["测试连接", "账号保活", ...(account.recoverable ? ["恢复状态"] : []), ...(openai ? ["应用模板"] : []), "模型白名单", "模型映射", "选择代理", "删除"],
  };
}

async function accountActionButtons(h, container, expected, kind) {
  const buttons = container.getByRole("button");
  await h.state(isDeepStrictEqual((await buttons.allTextContents()).map((label) => label.trim()), expected), "account-action-set-mismatch", container);
  const iconLayout = await buttons.evaluateAll((nodes) => nodes.map((button) => {
    const icon = button.querySelector(".account-action-icon");
    const text = icon?.nextElementSibling;
    const buttonRect = button.getBoundingClientRect();
    const iconRect = icon?.getBoundingClientRect();
    const textRect = text?.getBoundingClientRect();
    const style = icon ? getComputedStyle(icon) : null;
    return {
      iconCount: button.querySelectorAll(":scope > .account-action-icon").length,
      width: iconRect?.width ?? 0,
      minWidth: style?.minWidth ?? "",
      flexBasis: style?.flexBasis ?? "",
      textLeft: textRect ? textRect.left - buttonRect.left : -1,
      textWidth: textRect?.width ?? 0,
    };
  }));
  const firstTextLeft = iconLayout[0]?.textLeft ?? -1;
  await h.state(iconLayout.length === expected.length
    && iconLayout.every((item) => item.iconCount === 1
      && Math.abs(item.width - 12) <= 1
      && item.minWidth === "12px"
      && item.flexBasis === "12px"
      && item.textWidth > 0
      && Math.abs(item.textLeft - firstTextLeft) <= 1),
  "account-action-icon-slot-misaligned", container);
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

async function schedulingChecks(h, sheet, account) {
  const { page } = h;
  await h.state(await sheet.getByRole("button", { name: `编辑${account.name}优先级`, exact: true }).count() === 0,
    "priority-remains-editable-in-scheduling-parameters", sheet);
  const fields = [["concurrency", "并发"], ["load_factor", "负载因子"]];
  for (const [field, label] of fields) {
    h.phase = `accounts/scheduling/${field}`;
    const trigger = sheet.getByRole("button", { name: `编辑${account.name}${label}`, exact: true });
    await h.within(trigger, "scheduling-parameter-trigger", { vertical: false, hit: true });
    const before = await geometry(sheet);
    await h.click(trigger, "scheduling-parameter-trigger", { vertical: false });
    const editor = page.getByRole("dialog", { name: `编辑${label}`, exact: true });
    await h.within(editor, "numeric-editor", { touch: false, scroll: false });
    const afterOpen = await geometry(sheet);
    await h.state(Math.abs(afterOpen.width - before.width) < 1 && Math.abs(afterOpen.height - before.height) < 1,
      "numeric-editor-squeezed-action-sheet", sheet);
    const input = editor.getByRole("textbox", { name: label, exact: true });
    await h.within(input, "numeric-editor-input", { vertical: false });
    if (field === "load_factor") {
      const automatic = editor.locator("label.numeric-auto");
      await h.within(automatic, "numeric-auto-toggle", { vertical: false });
      await h.click(automatic, "numeric-auto-toggle", { vertical: false });
      await h.state(await input.isEnabled(), "numeric-auto-input-remains-disabled", input);
    }
    await input.fill("");
    await h.state((await input.inputValue()) === "", "numeric-input-cannot-clear", input);
    await input.press("Enter");
    const alert = editor.getByRole("alert");
    await h.visible(alert, "numeric-input-enter-validation");
    await h.state((await alert.textContent())?.includes(label), "numeric-input-enter-validation-mismatch", alert);
    await h.layout();
    await page.keyboard.press("Escape");
    await editor.waitFor({ state: "hidden" });
    await h.state(await trigger.evaluate((element) => document.activeElement === element), "numeric-editor-focus-not-restored", trigger);
  }
}

async function openMobileSettings(h, card, account, action, heading) {
  const { page } = h;
  await h.click(card.getByRole("button", { name: `${account.name}操作`, exact: true }), "account-settings-action-trigger");
  const sheet = page.getByRole("dialog", { name: "账号操作", exact: true });
  await h.visible(sheet, "account-actions-sheet");
  await h.click(sheet.getByRole("button", { name: action, exact: true }), "account-settings-action");
  await sheet.waitFor({ state: "hidden" });
  const dialog = page.getByRole("dialog", { name: "账号设置", exact: true });
  await h.within(dialog, "account-settings-dialog", { touch: false, scroll: false });
  await h.visible(dialog.getByRole("heading", { name: heading, exact: true }), "account-settings-heading");
  return dialog;
}

async function settingsDraftChecks(h, surface, account) {
  const { page } = h;
  const card = surface.locator(".mobile-account").filter({ has: page.getByRole("checkbox", { name: `选择 ${account.name}`, exact: true }) });
  const original = await accountSettingsData(page, account.id);

  h.phase = "accounts/settings-model-draft";
  let dialog = await openMobileSettings(h, card, account, "模型白名单", "模型配置");
  const whitelist = dialog.getByRole("textbox", { name: "白名单模型 1", exact: true });
  await h.visible(whitelist, "account-settings-whitelist");
  await whitelist.fill("draft-model");
  await h.state(await dialog.getByRole("button", { name: "保存", exact: true }).isEnabled(), "model-settings-draft-not-dirty", dialog);
  await h.click(dialog.getByRole("tab", { name: "模型映射", exact: true }), "account-settings-mapping-tab");
  await h.click(dialog.getByRole("button", { name: "添加映射", exact: true }), "account-settings-add-mapping");
  const source = dialog.getByRole("textbox", { name: "请求模型 1", exact: true });
  const target = dialog.getByRole("textbox", { name: "转发模型 1", exact: true });
  await source.fill("draft-source*");
  await target.fill("draft-target");
  await h.state(await dialog.getByRole("button", { name: "保存", exact: true }).isEnabled(), "model-mapping-draft-not-dirty", dialog);
  await h.layout();
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });
  await h.state(isDeepStrictEqual(await accountSettingsData(page, account.id), original), "model-settings-draft-wrote-preview");

  h.phase = "accounts/settings-proxy-draft";
  dialog = await openMobileSettings(h, card, account, "选择代理", "选择代理");
  const proxyOptions = dialog.locator(".proxy-options");
  await h.visible(proxyOptions, "account-settings-proxy-options");
  await proxyDiagnosticChecks(h, proxyOptions, "account-settings-proxy", {
    confirmation: dialog.getByRole("button", { name: "保存", exact: true }),
  });
  const proxyLabel = proxyOptions.locator("label").filter({ hasText: "演示代理" });
  const proxyRadio = proxyLabel.getByRole("radio");
  await h.within(proxyLabel, "account-settings-proxy-option", { vertical: false });
  await h.click(proxyLabel, "account-settings-proxy-option", { vertical: false });
  await h.state(await proxyRadio.isChecked(), "proxy-draft-not-selected", proxyLabel);
  await h.state(await dialog.getByRole("button", { name: "保存", exact: true }).isEnabled(), "proxy-draft-not-dirty", dialog);
  await h.layout();
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });
  await h.state(isDeepStrictEqual(await accountSettingsData(page, account.id), original), "proxy-settings-draft-wrote-preview");
}

async function proxyDiagnosticChecks(h, options, scope, { confirmation } = {}) {
  const proxyCards = options.locator(".proxy-option");
  const diagnosticButtons = options.locator(".proxy-diagnostic-actions button");
  await h.state(await proxyCards.count() > 0, `${scope}-proxy-card-missing`, options);
  await h.state(await diagnosticButtons.count() === await proxyCards.count() * 2,
    `${scope}-diagnostic-action-count-mismatch`, options);
  await h.allWithin(diagnosticButtons, `${scope}-diagnostic-action`, { vertical: false });
  const actionLabels = await diagnosticButtons.allTextContents();
  await h.state(actionLabels.every((label) => ["测试连接", "质量检测"].includes(label.trim())),
    `${scope}-diagnostic-action-label-mismatch`, diagnosticButtons);

  const article = proxyCards.filter({ hasText: "演示代理" });
  await h.state(await article.count() === 1, `${scope}-active-proxy-not-unique`, article);
  const test = article.getByRole("button", { name: "检测代理 1", exact: true });
  const quality = article.getByRole("button", { name: "质量检查代理 1", exact: true });
  await h.state(await test.isEnabled() && await quality.isEnabled(), `${scope}-diagnostic-action-disabled`, article);
  const selectedBefore = await options.locator('input[type="radio"]').evaluateAll((nodes) => nodes.map((node) => node.checked));

  await h.click(test, `${scope}-test-action`, { vertical: false });
  await h.visible(article.getByRole("status").first(), `${scope}-test-result`);
  await h.state((await article.textContent() || "").includes("连接成功")
    && (await article.textContent() || "").includes("192.0.2.1"), `${scope}-test-result-missing`, article);
  await h.state(isDeepStrictEqual(await options.locator('input[type="radio"]').evaluateAll((nodes) => nodes.map((node) => node.checked)), selectedBefore),
    `${scope}-test-changed-selection`, options);
  if (confirmation) await h.state(await confirmation.isDisabled(), `${scope}-test-enabled-save`, confirmation);

  await h.click(quality, `${scope}-quality-action`, { vertical: false });
  await h.visible(article.locator(".proxy-diagnostic-result").filter({ hasText: "90 分 · A" }), `${scope}-quality-result`);
  await h.state((await article.textContent() || "").includes("原生质量检测通过")
    && await article.locator("[role=\"status\"]").count() === 2, `${scope}-quality-result-missing-or-duplicated`, article);
  await h.state(isDeepStrictEqual(await options.locator('input[type="radio"]').evaluateAll((nodes) => nodes.map((node) => node.checked)), selectedBefore),
    `${scope}-quality-changed-selection`, options);
  if (confirmation) await h.state(await confirmation.isDisabled(), `${scope}-quality-enabled-save`, confirmation);
}

async function groupMenuChecks(h, fixture, manager) {
  const { page } = h;
  const tabs = manager.getByRole("tablist", { name: "分组平台" });
  const search = manager.getByRole("textbox", { name: "搜索分组账号", exact: true });
  const menus = page.locator(".account-action-popup");
  const triggerFor = (account) => manager.getByRole("button", { name: `${account.name}操作`, exact: true });

  async function open(account, keyboard = false) {
    const trigger = triggerFor(account);
    let keyboardActivation;
    if (keyboard) {
      await h.visible(trigger, "group-account-actions-trigger");
      await trigger.scrollIntoViewIfNeeded();
      await trigger.focus();
      // Flush scroll events before opening a menu that correctly dismisses on scroll.
      await page.evaluate(() => new Promise((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(resolve));
      }));
      keyboardActivation = {
        trigger: await geometry(trigger),
        scroll: await trigger.evaluate((element) => {
          const ancestors = [];
          for (let node = element.parentElement; node; node = node.parentElement) {
            if (node.scrollHeight > node.clientHeight || node.scrollWidth > node.clientWidth
              || node === document.scrollingElement) {
              ancestors.push({
                element: node.tagName.toLowerCase(), class: node.className,
                top: node.scrollTop, left: node.scrollLeft,
                height: node.scrollHeight, clientHeight: node.clientHeight,
                width: node.scrollWidth, clientWidth: node.clientWidth,
                behavior: getComputedStyle(node).scrollBehavior,
              });
            }
          }
          return { focused: document.activeElement === element, ancestors };
        }),
      };
      await trigger.press("Enter");
    } else {
      await h.click(trigger, "group-account-actions-trigger");
    }
    const popup = page.getByRole("group", { name: `${account.name}操作`, exact: true });
    try {
      await h.within(popup, "group-account-actions-popup", { touch: false, scroll: false });
    } catch (error) {
      if (keyboardActivation) error.keyboardActivation = keyboardActivation;
      throw error;
    }
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
      await h.within(card.locator(".group-account-identity strong"), "group-account-name", { touch: false, vertical: false });
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
    await h.state(await manager.locator(".group-draft-panel").count() === 0, "group-menu-changed-membership-draft", manager);
    await search.fill("");
    await h.layout();
  }
  await h.state(isDeepStrictEqual((await fixtureData(page)).accounts, fixture.accounts), "group-menu-mutated-preview-accounts");
}

async function accountChecks(h, fixture) {
  const { page } = h;
  h.phase = "accounts";
  const surface = page.locator('.page-surface[data-page="accounts"]');
  const bottomNavigation = page.locator(".bottom-nav button");
  await h.state(await bottomNavigation.count() === 5
    && isDeepStrictEqual((await bottomNavigation.allTextContents()).map((label) => label.trim()), ["账号", "记录", "事件", "功能", "设置"]),
  "mobile-navigation-count-or-order", bottomNavigation);
  await h.allWithin(bottomNavigation, "bottom-navigation", { scroll: false, hit: true });
  const mobileSort = surface.locator(".mobile-sort");
  await h.within(mobileSort, "mobile-account-sort", { touch: false, vertical: false });
  const mobileSortTypography = await measureTypography(page, {
    count: '.page-surface[data-page="accounts"] .mobile-sort > span',
    select: '.page-surface[data-page="accounts"] .mobile-sort > select',
  });
  const mobileCount = mobileSortTypography.count[0];
  const mobileSelect = mobileSortTypography.select[0];
  await h.state(mobileCount?.fontSize === "12px"
    && mobileSelect?.fontSize === "12px"
    && mobileSelect?.lineHeightOk,
  "mobile-account-sort-typography-mismatch", mobileSort);
  await h.state(mobileSelect?.height >= 48, "mobile-account-sort-select-under-48px", mobileSort);
  const search = surface.getByPlaceholder("搜索名称或账号 ID", { exact: true });
  await h.within(search, "account-search");
  const account = fixture.accounts.find((item) => item.id === 102);
  await h.state(!!account, "missing-long-name-fixture");
  await search.fill(String(account.id));
  const card = surface.locator(".mobile-account");
  await h.visible(card, "mobile-account");
  await h.state(await card.count() === 1, "account-search-not-specific", card);
  await h.within(card.locator(".mobile-identity strong"), "long-account-name", { touch: false, vertical: false });
  const priorityFooter = card.locator("footer");
  await h.visible(priorityFooter, "mobile-account-footer");
  await h.state((await priorityFooter.textContent() || "").includes(`优先级 ${account.priority}`)
    && await priorityFooter.locator("button").filter({ hasText: "优先级" }).count() === 0,
  "mobile-account-priority-is-editable", priorityFooter);
  const actions = fixtureAccountActions(account);
  await accountActionButtons(h, card.locator(".mobile-primary-actions"), actions.primary, "mobile-primary-action");
  await h.layout();

  h.phase = "accounts/action-sheet";
  await h.click(card.getByRole("button", { name: `${account.name}操作`, exact: true }), "account-actions-trigger");
  const sheet = page.getByRole("dialog", { name: "账号操作", exact: true });
  await h.within(sheet, "account-actions-sheet", { touch: false, scroll: false });
  const more = sheet.locator("div.mobile-more-actions");
  await h.visible(more, "mobile-more-actions");
  await accountActionButtons(h, more, actions.more, "mobile-more-action");
  await schedulingChecks(h, sheet, account);
  await h.layout();
  await h.click(sheet.getByRole("button", { name: "关闭账号操作", exact: true }), "account-actions-close");
  await sheet.waitFor({ state: "hidden" });
  await h.click(card.getByRole("button", { name: `${account.name}操作`, exact: true }), "account-actions-trigger");
  await cancelDeleteAction(h, sheet, account);
  await h.state(isDeepStrictEqual((await fixtureData(page)).accounts, fixture.accounts), "account-sheet-mutated-preview-accounts");
  await search.fill("");
  await settingsDraftChecks(h, surface, account);

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
  const identity = manager.locator(".group-account-identity");
  await h.state(await identity.count() === 1, "group-search-not-specific", identity);
  await h.within(identity, "group-account-identity", { touch: false, vertical: false });
  await identity.click();
  await h.state(await manager.locator(".group-draft-panel").count() === 0, "group-name-click-moved-account", manager);
  await h.allWithin(manager.getByRole("switch"), "group-account-scheduling");
  await h.layout();
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
  const compactRecord = records.locator(".mobile-record").filter({
    has: page.locator('.mobile-record-open[aria-label="查看记录 #200"]'),
  });
  await h.visible(compactRecord, "record-compact-row");
  const compactType = compactRecord.locator(".mobile-record-type");
  await h.within(compactType, "record-type-badges", { touch: false, vertical: false });
  const compactBadges = compactType.locator(".record-type-badge");
  await h.state(await compactBadges.count() === 2
    && await compactType.locator(".record-type-badge.is-compact").count() === 1
    && await compactType.locator(".record-type-badge.is-stream, .record-type-badge.is-sync").count() === 1,
  "record-type-badges-missing-or-duplicated", compactType);
  const compactLayout = await compactType.evaluate((element) => {
    const badges = [...element.querySelectorAll(".record-type-badge")];
    const parent = element.getBoundingClientRect();
    const rects = badges.map((badge) => badge.getBoundingClientRect());
    return {
      parent: { width: parent.width, height: parent.height },
      rects: rects.map((rect) => ({ left: rect.left - parent.left, right: rect.right - parent.left, top: rect.top - parent.top, bottom: rect.bottom - parent.top })),
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
    };
  });
  await h.state(compactLayout.parent.width > 0
    && compactLayout.parent.height > 0
    && compactLayout.rects.every((rect) => rect.left >= 0 && rect.right <= compactLayout.parent.width + 1)
    && compactLayout.scrollHeight <= compactLayout.clientHeight + 1
    && compactLayout.scrollWidth <= compactLayout.clientWidth + 1,
  "record-type-badges-clipped-or-overflowing", compactType);
  const mobileBadgeStyle = await compactType.locator(".record-type-badge").first().evaluate((element) => {
    const computed = getComputedStyle(element);
    return { fontSize: computed.fontSize, padding: [computed.paddingTop, computed.paddingRight, computed.paddingBottom, computed.paddingLeft] };
  });
  await h.state(mobileBadgeStyle.fontSize === "11px"
    && isDeepStrictEqual(mobileBadgeStyle.padding, ["1px", "6px", "1px", "6px"]),
  "mobile-record-badge-density-mismatch", compactType);
  const syncCompactRecord = records.locator(".mobile-record").filter({
    has: page.locator('.mobile-record-open[aria-label="查看记录 #199"]'),
  });
  await h.visible(syncCompactRecord, "record-sync-compact-row");
  const syncCompactType = syncCompactRecord.locator(".mobile-record-type");
  await h.state(await syncCompactType.locator(".record-type-badge").count() === 2
    && await syncCompactType.locator(".record-type-badge.is-sync").count() === 1
    && await syncCompactType.locator(".record-type-badge.is-compact").count() === 1,
  "record-sync-compact-badges-missing-or-duplicated", syncCompactType);
  await h.layout();

  h.phase = "records/pagination";
  const scroll = records.locator(".records-scroll");
  const pagination = records.locator(".records-pagination");
  const firstPageIds = await records.locator(".mobile-record-open").evaluateAll((buttons) =>
    buttons.map((button) => Number(button.getAttribute("aria-label")?.match(/#(\d+)$/)?.[1])),
  );
  await h.state(firstPageIds.length === 50 && firstPageIds.every(Number.isInteger), "records-initial-page-size", records);
  await h.state(await pagination.getByRole("button", { name: "加载更多", exact: true }).count() === 0,
    "legacy-load-more-button-still-visible", pagination);
  const scrollState = await scroll.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
    element.dispatchEvent(new Event("scroll", { bubbles: true }));
    return {
      top: element.scrollTop,
      height: element.scrollHeight,
      clientHeight: element.clientHeight,
    };
  });
  await h.state(scrollState.top + scrollState.clientHeight >= scrollState.height - 1,
    "records-scroll-bottom-not-reached", scroll);
  await page.waitForFunction(() =>
    document.querySelectorAll('.page-surface[data-page="records"] .mobile-record').length === 75,
  );
  const allPageIds = await records.locator(".mobile-record-open").evaluateAll((buttons) =>
    buttons.map((button) => Number(button.getAttribute("aria-label")?.match(/#(\d+)$/)?.[1])),
  );
  await h.state(allPageIds.length === 75 && new Set(allPageIds).size === allPageIds.length
    && firstPageIds.every((id, index) => id === allPageIds[index]), "records-pagination-duplicates-or-order", records);
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
  const listTps = ((await compactTps.textContent()) || "").replace(/\s+/g, " ").trim();
  const previewRow = await page.evaluate(async () => {
    const preview = await import("/src/preview.ts");
    return preview.run("api_request", { method: "GET", path: "/usage-records/196" });
  });
  const expectedTps = previewRow.output_tokens > 0 && previewRow.duration_ms > 0
    ? `${(previewRow.output_tokens * 1000 / previewRow.duration_ms).toFixed(1)} tok/s`
    : "—";
  await h.state(listTps === expectedTps, "list-tps-does-not-use-native-output-rate", compactTps);
  await h.click(missingFirstToken.locator(".mobile-record-open"), "record-missing-first-detail-trigger");
  const detail = page.getByRole("dialog", { name: "调用记录详情", exact: true });
  await h.within(detail, "record-detail", { touch: false, scroll: false });
  await h.visible(detail.locator(".record-detail-section").first(), "record-detail-content");
  const detailBadgeStyle = await detail.locator(".record-type-badge").first().evaluate((element) => {
    const computed = getComputedStyle(element);
    return { fontSize: computed.fontSize, padding: [computed.paddingTop, computed.paddingRight, computed.paddingBottom, computed.paddingLeft] };
  });
  await h.state(detailBadgeStyle.fontSize === "11px"
    && isDeepStrictEqual(detailBadgeStyle.padding, ["1px", "6px", "1px", "6px"]),
  "record-detail-badge-density-mismatch", detail.locator(".record-type-badge").first());
  const tpsValue = detail.locator("dt").filter({ hasText: /^输出 TPS$/ }).locator("xpath=following-sibling::dd[1]");
  await h.state(((await tpsValue.textContent()) || "").replace(/\s+/g, " ").trim() === listTps,
    "detail-list-tps-mismatch", tpsValue);
  await h.layout();
  await h.click(detail.getByRole("button", { name: "关闭详情", exact: true }), "record-detail-close");
  await detail.waitFor({ state: "hidden" });
}

async function eventChecks(h, fixture) {
  const { page } = h;
  await h.navigate("事件", "events");
  const events = page.locator(".events-view");
  const tabs = events.getByRole("tablist", { name: "事件类型" });
  const expectedTabs = [["degradation", "降智错误"], ["other", "其他错误"], ["recoveries", "恢复历史"], ["logs", "日志"]];
  await h.state(await tabs.getByRole("tab").count() === expectedTabs.length, "event-tab-count", tabs);
  await h.state(isDeepStrictEqual(
    (await tabs.getByRole("tab").allTextContents()).map((label) => label.trim()),
    expectedTabs.map(([, label]) => label),
  ), "event-tab-labels", tabs);
  for (const [id, label] of expectedTabs.slice(0, 3)) {
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
      const columns = ["账号", "恢复类型", "验证方式", "测试模型", "用卡时间", "测试完成时间", "结果 / 恢复时间"];
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
        const scheduled = recovery.kind === "connection_test";
        await h.state(await cellText("账号") === recovery.account_name && await cellText("测试模型") === recovery.model_id,
          "recovery-history-fixture-mismatch", row);
        await h.state(await cellText("恢复类型") === (scheduled ? "预约测试" : card ? "用卡恢复" : "额度恢复"), "recovery-kind-label", row);
        if (card) {
          await h.state(!!recovery.reset_credit?.completed_at && ["model", "connection"].includes(recovery.reset_credit.verification_method), "missing-reset-credit-metadata", row);
          await h.state(await cellText("验证方式") === (recovery.reset_credit.verification_method === "model" ? "模型测试" : "测试连接")
            && !["", "—", "时间未知"].includes(await cellText("用卡时间")), "reset-credit-metadata-labels", row);
        } else {
          await h.state(await cellText("验证方式") === (scheduled ? "测试连接" : "—") && await cellText("用卡时间") === "—", "recovery-without-card-metadata", row);
        }
        await h.state(!["", "—", "时间未知"].includes(await cellText("测试完成时间"))
          && !["", "—", "时间未知"].includes(await cellText("结果 / 恢复时间")), "recovery-history-timestamps-missing", row);
        if (scheduled) await h.state(await cellText("结果 / 恢复时间") === "已完成", "scheduled-test-result-label", row);
      }
      await h.allWithin(panel.locator(".recovery-table td"), "recovery-history-cell", { touch: false, vertical: false });
    }
    await h.layout();
  }

  h.phase = "events/logs";
  const logsTab = tabs.getByRole("tab", { name: "日志", exact: true });
  await h.click(logsTab, "event-log-tab");
  await h.state(await logsTab.getAttribute("aria-selected") === "true", "event-log-tab-not-selected", logsTab);
  const logsPanel = events.locator("#logs-panel");
  await h.visible(logsPanel, "detection-log-panel");
  const logTable = logsPanel.locator("table");
  await h.visible(logTable, "detection-log-table");
  const logHeaders = await logTable.locator("thead th").allTextContents();
  await h.state(isDeepStrictEqual(logHeaders.map((label) => label.trim()),
    ["结果时间", "账号", "模型", "触发", "执行", "结论", "预测", "匹配度", "处置／恢复", "通知"]),
  "detection-log-column-labels", logTable);
  const logRows = logTable.locator("tbody tr.detection-log-row");
  await h.visible(logRows.first(), "detection-log-row");
  await h.state(await logRows.count() > 0 && await logRows.first().locator("td").count() === 10,
    "detection-log-column-count", logRows.first());
  const firstLog = logRows.first();
  const firstLogCells = firstLog.locator("td");
  await h.state((await firstLogCells.nth(4).textContent() || "").includes("已完成")
    && (await firstLogCells.nth(5).textContent() || "").includes("未确认降智")
    && await firstLogCells.nth(4).locator("span").count() > 0,
  "detection-log-status-or-conclusion-mismatch", firstLog);
  const accountOpen = firstLogCells.nth(1).getByRole("button");
  await h.visible(accountOpen, "detection-log-account-button");
  await accountOpen.click();
  const logDetail = page.getByRole("dialog", { name: "自动检测日志详情", exact: true });
  await h.within(logDetail, "detection-log-detail", { touch: false, scroll: false });
  await h.state((await logDetail.textContent() || "").includes("未确认降智"), "detection-log-detail-conclusion-missing", logDetail);
  await h.layout();
  await h.click(logDetail.getByRole("button", { name: "关闭日志详情", exact: true }), "detection-log-detail-close");
  await logDetail.waitFor({ state: "hidden" });
}

async function featureChecks(h, baseline) {
  const { page } = h;
  await h.navigate("功能", "features");
  const oauth = configForm(page, "恢复、测活与模型检测");
  await h.visible(oauth, "oauth-config-form");
  await h.state(await oauth.getByRole("button", { name: "保存", exact: true }).isDisabled(), "oauth-initial-draft-dirty");

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
  await unchangedConfig(h, baseline);
  await h.state(await oauth.getByRole("button", { name: "保存", exact: true }).isDisabled(), "recovery-draft-survived-remount");
  await h.state(await connection.getByRole("checkbox").isChecked() && !(await model.getByRole("checkbox").isChecked()), "recovery-unsaved-selection-persisted", connection);
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
  if (h.scenario.mobile) {
    const mobileSort = surface.locator(".mobile-sort");
    await h.state(await mobileSort.getByRole("checkbox", { name: "全选当前筛选账号", exact: true }).count() === 1
      && await selectionBar.locator('input[type="checkbox"]').count() === 0,
    "mobile-select-all-not-in-sort-row", mobileSort);
  }
  const selectionSummary = selectionBar.locator(".selection-summary");
  const selectionActions = selectionBar.locator(".selection-actions");
  const testConnectionAction = selectionActions.getByRole("button", { name: "测试连接", exact: true });
  const recoverAction = selectionActions.getByRole("button", { name: "恢复状态", exact: true });
  const proxyAction = selectionActions.getByRole("button", { name: "选择代理", exact: true });
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
  await selectionActions.scrollIntoViewIfNeeded();
  const actionLayout = await selectionActions.evaluate((element) => {
    const buttons = Array.from(element.querySelectorAll("button"));
    const rects = buttons.map((button) => button.getBoundingClientRect());
    const style = getComputedStyle(element);
    const rows = [[0, 1], [2, 3]];
    const columns = [[0, 2], [1, 3], [2, 4]];
    return {
      count: buttons.length,
      labels: buttons.map((button) => button.textContent?.trim()),
      adjacent: buttons.length === 5 && buttons.every((button, index) => index === 0 || button.previousElementSibling === buttons[index - 1]),
      sameRows: rects.length === 5 && rows.every(([left, right]) => Math.abs(rects[left].top - rects[right].top) < 1
        && Math.abs(rects[left].bottom - rects[right].bottom) < 1),
      sameColumns: rects.length === 5 && columns.every(([top, bottom]) => Math.abs(rects[top].left - rects[bottom].left) < 1
        && Math.abs(rects[top].right - rects[bottom].right) < 1),
      display: style.display,
      columnGap: style.columnGap,
      rowGap: style.rowGap,
      actualColumnGaps: rects.length === 5 ? rows.map(([left, right]) => rects[right].left - rects[left].right) : [],
      actualRowGaps: rects.length === 5 ? columns.map(([top, bottom]) => rects[bottom].top - rects[top].bottom) : [],
    };
  });
  await h.state(actionLayout.count === 5
    && isDeepStrictEqual(actionLayout.labels, ["测试连接", "恢复状态", "选择代理", "应用模板", "删除所选"])
    && actionLayout.adjacent && actionLayout.sameRows && actionLayout.sameColumns && actionLayout.display === "grid"
    && actionLayout.columnGap === "8px" && actionLayout.rowGap === "8px"
    && actionLayout.actualColumnGaps.length === 2 && actionLayout.actualColumnGaps.every((gap) => Math.abs(gap - 8) < 1)
    && actionLayout.actualRowGaps.length === 3 && actionLayout.actualRowGaps.every((gap) => Math.abs(gap - 8) < 1),
  "batch-actions-not-adjacent-at-eight-pixels", selectionActions);
  await h.within(testConnectionAction, "batch-connection-entry", { vertical: false });
  await h.within(recoverAction, "batch-recover-entry", { vertical: false });
  await h.within(proxyAction, "batch-proxy-entry", { vertical: false });
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

async function connectionBatchChecks(h, fixture) {
  const { page } = h;
  h.phase = "accounts/batch-connection-selection";
  const surface = page.locator('.page-surface[data-page="accounts"]');
  const selection = surface.locator(".selection-bar");
  const results = surface.getByRole("region", { name: "批量测试连接", exact: true });
  const accountRows = surface.locator(h.scenario.mobile ? ".mobile-account" : ".accounts-table tbody tr");
  const selectedAccounts = [101, 102, 201].map((id) => fixture.accounts.find((account) => account.id === id));
  await h.state(selectedAccounts.every(Boolean), "missing-batch-connection-fixture", surface);
  await h.state(await selection.count() === 0, "batch-connection-selection-bar-rendered-with-no-selection", surface);
  await h.state(await accountRows.count() === fixture.accounts.length, "batch-connection-account-list-incomplete", accountRows);

  const readJobs = () => page.evaluate(async () => {
    const preview = await import("/src/preview.ts");
    const response = await preview.run("api_request", { method: "GET", path: "/account-operations" });
    return response.items.filter((item) => item.action === "test");
  });
  const beforeJobs = await readJobs();
  for (const account of selectedAccounts) {
    const checkboxOptions = { name: `选择 ${account.name}`, exact: true };
    const checkbox = page.getByRole("checkbox", checkboxOptions);
    if (h.scenario.mobile) {
      const card = accountRows.filter({ has: checkbox });
      await h.click(card.locator("label.account-check"), "batch-connection-select-account");
    } else {
      await h.click(surface.getByRole("checkbox", checkboxOptions), "batch-connection-select-account", { touch: false, vertical: false });
    }
  }
  const entry = selection.getByRole("button", { name: "测试连接", exact: true });
  await h.state((await selection.textContent() || "").includes("已选 3 个账号"), "batch-connection-selection-count", selection);
  await h.state(await entry.isEnabled(), "batch-connection-selected-entry-disabled", entry);
  await h.allWithin(selection.locator(".selection-actions button"), "batch-connection-selection-action");
  await h.layout();

  // Freeze the short-lived result while geometry checks run on slower hosts.
  await page.clock.pauseAt(await page.evaluate(() => new Date(Date.now() + 60_000)));
  h.phase = "accounts/batch-connection-stop";
  await h.click(entry, "batch-connection-start");
  await h.state(await entry.isDisabled(), "batch-connection-running-entry-enabled", entry);
  // Stop immediately while the synthetic worker is queued, before its 1200ms dispatch.
  await h.click(results.getByRole("button", { name: "停止未开始", exact: true }), "batch-connection-stop");
  await h.visible(results.getByRole("status").filter({ hasText: "已结束 3/3 · 通过 0" }), "batch-connection-stopped-summary");
  const stoppedRows = results.locator("tbody tr");
  await h.state(await stoppedRows.count() === 3
    && (await stoppedRows.allTextContents()).every((text) => text.includes("已停止") && text.includes("未派发测试")),
  "batch-connection-stop-result-mismatch", results);
  const stoppedJobs = await readJobs();
  const stoppedIds = new Set(beforeJobs.map((job) => job.id));
  await h.state(stoppedJobs.filter((job) => !stoppedIds.has(job.id)).length === 3
    && stoppedJobs.filter((job) => !stoppedIds.has(job.id)).every((job) => job.status === "cancelled" && !job.started_at),
  "batch-connection-stop-dispatched-or-duplicated", results);

  h.phase = "accounts/batch-connection-results";
  await h.click(entry, "batch-connection-restart");
  await h.state(await entry.isDisabled(), "batch-connection-repeat-entry-enabled", entry);
  await h.visible(results.getByRole("status").filter({ hasText: "已完成 0/3" }), "batch-connection-queued-summary");
  await page.clock.runFor(2500);
  await h.visible(results.getByRole("status").filter({ hasText: "已结束 3/3 · 通过 2" }), "batch-connection-finished-summary");
  await h.within(results, "batch-connection-inline-results", { touch: false, vertical: false });
  await h.state(await page.getByRole("dialog").count() === 0, "batch-connection-opened-dialog", results);
  await h.state(await accountRows.count() === fixture.accounts.length, "batch-connection-replaced-account-list", accountRows);
  await h.state((await selection.textContent() || "").includes("已选 3 个账号"), "batch-connection-result-cleared-selection", selection);
  for (const account of selectedAccounts) {
    await h.state(await surface.getByRole("checkbox", { name: `选择 ${account.name}`, exact: true }).isChecked(),
      "batch-connection-result-unchecked-account", selection);
    const row = results.locator("tbody tr").filter({ has: page.getByRole("cell", { name: account.name, exact: true }) });
    const expected = account.platform === "grok"
      ? [account.name, "测试通过", "原生默认 → grok", "0.36 s", "—"]
      : [account.name, account.status === "error" ? "测试失败" : "测试通过", "gpt-6-luna", "0.36 s",
        account.status === "error" ? "测试请求失败 [http_401]" : "—"];
    await h.state(isDeepStrictEqual((await row.getByRole("cell").allTextContents()).map((text) => text.trim()), expected),
      "batch-connection-result-fields-mismatch", row);
    await h.allWithin(row.getByRole("cell"), "batch-connection-result-cell", { touch: false, vertical: false });
  }
  await h.state(await results.locator("td.good-text").count() === 2 && await results.locator("td.bad-text").count() === 1,
    "batch-connection-result-color-state-mismatch", results);
  await h.state(await results.getByRole("button", { name: "停止未开始", exact: true }).count() === 0
    && await entry.isEnabled(), "batch-connection-finished-controls-mismatch", results);
  const finishedJobs = await readJobs();
  await h.state(finishedJobs.length === beforeJobs.length + 6, "batch-connection-submitted-duplicate-jobs", results);
  await h.layout();
  await page.clock.runFor(3000);
  await results.waitFor({ state: "hidden" });
  await page.clock.resume();
  await h.state(await results.count() === 0, "batch-connection-results-not-auto-closed", surface);
  await h.state(await surface.getByRole("button", { name: "查看测活结果", exact: true }).count() === 0,
    "batch-connection-standalone-result-entry-remains", surface);

  h.phase = "accounts/batch-connection-filter";
  async function platform(value) {
    if (h.scenario.mobile) await h.click(surface.getByRole("button", { name: "筛选账号", exact: true }), "batch-connection-filter-entry");
    const select = surface.getByRole("combobox", { name: "平台筛选", exact: true });
    await h.within(select, "batch-connection-platform-filter");
    await select.selectOption(value);
    if (h.scenario.mobile) await h.click(page.getByRole("dialog", { name: "账号筛选", exact: true })
      .getByRole("button", { name: "关闭筛选", exact: true }), "batch-connection-filter-close");
  }
  await platform("grok");
  await h.state(await selection.count() === 0,
    "batch-connection-filter-kept-stale-selection", surface);
  await h.state(await accountRows.count() === fixture.accounts.filter((account) => account.platform === "grok").length,
    "batch-connection-filter-did-not-change-account-list", accountRows);
  await h.state(await results.count() === 0, "batch-connection-filter-restored-closed-results", surface);
  await h.layout();
  await platform("");

  h.phase = "accounts/batch-connection-page-return";
  async function navigate(label, id) {
    if (h.scenario.mobile) await h.navigate(label, id);
    else {
      await h.click(page.locator(".sidebar nav").getByRole("button", { name: label, exact: true }), "batch-connection-navigation");
      await h.visible(page.locator(`.page-surface[data-page="${id}"]`), "batch-connection-return-surface");
    }
  }
  await navigate("事件", "events");
  await h.state(await page.getByRole("region", { name: "批量测试连接", exact: true }).count() === 0,
    "batch-connection-results-leaked-to-other-page");
  await navigate("账号", "accounts");
  await h.state(await page.getByRole("region", { name: "批量测试连接", exact: true }).count() === 0,
    "batch-connection-results-reappeared-after-return");

  h.phase = "accounts/batch-connection-recovery";
  // The synthetic transport changes connection scope without making a network request.
  const connect = (baseUrl) => page.evaluate(async (url) => {
    const preview = await import("/src/preview.ts");
    await preview.run("connect", { baseUrl: url });
  }, baseUrl);
  await connect(`${ORIGIN}/alternate-connection-fixture`);
  await results.waitFor({ state: "hidden" });
  await h.state(await results.count() === 0, "batch-connection-results-leaked-to-other-connection", surface);
  await connect(fixture.baseUrl);
  await h.state(await results.count() === 0, "batch-connection-closed-results-restored", surface);
  await h.state(isDeepStrictEqual(await readJobs(), finishedJobs), "batch-connection-recovery-resubmitted-or-changed-jobs", results);
  await h.state(isDeepStrictEqual((await fixtureData(page)).accounts, fixture.accounts), "batch-connection-mutated-preview-accounts", surface);
  await h.state(await accountRows.count() === fixture.accounts.length, "batch-connection-return-account-list-incomplete", accountRows);
  await h.layout();
}

async function proxyBatchChecks(h, fixture, { navigate = false } = {}) {
  const { page } = h;
  if (navigate) await h.navigate("账号", "accounts");
  h.phase = "accounts/batch-proxy-selection";
  const surface = page.locator('.page-surface[data-page="accounts"]');
  await h.visible(surface, "batch-proxy-account-surface");
  const selectionBar = surface.locator(".selection-bar");
  await h.state(await selectionBar.count() === 0, "batch-proxy-selection-bar-rendered-with-no-selection", surface);
  const selectedAccounts = [101, 391].map((id) => fixture.accounts.find((account) => account.id === id));
  await h.state(selectedAccounts.every(Boolean), "missing-batch-proxy-fixture", surface);
  const accountNames = selectedAccounts.map((account) => account.name);
  const select = async (account, index) => {
    const checkbox = page.getByRole("checkbox", { name: `选择 ${account.name}`, exact: true });
    if (h.scenario.mobile) {
      const card = surface.locator(".mobile-account").filter({ has: checkbox });
      await h.state(await card.count() === 1, "batch-proxy-mobile-account-not-unique", card);
      await h.click(card.locator("label.account-check"), `batch-proxy-select-${index}`);
    } else {
      const row = surface.locator(".accounts-table tbody tr").filter({ has: checkbox });
      await h.state(await row.count() === 1, "batch-proxy-desktop-account-not-unique", row);
      await h.click(checkbox, `batch-proxy-select-${index}`, { touch: false, vertical: false });
    }
  };
  for (let index = 0; index < selectedAccounts.length; index++) await select(selectedAccounts[index], index);

  const actions = selectionBar.locator(".selection-actions");
  const proxyEntry = actions.getByRole("button", { name: "选择代理", exact: true });
  await h.state((await selectionBar.textContent() || "").includes("已选 2 个账号"), "batch-proxy-selection-count", selectionBar);
  await h.state(await proxyEntry.isEnabled(), "batch-proxy-entry-disabled", proxyEntry);
  await h.layout();
  await h.click(proxyEntry, "batch-proxy-entry");

  const dialog = page.getByRole("dialog", { name: "批量选择代理", exact: true });
  await h.within(dialog, "batch-proxy-dialog", { touch: false, scroll: false });
  await h.state((await dialog.textContent() || "").includes("已选择 2 个账号"), "batch-proxy-dialog-selection-count", dialog);
  const options = dialog.getByRole("radiogroup", { name: "账号代理", exact: true });
  await h.visible(options, "batch-proxy-options");
  await h.allWithin(options.locator("label"), "batch-proxy-option");
  await h.state(await options.getByRole("radio").count() === 3, "batch-proxy-option-count", options);
  const unavailable = options.getByRole("radio", { name: /备用代理/ });
  await h.state(await unavailable.isDisabled(), "inactive-proxy-option-enabled", unavailable);
  await proxyDiagnosticChecks(h, options, "batch-proxy", {
    confirmation: dialog.getByRole("button", { name: "确认应用", exact: true }),
  });

  const previews = dialog.locator(".proxy-batch-previews article");
  await h.state(await previews.count() === 2, "batch-proxy-preview-count", previews);
  for (const accountName of accountNames) {
    const preview = previews.filter({ hasText: accountName });
    await h.state(await preview.count() === 1, "batch-proxy-preview-not-unique", preview);
    await h.state(await preview.locator("dt").allTextContents().then((labels) => isDeepStrictEqual(labels, ["当前", "目标"])),
      "batch-proxy-preview-labels", preview);
    await h.within(preview.locator("dd").first(), "batch-proxy-current-value", { touch: false, vertical: false });
    await h.within(preview.locator("dd").last(), "batch-proxy-target-value", { touch: false, vertical: false });
  }
  const inherited = previews.filter({ hasText: selectedAccounts[1].name });
  await h.state((await inherited.textContent() || "").includes("继承"), "batch-proxy-inheritance-limit-missing", inherited);
  const previewTextLayout = await previews.evaluateAll((cards) => cards.map((card) => {
    const dds = [...card.querySelectorAll("dd")];
    const strong = card.querySelector("header strong");
    return {
      cardWidth: card.getBoundingClientRect().width,
      textOverflow: dds.some((dd) => dd.scrollWidth > dd.clientWidth + 1),
      strongWrap: strong ? getComputedStyle(strong).overflowWrap : "",
    };
  }));
  await h.state(previewTextLayout.every((item) => item.cardWidth > 0 && !item.textOverflow && item.strongWrap === "anywhere"),
    "batch-proxy-preview-text-overflow", previews);

  const activeOption = options.locator("label").filter({ hasText: "演示代理" });
  await h.click(activeOption, "batch-proxy-active-option");
  await h.state(await activeOption.getByRole("radio").isChecked(), "batch-proxy-active-option-not-selected", activeOption);
  await h.state(await dialog.getByRole("button", { name: "确认应用", exact: true }).isEnabled(), "batch-proxy-confirm-disabled-for-eligible-account", dialog);
  await h.layout();

  // Closing the preview cancels the draft and must leave the selected accounts intact.
  await h.click(dialog.getByRole("button", { name: "关闭代理选择", exact: true }), "batch-proxy-close-draft");
  await dialog.waitFor({ state: "hidden" });
  await h.state((await selectionBar.textContent() || "").includes("已选 2 个账号"), "batch-proxy-close-cleared-selection", selectionBar);
  await h.state(await page.locator('[role="dialog"][aria-label="批量选择代理"]').count() === 0, "batch-proxy-close-left-dialog", surface);

  // Reopen and clear the proxy. Both accounts start without a proxy, so the
  // eligible row must report no change while the inherited row is rejected.
  await h.click(proxyEntry, "batch-proxy-reopen");
  await h.within(dialog, "batch-proxy-dialog-reopened", { touch: false, scroll: false });
  const reopenedOptions = dialog.getByRole("radiogroup", { name: "账号代理", exact: true });
  await h.visible(reopenedOptions, "batch-proxy-options-reopened");
  await h.state(await reopenedOptions.locator(".proxy-diagnostic-result").count() === 0,
    "batch-proxy-close-kept-diagnostic-result", reopenedOptions);
  const clearOption = reopenedOptions.locator("label").filter({ hasText: "不使用代理" });
  await h.click(clearOption, "batch-proxy-clear-option");
  await h.state(await clearOption.getByRole("radio").isChecked(), "batch-proxy-clear-option-not-selected", clearOption);
  await h.state((await dialog.textContent() || "").includes("目标不使用代理"), "batch-proxy-clear-preview-missing", dialog);
  const confirm = dialog.getByRole("button", { name: "确认应用", exact: true });
  await h.click(confirm, "batch-proxy-clear-submit");
  const unchanged = previews.filter({ hasText: selectedAccounts[0].name }).filter({ hasText: "无变化" });
  await h.visible(unchanged, "batch-proxy-no-change-result");
  await h.state((await unchanged.textContent() || "").includes(selectedAccounts[0].name), "batch-proxy-no-change-account-missing", unchanged);
  await h.state((await inherited.textContent() || "").includes("继承"), "batch-proxy-inherited-result-missing", inherited);
  await h.layout();

  // Start a second request with an active proxy and cancel the queued eligible
  // account before the preview transport's bounded worker completes it.
  await h.click(dialog.getByRole("button", { name: "重新预览", exact: true }), "batch-proxy-repreview");
  await h.visible(dialog.getByRole("radiogroup", { name: "账号代理", exact: true }), "batch-proxy-options-after-repreview");
  const nextOptions = dialog.getByRole("radiogroup", { name: "账号代理", exact: true });
  const nextActive = nextOptions.locator("label").filter({ hasText: "演示代理" });
  await h.click(nextActive, "batch-proxy-queued-option");
  await h.click(dialog.getByRole("button", { name: "确认应用", exact: true }), "batch-proxy-queued-submit");
  const queuedPreview = previews.filter({ hasText: selectedAccounts[0].name });
  const cancel = queuedPreview.getByRole("button", { name: "取消", exact: true });
  await h.visible(cancel, "batch-proxy-queued-cancel");
  await h.click(cancel, "batch-proxy-cancel");
  await cancel.waitFor({ state: "hidden" });
  await h.state((await queuedPreview.textContent() || "").includes("已取消"), "batch-proxy-cancel-result-missing", queuedPreview);
  await h.layout();
  await h.click(dialog.getByRole("button", { name: "关闭", exact: true }), "batch-proxy-final-close");
  await dialog.waitFor({ state: "hidden" });
  for (let index = 0; index < selectedAccounts.length; index++) await select(selectedAccounts[index], index);
  await h.state(await selectionBar.count() === 0, "batch-proxy-selection-bar-still-rendered-after-clearing", surface);
  await h.state(isDeepStrictEqual((await fixtureData(page)).accounts, fixture.accounts), "batch-proxy-mutated-preview-accounts", surface);
}

async function desktopActionChecks(h, table, fixture) {
  const { page } = h;
  for (const account of fixture.accounts) {
    const row = table.locator("tbody tr").filter({
      has: page.getByRole("checkbox", { name: `选择 ${account.name}`, exact: true }),
    });
    await h.state(await row.count() === 1, "desktop-account-row-not-unique", row);
    const panel = row.locator(".account-actions");
    const actions = fixtureAccountActions(account);
    await accountActionButtons(h, panel, [...actions.primary, ...actions.more], "desktop-account-action");
    const grid = await panel.evaluate((element) => {
      const panelRect = element.getBoundingClientRect();
      const buttons = [...element.querySelectorAll("button")];
      const rects = buttons.map((button) => button.getBoundingClientRect());
      const style = getComputedStyle(element);
      return {
        width: panelRect.width,
        right: panelRect.right,
        minWidth: Math.min(...rects.map((rect) => rect.width)),
        maxRight: Math.max(...rects.map((rect) => rect.right)),
        columns: style.gridTemplateColumns.split(/\s+/).filter(Boolean).length,
        gridTemplateColumns: style.gridTemplateColumns,
        justify: buttons.map((button) => getComputedStyle(button).justifyContent),
        whiteSpace: buttons.map((button) => getComputedStyle(button).whiteSpace),
      };
    });
    await h.state(grid.width > 0 && grid.minWidth >= 95.5 && grid.maxRight <= grid.right + 1
      && (h.scenario.viewport.width < 1310 || grid.columns >= 2)
      && grid.gridTemplateColumns !== "none" && grid.justify.every((value) => value === "flex-start")
      && grid.whiteSpace.every((value) => value === "normal"),
    "desktop-account-action-grid-not-filled", panel);
  }
}

async function desktopMenuChecks(h, fixture) {
  const { page } = h;
  h.phase = "desktop/priorities";
  const sidebar = page.locator(".sidebar nav");
  const priorityEntry = sidebar.getByRole("button", { name: "优先级", exact: true });
  await h.visible(priorityEntry, "desktop-priority-entry");
  await h.click(priorityEntry, "desktop-priority-entry", { touch: false });
  const priorityManager = page.getByRole("region", { name: "全局优先级管理", exact: true });
  await h.visible(priorityManager, "desktop-priority-surface");
  await h.within(priorityManager, "desktop-priority-manager", { touch: false, scroll: false, vertical: false });
  await h.visible(priorityManager.locator(".priority-column:visible .priority-list > li").first(), "desktop-priority-list");
  await h.allWithin(priorityManager.locator(".priority-column:visible .priority-moves button"), "desktop-priority-move-control", { touch: false, vertical: false });
  const priorityLayout = await priorityManager.evaluate(element => {
    const groups = element.querySelector('.priority-platforms');
    const panels = [...element.querySelectorAll('.priority-column:not([hidden])')];
    return { width: element.getBoundingClientRect().width, columns: groups.classList.contains('is-columns'), count: element.querySelectorAll('.priority-column').length,
      panels: panels.map(panel => ({ width: panel.getBoundingClientRect().width, scroll: getComputedStyle(panel.querySelector('.priority-list')).overflowY })),
      numbers: panels.map(panel => [...panel.querySelectorAll('.priority-number span:last-child b')].map(value => Number(value.textContent))) };
  });
  await h.state(priorityLayout.columns === (priorityLayout.count > 1 && priorityLayout.count <= 3 && priorityLayout.width >= Math.max(960, priorityLayout.count * 320 + (priorityLayout.count - 1) * 12))
    && priorityLayout.panels.every(panel => (!priorityLayout.columns || panel.width >= 320) && panel.scroll === 'auto')
    && priorityLayout.numbers.every(numbers => numbers.every((value, index) => value === index + 1)), 'priority-platform-layout-or-numbering-mismatch', priorityManager);
  await h.state(!(await priorityManager.textContent()).includes('全部平台统一排序'), 'priority-description-still-present', priorityManager);
  await h.layout();
  await h.click(sidebar.getByRole("button", { name: "账号", exact: true }), "desktop-priority-return", { touch: false });
  h.phase = "desktop/table";
  const table = page.locator(".accounts-table");
  await h.visible(table, "desktop-account-table");
  const accountToolbar = page.locator('.page-surface[data-page="accounts"] .account-toolbar');
  const accountSort = page.locator('.page-surface[data-page="accounts"] .account-sort');
  const sortSelect = page.locator('.page-surface[data-page="accounts"] select[aria-label="账号排序"]');
  const accountFilterSelects = page.locator('.page-surface[data-page="accounts"] .filters select');
  await h.within(accountSort, "desktop-account-sort", { touch: false, vertical: false });
  await h.within(sortSelect, "desktop-account-sort-dropdown", { touch: false, vertical: false });
  await h.allWithin(accountToolbar.locator("button"), "desktop-account-toolbar-button", { touch: false, vertical: false });
  await h.allWithin(accountFilterSelects, "desktop-account-filter", { touch: false, vertical: false });
  const accountTypography = await measureTypography(page, {
    sort: '.page-surface[data-page="accounts"] .account-sort',
    dropdown: '.page-surface[data-page="accounts"] select[aria-label="账号排序"]',
    toolbar: '.page-surface[data-page="accounts"] .account-toolbar > button',
    filters: '.page-surface[data-page="accounts"] .filters select',
  });
  const desktopSort = accountTypography.sort[0];
  const desktopDropdown = accountTypography.dropdown[0];
  await h.state(desktopSort?.fontSize === "12px"
    && desktopSort?.lineHeight === "20px"
    && desktopDropdown?.fontSize === "12px"
    && desktopDropdown?.lineHeightOk
    && accountTypography.toolbar.length > 0
    && accountTypography.toolbar.every((item) => item.fontSize === "12px" && item.lineHeight === "20px")
    && accountTypography.filters.length > 0
    && accountTypography.filters.every((item) => item.fontSize === "12px" && item.lineHeightOk),
  "desktop-account-sort-typography-mismatch", accountToolbar);
  await h.state(desktopDropdown?.height >= 30
    && accountTypography.filters.every((item) => item.height >= 30),
  "desktop-account-select-under-30px", accountToolbar);
  const baseColumnsWidth = await table.locator("col:not(.col-name):not(.col-actions)").evaluateAll((columns) =>
    Math.round(columns.reduce((total, column) => total + column.getBoundingClientRect().width, 0)));
  await h.state(baseColumnsWidth === 734, "account-table-base-columns-width", table);
  await h.layout();
  h.phase = "desktop/account-column-preference";
  let columnToggle = table.getByRole("button", { name: "展开账号列", exact: true });
  const nameColumn = table.locator("col.col-name");
  const columnWidth = async () => nameColumn.evaluate((element) => Math.round(element.getBoundingClientRect().width));
  await h.within(columnToggle, "account-column-toggle", { touch: false, vertical: false, hit: true });
  await h.state(Math.abs(await columnWidth() - 128) <= 1, "account-column-default-width", nameColumn);
  await h.click(columnToggle, "account-column-toggle", { touch: false, vertical: false });
  await h.state((await table.getAttribute("class"))?.includes("name-expanded"), "account-column-did-not-expand", table);
  await h.state(await table.getByRole("button", { name: "收起账号列", exact: true }).getAttribute("aria-expanded") === "true", "account-column-expanded-state-mismatch", table);
  await h.state(Math.abs(await columnWidth() - 240) <= 1, "account-column-expanded-width", nameColumn);
  await h.state(await page.evaluate(() => localStorage.getItem("sub2ops-account-name-expanded")) === "true", "account-column-preference-not-saved", table);
  await page.reload({ waitUntil: "domcontentloaded" });
  await h.visible(page.locator('.page-surface[data-page="accounts"]'), "reloaded-account-surface");
  const reloadedFixture = await fixtureData(page);
  await h.visible(page.locator(".accounts-table"), "reloaded-desktop-account-table");
  const reloadedTable = page.locator(".accounts-table");
  await h.state((await reloadedTable.getAttribute("class"))?.includes("name-expanded"), "account-column-preference-not-restored", reloadedTable);
  columnToggle = reloadedTable.getByRole("button", { name: "收起账号列", exact: true });
  await h.state(Math.abs(await reloadedTable.locator("col.col-name").evaluate((element) => Math.round(element.getBoundingClientRect().width)) - 240) <= 1,
    "account-column-restored-width", reloadedTable.locator("col.col-name"));
  await h.click(columnToggle, "account-column-toggle", { touch: false, vertical: false });
  await h.state(Math.abs(await reloadedTable.locator("col.col-name").evaluate((element) => Math.round(element.getBoundingClientRect().width)) - 128) <= 1,
    "account-column-collapse-width", reloadedTable.locator("col.col-name"));
  await h.state(await page.evaluate(() => localStorage.getItem("sub2ops-account-name-expanded")) === "false", "account-column-collapse-preference-not-saved", reloadedTable);
  await h.layout();

  h.phase = "desktop/expanded-actions";
  await desktopActionChecks(h, reloadedTable, reloadedFixture);
  await connectionBatchChecks(h, reloadedFixture);
  await proxyBatchChecks(h, reloadedFixture);
  await h.layout();
  await h.click(page.locator(".sidebar nav").getByRole("button", { name: "分组", exact: true }), "desktop-group-entry", { touch: false });
  const manager = page.locator(".group-manager");
  await h.visible(manager, "group-manager");
  await groupMenuChecks(h, reloadedFixture, manager);

  h.phase = "desktop/records-type-badges";
  await h.click(page.locator(".sidebar nav").getByRole("button", { name: "记录", exact: true }), "desktop-record-entry", { touch: false });
  const records = page.getByRole("region", { name: "调用记录", exact: true });
  await h.visible(records.locator(".records-table"), "desktop-record-table");
  await h.visible(records.locator(".records-table tbody tr").first(), "desktop-record-row");
  const recordsDensity = await records.evaluate((section) => {
    const style = (element) => {
      if (!element) return null;
      const computed = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return {
        height: computed.height,
        minHeight: computed.minHeight,
        rectHeight: rect.height,
        paddingTop: computed.paddingTop,
        paddingRight: computed.paddingRight,
        paddingBottom: computed.paddingBottom,
        paddingLeft: computed.paddingLeft,
      };
    };
    const filterBar = section.querySelector(".records-filter-bar");
    const controls = [...section.querySelectorAll(
      ".records-toolbar input:not([type=checkbox]), .records-toolbar select, .records-toolbar .record-filter-trigger, .records-toolbar > button[type=submit]",
    )];
    return {
      filterGap: filterBar ? getComputedStyle(filterBar).gap : "",
      controls: controls.map((element) => style(element)),
      dateTrigger: style(section.querySelector(".records-date-trigger")),
      head: style(section.querySelector(".records-table thead th")),
      cell: style(section.querySelector(".records-table tbody td")),
      columns: Object.fromEntries([
        ["user", [".records-table th.record-user", ".records-table td.record-user"]],
        ["key", [".records-table th.record-col-api_key", ".records-table td.record-col-api_key"]],
        ["account", [".records-table th.record-col-account", ".records-table td.record-col-account"]],
        ["model", [".records-table th.record-col-model", ".records-table td.record-col-model"]],
        ["type", [".records-table th.record-col-type", ".records-table td.record-col-type"]],
        ["tokens", [".records-table th.record-col-tokens", ".records-table td.record-col-tokens .record-tokens"]],
        ["money", [".records-table th.record-col-cost", ".records-table td.record-col-cost .record-money"]],
        ["latency", [".records-table th.record-col-latency", ".records-table td.record-col-latency .record-latency"]],
        ["userAgent", [".records-table th.record-col-user_agent", ".records-table td.record-col-user_agent"]],
        ["ip", [".records-table th.record-col-ip", ".records-table td.record-col-ip"]],
      ].map(([key, selectors]) => {
        const header = section.querySelector(selectors[0]);
        const content = section.querySelector(selectors[1]);
        const measure = (element) => element ? {
          minWidth: getComputedStyle(element).minWidth,
          width: Math.round(element.getBoundingClientRect().width),
        } : null;
        return [key, { header: measure(header), content: measure(content), button: measure(content?.querySelector("button")) }];
      })),
    };
  });
  await h.state(recordsDensity.filterGap === "6px"
    && recordsDensity.controls.length > 0
    && recordsDensity.controls.every((control) => control?.rectHeight >= 27.5 && control?.rectHeight <= 28.5)
    && recordsDensity.dateTrigger?.rectHeight >= 27.5
    && recordsDensity.dateTrigger?.minHeight === "28px"
    && recordsDensity.head?.height === "26px"
    && recordsDensity.head?.paddingTop === "3px"
    && recordsDensity.head?.paddingRight === "4px"
    && recordsDensity.head?.paddingBottom === "3px"
    && recordsDensity.head?.paddingLeft === "4px"
    && recordsDensity.cell?.rectHeight >= 32
    && recordsDensity.cell?.paddingTop === "3px"
    && recordsDensity.cell?.paddingRight === "4px"
    && recordsDensity.cell?.paddingBottom === "3px"
    && recordsDensity.cell?.paddingLeft === "4px"
    && recordsDensity.columns.user?.header?.minWidth === "110px"
    && recordsDensity.columns.user?.content?.width >= 110
    && recordsDensity.columns.user?.button?.width === 96
    && recordsDensity.columns.key?.header?.minWidth === "88px"
    && recordsDensity.columns.account?.header?.minWidth === "88px"
    && recordsDensity.columns.model?.header?.minWidth === "124px"
    && recordsDensity.columns.type?.header?.minWidth === "72px"
    && recordsDensity.columns.tokens?.content?.minWidth === "118px"
    && recordsDensity.columns.money?.content?.minWidth === "80px"
    && recordsDensity.columns.latency?.content?.minWidth === "104px"
    && recordsDensity.columns.userAgent?.header?.minWidth === "120px"
    && recordsDensity.columns.ip?.header?.minWidth === "108px",
  "desktop-record-density-mismatch", records.locator(".records-table"));
  const compactRecord = records.locator("tbody tr").filter({ has: page.locator('button[title="查看记录 #200"]') });
  await h.visible(compactRecord, "desktop-record-compact-row");
  const typeCell = compactRecord.locator(".record-col-type");
  await h.within(typeCell, "desktop-record-type-cell", { touch: false, vertical: false });
  const badges = typeCell.locator(".record-type-badge");
  await h.state(await badges.count() === 2
    && await typeCell.locator(".record-type-badge.is-compact").count() === 1
    && await typeCell.locator(".record-type-badge.is-stream").count() === 1,
  "desktop-record-type-badges-missing-or-duplicated", typeCell);
  const cellLayout = await typeCell.evaluate((element) => {
    const cell = element.getBoundingClientRect();
    const rects = [...element.querySelectorAll(".record-type-badge")].map((badge) => {
      const rect = badge.getBoundingClientRect();
      return { left: rect.left - cell.left, right: rect.right - cell.left, top: rect.top - cell.top, bottom: rect.bottom - cell.top };
    });
    return {
      width: cell.width,
      height: cell.height,
      rects,
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
    };
  });
  await h.state(cellLayout.width > 0 && cellLayout.height > 0
    && cellLayout.rects.every((rect) => rect.left >= 0 && rect.right <= cellLayout.width + 1)
    && cellLayout.scrollWidth <= cellLayout.clientWidth + 1
    && cellLayout.scrollHeight <= cellLayout.clientHeight + 1,
  "desktop-record-type-badges-clipped-or-overflowing", typeCell);
  const desktopBadgeStyle = await typeCell.locator(".record-type-badge").first().evaluate((element) => {
    const computed = getComputedStyle(element);
    return { fontSize: computed.fontSize, padding: [computed.paddingTop, computed.paddingRight, computed.paddingBottom, computed.paddingLeft] };
  });
  await h.state(desktopBadgeStyle.fontSize === "10px"
    && isDeepStrictEqual(desktopBadgeStyle.padding, ["0px", "5px", "0px", "5px"]),
  "desktop-record-badge-density-mismatch", typeCell);
  h.phase = "desktop/record-column-widths";
  const columnMenu = records.locator("details.records-column-menu");
  await h.click(columnMenu.locator("summary"), "desktop-record-columns-open", { touch: false, vertical: false });
  for (const label of ["请求 ID", "上游 ID"]) {
    const option = columnMenu.locator("label").filter({ hasText: label });
    const checkbox = option.getByRole("checkbox");
    if (!(await checkbox.isChecked())) {
      await checkbox.click({ force: true });
      await page.waitForFunction(() => [...document.querySelectorAll(".records-column-menu input")].every((input) => !(input instanceof HTMLInputElement) || !input.disabled));
    }
  }
  await h.state(await records.locator("th.record-col-request_id").evaluate((element) => getComputedStyle(element).minWidth) === "130px"
    && await records.locator("th.record-col-upstream_id").evaluate((element) => getComputedStyle(element).minWidth) === "130px",
  "desktop-record-request-id-width-mismatch", records.locator(".records-table"));
  await h.click(columnMenu.locator("summary"), "desktop-record-columns-close", { touch: false, vertical: false });
  const multilineLayout = await compactRecord.evaluate((row) => {
    const within = (parent, child) => {
      const parentRect = parent.getBoundingClientRect();
      const childRect = child.getBoundingClientRect();
      return childRect.top >= parentRect.top - 1
        && childRect.bottom <= parentRect.bottom + 1
        && childRect.left >= parentRect.left - 1
        && childRect.right <= parentRect.right + 1;
    };
    const measure = (selector, childSelector) => {
      const node = row.querySelector(selector);
      const cell = node?.closest("td");
      if (!node || !cell) return null;
      return {
        childCount: node.querySelectorAll(childSelector).length,
        withinCell: within(cell, node),
        scrollHeight: node.scrollHeight,
        clientHeight: node.clientHeight,
        scrollWidth: node.scrollWidth,
        clientWidth: node.clientWidth,
        overflowY: getComputedStyle(node).overflowY,
        childBounds: [...node.querySelectorAll(childSelector)].map((child) => within(node, child)),
      };
    };
    return {
      route: measure(".record-col-model .record-route", ":scope > div"),
      tokens: measure(".record-col-tokens .record-tokens", ":scope > span"),
      latency: measure(".record-col-latency .record-latency", ":scope > div > *"),
    };
  });
  await h.state(multilineLayout.route?.childCount >= 2
    && multilineLayout.route.withinCell
    && multilineLayout.route.overflowY !== "hidden"
    && multilineLayout.route.scrollHeight <= multilineLayout.route.clientHeight + 1
    && multilineLayout.route.childBounds.every(Boolean)
    && multilineLayout.tokens?.childCount >= 4
    && multilineLayout.tokens.withinCell
    && multilineLayout.tokens.overflowY !== "hidden"
    && multilineLayout.tokens.scrollHeight <= multilineLayout.tokens.clientHeight + 1
    && multilineLayout.tokens.childBounds.every(Boolean)
    && multilineLayout.latency?.childCount >= 6
    && multilineLayout.latency.withinCell
    && multilineLayout.latency.overflowY !== "hidden"
    && multilineLayout.latency.scrollHeight <= multilineLayout.latency.clientHeight + 1
    && multilineLayout.latency.childBounds.every(Boolean),
  "desktop-record-multiline-content-clipped-or-overflowing", compactRecord);
  const syncCompactRecord = records.locator("tbody tr").filter({ has: page.locator('button[title="查看记录 #199"]') });
  await h.visible(syncCompactRecord, "desktop-record-sync-compact-row");
  const syncCompactTypeCell = syncCompactRecord.locator(".record-col-type");
  await h.state(await syncCompactTypeCell.locator(".record-type-badge").count() === 2
    && await syncCompactTypeCell.locator(".record-type-badge.is-sync").count() === 1
    && await syncCompactTypeCell.locator(".record-type-badge.is-compact").count() === 1,
  "desktop-record-sync-compact-badges-missing-or-duplicated", syncCompactTypeCell);
  await h.layout();
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
    // Keep the 75 synthetic records inside the default Beijing-day filter.
    // Time still advances so queue timers and revision timestamps remain active.
    await page.clock.install({ time: new Date("2026-10-01T04:00:00.000Z") });
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
      h.phase = "mobile/priorities";
      await h.click(page.getByRole("button", { name: "优先级管理", exact: true }), "mobile-priority-entry");
      const priority = page.getByRole("region", { name: "全局优先级管理", exact: true });
      await h.visible(priority, "mobile-priority-surface");
      await h.state(await priority.locator('.priority-platforms.is-columns').count() === 0, "mobile-priority-must-use-tabs", priority);
      await h.allWithin(priority.locator('.priority-column:visible .priority-moves button'), "mobile-priority-controls");
      await h.click(priority.getByRole('button', { name: '返回账号', exact: true }), "mobile-priority-return");
      await connectionBatchChecks(h, fixture);
      await recordChecks(h);
      await eventChecks(h, fixture);
      await featureChecks(h, fixture.config);
      await proxyBatchChecks(h, fixture, { navigate: true });
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
      ...evidence, ...(error.keyboardActivation ? { keyboardActivation: error.keyboardActivation } : {}),
      blocked, pageErrorCount: pageErrors.length,
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
    for (const viewport of mobile ? viewports : [{ width: 900, height: 540 }, { width: 1310, height: 900 }]) {
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
