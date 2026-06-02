(function () {
  function editorTarget(details) {
    return details ? details.querySelector("[data-usage-query-editor-target]") : null;
  }

  function loadUsageQueryEditor(details) {
    var target = editorTarget(details);
    var url = details ? details.getAttribute("data-usage-query-editor-url") : "";
    if (!details || !target || !url || target.getAttribute("data-usage-query-editor-loaded") === "1") {
      return Promise.resolve();
    }
    if (target.getAttribute("data-usage-query-editor-loading") === "1") {
      return Promise.resolve();
    }
    target.setAttribute("data-usage-query-editor-loading", "1");
    target.classList.add("usage-query-editor-loading");
    target.textContent = "正在加载额度查询编辑器...";
    return fetch(url, { credentials: "same-origin" })
      .then(function (response) {
        if (!response.ok) {
          throw new Error("HTTP " + response.status);
        }
        return response.text();
      })
      .then(function (html) {
        target.innerHTML = html;
        target.setAttribute("data-usage-query-editor-loaded", "1");
        target.classList.remove("usage-query-editor-loading");
      })
      .catch(function (error) {
        target.classList.remove("usage-query-editor-loading");
        target.classList.add("usage-query-editor-error");
        target.textContent = "编辑器加载失败：" + (error && error.message ? error.message : "未知错误");
      })
      .finally(function () {
        target.removeAttribute("data-usage-query-editor-loading");
      });
  }

  function wireUsageQueryEditors() {
    document.querySelectorAll("details.usage-query-config[data-usage-query-editor-url]").forEach(function (details) {
      details.addEventListener("toggle", function () {
        if (details.open) {
          loadUsageQueryEditor(details);
        }
      });
    });
  }

  function openUsageQueryEditorFromHash() {
    var hash = window.location.hash || "";
    if (!/^#usage-query-\d+$/.test(hash)) {
      return;
    }
    var target = document.getElementById(hash.slice(1));
    if (!target) {
      return;
    }
    var details = target.querySelector("details.usage-query-config");
    if (!details) {
      return;
    }
    details.open = true;
    loadUsageQueryEditor(details);
    window.requestAnimationFrame(function () {
      target.scrollIntoView({ block: "start" });
    });
  }

  document.addEventListener("DOMContentLoaded", function () {
    wireUsageQueryEditors();
    openUsageQueryEditorFromHash();
  });
  window.addEventListener("hashchange", openUsageQueryEditorFromHash);
})();
