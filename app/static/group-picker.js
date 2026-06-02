(() => {
  const optionLabel = (input) => input.closest("label")?.querySelector("span")?.textContent?.trim() || input.value;

  const selectedOptions = (picker) => Array.from(picker.querySelectorAll("[data-group-picker-option]:checked"));

  const pickerConfig = (picker) => ({
    allowEmpty: picker.dataset.allowEmpty === "1",
    allLabel: picker.dataset.allLabel || "全部分组",
    countSuffix: picker.dataset.countSuffix || "个分组",
    defaultLabel: picker.dataset.defaultLabel || "默认分组",
    emptyLabel: picker.dataset.emptyLabel || "未选择",
  });

  const updateLabel = (picker) => {
    const config = pickerConfig(picker);
    const label = picker.querySelector("[data-group-picker-label]");
    const options = Array.from(picker.querySelectorAll("[data-group-picker-option]"));
    const selected = selectedOptions(picker);
    if (!label) return;
    if (options.length > 0 && selected.length === options.length) {
      label.textContent = config.allLabel;
    } else if (selected.length === 1) {
      label.textContent = selected[0].dataset.defaultGroup === "1" ? config.defaultLabel : optionLabel(selected[0]);
    } else if (selected.length > 1) {
      label.textContent = `${selected.length} ${config.countSuffix}`;
    } else {
      label.textContent = config.emptyLabel;
    }
  };

  const ensureSelection = (picker) => {
    if (selectedOptions(picker).length > 0) return;
    if (pickerConfig(picker).allowEmpty) {
      updateLabel(picker);
      return;
    }
    const fallback =
      picker.querySelector('[data-group-picker-option][data-default-group="1"]') ||
      picker.querySelector("[data-group-picker-option]");
    if (fallback) fallback.checked = true;
    updateLabel(picker);
  };

  const wireGroupPickers = (root = document) => {
    root.querySelectorAll("[data-group-picker]").forEach((picker) => {
      if (picker.dataset.groupPickerWired === "1") {
        updateLabel(picker);
        return;
      }
      picker.dataset.groupPickerWired = "1";
      const options = Array.from(picker.querySelectorAll("[data-group-picker-option]"));
      const defaultButton = picker.querySelector("[data-group-picker-default]");
      const allButton = picker.querySelector("[data-group-picker-all]");
      const clearButton = picker.querySelector("[data-group-picker-clear]");

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

      clearButton?.addEventListener("click", () => {
        options.forEach((option) => {
          option.checked = false;
        });
        ensureSelection(picker);
        updateLabel(picker);
      });

      picker.closest("form")?.addEventListener("submit", () => {
        ensureSelection(picker);
      });

      updateLabel(picker);
    });
  };

  window.sub2opsWireGroupPickers = wireGroupPickers;

  document.addEventListener("DOMContentLoaded", () => wireGroupPickers(document));
})();
