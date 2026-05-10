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

  function wireTable(tableId) {
    var table = document.querySelector('[data-table-id="' + tableId + '"]');
    var toggles = Array.from(document.querySelectorAll('.column-toggle[data-table="' + tableId + '"]'));
    var reset = document.querySelector('.columns-reset[data-table="' + tableId + '"]');
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
      reset.addEventListener("click", function () {
        hiddenColumns = [];
        localStorage.removeItem(storageKey);
        apply();
      });
    }

    apply();
  }

  document.addEventListener("DOMContentLoaded", function () {
    document.querySelectorAll("[data-table-id]").forEach(function (table) {
      wireTable(table.dataset.tableId);
    });
  });
})();
