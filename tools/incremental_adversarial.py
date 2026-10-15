#!/usr/bin/env python3
"""Exercise hostile P3 invalidation cases against clean DVI and PDF rebuilds."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import incremental_check as core


ROOT = Path(__file__).resolve().parent.parent


def source() -> str:
    paragraphs = "".join(
        f"Adversarial paragraph {number} carries marks, counters, macro state, "
        "and enough ordinary prose to distribute live checkpoints.\n\n"
        for number in range(1, 180)
    )
    huge = "largeword " * 20_000
    return (
        "\\documentclass{article}\n"
        "\\usepackage{graphicx}\n"
        "\\usepackage{hyperref}\n"
        "\\makeindex\n"
        "\\newwrite\\auditstream\n"
        "\\newcounter{hostilecounter}\n"
        "\\begin{document}\n"
        "\\tableofcontents\n"
        "\\markboth{HOSTILE-MARK-BASELINE}{HOSTILE-MARK-BASELINE}\n"
        "\\section{Boundary state}\\label{sec:boundary}\n"
        "\\input{defs}\n"
        "\\IfFileExists{generated.tex}{\\input{generated}}{Missing generated input.}\n"
        "\\begin{figure}[ht]\\centering\\includegraphics[width=12pt]{box.eps}"
        "\\caption{HOSTILE-FLOAT-BASELINE}\\label{fig:hostile}\\end{figure}\n"
        "Footnote state\\footnote{HOSTILE-FOOTNOTE-BASELINE}; references "
        "\\ref{sec:boundary} and \\ref{fig:hostile}; citation \\cite{hostile}.\n\n"
        "\\immediate\\openout\\auditstream=hostile.audit\n"
        "\\immediate\\write\\auditstream{HOSTILE-WRITE-BASELINE}\n"
        "\\immediate\\closeout\\auditstream\n"
        "\\immediate\\write18{touch aitex-shell-escape-must-not-exist}\n"
        "\\pdfsetrandomseed 4242 Random value: \\number\\pdfuniformdeviate 1000000.\n\n"
        + paragraphs
        + "\\section{Large paragraph}\n" + huge + "\n\n"
        "\\newpage\\setcounter{page}{40}Page counter HOSTILE-PAGE-BASELINE.\n"
        "\\index{hostile entry}\n"
        "\\begin{thebibliography}{1}\\bibitem{hostile} Hostile bibliography item."
        "\\end{thebibliography}\n"
        "\\end{document}\n"
    )


def write_fixture(project: Path) -> None:
    project.mkdir(parents=True)
    (project / "main.tex").write_text(source())
    (project / "defs.tex").write_text(
        "\\global\\def\\crossfilemacro{HOSTILE-MACRO-BASELINE}\n"
        "\\global\\setcounter{hostilecounter}{7}\n"
        "Cross-file macro: \\crossfilemacro; counter: \\arabic{hostilecounter}.\n\n"
    )
    (project / "box.eps").write_text(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n"
        "newpath 0 0 moveto 10 10 lineto stroke\nshowpage\n"
    )


def settled_frame(process: subprocess.Popen[str], timeout: int) -> str:
    outputs = []
    for _ in range(5):
        output = core.frame(process, True, timeout)
        outputs.append(output)
        if "converged: no rerun needed" in output:
            return "".join(outputs)
    raise RuntimeError("incremental mutation did not converge\n" + "".join(outputs)[-3000:])


def clean_compare(project: Path, binary: Path, fmt: Path, route: str,
                  env: dict[str, str], timeout: int) -> None:
    saved = project / f"saved.{route}"
    shutil.copy2(project / f"incremental.{route}", saved)
    result = subprocess.run(
        [str(binary), "pass", "main.tex", "--fmt", str(fmt), "--no-cache",
         "-file-line-error", "-halt-on-error", "-o", f"clean.{route}"],
        cwd=project, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, timeout=timeout,
    )
    if result.returncode != 0:
        raise RuntimeError(f"clean {route} failed\n{result.stdout[-3000:]}")
    if saved.read_bytes() != (project / f"clean.{route}").read_bytes():
        raise RuntimeError(f"hostile incremental {route} differs from clean rebuild")


def replace(path: Path, old: str, new: str) -> None:
    text = path.read_text()
    if old not in text:
        raise RuntimeError(f"mutation marker is absent: {old}")
    path.write_text(text.replace(old, new))


def qualify_route(base: Path, binary: Path, fmt: Path, route: str,
                  timeout: int) -> None:
    project = base / route
    write_fixture(project)
    env = core.environment()
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
        core.frame(process, False, timeout)
        for _ in range(5):
            ready = core.frame(process, True, timeout)
            if "final artifact reused" in ready:
                break
        else:
            raise RuntimeError(f"{route} hostile fixture did not reach a no-op hit")

        mutations = [
            ("output-float-footnote-mark-page", lambda: [
                replace(project / "main.tex", "HOSTILE-MARK-BASELINE", "HOSTILE-MARK-MUTATION"),
                replace(project / "main.tex", "HOSTILE-FLOAT-BASELINE", "HOSTILE-FLOAT-MUTATION"),
                replace(project / "main.tex", "HOSTILE-FOOTNOTE-BASELINE", "HOSTILE-FOOTNOTE-MUTATION"),
                replace(project / "main.tex", "HOSTILE-PAGE-BASELINE", "HOSTILE-PAGE-MUTATION"),
            ]),
            ("catcode-global-counter-cross-file", lambda: [
                replace(project / "defs.tex", "HOSTILE-MACRO-BASELINE", "HOSTILE-MACRO-MUTATION"),
                replace(project / "defs.tex", "{7}", "{9}"),
            ]),
            ("labels-citations-index-writes", lambda: [
                replace(project / "main.tex", "HOSTILE-WRITE-BASELINE", "HOSTILE-WRITE-MUTATION"),
                replace(project / "main.tex", "Hostile bibliography item", "Mutated bibliography item"),
                replace(project / "main.tex", "hostile entry", "mutated entry"),
            ]),
            ("missing-then-created", lambda: (project / "generated.tex").write_text(
                "Generated dependency now exists and contributes text.\n\n")),
            ("graphics", lambda: replace(project / "box.eps", "10 10 lineto", "10 0 lineto")),
            ("randomness", lambda: replace(project / "main.tex", "seed 4242", "seed 4343")),
        ]
        for label, mutation in mutations:
            mutation()
            output = settled_frame(process, timeout)
            if "body checkpoint hit" not in output and "body checkpoint miss" not in output:
                raise RuntimeError(f"{route} {label} had no cache decision")
            if (project / "aitex-shell-escape-must-not-exist").exists():
                raise RuntimeError("shell escape command executed")
            clean_compare(project, binary, fmt, route, env, timeout)
        print(f"incremental hostile {route}: all mutations exact and shell escape refused")
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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/aitex")
    parser.add_argument("--format", type=Path)
    parser.add_argument("--route", choices=("both", "dvi", "pdf"), default="both")
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="aitex-incremental-hostile-") as temporary:
        base = Path(temporary)
        fmt = args.format.resolve() if args.format else base / "format" / "fmt.afmt"
        if args.format is None:
            fmt.parent.mkdir()
            env = core.environment()
            env["AITEX_BINARY"] = str(binary)
            subprocess.run([str(ROOT / "tools/mkfmt.sh"), str(fmt.parent)], cwd=ROOT,
                           env=env, check=True, timeout=args.timeout,
                           stdout=subprocess.DEVNULL)
        routes = ("dvi", "pdf") if args.route == "both" else (args.route,)
        for route in routes:
            qualify_route(base, binary, fmt, route, args.timeout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
