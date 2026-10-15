#!/usr/bin/env python3
"""Materialize the holdout's locked TeX Live overlay without host installation.

The compiler remains the host's TeX Live 2023 build. Locked Ubuntu data
packages only add package/class/font/language trees through TEXMFHOME, so the benchmark
gets a fuller and reproducible package surface without running maintainer
scripts, changing system maps, or installing anything as root.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "qualification" / "texlive-overlay.lock.json"
PK_LOCK = ROOT / "qualification" / "texlive-overlay.pk.json"
DESTINATION = ROOT / "corpus" / "texlive-overlay"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def pk_magnitude(dpi: int, base_dpi: int) -> str:
    if dpi == base_dpi:
        return f"1+0/{base_dpi}"
    if dpi > base_dpi:
        return f"1+{dpi - base_dpi}/{base_dpi}"
    return f"{dpi}/{base_dpi}"


def generate_pk_font(texmf: Path, font: str, dpi: int, base_dpi: int,
                     source_date_epoch: int) -> bytes:
    """Generate one bitmap without consulting any existing PK cache."""
    with tempfile.TemporaryDirectory(prefix="aitex-mktexpk-") as staging_text:
        staging = Path(staging_text)
        home = staging / "home"
        var = staging / "var"
        work = staging / "work"
        home.mkdir()
        var.mkdir()
        work.mkdir()
        pk_cache = var / "fonts" / "pk"
        pk_cache.mkdir(parents=True)
        environment = dict(os.environ)
        environment.update({
            "LC_ALL": "C",
            "SOURCE_DATE_EPOCH": str(source_date_epoch),
            "FORCE_SOURCE_DATE": "1",
            # TEXMFHOME and TEXMFVAR must not expose the destination's existing
            # PK files. MFINPUTS supplies the locked overlay sources first and
            # retains the pinned base TeX tree through the trailing colon.
            "TEXMFHOME": str(home),
            "TEXMFVAR": str(var),
            "VARTEXFONTS": str(var / "fonts"),
            "PKFONTS": f"{pk_cache}//",
            "MFINPUTS": f"{texmf / 'fonts' / 'source'}//:",
        })
        process = subprocess.run(
            ["mktexpk", "--dpi", str(dpi), "--bdpi", str(base_dpi),
             "--mag", pk_magnitude(dpi, base_dpi), font],
            cwd=work, env=environment, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True,
        )
        if process.returncode != 0:
            detail = "\n".join(process.stdout.splitlines()[-20:])
            raise SystemExit(
                f"could not reproduce PK bitmap {font}.{dpi}pk:\n{detail}"
            )
        matches = list(staging.rglob(f"{font}.{dpi}pk"))
        if len(matches) != 1:
            raise SystemExit(
                f"PK generator produced {len(matches)} copies of {font}.{dpi}pk"
            )
        return matches[0].read_bytes()


def rewrite_pk_lock(lock: dict, hashes: dict[str, str]) -> None:
    source = PK_LOCK.read_text()
    for item in lock["files"]:
        path = item["path"]
        replacement = hashes[path]
        pattern = re.compile(
            rf'("path":\s*{re.escape(json.dumps(path))},\s*"sha256":\s*")'
            r'[0-9a-f]{64}(")'
        )
        source, count = pattern.subn(rf'\g<1>{replacement}\2', source)
        if count != 1:
            raise SystemExit(f"could not update PK lock entry: {path}")
    PK_LOCK.write_text(source)


def materialize_pk_fonts(texmf: Path, refresh_lock: bool = False) -> str:
    lock_bytes = PK_LOCK.read_bytes()
    lock = json.loads(lock_bytes)
    base_dpi = int(lock["base_dpi"])
    environment = dict(os.environ)
    environment.update({
        "LC_ALL": "C",
        "SOURCE_DATE_EPOCH": str(lock["source_date_epoch"]),
        "FORCE_SOURCE_DATE": "1",
        # Pin both the input tree and kpathsea's generated-file tree. Setting
        # TEXMFHOME alone does not override TEXMFVAR: a developer's warm
        # ~/.texlive cache can then satisfy mktexpk without materializing the
        # locked file in this overlay, while a clean CI host behaves
        # differently.
        "TEXMFHOME": str(texmf),
        "TEXMFVAR": str(texmf),
        "VARTEXFONTS": str(texmf / "fonts"),
    })
    for item in lock.get("tfm_files", []):
        relative = Path(item["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise SystemExit(f"unsafe TFM lock path: {relative}")
        path = texmf / relative
        if not path.is_file():
            with tempfile.TemporaryDirectory(prefix="aitex-mktextfm-") as work:
                subprocess.run(
                    ["mktextfm", item["name"]], cwd=work, env=environment, check=True,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
        if not path.is_file() or sha256(path) != item["sha256"]:
            raise SystemExit(f"locked TFM metric mismatch: {relative}")
    expected: set[Path] = set()
    generated_hashes: dict[str, str] = {}
    pattern = re.compile(r"^(?P<font>.+)\.(?P<dpi>[0-9]+)pk$")
    for item in lock["files"]:
        relative = Path(item["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise SystemExit(f"unsafe PK lock path: {relative}")
        expected.add(relative)
        path = texmf / relative
        match = pattern.match(path.name)
        if match is None:
            raise SystemExit(f"malformed PK lock path: {relative}")
        dpi = int(match.group("dpi"))
        generated = generate_pk_font(
            texmf, match.group("font"), dpi, base_dpi,
            int(lock["source_date_epoch"]),
        )
        actual_sha = hashlib.sha256(generated).hexdigest()
        generated_hashes[item["path"]] = actual_sha
        if not refresh_lock and actual_sha != item["sha256"]:
            raise SystemExit(
                f"locked PK bitmap mismatch: {relative}: "
                f"expected {item['sha256']}, reproduced {actual_sha}"
            )
        path = texmf / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(path.suffix + ".tmp")
        temporary.write_bytes(generated)
        os.replace(temporary, path)

    pk_root = texmf / "fonts" / "pk"
    actual = ({path.relative_to(texmf) for path in pk_root.rglob("*pk")
               if path.is_file()}
              if pk_root.is_dir() else set())
    if actual != expected:
        missing = sorted(str(path) for path in expected - actual)
        extra = sorted(str(path) for path in actual - expected)
        raise SystemExit(f"PK inventory differs: missing={missing}, extra={extra}")
    if refresh_lock:
        rewrite_pk_lock(lock, generated_hashes)
        lock_bytes = PK_LOCK.read_bytes()
        print(f"refreshed {len(generated_hashes)} reproducible PK bitmap hashes")
    return hashlib.sha256(lock_bytes).hexdigest()


def prepare(destination: Path, lock_path: Path, refresh_pk_lock: bool = False) -> None:
    lock_bytes = lock_path.read_bytes()
    lock = json.loads(lock_bytes)
    lock_sha = hashlib.sha256(lock_bytes).hexdigest()
    debs = destination / "debs"
    extracted = destination / "root"
    texmf = extracted / "usr" / "share" / "texlive" / "texmf-dist"
    marker = destination / "overlay.json"
    debs.mkdir(parents=True, exist_ok=True)
    extracted.mkdir(parents=True, exist_ok=True)

    for package in lock["packages"]:
        archive = debs / package["filename"]
        if not archive.is_file():
            subprocess.run(
                ["apt-get", "download", f"{package['name']}={package['version']}"],
                cwd=debs,
                check=True,
            )
        actual_size = archive.stat().st_size
        actual_sha = sha256(archive)
        if actual_size != package["bytes"] or actual_sha != package["sha256"]:
            raise SystemExit(
                f"locked package mismatch: {archive} has {actual_size} bytes/{actual_sha}"
            )
        # dpkg-deb extraction is idempotent. It does not execute package scripts
        # and cannot modify the host TeX installation.
        subprocess.run(["dpkg-deb", "-x", str(archive), str(extracted)], check=True)

    # Debian's TeX Live data packages install below texlive/texmf-dist, while
    # a few compatible font packages (notably cm-super-minimal) use the
    # distribution-neutral /usr/share/texmf tree. Merge that data into the
    # derived overlay so one locked, database-backed TEXMFHOME sees both. The
    # downloaded archives remain the authority and are verified above.
    distribution_neutral = extracted / "usr" / "share" / "texmf"
    if distribution_neutral.is_dir():
        shutil.copytree(distribution_neutral, texmf, dirs_exist_ok=True)

    pk_lock_sha = materialize_pk_fonts(texmf, refresh_pk_lock)

    # TEXMFHOME normally permits recursive filesystem searches. This overlay is
    # a complete 2 GiB TeX tree, so repeated dependency re-resolution would
    # otherwise walk more than 100,000 files for every cache validation. Build
    # kpathsea's deterministic filename database, add it to TEXMFDBS, and use
    # the standard `!!` database-only spelling. Package contents remain the
    # locked authority.
    environment = dict(os.environ)
    # mktexlsr delegates directory enumeration to the host `ls`. Pin collation
    # so the database hash is identical on developer and hosted machines.
    environment["LC_ALL"] = "C"
    subprocess.run(["mktexlsr", str(texmf)], env=environment, check=True)
    filename_database = texmf / "ls-R"
    if not filename_database.is_file():
        raise SystemExit(f"mktexlsr did not create {filename_database}")

    environment["TEXMFHOME"] = f"!!{texmf}"
    environment["TEXMFDBS"] = f"{texmf}:{environment.get('TEXMFDBS', '')}"
    resolved: dict[str, str] = {}
    for name in lock["required_files"]:
        process = subprocess.run(
            ["kpsewhich", name], env=environment, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, check=True,
        )
        path = process.stdout.strip()
        if not path or not Path(path).is_relative_to(texmf):
            raise SystemExit(f"overlay does not resolve locked file {name}: {path or 'absent'}")
        resolved[name] = str(Path(path).relative_to(texmf))

    value = {
        "schema": 2,
        "lock_sha256": lock_sha,
        "texmfhome": str(texmf),
        "texmfhome_search": f"!!{texmf}",
        "texmfdbs_search": environment["TEXMFDBS"],
        "filename_database_sha256": sha256(filename_database),
        "pk_lock_sha256": pk_lock_sha,
        "resolved": resolved,
    }
    temporary = marker.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, marker)
    print(f"locked TeX overlay ready: {texmf}")
    print(f"lock sha256: {lock_sha}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, default=DESTINATION)
    parser.add_argument("--lock", type=Path, default=LOCK)
    parser.add_argument(
        "--refresh-pk-lock", action="store_true",
        help="rewrite PK hashes from isolated fixed-epoch reproductions",
    )
    args = parser.parse_args()
    prepare(args.destination.resolve(), args.lock.resolve(), args.refresh_pk_lock)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
