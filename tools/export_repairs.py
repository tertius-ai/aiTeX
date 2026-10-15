#!/usr/bin/env python3
"""Export local aiTeX repair sessions as one stable JSONL row per attempt.

Session files are intentionally private project data. This command is an
explicit publication boundary: its default is full-fidelity data suitable for
model evaluation, including bounded source context and raw model responses.
Use --metadata-only when those fields must not leave the machine.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any

SESSION_SCHEMA = "aitex-repair-session-v1"
EXPORT_SCHEMA = "aitex-repair-corpus-record-v1"
MAX_SESSION_BYTES = 64 * 1024 * 1024


def session_files(inputs: list[Path]) -> list[Path]:
    found: set[Path] = set()
    for supplied in inputs:
        path = supplied.resolve()
        if path.is_file():
            found.add(path)
            continue
        if not path.is_dir():
            raise ValueError(f"input does not exist: {supplied}")
        repair_dir = path / ".aitex" / "repairs"
        root = repair_dir if repair_dir.is_dir() else path
        found.update(item.resolve() for item in root.rglob("*.json") if item.is_file())
    return sorted(found, key=lambda item: str(item))


def read_session(path: Path) -> dict[str, Any]:
    size = path.stat().st_size
    if size > MAX_SESSION_BYTES:
        raise ValueError(f"session is larger than {MAX_SESSION_BYTES} bytes: {path}")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("schema") != SESSION_SCHEMA:
        raise ValueError(f"not an {SESSION_SCHEMA} record: {path}")
    if not isinstance(value.get("attempts"), list):
        raise ValueError(f"session has no attempts array: {path}")
    return value


def metadata_attempt(attempt: dict[str, Any]) -> dict[str, Any]:
    redacted = dict(attempt)
    envelope = redacted.pop("context_envelope", None)
    response = redacted.pop("raw_response", None)
    patches = [
        redacted.pop("raw_patch", None),
        redacted.pop("normalized_patch", None),
    ]
    private = json.dumps(
        {"context_envelope": envelope, "raw_response": response, "patches": patches},
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    redacted["private_payload_sha256"] = hashlib.sha256(private).hexdigest()
    redacted["private_payload_redacted"] = True
    return redacted


def export_rows(
    path: Path, session: dict[str, Any], metadata_only: bool
) -> list[dict[str, Any]]:
    session_facts = {
        key: session.get(key)
        for key in (
            "id",
            "created_unix_ms",
            "updated_unix_ms",
            "input",
            "input_sha256",
            "compiler",
            "status",
            "limits",
            "usage",
            "applied_targets",
            "final_build_succeeded",
        )
    }
    rows = []
    for offset, raw_attempt in enumerate(session["attempts"]):
        if not isinstance(raw_attempt, dict):
            raise ValueError(f"attempt {offset} is not an object: {path}")
        attempt = metadata_attempt(raw_attempt) if metadata_only else raw_attempt
        identity = {"session": session_facts, "attempt_offset": offset, "attempt": raw_attempt}
        record_id = hashlib.sha256(
            json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        rows.append(
            {
                "schema": EXPORT_SCHEMA,
                "record_id": record_id,
                "source_record": path.name,
                "session": session_facts,
                "attempt": attempt,
            }
        )
    return rows


def encoded_rows(rows: list[dict[str, Any]]) -> bytes:
    return b"".join(
        json.dumps(row, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        for row in rows
    )


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "inputs",
        nargs="*",
        type=Path,
        default=[Path.cwd()],
        help="project, repairs directory, or individual session JSON (default: cwd)",
    )
    parser.add_argument("-o", "--output", type=Path, help="JSONL path (default: stdout)")
    parser.add_argument(
        "--metadata-only",
        action="store_true",
        help="remove prompt context, raw response, and patches; retain their digest",
    )
    arguments = parser.parse_args()

    try:
        files = session_files(arguments.inputs)
        if not files:
            raise ValueError("no repair session JSON files found")
        exported = [
            row
            for path in files
            for row in export_rows(path, read_session(path), arguments.metadata_only)
        ]
        rows = list({row["record_id"]: row for row in exported}.values())
        data = encoded_rows(rows)
        if arguments.output:
            atomic_write(arguments.output.resolve(), data)
        else:
            sys.stdout.buffer.write(data)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"export repairs: {error}", file=sys.stderr)
        return 1

    destination = arguments.output.resolve() if arguments.output else "stdout"
    print(
        f"export repairs: {len(rows)} attempts from {len(files)} sessions -> {destination}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
