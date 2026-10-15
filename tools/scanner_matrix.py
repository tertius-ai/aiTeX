#!/usr/bin/env python3
"""Generate token-identity/current-command cases and compare them with TeX."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
PROBE = re.compile(r"<[^>]*>")


def cases() -> list[tuple[str, str]]:
    """Return generated scanner cases, each with exactly one semantic probe."""
    result: list[tuple[str, str]] = []

    # TeX's macro-parameter scanner consults the resolved command for an
    # implicit '#', but uses token identity for braces. Cross those two axes so
    # fixing one cannot silently break the other again (#26 followed by #32).
    for delimiter, target, value in (
        ("close", "}", "C"),
        ("open", "{", "O"),
    ):
        result.append((
            f"implicit-{delimiter}-delimiter",
            rf"\let\delimiter={target}"
            rf"\def\take#1\delimiter{{\M{{{delimiter}=#1}}}}"
            rf"\take {value}\delimiter",
        ))
        result.append((
            f"implicit-parameter-and-{delimiter}-delimiter",
            rf"\let\parameter=#\let\delimiter={target}"
            rf"\def\take\parameter1\delimiter{{\M{{both=#1}}}}"
            rf"\take {value}\delimiter",
        ))

    for prefix, value in (("bracket", "B"), ("colon", "K")):
        opening, closing = ("[", "]") if prefix == "bracket" else (":", ";")
        result.append((
            f"implicit-parameter-{prefix}",
            rf"\let\parameter=#"
            rf"\def\take{opening}\parameter1{closing}{{\M{{parameter=#1}}}}"
            rf"\take{opening}{value}{closing}",
        ))

    result.extend((
        (
            "implicit-parameter-hash-brace",
            r"\let\parameter=#"
            r"\def\take\parameter1\parameter{\M{hash-brace=#1}}"
            r"\take H#{",
        ),
        (
            "alignment-template-token-identity",
            r"\let\probe={"
            r"\def\peek{\let\probe=*\M{alignment=preserved}}"
            r"\setbox4=\vbox{\halign{&\tabskip=-1pt\peek#\cr 1\cr}}",
        ),
    ))
    return result


def source(body: str) -> str:
    return (
        r"\input plain" "\n"
        r"\def\M#1{\message{<#1>}}" "\n"
        + body + "\n"
        + r"\bye" + "\n"
    )


def run(argv: list[str], cwd: Path, environment: dict[str, str]) -> tuple[int, str]:
    process = subprocess.run(
        argv,
        cwd=cwd,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=60,
        check=False,
    )
    return process.returncode, process.stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"release binary is absent: {binary}")
    if shutil.which("tex") is None:
        parser.error("tex is absent")

    environment = dict(os.environ)
    environment.update({
        "LC_ALL": "C",
        "max_print_line": "1000",
        "AITEX_MAX_COMMANDS": "2000000000",
        "AITEX_MAX_STEPS": "2000000000",
    })
    failures: list[str] = []
    definitions = cases()
    with tempfile.TemporaryDirectory(prefix="aitex-scanner-matrix-") as temporary:
        work = Path(temporary)
        shutil.copy2(ROOT / "vendor" / "plain" / "plain.tex", work / "plain.tex")
        for index, (name, body) in enumerate(definitions):
            case = work / f"case-{index}.tex"
            case.write_text(source(body), encoding="utf-8")
            ref_code, reference = run(
                ["tex", "-ini", "-interaction=nonstopmode", case.name],
                work,
                environment,
            )
            our_code, candidate = run(
                [str(binary), "pass", case.name, "--ini"],
                work,
                environment,
            )
            ref_probes = PROBE.findall(reference)
            our_probes = PROBE.findall(candidate)
            reasons = []
            if ref_code != 0 or re.search(r"(?m)^!", reference):
                reasons.append("reference error")
            if our_code != 0 or re.search(r"(?m)^!", candidate):
                reasons.append("candidate error")
            if not ref_probes:
                reasons.append("zero reference probes")
            if ref_probes != our_probes:
                reasons.append(f"probes differ: TeX={ref_probes!r} aiTeX={our_probes!r}")
            if reasons:
                failures.append(f"{name}: {'; '.join(reasons)}")
                print(f"  FAIL  {name}: {'; '.join(reasons)}")
            else:
                print(f"  ok    {name}: {ref_probes[0]}")

    matched = len(definitions) - len(failures)
    print(f"scanner matrix: {matched}/{len(definitions)} cases match TeX")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
