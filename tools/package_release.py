#!/usr/bin/env python3
"""Create reproducible aiTeX source and native prefix archives."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
import re
import subprocess
import tarfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
VERSION_RE = re.compile(r'^version = "([^"]+)"$', re.MULTILINE)


def workspace_version() -> str:
    match = VERSION_RE.search((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    if not match:
        raise SystemExit("workspace version is missing")
    return match.group(1)


def source_epoch() -> int:
    raw = os.environ.get("SOURCE_DATE_EPOCH")
    if raw is None:
        raw = subprocess.run(
            ["git", "show", "-s", "--format=%ct", "HEAD"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    try:
        epoch = int(raw)
    except ValueError as error:
        raise SystemExit("SOURCE_DATE_EPOCH must be an integer") from error
    if epoch < 0:
        raise SystemExit("SOURCE_DATE_EPOCH must not be negative")
    return epoch


def host_triple() -> str:
    output = subprocess.run(
        ["rustc", "-vV"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    raise SystemExit("rustc did not report a host triple")


def require_clean_tree() -> None:
    tracked = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=no"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    untracked = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    if tracked or untracked:
        raise SystemExit("source archive requires a clean worktree")


def write_digest(archive: Path) -> None:
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(archive.suffix + ".sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii"
    )


def tar_info(name: str, data: bytes, mode: int, epoch: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    info.mtime = epoch
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    return info


def write_bundle(output_dir: Path, binary: Path, target: str, epoch: int) -> Path:
    version = workspace_version()
    if not binary.is_file():
        raise SystemExit(f"missing release executable: {binary}")
    if not os.access(binary, os.X_OK):
        raise SystemExit(f"release executable is not executable: {binary}")
    reported = subprocess.run(
        [str(binary.resolve()), "--version"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if not reported.startswith(f"aitex {version}"):
        raise SystemExit(
            f"binary reports {reported!r}, expected an aitex {version} build"
        )

    prefix = f"aitex-{version}-{target}"
    install = f"""aiTeX {version} native bundle ({target})

Requires a local TeX Live or MacTeX installation with kpsewhich on PATH.
Copy bin/aitex into an installation prefix, or run it directly from this
extracted directory. The first document build creates a local format cache
from the installed LaTeX kernel.

Examples:
  ./bin/aitex --version
  ./bin/aitex /path/to/main.tex
  install -d "$PREFIX/bin"
  install -m 0755 bin/aitex "$PREFIX/bin/aitex"

See share/doc/aitex/DISTRIBUTION.md for dependencies and supported layouts.
""".encode()

    payload: dict[str, tuple[bytes, int]] = {
        "bin/aitex": (binary.read_bytes(), 0o755),
        "share/doc/aitex/INSTALL.txt": (install, 0o644),
    }
    sbom = output_dir / f"aitex-{version}.spdx.json"
    if not sbom.is_file():
        raise SystemExit(f"missing release SBOM: {sbom}")
    payload["share/doc/aitex/aitex.spdx.json"] = (sbom.read_bytes(), 0o644)
    for source in (
        "README.md",
        "docs/DISTRIBUTION.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "THIRD_PARTY_NOTICES.md",
    ):
        path = ROOT / source
        payload[f"share/doc/aitex/{path.name}"] = (path.read_bytes(), 0o644)

    sums = "".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n"
        for name, (data, _) in sorted(payload.items())
    ).encode()
    payload["SHA256SUMS"] = (sums, 0o644)

    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"{prefix}.tar.gz"
    with archive.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as zipped:
            with tarfile.open(fileobj=zipped, mode="w", format=tarfile.PAX_FORMAT) as tar:
                directories = {
                    prefix,
                    f"{prefix}/bin",
                    f"{prefix}/share",
                    f"{prefix}/share/doc",
                    f"{prefix}/share/doc/aitex",
                }
                for directory in sorted(directories):
                    info = tarfile.TarInfo(directory + "/")
                    info.type = tarfile.DIRTYPE
                    info.mode = 0o755
                    info.mtime = epoch
                    info.uid = info.gid = 0
                    info.uname = info.gname = "root"
                    tar.addfile(info)
                for name, (data, mode) in sorted(payload.items()):
                    info = tar_info(f"{prefix}/{name}", data, mode, epoch)
                    tar.addfile(info, io.BytesIO(data))

    write_digest(archive)
    print(archive.relative_to(ROOT))
    return archive


def write_source(output_dir: Path, epoch: int) -> Path:
    require_clean_tree()
    version = workspace_version()
    prefix = f"aitex-{version}/"
    archive_tar = subprocess.run(
        ["git", "archive", "--format=tar", f"--prefix={prefix}", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    ).stdout

    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"aitex-{version}-source.tar.gz"
    with archive.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as zipped:
            zipped.write(archive_tar)
    write_digest(archive)
    print(archive.relative_to(ROOT))
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("bundle", "source", "all"))
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/aitex")
    parser.add_argument("--target", default=None)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    args = parser.parse_args()

    epoch = source_epoch()
    output_dir = (
        args.output_dir
        if args.output_dir.is_absolute()
        else ROOT / args.output_dir
    )
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    if args.kind in ("bundle", "all"):
        write_bundle(
            output_dir,
            binary,
            args.target or host_triple(),
            epoch,
        )
    if args.kind in ("source", "all"):
        write_source(output_dir, epoch)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
