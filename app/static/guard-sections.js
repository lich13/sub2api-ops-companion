(function () {
  var sectionCache = Object.create(null);

  function rewireInsertedContent(root) {
    if (window.sub2opsWireTableColumns) {
      window.sub2opsWireTableColumns(root);
    }
    if (window.sub2opsWireGroupPickers) {
      window.sub2opsWireGroupPickers(root);
    }
    if (window.sub2opsWireGuardQueue) {
      window.sub2opsWireGuardQueue(root);
    }
  }

  function loadSection(section) {
    var target = section.querySelector("[data-guard-section-target]");
    var url = section.getAttribute("data-guard-section-url");
    if (!target || !url || section.getAttribute("data-guard-section-loaded") === "1") {
      return Promise.resolve();
    }
    if (section.getAttribute("data-guard-section-loading") === "1") {
      return Promise.resolve();
    }

    if (sectionCache[url]) {
      target.innerHTML = sectionCache[url];
      target.classList.remove("guard-section-loading", "guard-section-error");
      section.setAttribute("data-guard-section-loaded", "1");
      var cachedButton = section.querySelector("[data-guard-section-load]");
      if (cachedButton) {
        cachedButton.textContent = "已加载";
        cachedButton.disabled = true;
      }
      rewireInsertedContent(target);
      return Promise.resolve();
    }

    section.setAttribute("data-guard-section-loading", "1");
    target.classList.remove("guard-section-error");
    target.classList.add("guard-section-loading");
    target.innerHTML = '<span class="loading-dot" aria-hidden="true"></span><strong>正在加载</strong><p>正在获取最新 Guard 数据...</p>';

    return fetch(url, { credentials: "same-origin" })
      .then(function (response) {
        if (!response.ok) {
          throw new Error("HTTP " + response.status);
        }
        return response.text();
      })
      .then(function (html) {
        sectionCache[url] = html;
        target.innerHTML = html;
        target.classList.remove("guard-section-loading");
        section.setAttribute("data-guard-section-loaded", "1");
        section.removeAttribute("data-guard-section-loading");
        var button = section.querySelector("[data-guard-section-load]");
        if (button) {
          button.textContent = "已加载";
          button.disabled = true;
        }
        rewireInsertedContent(target);
      })
      .catch(function (error) {
        section.removeAttribute("data-guard-section-loading");
        target.classList.remove("guard-section-loading");
        target.classList.add("guard-section-error");
        target.innerHTML =
          "<strong>加载失败</strong><p>" +
          (error && error.message ? error.message : "未知错误") +
          '</p><button type="button" class="secondary-button small-button" data-guard-section-retry>重试</button>';
      });
  }

  function wireGuardSections(root) {
    var sections = Array.from((root || document).querySelectorAll("[data-guard-section][data-guard-section-url]"));
    if (!sections.length) {
      return;
    }

    sections.forEach(function (section) {
      if (section.getAttribute("data-guard-section-wired") === "1") {
        return;
      }
      section.setAttribute("data-guard-section-wired", "1");
      section.addEventListener("click", function (event) {
        if (event.target.closest("[data-guard-section-load], [data-guard-section-retry]")) {
          loadSection(section);
        }
      });
    });

  }

  document.addEventListener("DOMContentLoaded", function () {
    wireGuardSections(document);
  });
})();
