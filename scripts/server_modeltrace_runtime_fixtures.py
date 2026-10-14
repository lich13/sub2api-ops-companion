#!/usr/bin/env python3
"""Generate loopback ModelTrace runtime cases from the pinned Python source."""
from __future__ import annotations

import json
import random
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
REFERENCE_SHA = "6be7c291ed38dcc337cb88f693639040a3463899"
OUTPUT = ROOT / "tests" / "contracts" / "modeltrace-runtime" / "fixtures.json"

sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts"))


def export_pinned_app(app_root: Path, repo: Path) -> None:
    """Extract the oracle only from the fixed commit's archive."""
    with tempfile.TemporaryFile() as archive_file:
        subprocess.run(
            ["git", "archive", "--format=tar", REFERENCE_SHA, "app"],
            cwd=repo,
            check=True,
            stdout=archive_file,
        )
        archive_file.seek(0)
        with tarfile.open(fileobj=archive_file, mode="r:") as archive:
            members = archive.getmembers()
            if not members:
                raise RuntimeError(f"no app tree at pinned source {REFERENCE_SHA}")
            for member in members:
                parts = Path(member.name).parts
                if not parts or parts[0] != "app" or Path(member.name).is_absolute() or ".." in parts:
                    raise RuntimeError("pinned archive contains a path outside app/")
                destination = app_root.joinpath(*parts)
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    source = archive.extractfile(member)
                    if source is None:
                        raise RuntimeError(f"cannot read pinned archive member {member.name}")
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination.write_bytes(source.read())
                else:
                    raise RuntimeError(f"unsupported pinned archive member {member.name}")


def require_reference_commit() -> None:
    """Fail only when the pinned object is unavailable, never when HEAD advances."""
    subprocess.run(
        ["git", "cat-file", "-e", f"{REFERENCE_SHA}^{{commit}}"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )


def generated_text(numbers: list[int]) -> str:
    return ", ".join(str(number) for number in numbers)


def analysis_case(analyze: Any, snapshot: tuple[dict[str, Any], dict[str, Any]], outputs: list[str]) -> dict[str, Any]:
    return {
        "outputs": outputs,
        "prefix_reports": [
            analyze(outputs[:count], snapshot) for count in range(1, len(outputs) + 1)
        ],
    }


def runtime_cases(app_root: Path) -> dict[str, Any]:
    sys.path.insert(0, str(app_root))
    from app.modeltrace import analyze, bundled_bank, challenges, validate  # noqa: PLC0415

    bank, version = bundled_bank()
    snapshot = (bank, version)
    python_challenges = challenges()
    challenge_lengths = [length for length, _ in python_challenges]
    if (
        len(challenge_lengths) != 3
        or len(set(challenge_lengths)) != 3
        or any(not 292 <= length <= 332 for length in challenge_lengths)
    ):
        raise RuntimeError("pinned ModelTrace challenges violate the three-distinct 292..332 contract")
    known_outputs = [
        generated_text(list(range(1, 301))),
        generated_text([((index * 197 + 73) % 355) + 1 for index in range(300)]),
        generated_text([((index * 71) % 355) + 1 for index in range(300)]),
    ]

    below_99: tuple[str, dict[str, Any]] | None = None
    above_99: tuple[str, dict[str, Any]] | None = None
    automatic_target: tuple[str, dict[str, Any]] | None = None
    for model_index, model in enumerate(bank["models"]):
        for seed in range(1024):
            rng = random.Random(0x5A17 + model_index * 100000 + seed)
            numbers = rng.choices(range(1, 356), weights=model["counts"], k=300)
            rng.shuffle(numbers)
            output = generated_text(numbers)
            report = analyze([output], snapshot)
            if report is None:
                continue
            probability = report["probability"]
            if (
                below_99 is None
                and report["prediction"] == "gpt-5.6-luna"
                and 0.98995 <= probability < 0.99
                and round(probability, 2) == 0.99
            ):
                below_99 = (output, report)
            if (
                above_99 is None
                and 0.99 <= probability < 0.995
                and round(probability, 2) == 0.99
            ):
                above_99 = (output, report)
            if automatic_target is None and report["prediction"] == "gpt-5.6-luna":
                automatic_target = (output, report)
            if below_99 is not None and above_99 is not None and automatic_target is not None:
                break
        if below_99 is not None and above_99 is not None and automatic_target is not None:
            break

    if below_99 is None or above_99 is None or automatic_target is None:
        raise RuntimeError("pinned ModelTrace bank did not produce all boundary cases")

    below_outputs = [below_99[0], *known_outputs[:2]]
    above_outputs = [above_99[0], *known_outputs[:2]]
    automatic_outputs = [automatic_target[0]]
    below_case = analysis_case(analyze, snapshot, below_outputs)
    above_case = analysis_case(analyze, snapshot, above_outputs)
    automatic_case = analysis_case(analyze, snapshot, automatic_outputs)

    if not all(
        report is not None and report["probability"] < 0.99
        for report in below_case["prefix_reports"]
    ):
        raise RuntimeError("below-threshold sequence crosses the pinned 99% boundary")
    if not (
        below_case["prefix_reports"][0]["prediction"] == "gpt-5.6-luna"
        and below_case["prefix_reports"][0]["probability"] < 0.99
        and round(below_case["prefix_reports"][0]["probability"], 2) == 0.99
    ):
        raise RuntimeError("below-threshold fixture no longer exercises the raw boundary")
    if not (
        above_case["prefix_reports"][0]["probability"] >= 0.99
        and round(above_case["prefix_reports"][0]["probability"], 2) == 0.99
    ):
        raise RuntimeError("above-threshold fixture no longer exercises the raw boundary")
    if automatic_case["prefix_reports"][0]["prediction"] != "gpt-5.6-luna":
        raise RuntimeError("automatic first-group fixture lost its target prediction")

    bank_values = ",".join(str((index * 71) % 355 + 1) for index in range(80))
    thought_values = " ".join(str((index * 13) % 355 + 1) for index in range(80))
    thought_fence = f"<think>not scored 123</think>```text\n{thought_values}\n```"
    bank_report = analyze([bank_values], snapshot)
    thought_report = analyze([thought_values], snapshot)
    wrapped_report = analyze([thought_fence], snapshot)
    if len(validate(bank_values)) != 80:
        raise RuntimeError("pinned 80-number bank sequence no longer validates")
    if validate(thought_fence) != validate(thought_values):
        raise RuntimeError("pinned thought/fence cleanup changed the 80-number sequence")
    if bank_report is None or thought_report is None or wrapped_report != thought_report:
        raise RuntimeError("pinned 80-number analysis or thought/fence parity changed")

    return {
        "bank_version": version,
        "legacy_80_number_vectors": {
            "bank_sha256": version["sha256"],
            "bank_is_pinned": True,
            "valid_sequence": {
                "output": bank_values,
                "validated_count": len(validate(bank_values)),
                "report": bank_report,
            },
            "thought_fence": {
                "plain_output": thought_values,
                "wrapped_output": thought_fence,
                "plain_validated_count": len(validate(thought_values)),
                "wrapped_validated_count": len(validate(thought_fence)),
                "plain_report": thought_report,
                "wrapped_report": wrapped_report,
                "invalid_mixed_text": "1, two, 3",
                "invalid_mixed_count": len(validate("1, two, 3")),
            },
        },
        "challenge_length_contract": {
            "count": 3,
            "minimum": 292,
            "maximum": 332,
            "distinct": True,
        },
        "below_99": {
            **below_case,
            "display_probability_2dp": round(below_case["prefix_reports"][0]["probability"], 2),
        },
        "above_99": {
            **above_case,
            "display_probability_2dp": round(above_case["prefix_reports"][0]["probability"], 2),
        },
        "automatic_first_group": automatic_case,
    }


def main() -> None:
    require_reference_commit()
    with tempfile.TemporaryDirectory(prefix="sub2ops-modeltrace-runtime-") as temporary:
        app_root = Path(temporary)
        export_pinned_app(app_root, ROOT)
        fixture = {
            "schema_version": 1,
            "reference_sha": REFERENCE_SHA,
            "synthetic_only": True,
            "modeltrace_runtime": runtime_cases(app_root),
        }
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(
        json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"wrote {OUTPUT.relative_to(ROOT)} from {REFERENCE_SHA}")


if __name__ == "__main__":
    main()
