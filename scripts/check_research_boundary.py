#!/usr/bin/env python3
"""Enforce the production SDK/research repository boundary."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MAX_FILE_BYTES = 1_000_000
BANNED_SUFFIXES = {
    ".7z", ".aab", ".apk", ".apks", ".bndb", ".dex", ".dll", ".dylib",
    ".elf", ".exe", ".img", ".ipa", ".iso", ".jar", ".o", ".pdf", ".rar",
    ".rom", ".so", ".sqlite", ".sqlite3", ".tar", ".tgz", ".xz", ".zip", ".zst",
}
BANNED_COMPONENTS = {
    "artifacts", "downloads", "recovered_dex", "recovered_src", "reverse",
    "brother-reverse-engineering",
}
COMMIT = re.compile(r"^[0-9a-f]{40}$")
VERSION = re.compile(r"^\d+\.\d+\.\d+$")


def candidate_paths() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    return [ROOT / value.decode() for value in result.stdout.split(b"\0") if value]


def check_files() -> list[str]:
    errors: list[str] = []
    for path in candidate_paths():
        if not path.is_file():
            continue
        relative = path.relative_to(ROOT)
        if path.stat().st_size > MAX_FILE_BYTES:
            errors.append(f"{relative}: exceeds the committed SDK file-size limit")
        if path.suffix.lower() in BANNED_SUFFIXES:
            errors.append(f"{relative}: vendor/research payload suffix is forbidden")
        if any(
            component in BANNED_COMPONENTS
            or component.startswith(("recovered_dex-", "recovered_src-"))
            for component in relative.parts
        ):
            errors.append(f"{relative}: research workspace path is forbidden")
    return errors


def check_protocol_metadata() -> list[str]:
    errors: list[str] = []
    metadata = json.loads((ROOT / "fixtures/protocol/specifications.json").read_text())
    typed = json.loads((ROOT / "fixtures/protocol/typed-contract.json").read_text())
    actions = json.loads((ROOT / "fixtures/protocol/python-actions.json").read_text())
    profiles = set(metadata["profiles"])
    metadata_keys = {(item["protocol"], item["model"]) for item in metadata["protocols"]}
    typed_keys = {(item["protocol"], item["model"]) for item in typed["protocols"]}
    if metadata_keys != typed_keys:
        errors.append("protocol specification metadata does not cover the typed contract exactly")
    action_keys = {
        (item["protocol"], item["model"], item["profile"])
        for item in actions["cases"]
    }
    expected_actions = {
        (protocol, model, profile)
        for protocol, model in metadata_keys
        for profile in profiles
    }
    if action_keys != expected_actions:
        errors.append("protocol fixture IDs do not cover every protocol/model/profile exactly")
    for item in metadata["protocols"]:
        authority = item["authority"]
        if not COMMIT.fullmatch(authority["commit"]):
            errors.append(f"{item['protocol']}: authority is not pinned to a commit")
        specification = item["specification"]
        if specification is None:
            continue
        if not COMMIT.fullmatch(specification["commit"]):
            errors.append(f"{item['protocol']}: research specification is not pinned")
        if not VERSION.fullmatch(specification["version"]):
            errors.append(f"{item['protocol']}: malformed specification version")
        if specification["maturity"] not in {"notes", "draft", "candidate", "final", "superseded"}:
            errors.append(f"{item['protocol']}: invalid specification maturity")
        if specification["authoritative"] and specification["maturity"] != "final":
            errors.append(f"{item['protocol']}: only final specifications may be authoritative")
    return errors


def main() -> int:
    errors = [*check_files(), *check_protocol_metadata()]
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
