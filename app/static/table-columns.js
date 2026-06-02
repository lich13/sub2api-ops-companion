(function () {
  function safeParse(raw) {
    try {
      var parsed = JSON.parse(raw || "[]");
      return Array.isArray(parsed) ? parsed : [];
    } catch (_) {
      return [];
    }
  }

  function setColumnVisibility(table, column, hidden) {
    table.querySelectorAll('[data-col="' + column + '"]').forEach(function (cell) {
      cell.hidden = hidden;
    });
  }

  function scopedQuery(root, selector) {
    return Array.from((root || document).querySelectorAll(selector));
  }

  function wireTable(root, table) {
    var tableId = table ? table.dataset.tableId : "";
    var scope = root || document;
    var toggles = scopedQuery(scope, '.column-toggle[data-table="' + tableId + '"]');
    var reset = scope.querySelector ? scope.querySelector('.columns-reset[data-table="' + tableId + '"]') : null;
    if (!table || toggles.length === 0) {
      return;
    }

    var storageKey = "sub2ops.hiddenColumns." + tableId;
    var hiddenColumns = safeParse(localStorage.getItem(storageKey));

    function apply() {
      toggles.forEach(function (toggle) {
        var column = toggle.dataset.column;
        var hidden = hiddenColumns.indexOf(column) !== -1;
        toggle.checked = !hidden;
        setColumnVisibility(table, column, hidden);
      });
    }

    toggles.forEach(function (toggle) {
      if (toggle.getAttribute("data-column-toggle-wired") === "1") {
        return;
      }
      toggle.setAttribute("data-column-toggle-wired", "1");
      toggle.addEventListener("change", function () {
        hiddenColumns = toggles.filter(function (item) {
          return !item.checked;
        }).map(function (item) {
          return item.dataset.column;
        });
        localStorage.setItem(storageKey, JSON.stringify(hiddenColumns));
        apply();
      });
    });

    if (reset) {
      if (reset.getAttribute("data-columns-reset-wired") === "1") {
        apply();
        return;
      }
      reset.setAttribute("data-columns-reset-wired", "1");
      reset.addEventListener("click", function () {
        hiddenColumns = [];
        localStorage.removeItem(storageKey);
        apply();
      });
    }

    apply();
  }

  function wireTableColumns(root) {
    var scope = root || document;
    scopedQuery(scope, "[data-table-id]").forEach(function (table) {
      wireTable(scope, table);
    });
  }

  window.sub2opsWireTableColumns = wireTableColumns;

  document.addEventListener("DOMContentLoaded", function () {
    wireTableColumns(document);
  });
})();
