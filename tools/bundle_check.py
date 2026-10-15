#!/usr/bin/env python3
"""Verify an aiTeX native bundle and optionally run a TeX Live smoke build."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tarfile
import tempfile
from pathlib import Path, PurePosixPath


ROOT_RE = re.compile(
    r"^aitex-(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?)-"
    r"(?P<target>(?:aarch64|x86_64)-[A-Za-z0-9_.-]+)$"
)
REQUIRED = {
    "bin/aitex",
    "share/doc/aitex/INSTALL.txt",
    "share/doc/aitex/README.md",
    "share/doc/aitex/DISTRIBUTION.md",
    "share/doc/aitex/LICENSE-MIT",
    "share/doc/aitex/LICENSE-APACHE",
    "share/doc/aitex/THIRD_PARTY_NOTICES.md",
    "share/doc/aitex/aitex.spdx.json",
    "SHA256SUMS",
}
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_MEMBER_BYTES = 256 * 1024 * 1024
MAX_MEMBERS = 64


def self_test() -> None:
    fixtures = {
        "aitex-0.3.1-x86_64-unknown-linux-gnu": (
            "0.3.1",
            "x86_64-unknown-linux-gnu",
        ),
        "aitex-0.3.1-aarch64-unknown-linux-gnu": (
            "0.3.1",
            "aarch64-unknown-linux-gnu",
        ),
        "aitex-0.3.1-aarch64-apple-darwin": (
            "0.3.1",
            "aarch64-apple-darwin",
        ),
        "aitex-0.3.1-rc.2-x86_64-apple-darwin": (
            "0.3.1-rc.2",
            "x86_64-apple-darwin",
        ),
    }
    for root, expected in fixtures.items():
        match = ROOT_RE.fullmatch(root)
        actual = (match.group("version"), match.group("target")) if match else None
        if actual != expected:
            raise SystemExit(f"bundle root parser: {root!r} gave {actual!r}")
    for root in ("aitex-0.3.1", "aitex-0.3.1-unknown-linux-gnu"):
        if ROOT_RE.fullmatch(root):
            raise SystemExit(f"bundle root parser accepted invalid root: {root!r}")


def safe_relative(name: str) -> PurePosixPath:
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts:
        raise SystemExit(f"unsafe archive member: {name}")
    return path


def parse_sums(data: bytes) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in data.decode("ascii").splitlines():
        digest, separator, name = line.partition("  ")
        if (
            not separator
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
            or not name
        ):
            raise SystemExit(f"invalid SHA256SUMS line: {line!r}")
        if name in result:
            raise SystemExit(f"duplicate SHA256SUMS path: {name}")
        safe_relative(name)
        result[name] = digest
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path, nargs="?")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--smoke",
        action="store_true",
        help="require TeX Live and compile one PDF from the extracted bundle",
    )
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("bundle checker self-test: pass")
        return 0
    if args.archive is None:
        parser.error("archive is required unless --self-test is used")

    archive = args.archive.resolve()
    if not archive.is_file():
        raise SystemExit(f"bundle does not exist: {archive}")
    if archive.stat().st_size > MAX_ARCHIVE_BYTES:
        raise SystemExit("bundle exceeds the 512 MiB archive limit")

    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        if len(members) > MAX_MEMBERS:
            raise SystemExit(f"bundle contains more than {MAX_MEMBERS} members")
        regular: dict[str, tuple[tarfile.TarInfo, bytes]] = {}
        roots: set[str] = set()
        total_size = 0
        for member in members:
            path = safe_relative(member.name.rstrip("/"))
            if not path.parts:
                raise SystemExit("archive contains an empty path")
            roots.add(path.parts[0])
            if member.issym() or member.islnk() or member.isdev():
                raise SystemExit(f"unsupported archive member type: {member.name}")
            if member.isdir():
                if member.mode & 0o777 != 0o755:
                    raise SystemExit(f"directory mode is not 0755: {member.name}")
                continue
            if not member.isfile():
                raise SystemExit(f"unsupported archive member: {member.name}")
            if member.size > MAX_MEMBER_BYTES:
                raise SystemExit(f"archive member exceeds 256 MiB: {member.name}")
            total_size += member.size
            if total_size > MAX_ARCHIVE_BYTES:
                raise SystemExit("bundle payload exceeds 512 MiB")
            relative = PurePosixPath(*path.parts[1:]).as_posix()
            if relative in regular:
                raise SystemExit(f"duplicate archive path: {relative}")
            stream = tar.extractfile(member)
            if stream is None:
                raise SystemExit(f"cannot read archive member: {member.name}")
            regular[relative] = (member, stream.read())

    if len(roots) != 1:
        raise SystemExit(f"bundle must contain exactly one top-level directory: {roots}")
    root = next(iter(roots))
    match = ROOT_RE.fullmatch(root)
    if not match:
        raise SystemExit(f"unexpected bundle root: {root}")

    missing = REQUIRED - regular.keys()
    if missing:
        raise SystemExit(f"bundle is missing: {', '.join(sorted(missing))}")
    if regular["bin/aitex"][0].mode & 0o777 != 0o755:
        raise SystemExit("bin/aitex mode is not 0755")
    for name, (member, _) in regular.items():
        expected = 0o755 if name == "bin/aitex" else 0o644
        if member.mode & 0o777 != expected:
            raise SystemExit(f"unexpected mode for {name}: {member.mode & 0o777:o}")

    expected_sums = parse_sums(regular["SHA256SUMS"][1])
    payload_names = set(regular) - {"SHA256SUMS"}
    if set(expected_sums) != payload_names:
        missing_sums = payload_names - expected_sums.keys()
        extra_sums = expected_sums.keys() - payload_names
        raise SystemExit(
            "SHA256SUMS coverage differs: "
            f"missing={sorted(missing_sums)} extra={sorted(extra_sums)}"
        )
    for name, expected in expected_sums.items():
        actual = hashlib.sha256(regular[name][1]).hexdigest()
        if actual != expected:
            raise SystemExit(f"SHA-256 mismatch for {name}")

    sbom = json.loads(regular["share/doc/aitex/aitex.spdx.json"][1])
    if sbom.get("spdxVersion") != "SPDX-2.3":
        raise SystemExit("bundled dependency inventory is not SPDX 2.3")
    cli_versions = {
        package.get("versionInfo")
        for package in sbom.get("packages", [])
        if package.get("name") == "aitex-cli"
    }
    if cli_versions != {match.group("version")}:
        raise SystemExit(f"bundled SBOM has unexpected aitex-cli versions: {cli_versions}")

    with tempfile.TemporaryDirectory(prefix="aitex-bundle-check-") as temporary:
        extracted = Path(temporary) / root
        for name, (member, data) in regular.items():
            destination = extracted.joinpath(*PurePosixPath(name).parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
            destination.chmod(member.mode & 0o777)

        binary = extracted / "bin/aitex"
        reported = subprocess.run(
            [str(binary), "--version"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        version = match.group("version")
        if not reported.startswith(f"aitex {version}"):
            raise SystemExit(
                f"bundle reports {reported!r}, expected an aitex {version} build"
            )

        if args.smoke:
            if subprocess.run(["sh", "-c", "command -v kpsewhich >/dev/null"]).returncode:
                raise SystemExit("--smoke requires TeX Live or MacTeX on PATH")
            project = Path(temporary) / "project"
            project.mkdir()
            (project / "main.tex").write_text(
                "\\documentclass{article}\n"
                "\\begin{document}\n"
                "aiTeX native bundle smoke test.\n"
                "\\end{document}\n",
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment.pop("AITEX_FORMAT", None)
            environment["XDG_CACHE_HOME"] = str(Path(temporary) / "cache")
            subprocess.run(
                [str(binary), "main.tex", "-halt-on-error", "--no-cache"],
                cwd=project,
                env=environment,
                check=True,
            )
            pdf = project / "main.pdf"
            if not pdf.is_file() or not pdf.read_bytes().startswith(b"%PDF-"):
                raise SystemExit("bundle smoke build did not produce a PDF")
            formats = list((Path(temporary) / "cache" / "aitex" / "formats").glob("*"))
            if not any(path.name == "latex.afmt" for path in formats):
                raise SystemExit("bundle smoke build did not create its local format cache")

    print(
        f"native bundle: valid · aiTeX {match.group('version')} · "
        f"{match.group('target')} · {len(payload_names)} payload files"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
