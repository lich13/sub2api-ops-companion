from __future__ import annotations

import unittest
from pathlib import Path

from app.group_selection import ALL_GROUP_VALUE, build_group_selection
from app.sql import QUALITY_SQL, SCHEDULED_TEST_ACCOUNTS_SQL

REPO_ROOT = Path(__file__).resolve().parents[1]


class GroupSelectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.groups = [
            {"name": "openai-default", "platform": "openai"},
            {"name": "openai-backup", "platform": "openai"},
            {"name": "anthropic-default", "platform": "anthropic"},
        ]

    def test_missing_group_defaults_to_all_groups_for_panel_navigation(self) -> None:
        selection = build_group_selection([], self.groups)

        self.assertEqual(selection["selected"], ["openai-default", "openai-backup", "anthropic-default"])
        self.assertEqual(selection["label"], "全部分组")
        self.assertEqual(selection["form_values"], [ALL_GROUP_VALUE])

    def test_all_group_value_selects_every_group(self) -> None:
        selection = build_group_selection([ALL_GROUP_VALUE], self.groups)

        self.assertEqual(selection["selected"], ["openai-default", "openai-backup", "anthropic-default"])
        self.assertEqual(selection["label"], "全部分组")
        self.assertEqual(selection["form_values"], [ALL_GROUP_VALUE])

    def test_multiple_groups_are_deduplicated_and_ordered_by_available_groups(self) -> None:
        selection = build_group_selection(["openai-backup", "missing", "openai-default", "openai-backup"], self.groups)

        self.assertEqual(selection["selected"], ["openai-default", "openai-backup"])
        self.assertEqual(selection["label"], "2 个分组")
        self.assertEqual(selection["form_values"], ["openai-default", "openai-backup"])

    def test_panels_use_shared_group_picker(self) -> None:
        base = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")
        stability = (REPO_ROOT / "app" / "templates" / "index.html").read_text(encoding="utf-8")
        speed = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")
        scheduled = (REPO_ROOT / "app" / "templates" / "scheduled_tests.html").read_text(encoding="utf-8")
        picker = (REPO_ROOT / "app" / "templates" / "group_picker.html").read_text(encoding="utf-8")

        self.assertIn("group-picker.js", base)
        self.assertIn("group_picker(group_selection)", stability)
        self.assertIn("group_picker(group_selection)", speed)
        self.assertIn("group_picker(group_selection)", scheduled)
        self.assertIn("data-group-picker-default", picker)
        self.assertIn("data-group-picker-all", picker)
        self.assertIn('type="checkbox"', picker)
        self.assertNotIn('<select name="group">', stability + speed + scheduled)

    def test_group_picker_script_supports_optional_clearable_picker(self) -> None:
        script = (REPO_ROOT / "app" / "static" / "group-picker.js").read_text(encoding="utf-8")

        self.assertIn("allowEmpty", script)
        self.assertIn("data-group-picker-clear", script)
        self.assertIn("emptyLabel", script)
        self.assertIn("countSuffix", script)

    def test_sql_filters_by_group_array(self) -> None:
        self.assertIn("g.name = ANY(%(group_names)s::text[])", QUALITY_SQL)
        self.assertIn("g.name = ANY(%(group_names)s::text[])", SCHEDULED_TEST_ACCOUNTS_SQL)
        self.assertIn("SELECT DISTINCT ON (a.id)", QUALITY_SQL)
        self.assertNotIn("%(group_name)s", QUALITY_SQL + SCHEDULED_TEST_ACCOUNTS_SQL)


if __name__ == "__main__":
    unittest.main()
