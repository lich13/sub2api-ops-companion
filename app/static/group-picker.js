(() => {
  const pickers = document.querySelectorAll("[data-group-picker]");

  const optionLabel = (input) => input.closest("label")?.querySelector("span")?.textContent?.trim() || input.value;

  const selectedOptions = (picker) => Array.from(picker.querySelectorAll("[data-group-picker-option]:checked"));

  const updateLabel = (picker) => {
    const label = picker.querySelector("[data-group-picker-label]");
    const options = Array.from(picker.querySelectorAll("[data-group-picker-option]"));
    const selected = selectedOptions(picker);
    if (!label) return;
    if (options.length > 0 && selected.length === options.length) {
      label.textContent = "全部分组";
    } else if (selected.length === 1) {
      label.textContent = selected[0].dataset.defaultGroup === "1" ? "默认分组" : optionLabel(selected[0]);
    } else if (selected.length > 1) {
      label.textContent = `${selected.length} 个分组`;
    } else {
      label.textContent = "未选择";
    }
  };

  const ensureSelection = (picker) => {
    if (selectedOptions(picker).length > 0) return;
    const fallback =
      picker.querySelector('[data-group-picker-option][data-default-group="1"]') ||
      picker.querySelector("[data-group-picker-option]");
    if (fallback) fallback.checked = true;
    updateLabel(picker);
  };

  pickers.forEach((picker) => {
    const options = Array.from(picker.querySelectorAll("[data-group-picker-option]"));
    const defaultButton = picker.querySelector("[data-group-picker-default]");
    const allButton = picker.querySelector("[data-group-picker-all]");

    options.forEach((option) => {
      option.addEventListener("change", () => {
        ensureSelection(picker);
        updateLabel(picker);
      });
    });

    defaultButton?.addEventListener("click", () => {
      const defaultOption = options.find((option) => option.dataset.defaultGroup === "1") || options[0];
      options.forEach((option) => {
        option.checked = option === defaultOption;
      });
      updateLabel(picker);
    });

    allButton?.addEventListener("click", () => {
      options.forEach((option) => {
        option.checked = true;
      });
      updateLabel(picker);
    });

    picker.closest("form")?.addEventListener("submit", () => {
      ensureSelection(picker);
    });

    updateLabel(picker);
  });
})();
