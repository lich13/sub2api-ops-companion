from __future__ import annotations

from typing import Any

ALL_GROUP_VALUE = "__all__"
DEFAULT_GROUP_NAME = "openai-default"


def unique_group_values(values: list[Any]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        item = str(value or "").strip()
        if not item or item in seen:
            continue
        seen.add(item)
        result.append(item)
    return result


def build_group_selection(
    raw_values: list[Any],
    group_rows: list[dict[str, Any]],
    default_group: str = DEFAULT_GROUP_NAME,
) -> dict[str, Any]:
    available_names = unique_group_values([row.get("name") for row in group_rows])
    default_value = default_group if default_group in available_names else (available_names[0] if available_names else default_group)
    requested = unique_group_values(raw_values)

    explicit_all = ALL_GROUP_VALUE in requested
    if explicit_all:
        selected_names = list(available_names) or [default_value]
    elif not requested:
        selected_names = list(available_names) or [default_value]
    else:
        requested_names = [value for value in requested if value != ALL_GROUP_VALUE]
        if available_names:
            requested_set = set(requested_names)
            selected_names = [name for name in available_names if name in requested_set]
        else:
            selected_names = requested_names
        if not selected_names:
            selected_names = [default_value]

    all_selected = bool(available_names) and set(selected_names) == set(available_names)
    if all_selected:
        label = "全部分组"
        form_values = [ALL_GROUP_VALUE]
    elif len(selected_names) == 1:
        label = "默认分组" if selected_names[0] == default_value else selected_names[0]
        form_values = selected_names
    else:
        label = f"{len(selected_names)} 个分组"
        form_values = selected_names

    selected_set = set(selected_names)
    options = [
        {
            **row,
            "checked": row.get("name") in selected_set,
            "is_default": row.get("name") == default_value,
        }
        for row in group_rows
    ]

    return {
        "all_value": ALL_GROUP_VALUE,
        "default_value": default_value,
        "selected": selected_names,
        "selected_set": selected_set,
        "form_values": form_values,
        "all_selected": all_selected,
        "label": label,
        "options": options,
    }
