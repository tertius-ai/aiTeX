#!/usr/bin/env python3
"""Build canonically locked classical LaTeX reference formats."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "qualification" / "reference-formats.lock.json"
CONFIG = ROOT / "qualification" / "reference-formats.cnf"
DEFAULT_OUTPUT = ROOT / "target" / "qualification" / "reference-formats"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical_fingerprint(path: Path, item: dict) -> tuple[int, str]:
    data = bytearray(gzip.decompress(path.read_bytes()))
    for offset, length in item.get("ignored_ranges", []):
        end = offset + length
        if offset < 0 or length < 1 or end > len(data):
            raise ValueError(f"invalid ignored range {offset}:{end}")
        data[offset:end] = b"\0" * length
    return len(data), hashlib.sha256(data).hexdigest()


def valid_format(path: Path, item: dict) -> bool:
    if not path.is_file():
        return False
    try:
        size, fingerprint = canonical_fingerprint(path, item)
    except (OSError, ValueError):
        return False
    return (size == item["uncompressed_size"]
            and fingerprint == item["canonical_sha256"])


def valid(output: Path, lock: dict) -> bool:
    return all(
        valid_format(output / f"{name}.fmt", item)
        for name, item in lock["formats"].items()
    )


def locked_environment(lock: dict) -> dict[str, str]:
    env = dict(os.environ)
    env.update({
        "SOURCE_DATE_EPOCH": str(lock["source_date_epoch"]),
        "FORCE_SOURCE_DATE": "1" if lock["force_source_date"] else "0",
        "TZ": lock["timezone"],
        "LC_ALL": "C",
    })
    return env


def input_directory(lock: dict) -> Path:
    encoded = json.dumps(lock.get("input_sha256", {}), sort_keys=True).encode()
    identity = hashlib.sha256(encoded).hexdigest()
    return Path("/tmp") / f"aitex-reference-format-inputs-{identity}"


def stage_inputs(lock: dict) -> Path:
    directory = input_directory(lock)
    # The canonical path is embedded in the locked TeX formats. Make this
    # immutable cache traversable across accounts on a self-hosted runner;
    # every file is still accepted only after its SHA-256 matches the lock.
    directory.mkdir(mode=0o1777, parents=True, exist_ok=True)
    try:
        directory.chmod(0o1777)
    except PermissionError:
        if directory.stat().st_mode & 0o7777 != 0o1777:
            raise
    for name, expected in lock.get("input_sha256", {}).items():
        target = directory / name
        if target.exists():
            if not target.is_file() or sha256(target) != expected:
                raise ValueError(f"staged reference format input {target} differs")
        else:
            shutil.copyfile(ROOT / "qualification" / name, target)
    return directory


def verify_probes(output: Path, lock: dict, env: dict[str, str]) -> bool:
    probe_source = lock["probe_source"]
    probe_env = dict(env)
    probe_env["TEXFORMATS"] = f"{output}:{env.get('TEXFORMATS', '')}"
    with tempfile.TemporaryDirectory(prefix="aitex-reference-format-probe-") as tmp:
        directory = Path(tmp)
        for name, item in lock["formats"].items():
            result = subprocess.run(
                [
                    name,
                    "-interaction=batchmode",
                    "-halt-on-error",
                    "-jobname=reference-format-probe",
                    probe_source,
                ],
                cwd=directory,
                env=probe_env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
            if result.returncode != 0:
                print(result.stdout, file=sys.stderr)
                print(f"reference formats: {name} probe failed", file=sys.stderr)
                return False
            probe = item["probe"]
            artifact = directory / f"reference-format-probe.{probe['extension']}"
            actual_size = artifact.stat().st_size if artifact.is_file() else None
            actual_sha256 = sha256(artifact) if artifact.is_file() else None
            if (actual_size != probe["size"]
                    or actual_sha256 != probe["sha256"]):
                print(
                    f"reference formats: {name} probe differs from the lock: "
                    f"size={actual_size!r} sha256={actual_sha256!r}; "
                    f"expected size={probe['size']} sha256={probe['sha256']}",
                    file=sys.stderr,
                )
                return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    output = args.output.resolve()
    lock = json.loads(LOCK.read_text())
    if lock.get("schema") != 2:
        parser.error("reference format lock has an unsupported schema")
    if sha256(CONFIG) != lock.get("config_sha256"):
        parser.error("reference format configuration differs from its lock")
    for name, expected in lock.get("input_sha256", {}).items():
        if sha256(ROOT / "qualification" / name) != expected:
            parser.error(f"reference format input {name} differs from its lock")
    env = locked_environment(lock)
    try:
        inputs = stage_inputs(lock)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    env["TEXINPUTS"] = f"{inputs}:{env.get('TEXINPUTS', '')}"
    if valid(output, lock):
        if not verify_probes(output, lock, env):
            return 1
        print(f"locked reference formats ready: {output}")
        return 0

    output.mkdir(parents=True, exist_ok=True)
    failed = False
    for name, item in lock["formats"].items():
        result = subprocess.run(
            [
                "fmtutil-sys", "--byfmt", name, f"--fmtdir={output}",
                f"--cnffile={CONFIG}", "--no-engine-subdir", "--nohash",
            ],
            cwd=output,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            print(result.stdout, file=sys.stderr)
            print(f"reference formats: could not build {name}", file=sys.stderr)
            failed = True
            continue
        path = output / f"{name}.fmt"
        if not valid_format(path, item):
            try:
                actual_size, actual_fingerprint = canonical_fingerprint(path, item)
            except (OSError, ValueError):
                actual_size, actual_fingerprint = None, None
            print(
                f"reference formats: {name}.fmt differs from the lock: "
                f"canonical_size={actual_size!r} "
                f"canonical_sha256={actual_fingerprint!r}; "
                f"expected size={item['uncompressed_size']} "
                f"sha256={item['canonical_sha256']}",
                file=sys.stderr,
            )
            failed = True

    if failed:
        for source in (
            CONFIG,
            Path("/var/lib/texmf/tex/generic/config/language.dat"),
            Path("/var/lib/texmf/tex/generic/config/language.def"),
        ):
            if source.is_file():
                print(
                    f"reference formats: input {source} "
                    f"size={source.stat().st_size} sha256={sha256(source)}",
                    file=sys.stderr,
                )
        return 1
    if not verify_probes(output, lock, env):
        return 1

    print(f"locked reference formats ready: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
