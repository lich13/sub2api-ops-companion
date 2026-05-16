(function () {
  function text(node, value) {
    if (node) node.textContent = value;
  }

  function setBusy(root, busy) {
    root.classList.toggle("version-loading", busy);
    var refresh = root.querySelector("[data-version-refresh]");
    var update = root.querySelector("[data-version-update]");
    if (refresh) refresh.disabled = busy;
    if (update) update.disabled = busy || update.dataset.enabled !== "true";
  }

  async function readJson(response) {
    var contentType = response.headers.get("content-type") || "";
    if (!contentType.includes("application/json")) {
      throw new Error("SSO 会话可能已失效，请从 Sub2API 菜单重新进入");
    }
    var payload = await response.json();
    if (!response.ok) {
      throw new Error(payload.detail || payload.message || "请求失败");
    }
    return payload;
  }

  async function refresh(root, force) {
    var basePath = root.dataset.basePath || "";
    setBusy(root, true);
    try {
      var response = await fetch(basePath + "/system/check-updates" + (force ? "?force=true" : ""), {
        credentials: "same-origin"
      });
      var data = await readJson(response);
      var hasUpdate = Boolean(data.has_update);
      var supported = Boolean(data.update_supported);
      var dot = root.querySelector("[data-version-dot]");
      var check = root.querySelector("[data-version-check]");
      var update = root.querySelector("[data-version-update]");

      root.classList.toggle("has-update", hasUpdate);
      root.classList.toggle("has-warning", Boolean(data.warning));
      if (dot) dot.hidden = !hasUpdate && !data.warning;
      if (check) check.hidden = hasUpdate || data.warning;
      text(root.querySelector("[data-version-current]"), "v" + (data.current_version || "0.1.0"));
      text(root.querySelector("[data-version-current-commit]"), data.current_commit_short || "-");
      text(root.querySelector("[data-version-latest-commit]"), data.latest_commit_short || "-");

      if (data.warning) {
        text(root.querySelector("[data-version-status]"), data.warning);
      } else if (hasUpdate) {
        var suffix = data.latest_commit_short ? " · " + data.latest_commit_short : "";
        text(root.querySelector("[data-version-status]"), "发现可更新版本 v" + data.latest_version + suffix);
      } else {
        text(root.querySelector("[data-version-status]"), "已是最新版本");
      }

      if (update) {
        update.dataset.enabled = hasUpdate && supported ? "true" : "false";
        update.disabled = !(hasUpdate && supported);
        update.textContent = hasUpdate ? "立即更新" : "暂无更新";
      }
    } catch (error) {
      root.classList.add("has-warning");
      var dot = root.querySelector("[data-version-dot]");
      if (dot) dot.hidden = false;
      text(root.querySelector("[data-version-status]"), error.message || String(error));
    } finally {
      setBusy(root, false);
    }
  }

  function pollHealth(basePath) {
    var started = Date.now();
    var timer = window.setInterval(async function () {
      if (Date.now() - started > 30000) {
        window.clearInterval(timer);
        window.location.reload();
        return;
      }
      try {
        var response = await fetch(basePath + "/healthz", { credentials: "same-origin" });
        if (response.ok) {
          window.clearInterval(timer);
          window.location.reload();
        }
      } catch (error) {
        // Service is expected to be unavailable briefly while restarting.
      }
    }, 1500);
  }

  async function update(root) {
    var basePath = root.dataset.basePath || "";
    var updateButton = root.querySelector("[data-version-update]");
    if (!updateButton || updateButton.dataset.enabled !== "true") return;
    if (!window.confirm("确认从 GitHub 拉取最新版本并重启 Ops 面板？")) return;
    setBusy(root, true);
    updateButton.textContent = "更新中";
    try {
      var response = await fetch(basePath + "/system/update", {
        method: "POST",
        credentials: "same-origin"
      });
      var data = await readJson(response);
      text(root.querySelector("[data-version-status]"), data.message || "更新完成");
      if (data.need_restart) {
        updateButton.textContent = "重启中";
        pollHealth(basePath);
      } else {
        await refresh(root, true);
      }
    } catch (error) {
      root.classList.add("has-warning");
      text(root.querySelector("[data-version-status]"), error.message || String(error));
      updateButton.textContent = "重试更新";
    } finally {
      setBusy(root, false);
    }
  }

  function closeAll(except) {
    document.querySelectorAll("[data-version-widget]").forEach(function (root) {
      if (root === except) return;
      var popover = root.querySelector("[data-version-popover]");
      var toggle = root.querySelector("[data-version-toggle]");
      if (popover) popover.hidden = true;
      if (toggle) toggle.setAttribute("aria-expanded", "false");
    });
  }

  document.addEventListener("DOMContentLoaded", function () {
    document.querySelectorAll("[data-version-widget]").forEach(function (root) {
      var toggle = root.querySelector("[data-version-toggle]");
      var popover = root.querySelector("[data-version-popover]");
      var refreshButton = root.querySelector("[data-version-refresh]");
      var updateButton = root.querySelector("[data-version-update]");
      if (!toggle || !popover) return;

      toggle.addEventListener("click", function () {
        var nextOpen = popover.hidden;
        closeAll(root);
        popover.hidden = !nextOpen;
        toggle.setAttribute("aria-expanded", String(nextOpen));
        if (nextOpen) refresh(root, true);
      });
      if (refreshButton) refreshButton.addEventListener("click", function () {
        refresh(root, true);
      });
      if (updateButton) updateButton.addEventListener("click", function () {
        update(root);
      });
      refresh(root, false);
    });

    document.addEventListener("click", function (event) {
      if (event.target.closest("[data-version-widget]")) return;
      closeAll();
    });
  });
})();
