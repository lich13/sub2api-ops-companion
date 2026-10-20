from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import server_history_memory as history


def completed_items() -> list[dict[str, object]]:
    return [
        {
            "id": f"{number:032x}",
            "action": "priority",
            "status": "completed",
        }
        for number in range(1, 31)
    ]


def model_item(job_id: str, status: str = "needs_confirmation") -> dict[str, object]:
    return {
        "id": job_id,
        "action": "model_test",
        "status": status,
    }


class HistoryFixtureTests(unittest.TestCase):
    def test_detection_fixture_reports_every_generated_needs_confirmation_job(self) -> None:
        with tempfile.TemporaryDirectory(prefix="history-memory-test-") as directory:
            result = history.write_detection_history(Path(directory))
            document = json.loads((Path(directory) / "model-test-results.json").read_text())

        jobs = document["jobs"]
        expected = {
            job_id for job_id, job in jobs.items()
            if job["status"] == "needs_confirmation"
        }
        actual = set(result["needs_confirmation_model_ids"])
        self.assertEqual(result["automatic_jobs"], history.DETECTION_COUNT)
        self.assertEqual(result["manual_jobs"], history.MANUAL_TEST_COUNT)
        self.assertEqual(actual, expected)
        self.assertEqual(len(actual), 733)
        self.assertTrue(all(int(job_id, 16) % 6 == 5 for job_id in actual))
        self.assertEqual(
            sum(job["automatic"] for job in jobs.values()),
            history.DETECTION_COUNT,
        )
        self.assertEqual(
            sum(not job["automatic"] for job in jobs.values()),
            history.MANUAL_TEST_COUNT,
        )

    def test_python_listing_accepts_only_completed_operations(self) -> None:
        listing = {"pending": 0, "items": completed_items()}

        result = history.validate_operation_history(listing, ())

        self.assertEqual(result["returned_completed_items"], 30)
        self.assertEqual(result["returned_items"], 30)
        self.assertEqual(result["expected_pending_model_test_items"], 0)

    def test_rust_listing_accepts_expected_model_test_confirmation_jobs(self) -> None:
        expected = frozenset(f"{number:032x}" for number in range(5, 4401, 6))
        listing = {
            "pending": len(expected),
            "items": completed_items() + [model_item(job_id) for job_id in sorted(expected)],
        }

        result = history.validate_operation_history(listing, expected)

        self.assertEqual(result["returned_completed_items"], 30)
        self.assertEqual(result["returned_items"], 763)
        self.assertEqual(result["expected_pending_model_test_items"], 733)

    def test_listing_rejects_unexpected_pending_status(self) -> None:
        expected = {"00000000000000000000000000000005"}
        listing = {
            "pending": 1,
            "items": completed_items() + [model_item(next(iter(expected)), "queued")],
        }

        with self.assertRaises(history.CheckFailure):
            history.validate_operation_history(listing, expected)

    def test_listing_rejects_missing_model_test_job(self) -> None:
        expected = {
            "00000000000000000000000000000005",
            "0000000000000000000000000000000b",
        }
        listing = {
            "pending": 1,
            "items": completed_items() + [model_item(next(iter(expected)))],
        }

        with self.assertRaises(history.CheckFailure):
            history.validate_operation_history(listing, expected)

    def test_listing_rejects_duplicate_model_test_job(self) -> None:
        job_id = "00000000000000000000000000000005"
        listing = {
            "pending": 1,
            "items": completed_items() + [model_item(job_id), model_item(job_id)],
        }

        with self.assertRaises(history.CheckFailure):
            history.validate_operation_history(listing, {job_id})


if __name__ == "__main__":
    unittest.main()
