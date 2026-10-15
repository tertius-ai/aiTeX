#!/usr/bin/env python3
"""Generate a deterministic SPDX 2.3 dependency inventory for aiTeX."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
from pathlib import Path
from urllib.parse import quote


ROOT = Path(__file__).resolve().parent.parent
VERSION_RE = re.compile(r'^version = "([^"]+)"$', re.MULTILINE)


def workspace_version() -> str:
    match = VERSION_RE.search((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    if not match:
        raise SystemExit("workspace version is missing")
    return match.group(1)


def created_time() -> str:
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
        moment = dt.datetime.fromtimestamp(int(raw), tz=dt.timezone.utc)
    except (ValueError, OSError) as error:
        raise SystemExit("invalid SOURCE_DATE_EPOCH") from error
    return moment.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def spdx_id(package_id: str) -> str:
    digest = hashlib.sha256(package_id.encode()).hexdigest()[:16]
    return f"SPDXRef-Package-{digest}"


def purl(name: str, version: str) -> str:
    return f"pkg:cargo/{quote(name, safe='')}@{quote(version, safe='')}"


def generate(output: Path) -> dict[str, object]:
    metadata = json.loads(
        subprocess.run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--all-features",
            ],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    version = workspace_version()
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()

    package_by_id = {package["id"]: package for package in metadata["packages"]}
    workspace = set(metadata["workspace_members"])
    packages: list[dict[str, object]] = []
    for package_id, package in sorted(
        package_by_id.items(), key=lambda item: (item[1]["name"], item[1]["version"], item[0])
    ):
        source = package.get("source")
        download = "NOASSERTION"
        if isinstance(source, str) and source.startswith("registry+"):
            download = (
                f"https://crates.io/api/v1/crates/{package['name']}/"
                f"{package['version']}/download"
            )
        entry: dict[str, object] = {
            "name": package["name"],
            "SPDXID": spdx_id(package_id),
            "versionInfo": package["version"],
            "downloadLocation": download,
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": package.get("license") or "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": purl(package["name"], package["version"]),
                }
            ],
            "primaryPackagePurpose": (
                "APPLICATION"
                if package["name"] == "aitex-cli"
                else "LIBRARY"
            ),
        }
        if source:
            entry["sourceInfo"] = str(source)
        packages.append(entry)

    relationships: list[dict[str, str]] = []
    for package_id in sorted(workspace):
        relationships.append(
            {
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": spdx_id(package_id),
            }
        )
    resolve = metadata.get("resolve") or {}
    for node in sorted(resolve.get("nodes") or [], key=lambda item: item["id"]):
        for dependency in sorted(node.get("dependencies") or []):
            relationships.append(
                {
                    "spdxElementId": spdx_id(node["id"]),
                    "relationshipType": "DEPENDS_ON",
                    "relatedSpdxElement": spdx_id(dependency),
                }
            )

    document: dict[str, object] = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"aiTeX-{version}",
        "documentNamespace": (
            f"https://github.com/tertius-ai/aiTeX/sbom/{version}/{commit}"
        ),
        "creationInfo": {
            "created": created_time(),
            "creators": ["Tool: aiTeX-tools-sbom.py"],
        },
        "packages": packages,
        "relationships": relationships,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(output.relative_to(ROOT))
    return document


def check(path: Path) -> None:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("spdxVersion") != "SPDX-2.3":
        raise SystemExit("SBOM is not SPDX 2.3")
    packages = document.get("packages")
    if not isinstance(packages, list) or not packages:
        raise SystemExit("SBOM contains no packages")
    names = {package.get("name") for package in packages}
    required = {
        "aitex-cli",
        "aitex-core",
        "aitex-driver",
        "aitex-engine",
        "aitex-fonts",
        "aitex-harness",
        "aitex-page",
        "aitex-unicode",
    }
    missing = required - names
    if missing:
        raise SystemExit(f"SBOM is missing workspace packages: {sorted(missing)}")
    ids = [package.get("SPDXID") for package in packages]
    if len(ids) != len(set(ids)):
        raise SystemExit("SBOM contains duplicate package SPDX identifiers")
    print(f"SBOM: valid · SPDX 2.3 · {len(packages)} packages")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", type=Path)
    args = parser.parse_args()
    if args.check:
        check(args.check)
        return 0
    output = args.output or ROOT / "dist" / f"aitex-{workspace_version()}.spdx.json"
    if not output.is_absolute():
        output = ROOT / output
    generate(output)
    check(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
