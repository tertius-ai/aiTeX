#!/usr/bin/env python3
"""Black-box correctness checks for the persistent converged-output cache."""

from __future__ import annotations

import argparse
import os
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
HIT = "persistent output cache hit"
WRITTEN = "persistent output cache written"


def invoke(binary: Path, fmt: Path, project: Path, output: str,
           options_first: bool = False, no_cache: bool = False,
           source_date_epoch: str = "1000000000",
           test_clock: str | None = None) -> tuple[bytes, str]:
    env = dict(os.environ)
    env.update({
        "SOURCE_DATE_EPOCH": source_date_epoch,
        "FORCE_SOURCE_DATE": "1",
        "max_print_line": "1000",
        "AITEX_MAX_COMMANDS": "2000000000",
        "AITEX_MAX_STEPS": "2000000000",
        "AITEX_STACK_MB": "64",
        "LC_ALL": "C",
        "TZ": "UTC",
    })
    if test_clock is not None:
        env["AITEX_TEST_CLOCK"] = test_clock
    options = ["--fmt", str(fmt)]
    options.extend(["--no-cache"] if no_cache else ["--cache", ".cache"])
    options.extend(["-file-line-error", "-halt-on-error", "-o", output])
    arguments = (["pass", *options, "main.tex"] if options_first
                 else ["pass", "main.tex", *options])
    result = subprocess.run(
        [str(binary), *arguments],
        cwd=project,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    transcript = result.stdout.decode("latin-1", "replace")
    if result.returncode != 0:
        raise AssertionError(
            f"compiler exited {result.returncode} for {output}:\n{transcript[-4000:]}"
        )
    path = project / output
    if output == "/dev/null":
        if not path.is_char_device():
            raise AssertionError("/dev/null is no longer a character device")
    elif not path.is_file():
        raise AssertionError(f"compiler did not create {path}:\n{transcript[-4000:]}")
    return path.read_bytes(), transcript


def warm_until_ready(binary: Path, fmt: Path, project: Path,
                     output: str, source_date_epoch: str = "1000000000",
                     test_clock: str | None = None) -> bytes:
    for _ in range(6):
        artifact, transcript = invoke(
            binary, fmt, project, output,
            source_date_epoch=source_date_epoch,
            test_clock=test_clock,
        )
        if WRITTEN in transcript or HIT in transcript:
            return artifact
    raise AssertionError(f"{output} did not reach a cacheable fixed point")


def require_hit(binary: Path, fmt: Path, project: Path, output: str,
                expected: bytes, source_date_epoch: str = "1000000000",
                test_clock: str | None = None) -> None:
    artifact, transcript = invoke(
        binary, fmt, project, output,
        source_date_epoch=source_date_epoch,
        test_clock=test_clock,
    )
    if HIT not in transcript:
        raise AssertionError(f"expected a persistent output hit for {output}:\n{transcript}")
    if artifact != expected:
        raise AssertionError(f"cache hit changed {output}")


def require_miss(binary: Path, fmt: Path, project: Path, output: str,
                 source_date_epoch: str = "1000000000",
                 test_clock: str | None = None) -> bytes:
    artifact, transcript = invoke(
        binary, fmt, project, output,
        source_date_epoch=source_date_epoch,
        test_clock=test_clock,
    )
    if HIT in transcript:
        raise AssertionError(f"unsafe persistent output hit for {output}:\n{transcript}")
    return artifact


def exercise(binary: Path, fmt: Path, extension: str) -> None:
    with tempfile.TemporaryDirectory(prefix=f"aitex-output-cache-{extension}.") as tmp:
        project = Path(tmp)
        (project / "main.tex").write_text(
            "\\documentclass{article}\n"
            "\\newwrite\\side\n"
            "\\begin{document}\n"
            "\\immediate\\openout\\side=state.dat\n"
            "\\immediate\\write\\side{stable side artifact}\n"
            "\\input{body}\n"
            "\\label{anchor}\n"
            "\\immediate\\closeout\\side\n"
            "\\end{document}\n",
            encoding="ascii",
        )
        body = project / "body.tex"
        body.write_text("Original body.\\par\n", encoding="ascii")
        output = f"main.{extension}"

        _, first_transcript = invoke(binary, fmt, project, output)
        if "rerun needed: main.aux changed" not in first_transcript:
            raise AssertionError(
                "final-page auxiliary writes were not included in rerun detection"
            )
        baseline = warm_until_ready(binary, fmt, project, output)
        if extension == "dvi" and not baseline.startswith(b"\xf7"):
            raise AssertionError("cached DVI has no preamble")
        if extension == "pdf" and not baseline.startswith(b"%PDF-"):
            raise AssertionError("cached PDF has no header")
        require_hit(binary, fmt, project, output, baseline)
        artifact, transcript = invoke(binary, fmt, project, output, options_first=True)
        if HIT not in transcript or artifact != baseline:
            raise AssertionError(f"options-first invocation missed for {output}:\n{transcript}")

        # The primary artifact is the payload, not a dependency: deleting or
        # replacing it must be repairable without executing TeX.
        (project / output).unlink()
        require_hit(binary, fmt, project, output, baseline)
        (project / output).write_bytes(b"not the compiled artifact")
        require_hit(binary, fmt, project, output, baseline)

        # The complete top-level source is in the key and transitive inputs are
        # in the manifest, so either kind of edit must execute the document.
        body.write_text("Changed body with visibly different words.\\par\n", encoding="ascii")
        changed = require_miss(binary, fmt, project, output)
        if changed == baseline:
            raise AssertionError("included-source edit did not change the output")

        # Every non-primary output is a proof obligation. Missing or edited
        # auxiliary state, custom write streams, and the transcript all force a
        # real pass, which repairs them before a later entry can be reused.
        warm_until_ready(binary, fmt, project, output)
        require_hit(binary, fmt, project, output, changed)
        mutations = (
            ("main.aux", lambda path: path.write_bytes(
                path.read_bytes() + b"% externally changed\n")),
            ("state.dat", lambda path: path.write_bytes(b"externally changed\n")),
            ("main.log", lambda path: path.unlink()),
        )
        for artifact, mutate in mutations:
            path = project / artifact
            mutate(path)
            require_miss(binary, fmt, project, output)
            fixed = warm_until_ready(binary, fmt, project, output)
            require_hit(binary, fmt, project, output, fixed)


def exercise_preamble_cache_safety(binary: Path, fmt: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="aitex-preamble-cache-safety.") as tmp:
        project = Path(tmp)
        source = project / "main.tex"
        source.write_text(
            "\\documentclass{article}\n"
            "% \\begin{document} is documentation, not a boundary\n"
            "\\newcommand{\\fakeboundary}{\\begin{document}}\n"
            "\\long\\def\\capture#1END{}\n"
            "\\capture \\begin{document} END\n"
            "\\begin {document}\n"
            "The real body.\n"
            "\\end{document}\n",
            encoding="ascii",
        )
        uncached, _ = invoke(binary, fmt, project, "uncached.dvi", no_cache=True)
        cached, transcript = invoke(binary, fmt, project, "cached.dvi")
        if cached != uncached:
            raise AssertionError("ambiguous preamble changed under cache mode")
        if "project snapshot written" in transcript:
            raise AssertionError("ambiguous preamble published a project snapshot")

        # Even a sole raw marker is not proof: it can be swallowed while the
        # real boundary is produced through a control-sequence alias.
        source.write_text(
            "\\documentclass{article}\n"
            "\\let\\realbegin\\begin\n"
            "\\long\\def\\capture#1END{}\n"
            "\\capture \\begin{document} END\n"
            "\\realbegin{document}\n"
            "Expanded real boundary.\n"
            "\\end{document}\n",
            encoding="ascii",
        )
        uncached, _ = invoke(binary, fmt, project, "expanded-uncached.dvi", no_cache=True)
        cached, transcript = invoke(binary, fmt, project, "expanded-cached.dvi")
        if cached != uncached:
            raise AssertionError("expanded document boundary changed under cache mode")
        if "project snapshot written" in transcript:
            raise AssertionError("expanded document boundary published a project snapshot")

        # A recoverable preamble error must be observed on every invocation.
        # Publishing that state would let the second run restore after the
        # error site and incorrectly exit successfully.
        source.write_text(
            "\\documentclass{article}\n"
            "\\undefinedPreambleCommand\n"
            "\\begin{document}Body\\end{document}\n",
            encoding="ascii",
        )
        env = dict(os.environ)
        env.update({
            "SOURCE_DATE_EPOCH": "1000000000",
            "FORCE_SOURCE_DATE": "1",
            "max_print_line": "1000",
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "LC_ALL": "C",
        })
        command = [
            str(binary), "pass", "main.tex", "--fmt", str(fmt), "--cache", ".bad-cache",
            "-file-line-error", "-o", "bad.dvi",
        ]
        for attempt in range(2):
            result = subprocess.run(
                command, cwd=project, env=env, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, check=False,
            )
            if result.returncode == 0:
                raise AssertionError(
                    f"recoverable preamble error disappeared on attempt {attempt + 1}"
                )
        project_snapshots = list((project / ".bad-cache").glob("project-*.asnap"))
        if project_snapshots:
            raise AssertionError("an erroneous preamble published a project snapshot")


def exercise_host_final_page_write(binary: Path, fmt: Path) -> None:
    """A deferred write must survive the host's controlled-EOF page finish."""
    with tempfile.TemporaryDirectory(prefix="aitex-final-page-write.") as tmp:
        project = Path(tmp)
        (project / "main.tex").write_text(
            "\\documentclass{article}\n"
            "\\newwrite\\late\n"
            "\\begin{document}\n"
            "\\output{\\shipout\\box255}\n"
            "\\immediate\\openout\\late=late.aux\n"
            "A page finalized by the host at end of input.\n"
            "\\write\\late{late final-page write}\n",
            encoding="ascii",
        )
        _, transcript = invoke(
            binary, fmt, project, "main.dvi", no_cache=True,
        )
        auxiliary = project / "late.aux"
        if not auxiliary.is_file() or "late final-page write" not in auxiliary.read_text(
            encoding="latin-1"
        ):
            raise AssertionError("host-finalized page lost its deferred write")
        if "late.aux changed" not in transcript:
            raise AssertionError(
                "host-finalized deferred write was absent from rerun detection:\n"
                + transcript[-4000:]
            )


def exercise_host_finalization_error(binary: Path, fmt: Path) -> None:
    """An error raised only by host final-page execution must fail the CLI."""
    with tempfile.TemporaryDirectory(prefix="aitex-final-page-error.") as tmp:
        project = Path(tmp)
        (project / "main.tex").write_text(
            "\\documentclass{article}\n"
            "\\begin{document}\n"
            "\\output{\\shipout\\box255\\undefinedOnlyDuringFinalization}\n"
            "The source reaches EOF without executing its output routine.\n",
            encoding="ascii",
        )
        previous = b"previous valid artifact\n"
        (project / "failed.dvi").write_bytes(previous)
        env = dict(os.environ)
        env.update({
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "LC_ALL": "C",
        })
        result = subprocess.run(
            [
                str(binary), "pass", "main.tex", "--fmt", str(fmt), "--no-cache",
                "-halt-on-error", "-o", "failed.dvi",
            ],
            cwd=project, env=env, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, check=False,
        )
        transcript = result.stdout.decode("latin-1", "replace")
        if result.returncode == 0:
            raise AssertionError("host-finalization error exited successfully")
        if (project / "failed.dvi").read_bytes() != previous:
            raise AssertionError("host-finalization error replaced the prior artifact")
        if "undefinedOnlyDuringFinalization" not in transcript:
            raise AssertionError("host-finalization error was not diagnosed")


def exercise_dev_null_sink(binary: Path, fmt: Path) -> None:
    """The null sink is never a cache payload or atomic-rename target."""
    with tempfile.TemporaryDirectory(prefix="aitex-dev-null.") as tmp:
        project = Path(tmp)
        (project / "main.tex").write_text(
            "\\documentclass{article}\n"
            "\\begin{document}Null sink.\\end{document}\n",
            encoding="ascii",
        )
        for _ in range(2):
            _, transcript = invoke(
                binary, fmt, project, "/dev/null",
            )
            if "persistent output cache" in transcript:
                raise AssertionError("/dev/null entered the persistent output cache")
        env = dict(os.environ)
        env.update({
            "AITEX_DVI": "/dev/./null",
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "LC_ALL": "C",
        })
        for _ in range(2):
            result = subprocess.run(
                [str(binary), "pass", "main.tex", "--fmt", str(fmt), "--cache", ".alias-cache"],
                cwd=project, env=env, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, check=False,
            )
            transcript = result.stdout.decode("latin-1", "replace")
            if result.returncode != 0:
                raise AssertionError("aliased /dev/null sink failed:\n" + transcript[-4000:])
            if "persistent output cache" in transcript:
                raise AssertionError("aliased /dev/null entered the persistent output cache")
        if not Path("/dev/null").is_char_device():
            raise AssertionError("/dev/null is no longer a character device")


def exercise_cache_warm(binary: Path, fmt: Path) -> None:
    """Warm must publish a requested artifact and make the next run a hit."""
    with tempfile.TemporaryDirectory(prefix="aitex-cache-warm.") as tmp:
        project = Path(tmp)
        (project / "main.tex").write_text(
            "\\documentclass{article}\n"
            "\\begin{document}Warm output.\\end{document}\n",
            encoding="ascii",
        )
        env = dict(os.environ)
        env.update({
            "SOURCE_DATE_EPOCH": "1000000000",
            "FORCE_SOURCE_DATE": "1",
            "max_print_line": "1000",
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "LC_ALL": "C",
            "TZ": "UTC",
        })
        missing_output = subprocess.run(
            [str(binary), "cache", "warm", "main.tex", "--fmt", str(fmt)],
            cwd=project, env=env, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, check=False,
        )
        if missing_output.returncode != 2:
            raise AssertionError("cache warm without -o was not rejected as usage")
        warmed = subprocess.run(
            [
                str(binary), "cache", "warm", "main.tex", "--fmt", str(fmt),
                "--cache", ".cache", "-file-line-error", "-halt-on-error",
                "-o", "main.dvi",
            ],
            cwd=project, env=env, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, check=False,
        )
        transcript = warmed.stdout.decode("latin-1", "replace")
        if warmed.returncode != 0 or WRITTEN not in transcript:
            raise AssertionError(
                "cache warm did not publish exact output:\n" + transcript[-4000:]
            )
        expected = (project / "main.dvi").read_bytes()
        (project / "main.dvi").unlink()
        require_hit(binary, fmt, project, "main.dvi", expected)


def exercise_clock_dependencies(binary: Path, fmt: Path) -> None:
    """Only clock values observed by the document may expire an output entry."""
    before = "106,9,9,2001"
    two_minutes_later = "108,9,9,2001"
    next_day = "106,10,9,2001"
    next_year = "106,9,9,2002"
    with tempfile.TemporaryDirectory(prefix="aitex-output-cache-clock.") as tmp:
        project = Path(tmp)
        source = project / "main.tex"
        output = "main.dvi"

        source.write_text(
            "\\documentclass{article}\n"
            "\\begin{document}No clock observation.\\end{document}\n",
            encoding="ascii",
        )
        stable = warm_until_ready(
            binary, fmt, project, output, test_clock=before,
        )
        require_hit(
            binary, fmt, project, output, stable,
            test_clock=two_minutes_later,
        )
        require_hit(
            binary, fmt, project, output, stable,
            test_clock=next_day,
        )

        # PGF seeds its random-number generator from `\time * \year` while it
        # loads. Merely loading TikZ must not make an otherwise deterministic
        # document expire every minute.
        source.write_text(
            "\\documentclass{article}\n"
            "\\usepackage{tikz}\n"
            "\\begin{document}TikZ loaded without randomness.\\end{document}\n",
            encoding="ascii",
        )
        tikz = warm_until_ready(
            binary, fmt, project, output, test_clock=before,
        )
        require_hit(
            binary, fmt, project, output, tikz,
            test_clock=two_minutes_later,
        )
        require_hit(
            binary, fmt, project, output, tikz,
            test_clock=next_year,
        )

        # Consuming PGF's default seed makes the output depend on the minute
        # and year from which that seed was derived.
        source.write_text(
            "\\documentclass{article}\n"
            "\\usepackage{tikz}\n"
            "\\begin{document}\\pgfmathparse{rnd}\\pgfmathresult\\end{document}\n",
            encoding="ascii",
        )
        warm_until_ready(
            binary, fmt, project, output, test_clock=before,
        )
        require_miss(
            binary, fmt, project, output,
            test_clock=two_minutes_later,
        )

        # `\today` consumes date fields. It remains valid across a minute
        # change, but not across midnight.
        source.write_text(
            "\\documentclass{article}\n"
            "\\begin{document}\\today\\end{document}\n",
            encoding="ascii",
        )
        dated = warm_until_ready(
            binary, fmt, project, output, test_clock=before,
        )
        require_hit(
            binary, fmt, project, output, dated,
            test_clock=two_minutes_later,
        )
        require_miss(
            binary, fmt, project, output,
            test_clock=next_day,
        )

        # A TeX `\let` copies the numeric meaning but changes its control-
        # sequence identity. The copy itself must retain the date dependency.
        source.write_text(
            "\\documentclass{article}\n"
            "\\ExplSyntaxOn\n"
            "\\cs_new_eq:NN \\tertius_clock_alias_int \\c_sys_day_int\n"
            "\\ExplSyntaxOff\n"
            "\\begin{document}"
            "\\ExplSyntaxOn\\int_use:N \\tertius_clock_alias_int\\ExplSyntaxOff"
            "\\end{document}\n",
            encoding="ascii",
        )
        warm_until_ready(
            binary, fmt, project, output, test_clock=before,
        )
        require_miss(
            binary, fmt, project, output,
            test_clock=next_day,
        )

        # A same-named project file is not the reviewed PGF initializer. TeX
        # intentionally resolves local shadows first, so a basename-only
        # exemption would turn this observable read into an unsafe cache hit.
        (project / "pgfmathfunctions.random.code.tex").write_text(
            "\\dimen0=\\time sp\n"
            "\\edef\\localclock{\\the\\dimen0}\n",
            encoding="ascii",
        )
        source.write_text(
            "\\documentclass{article}\n"
            "\\input{pgfmathfunctions.random.code.tex}\n"
            "\\begin{document}\\localclock\\end{document}\n",
            encoding="ascii",
        )
        warm_until_ready(
            binary, fmt, project, output, test_clock=before,
        )
        require_miss(
            binary, fmt, project, output,
            test_clock=two_minutes_later,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/aitex")
    parser.add_argument(
        "--format", type=Path,
        help="use an explicit candidate-compatible format instead of building a temporary one",
    )
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary is absent: {binary}")

    with tempfile.TemporaryDirectory(prefix="aitex-output-cache-format.") as tmp:
        if args.format:
            fmt = args.format.resolve()
            if not fmt.is_file():
                parser.error(f"format is absent: {fmt}")
        else:
            format_dir = Path(tmp) / "format"
            env = dict(os.environ)
            env["AITEX_BINARY"] = str(binary)
            result = subprocess.run(
                [str(ROOT / "tools" / "mkfmt.sh"), str(format_dir)],
                cwd=ROOT,
                env=env,
                check=False,
            )
            if result.returncode != 0:
                parser.error("could not build a temporary candidate format")
            fmt = format_dir / "fmt.afmt"

        exercise(binary, fmt, "dvi")
        exercise(binary, fmt, "pdf")
        exercise_preamble_cache_safety(binary, fmt)
        exercise_host_final_page_write(binary, fmt)
        exercise_host_finalization_error(binary, fmt)
        exercise_dev_null_sink(binary, fmt)
        exercise_cache_warm(binary, fmt)
        exercise_clock_dependencies(binary, fmt)
    print("persistent output cache: DVI/PDF restoration and invalidation passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
