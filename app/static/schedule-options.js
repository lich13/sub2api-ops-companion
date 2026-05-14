(() => {
  const syncGrid = (grid) => {
    const options = Array.from(grid.querySelectorAll("[data-schedule-option]"));
    options.forEach((option) => {
      const input = option.querySelector('input[type="radio"][name="interval_minutes"]');
      option.classList.toggle("selected", Boolean(input?.checked));
    });
  };

  document.querySelectorAll(".schedule-option-grid").forEach((grid) => {
    syncGrid(grid);
    grid.addEventListener("change", (event) => {
      const target = event.target;
      if (!(target instanceof HTMLInputElement)) return;
      if (target.type !== "radio" || target.name !== "interval_minutes") return;
      syncGrid(grid);
    });
  });
})();
