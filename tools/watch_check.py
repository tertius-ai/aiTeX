#!/usr/bin/env python3
"""Exercise the native watch service and the retained stdin protocol."""

import argparse
import hashlib
import os
import queue
import subprocess
import tempfile
import threading
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
READERS: dict[int, queue.Queue[str]] = {}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture(project: Path) -> None:
    project.mkdir()
    (project / "main.tex").write_text(
        "\\documentclass{article}\n"
        "\\begin{document}\n"
        "\\tableofcontents\n"
        "\\section{Watch qualification}\n"
        "\\input{chapter}\n"
        "\\nocite{watch}\n"
        "\\bibliographystyle{plain}\n"
        "\\bibliography{references}\n"
        "\\end{document}\n",
        encoding="utf-8",
    )
    (project / "chapter.tex").write_text("chapter version one\n", encoding="utf-8")
    (project / "references.bib").write_text(
        "@book{watch, author={Ada Example}, title={First Title}, year={2026}}\n",
        encoding="utf-8",
    )


def environment() -> dict[str, str]:
    env = dict(os.environ)
    env.update(
        {
            "FORCE_SOURCE_DATE": "1",
            "SOURCE_DATE_EPOCH": "1700000000",
            "LC_ALL": "C",
            "TZ": "UTC",
            "max_print_line": "1000",
        }
    )
    return env


def line_queue(process: subprocess.Popen[str]) -> queue.Queue[str]:
    existing = READERS.get(process.pid)
    if existing is not None:
        return existing
    assert process.stdout is not None
    result: queue.Queue[str] = queue.Queue()

    def read() -> None:
        for line in process.stdout:
            result.put(line)

    threading.Thread(target=read, daemon=True).start()
    READERS[process.pid] = result
    return result


def frame(process: subprocess.Popen[str], timeout: float) -> tuple[str, str]:
    incoming = line_queue(process)
    lines: list[str] = []
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            line = incoming.get(timeout=min(0.2, deadline - time.monotonic()))
        except queue.Empty:
            if process.poll() is not None:
                raise RuntimeError("watch exited before a ready frame\n" + "".join(lines)[-4000:])
            continue
        lines.append(line)
        if line.startswith("AITEX_WATCH ready "):
            fields = dict(field.split("=", 1) for field in line.split()[2:])
            return fields["code"], "".join(lines)
    raise RuntimeError("watch frame timed out\n" + "".join(lines)[-4000:])


def no_frame(process: subprocess.Popen[str], duration: float) -> None:
    incoming = line_queue(process)
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        try:
            line = incoming.get(timeout=min(0.05, deadline - time.monotonic()))
        except queue.Empty:
            continue
        if line.startswith("AITEX_WATCH ready "):
            raise RuntimeError(f"watch rebuilt without a new input change: {line.rstrip()}")


def stop(process: subprocess.Popen[str]) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
    READERS.pop(process.pid, None)


def native_check(project: Path, command: list[str], env: dict[str, str], timeout: int) -> None:
    process = subprocess.Popen(
        command,
        cwd=project,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    output = project / "main.pdf"
    try:
        code, transcript = frame(process, timeout)
        if code != "0" or "converged after" not in transcript or not output.is_file():
            raise RuntimeError("initial complete watch build failed\n" + transcript[-4000:])
        initial = digest(output)

        source = project / "main.tex"
        source.write_bytes(source.read_bytes())
        no_frame(process, 0.35)

        (project / "chapter.tex").write_text("chapter version two\n", encoding="utf-8")
        code, transcript = frame(process, timeout)
        if code != "0" or digest(output) == initial:
            raise RuntimeError("included-file edit was not rebuilt\n" + transcript[-4000:])

        before_bibliography = digest(output)
        (project / "references.bib").write_text(
            "@book{watch, author={Ada Example}, title={Second Title}, year={2026}}\n",
            encoding="utf-8",
        )
        code, transcript = frame(process, timeout)
        if code != "0" or digest(output) == before_bibliography:
            raise RuntimeError("bibliography edit was not rebuilt\n" + transcript[-4000:])
        if "post-build dependency changed" in transcript:
            raise RuntimeError("spurious post-build race\n" + transcript[-4000:])

        good = digest(output)
        source = project / "main.tex"
        healthy = source.read_text(encoding="utf-8")
        source.write_text(healthy.replace("\\end{document}", "\\undefinedwatchcommand\n\\end{document}"), encoding="utf-8")
        code, transcript = frame(process, timeout)
        after_failure = digest(output)
        if code == "0" or process.poll() is not None or after_failure != good:
            raise RuntimeError(
                "failed rebuild did not preserve the live service and last good PDF "
                f"(code={code}, process={process.poll()}, before={good}, after={after_failure})\n"
                + transcript[-4000:]
            )
        source.write_text(healthy, encoding="utf-8")
        code, transcript = frame(process, timeout)
        if code != "0":
            raise RuntimeError("watch did not recover after the source was fixed\n" + transcript[-4000:])

        chapter = project / "chapter.tex"
        for number in range(3):
            temporary = project / f".chapter-save-{number}.tex"
            temporary.write_text(f"atomic save version {number}\n", encoding="utf-8")
            os.replace(temporary, chapter)
            time.sleep(0.015)
        code, transcript = frame(process, timeout)
        if code != "0":
            raise RuntimeError("atomic-save burst failed\n" + transcript[-4000:])
        no_frame(process, 0.35)

        temporary = project / ".chapter-identical-save.tex"
        temporary.write_bytes(chapter.read_bytes())
        os.replace(temporary, chapter)
        no_frame(process, 0.35)

        good = digest(output)
        removed = project / "chapter.tex.removed"
        os.replace(chapter, removed)
        code, transcript = frame(process, timeout)
        removed_digest = digest(output)
        if "resolution changed for chapter" not in transcript or process.poll() is not None:
            raise RuntimeError(
                f"removed dependency was not invalidated (code={code}, "
                f"before={good}, after={removed_digest})\n" + transcript[-4000:]
            )
        if code != "0" and removed_digest != good:
            raise RuntimeError("failed missing-input rebuild replaced the previous good PDF")
        os.replace(removed, chapter)
        code, transcript = frame(process, timeout)
        if code != "0":
            raise RuntimeError("recreated dependency did not recover\n" + transcript[-4000:])
        no_frame(process, 0.35)
    finally:
        stop(process)


def stdin_check(project: Path, command: list[str], env: dict[str, str], timeout: int) -> None:
    command.insert(2, "--stdin")
    process = subprocess.Popen(
        command,
        cwd=project,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    try:
        code, transcript = frame(process, timeout)
        if code != "0":
            raise RuntimeError("stdin watch initial pass failed\n" + transcript[-4000:])
        (project / "chapter.tex").write_text("stdin edit\n", encoding="utf-8")
        no_frame(process, 0.25)
        assert process.stdin is not None
        process.stdin.write("build\n")
        process.stdin.flush()
        code, transcript = frame(process, timeout)
        if code != "0":
            raise RuntimeError("stdin-requested pass failed\n" + transcript[-4000:])
        process.stdin.write("quit\n")
        process.stdin.flush()
        process.wait(timeout=3)
    finally:
        stop(process)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target" / "release" / "aitex")
    parser.add_argument("--fmt", type=Path, default=ROOT / ".aitex" / "format" / "fmt.afmt")
    parser.add_argument("--timeout", type=int, default=60)
    args = parser.parse_args()
    binary = args.binary.resolve()
    fmt = args.fmt.resolve()
    if not binary.is_file() or not fmt.is_file():
        raise SystemExit("build the release binary and format before running watch_check.py")
    with tempfile.TemporaryDirectory(prefix="aitex-native-watch-") as temporary:
        project = Path(temporary) / "project"
        fixture(project)
        command = [
            str(binary),
            "watch",
            "main.tex",
            "--fmt",
            str(fmt),
            "--cache",
            str(Path(temporary) / "cache"),
            "-file-line-error",
            "-halt-on-error",
            "-o",
            "main.pdf",
        ]
        native_check(project, command.copy(), environment(), args.timeout)
        stdin_check(project, command.copy(), environment(), args.timeout)
    print("native watch qualification passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
