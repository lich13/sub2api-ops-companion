(function () {
  function formatLabel(start, end) {
    if (!start && !end) {
      return "自定义";
    }
    if (start && end && start === end) {
      return start.split("-").join("/");
    }
    return [start || "开始", end || "结束"].map(function (item) {
      return item.split("-").join("/");
    }).join(" - ");
  }

  function setActiveOption(picker, activeButton) {
    picker.querySelectorAll("[data-time-range-option]").forEach(function (button) {
      button.classList.toggle("active", button === activeButton);
    });
  }

  function setPickerValue(picker, preset, label, start, end, activeButton) {
    var presetInput = picker.querySelector("[data-time-range-preset]");
    var labelNode = picker.querySelector("[data-time-range-label]");
    var startInput = picker.querySelector("[data-time-range-start]");
    var endInput = picker.querySelector("[data-time-range-end]");
    if (presetInput) {
      presetInput.value = preset;
    }
    if (labelNode) {
      labelNode.textContent = label;
    }
    if (startInput) {
      startInput.value = start || "";
    }
    if (endInput) {
      endInput.value = end || "";
    }
    setActiveOption(picker, activeButton || null);
  }

  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-time-range-option]");
    if (!button) {
      return;
    }
    var picker = button.closest("[data-time-range-picker]");
    if (!picker) {
      return;
    }
    setPickerValue(
      picker,
      button.dataset.preset || "today",
      button.dataset.label || button.textContent.trim(),
      button.dataset.start || "",
      button.dataset.end || "",
      button
    );
  });

  function handleDateChange(event) {
    if (!event.target.matches("[data-time-range-start], [data-time-range-end]")) {
      return;
    }
    var picker = event.target.closest("[data-time-range-picker]");
    if (!picker) {
      return;
    }
    var start = (picker.querySelector("[data-time-range-start]") || {}).value || "";
    var end = (picker.querySelector("[data-time-range-end]") || {}).value || "";
    setPickerValue(picker, "custom", formatLabel(start, end), start, end, null);
  }

  document.addEventListener("input", handleDateChange);
  document.addEventListener("change", handleDateChange);
})();
