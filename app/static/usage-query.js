(function () {
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
    window.requestAnimationFrame(function () {
      target.scrollIntoView({ block: "start" });
    });
  }

  document.addEventListener("DOMContentLoaded", openUsageQueryEditorFromHash);
  window.addEventListener("hashchange", openUsageQueryEditorFromHash);
})();
