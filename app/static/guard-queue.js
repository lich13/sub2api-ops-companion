(() => {
  const lists = Array.from(document.querySelectorAll("[data-guard-sort-list]"));
  if (!lists.length) return;

  let dragged = null;
  let pendingFrame = null;
  let pendingList = null;
  let pendingY = 0;

  const items = (list) => Array.from(list.querySelectorAll("[data-guard-sort-item]"));

  const syncList = (list) => {
    const listItems = items(list);
    listItems.forEach((item, index) => {
      const rank = item.querySelector("[data-guard-rank]");
      const up = item.querySelector('[data-guard-move="up"]');
      const down = item.querySelector('[data-guard-move="down"]');
      if (rank) rank.textContent = `P${index + 1}`;
      if (up) up.disabled = index === 0;
      if (down) down.disabled = index === listItems.length - 1;
    });
  };

  const syncAll = () => lists.forEach(syncList);

  const afterElement = (list, y) => {
    const listItems = items(list);
    return listItems
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

  const moveDragged = () => {
    pendingFrame = null;
    if (!dragged || !pendingList) return;
    const list = pendingList;
    const next = afterElement(list, pendingY);
    if (next && next !== dragged.nextElementSibling) {
      list.insertBefore(dragged, next);
    } else if (!next && dragged !== list.lastElementChild) {
      list.appendChild(dragged);
    }
  };

  lists.forEach((list) => {
    items(list).forEach((item) => {
      item.addEventListener("dragstart", (event) => {
        dragged = item;
        event.dataTransfer.effectAllowed = "move";
        const input = item.querySelector('input[name="account_order"]');
        event.dataTransfer.setData("text/plain", input ? input.value : "");
        window.requestAnimationFrame(() => item.classList.add("dragging"));
      });

      item.addEventListener("dragend", () => {
        if (pendingFrame) window.cancelAnimationFrame(pendingFrame);
        pendingFrame = null;
        pendingList = null;
        item.classList.remove("dragging");
        dragged = null;
        syncAll();
      });
    });

    list.addEventListener("dragover", (event) => {
      if (!dragged || dragged.closest("[data-guard-sort-list]") !== list) return;
      event.preventDefault();
      pendingList = list;
      pendingY = event.clientY;
      if (!pendingFrame) pendingFrame = window.requestAnimationFrame(moveDragged);
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
