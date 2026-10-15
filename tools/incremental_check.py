#!/usr/bin/env python3
"""Qualify P3 body checkpoints against clean aiTeX DVI and PDF rebuilds.

The gate exercises a no-op hit, a late root-source edit, included-file
invalidation, and preamble invalidation in one persistent watch process. The
incremental artifact after the body edit must be byte-identical to a clean
format-based rebuild; uncertainty must produce an explicit miss.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture_source() -> str:
    paragraphs = []
    for number in range(1, 129):
        marker = " BODY-BASELINE" if number == 120 else ""
        paragraphs.append(
            f"Paragraph {number}{marker} has enough deterministic prose to exercise "
            "line breaking, page contribution state, and checkpoint provenance. "
            "The second sentence keeps the paragraph long enough to be representative.\n"
        )
        if number == 119:
            paragraphs.append("% RECONVERGENCE-BASELINE\n")
    return (
        "\\documentclass{article}\n"
        "\\usepackage{hyperref}\n"
        "\\newcommand{\\preambleword}{baseline}\n"
        "\\begin{document}\n"
        "\\section{Incremental qualification}\n"
        "\\input{fragment}\n\n"
        + "\n".join(paragraphs)
        + "\\input{late-fragment}\n\n"
        + "\\end{document}\n"
    )


def environment() -> dict[str, str]:
    env = dict(os.environ)
    env.update({
        "AITEX_INCREMENTAL": "1",
        "AITEX_MAX_COMMANDS": "2000000000",
        "AITEX_MAX_STEPS": "2000000000",
        "AITEX_STACK_MB": "64",
        "FORCE_SOURCE_DATE": "1",
        "SOURCE_DATE_EPOCH": "1700000000",
        "LC_ALL": "C",
        "TZ": "UTC",
        "max_print_line": "1000",
    })
    return env


def frame(process: subprocess.Popen[str], request: bool, timeout: int) -> str:
    if request:
        assert process.stdin is not None
        process.stdin.write("build\n")
        process.stdin.flush()
    lines: list[str] = []
    deadline = time.monotonic() + timeout
    assert process.stdout is not None
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            if process.poll() is not None:
                raise RuntimeError("watch exited before its ready frame\n" + "".join(lines)[-4000:])
            continue
        lines.append(line)
        if line.startswith("AITEX_WATCH ready "):
            output = "".join(lines)
            if " code=0 " not in line:
                raise RuntimeError("watch build failed\n" + output[-4000:])
            return output
    process.kill()
    raise RuntimeError("watch frame timed out\n" + "".join(lines)[-4000:])


def require(output: str, phrase: str, stage: str) -> None:
    if phrase not in output:
        raise RuntimeError(f"{stage} did not report {phrase!r}\n{output[-4000:]}")


def write_fixture(project: Path) -> None:
    project.mkdir(parents=True)
    (project / "main.tex").write_text(fixture_source())
    (project / "fragment.tex").write_text(
        "Included dependency version one has observable text.\n\n"
    )
    (project / "late-fragment.tex").write_text(
        "Late included dependency version one has observable text.\n\n"
    )


def clean_compare(project: Path, binary: Path, fmt: Path, route: str,
                  env: dict[str, str], timeout: int) -> None:
    incremental = project / f"incremental.{route}"
    saved = project / f"saved-incremental.{route}"
    fresh = project / f"fresh.{route}"
    shutil.copy2(incremental, saved)
    command = [
        str(binary), "pass", "main.tex", "--fmt", str(fmt), "--no-cache",
        "-file-line-error", "-halt-on-error", "-o", fresh.name,
    ]
    result = subprocess.run(
        command, cwd=project, env=env, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, timeout=timeout,
    )
    if result.returncode != 0:
        raise RuntimeError(f"clean {route} rebuild failed\n{result.stdout[-4000:]}")
    if route == "pdf" and not saved.read_bytes().startswith(b"%PDF-"):
        raise RuntimeError("incremental PDF route left private DVI bytes under a .pdf name")
    if saved.read_bytes() != fresh.read_bytes():
        raise RuntimeError(
            f"incremental {route} differs from clean rebuild: "
            f"{digest(saved)} != {digest(fresh)}"
        )


def qualify_route(base: Path, binary: Path, fmt: Path, route: str,
                  env: dict[str, str], timeout: int) -> None:
    project = base / route
    write_fixture(project)
    command = [
        str(binary), "watch", "--stdin", "main.tex", "--fmt", str(fmt),
        "--cache", str(project / ".cache"), "-file-line-error",
        "-halt-on-error", "-o", f"incremental.{route}",
    ]
    process = subprocess.Popen(
        command, cwd=project, env=env, stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1,
    )
    try:
        frame(process, False, timeout)
        # The first build creates .aux/.out inputs. The second safely misses and
        # publishes an index over their stable contents.
        frame(process, True, timeout)
        no_op = frame(process, True, timeout)
        require(no_op, "body checkpoint hit", f"{route} no-op")
        require(no_op, "no source edit", f"{route} no-op")
        require(no_op, "final artifact reused", f"{route} no-op")

        artifact = project / f"incremental.{route}"
        artifact.write_bytes(b"externally replaced output")
        repaired = frame(process, True, timeout)
        require(repaired, "body checkpoint hit", f"{route} output repair")
        require(repaired, "no source edit", f"{route} output repair")
        if "final artifact reused" in repaired:
            raise RuntimeError(f"{route} output repair trusted a changed artifact")
        if route == "pdf":
            require(repaired, "PDF object graph reused", f"{route} output repair")
        clean_compare(project, binary, fmt, route, env, timeout)
        # The clean oracle intentionally shares the project directory and thus
        # replaces main.log with a transcript naming fresh.<route>. Re-establish
        # the watched artifact set before testing early-stop reconvergence.
        restored_after_oracle = frame(process, True, timeout)
        require(restored_after_oracle, "body checkpoint hit",
                f"{route} post-oracle artifact restore")

        source = project / "main.tex"
        source.write_text(source.read_text().replace(
            "RECONVERGENCE-BASELINE", "RECONVERGENCE-MUTATION"))
        inert = frame(process, True, timeout)
        require(inert, "body checkpoint hit", f"{route} inert source edit")
        require(inert, "source suffix changed", f"{route} inert source edit")
        require(inert, "semantic reconvergence hit", f"{route} inert source edit")
        clean_compare(project, binary, fmt, route, env, timeout)
        restored_after_inert_oracle = frame(process, True, timeout)
        require(restored_after_inert_oracle, "body checkpoint hit",
                f"{route} post-inert-oracle artifact restore")

        source.write_text(source.read_text().replace("BODY-BASELINE", "BODY-EDITED"))
        edited = frame(process, True, timeout)
        require(edited, "body checkpoint hit", f"{route} body edit")
        require(edited, "source suffix changed", f"{route} body edit")
        clean_compare(project, binary, fmt, route, env, timeout)
        restored_after_body_oracle = frame(process, True, timeout)
        require(restored_after_body_oracle, "body checkpoint hit",
                f"{route} post-body-oracle artifact restore")

        late_fragment = project / "late-fragment.tex"
        late_fragment.write_text(late_fragment.read_text().replace(
            "version one", "version two with changed layout"))
        dependency_suffix = frame(process, True, timeout)
        require(dependency_suffix, "body checkpoint hit", f"{route} late dependency edit")
        require(dependency_suffix, "dependency suffix changed",
                f"{route} late dependency edit")
        clean_compare(project, binary, fmt, route, env, timeout)
        restored_after_late_dependency_oracle = frame(process, True, timeout)
        require(restored_after_late_dependency_oracle, "body checkpoint hit",
                f"{route} post-late-dependency-oracle artifact restore")

        fragment = project / "fragment.tex"
        fragment.write_text(fragment.read_text().replace(
            "version one", "version two with changed layout"))
        dependency = frame(process, True, timeout)
        require(dependency, "body checkpoint hit",
                f"{route} dependency edit")
        require(dependency, "dependency suffix changed",
                f"{route} dependency edit")
        clean_compare(project, binary, fmt, route, env, timeout)

        source.write_text(source.read_text().replace(
            "\\newcommand{\\preambleword}{baseline}",
            "\\newcommand{\\preambleword}{edited}"))
        preamble = frame(process, True, timeout)
        require(preamble, "body checkpoint miss (project prefix changed)",
                f"{route} preamble edit")
    finally:
        if process.poll() is None:
            assert process.stdin is not None
            process.stdin.write("quit\n")
            process.stdin.flush()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    print(f"incremental {route}: exact clean output and safe invalidation OK")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--format", type=Path)
    parser.add_argument("--route", choices=("both", "dvi", "pdf"), default="both")
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"binary is absent: {binary}")

    with tempfile.TemporaryDirectory(prefix="aitex-incremental-check-") as temporary:
        base = Path(temporary)
        fmt = args.format.resolve() if args.format else base / "format" / "fmt.afmt"
        if args.format is None:
            fmt.parent.mkdir()
            env = environment()
            env["AITEX_BINARY"] = str(binary)
            subprocess.run([str(ROOT / "tools" / "mkfmt.sh"), str(fmt.parent)],
                           cwd=ROOT, env=env, check=True, timeout=args.timeout)
        elif not fmt.is_file():
            raise SystemExit(f"format is absent: {fmt}")

        routes = ("dvi", "pdf") if args.route == "both" else (args.route,)
        for route in routes:
            qualify_route(base, binary, fmt, route, environment(), args.timeout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
