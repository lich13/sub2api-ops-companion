(() => {
  const lists = Array.from(document.querySelectorAll("[data-guard-sort-list]"));
  if (!lists.length) return;

  let dragged = null;

  const items = (list) => Array.from(list.querySelectorAll("[data-guard-sort-item]"));

  const syncList = (list) => {
    items(list).forEach((item, index) => {
      const rank = item.querySelector("[data-guard-rank]");
      const up = item.querySelector('[data-guard-move="up"]');
      const down = item.querySelector('[data-guard-move="down"]');
      if (rank) rank.textContent = `P${index + 1}`;
      if (up) up.disabled = index === 0;
      if (down) down.disabled = index === items(list).length - 1;
    });
  };

  const syncAll = () => lists.forEach(syncList);

  const afterElement = (list, y) => {
    return items(list)
      .filter((item) => item !== dragged)
      .reduce(
        (closest, item) => {
          const box = item.getBoundingClientRect();
          const offset = y - box.top - box.height / 2;
          if (offset < 0 && offset > closest.offset) {
            return { offset, element: item };
          }
          return closest;
        },
        { offset: Number.NEGATIVE_INFINITY, element: null },
      ).element;
  };

  lists.forEach((list) => {
    items(list).forEach((item) => {
      item.addEventListener("dragstart", (event) => {
        dragged = item;
        event.dataTransfer.effectAllowed = "move";
        event.dataTransfer.setData("text/plain", item.querySelector('input[name="account_order"]')?.value || "");
        window.requestAnimationFrame(() => item.classList.add("dragging"));
      });

      item.addEventListener("dragend", () => {
        item.classList.remove("dragging");
        dragged = null;
        syncAll();
      });
    });

    list.addEventListener("dragover", (event) => {
      if (!dragged || dragged.closest("[data-guard-sort-list]") !== list) return;
      event.preventDefault();
      const next = afterElement(list, event.clientY);
      if (next) {
        list.insertBefore(dragged, next);
      } else {
        list.appendChild(dragged);
      }
      syncList(list);
    });

    list.addEventListener("click", (event) => {
      const button = event.target.closest("[data-guard-move]");
      if (!button) return;
      const item = button.closest("[data-guard-sort-item]");
      if (!item) return;
      if (button.dataset.guardMove === "up" && item.previousElementSibling) {
        list.insertBefore(item, item.previousElementSibling);
      }
      if (button.dataset.guardMove === "down" && item.nextElementSibling) {
        list.insertBefore(item.nextElementSibling, item);
      }
      syncList(list);
    });
  });

  syncAll();
})();
