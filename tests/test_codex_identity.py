from __future__ import annotations

import unittest

from app.codex_identity import codex_identity, standard_codex_user_agent, synced_codex_user_agent


class CodexIdentityTests(unittest.TestCase):
    def test_empty_user_agent_uses_synced_version_in_standard_shape(self):
        version, user_agent = codex_identity({"openai_codex_client_version_synced": "0.159.3"})
        self.assertEqual(version, "0.159.3")
        self.assertEqual(user_agent, standard_codex_user_agent("0.159.3"))

    def test_manual_version_rewrites_custom_leading_and_trailing_versions(self):
        value = "codex-tui/0.146.1 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.146.1)"
        self.assertEqual(
            synced_codex_user_agent(value, "0.160.0"),
            "codex-tui/0.160.0 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.160.0)",
        )

    def test_invalid_versions_fall_back_without_accepting_newlines(self):
        version, user_agent = codex_identity({
            "openai_codex_client_version": "latest",
            "openai_codex_client_version_synced": "bad",
            "openai_codex_user_agent": "codex-tui/0.1.0\nunsafe",
        })
        self.assertEqual(version, "0.146.0")
        self.assertNotIn("\n", user_agent)
        self.assertIn("codex-tui/0.146.0", user_agent)


if __name__ == "__main__":
    unittest.main()
