#!/usr/bin/env python3
"""Qualify P3 incremental compilation on every eligible locked-corpus root.

Eligibility is deliberately mechanical: a root must have passed the locked P2
DVI gate, contain a literal LaTeX document body large enough for P3 admission,
and produce at least one safe live checkpoint. Each eligible root must serve an
exact no-op artifact, compile a semantically inert late source edit, and match a
clean aiTeX rebuild under the same stabilized auxiliary state.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import arxiv_holdout as holdout


ROOT = Path(__file__).resolve().parent.parent
WARM = ROOT / "qualification" / "holdout-200-p2.warm.json"
SOURCES = ROOT / "qualification" / "holdout-200.sources.json"
OUTPUT = ROOT / "qualification" / "holdout-200-p3.incremental.json"


def environment(texmf: Path) -> dict[str, str]:
    env = dict(os.environ)
    env.update({
        "AITEX_INCREMENTAL": "1",
        "AITEX_MAX_COMMANDS": "2000000000",
        "AITEX_MAX_STEPS": "2000000000",
        "AITEX_STACK_MB": "64",
        "SOURCE_DATE_EPOCH": "1000000000",
        "FORCE_SOURCE_DATE": "1",
        "LC_ALL": "C",
        "TZ": "UTC",
        "max_print_line": "1000",
        "TEXMFHOME": f"!!{texmf}",
        "TEXMFDBS": f"{texmf}:{env.get('TEXMFDBS', '')}",
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
                raise RuntimeError("watch exited\n" + "".join(lines)[-2000:])
            continue
        lines.append(line)
        if line.startswith("AITEX_WATCH ready "):
            output = "".join(lines)
            if " code=0 " not in line:
                raise RuntimeError("watch build failed\n" + output[-2000:])
            return output
    process.kill()
    raise RuntimeError("watch timed out\n" + "".join(lines)[-2000:])


def mutate_root(root: Path) -> bool:
    source = root.read_bytes().decode("latin-1")
    marker = "\\end{document}"
    offset = source.rfind(marker)
    if offset < 0:
        return False
    source = source[:offset] + "% aitex-p3-corpus-inert-edit\n" + source[offset:]
    root.write_bytes(source.encode("latin-1"))
    return True


def run_one(item: dict, binary: Path, fmt: Path, texmf: Path,
            timeout: int) -> dict:
    source_tree = holdout.CORPUS / "sources" / item["id"].replace("/", "_")
    with tempfile.TemporaryDirectory(prefix=f"aitex-p3-{item['id']}.") as temporary:
        project = Path(temporary) / "project"
        shutil.copytree(source_tree, project)
        root = project / item["root"]
        cwd = root.parent
        relative_root = root.name
        env = environment(texmf)
        command = [
            str(binary), "watch", "--stdin", relative_root, "--fmt", str(fmt),
            "--cache", str(project / ".aitex-cache"), "-file-line-error",
            "-halt-on-error", "-o", "incremental.dvi",
        ]
        process = subprocess.Popen(
            command, cwd=cwd, env=env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1,
        )
        try:
            outputs = [frame(process, False, timeout)]
            for _ in range(5):
                outputs.append(frame(process, True, timeout))
                if "final artifact reused" in outputs[-1]:
                    break
            transcript = "".join(outputs)
            if "body checkpoints bypassed (body is below" in transcript:
                return {"id": item["id"], "status": "ineligible_small_body"}
            if "body checkpoints unavailable" in transcript:
                return {"id": item["id"], "status": "ineligible_no_safe_boundary"}
            if "body checkpoints retained" not in transcript:
                return {"id": item["id"], "status": "setup_failed",
                        "error": transcript[-1000:]}
            if "final artifact reused" not in transcript:
                return {"id": item["id"], "status": "invalid_no_op",
                        "error": transcript[-1000:]}
            if not mutate_root(root):
                return {"id": item["id"], "status": "ineligible_nonliteral_body"}
            edited = frame(process, True, timeout)
            incremental = cwd / "incremental.dvi"
            saved = cwd / "saved-incremental.dvi"
            shutil.copy2(incremental, saved)
            clean = subprocess.run(
                [str(binary), "pass", relative_root, "--fmt", str(fmt), "--no-cache",
                 "-file-line-error", "-halt-on-error", "-o", "clean.dvi"],
                cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, timeout=timeout,
            )
            if clean.returncode != 0:
                return {"id": item["id"], "status": "clean_failed",
                        "error": clean.stdout[-1000:]}
            comparison = holdout.compare_dvi_semantics(saved, cwd / "clean.dvi", env)
            valid = bool(comparison.get("valid") and comparison.get("equivalent"))
            return {
                "id": item["id"],
                "status": "valid" if valid else "invalid",
                "edit_path": (
                    "reconverged" if "semantic reconvergence hit" in edited
                    else "checkpoint" if "body checkpoint hit" in edited
                    else "safe_fallback"
                ),
                "comparison": {
                    key: comparison.get(key)
                    for key in ("valid", "equivalent", "drawing_equivalent",
                                "specials_equivalent", "first_difference")
                    if key in comparison
                },
                **({"error": edited[-1000:]} if not valid else {}),
            }
        except Exception as error:
            return {"id": item["id"], "status": "worker_error", "error": repr(error)}
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
    parser.add_argument("--format", type=Path, default=ROOT / "target/performance/fmt.afmt")
    parser.add_argument("--texmf-overlay", type=Path, default=holdout.TEXMF_OVERLAY)
    parser.add_argument("--warm", type=Path, default=WARM)
    parser.add_argument("--sources", type=Path, default=SOURCES)
    parser.add_argument("--output", type=Path, default=OUTPUT)
    parser.add_argument("--jobs", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--limit", type=int, default=0)
    args = parser.parse_args()
    binary, fmt, texmf = args.binary.resolve(), args.format.resolve(), args.texmf_overlay.resolve()
    for path, label in [(binary, "binary"), (fmt, "format"), (texmf / "ls-R", "overlay")]:
        if not path.exists():
            raise SystemExit(f"{label} is absent: {path}")
    warm = json.loads(args.warm.read_text())
    sources = json.loads(args.sources.read_text())
    by_id = {entry["id"]: entry for entry in sources["entries"]}
    selected = [by_id[entry["id"]] for entry in warm["entries"]
                if entry["status"] == "valid"]
    if args.limit:
        selected = selected[:args.limit]
    results: dict[str, dict] = {}
    with concurrent.futures.ProcessPoolExecutor(max_workers=args.jobs) as executor:
        futures = {
            executor.submit(run_one, item, binary, fmt, texmf, args.timeout): item
            for item in selected
        }
        for number, future in enumerate(concurrent.futures.as_completed(futures), 1):
            item = futures[future]
            try:
                result = future.result()
            except Exception as error:
                result = {"id": item["id"], "status": "worker_error", "error": repr(error)}
            results[result["id"]] = result
            print(f"[{number:2d}/{len(selected)}] {result['id']} {result['status']}", flush=True)
    entries = [results[item["id"]] for item in selected]
    eligible = [entry for entry in entries if not entry["status"].startswith("ineligible_")]
    valid = [entry for entry in eligible if entry["status"] == "valid"]
    artifact = {
        "schema": 1,
        "description": "Locked-corpus P3 incremental exactness qualification",
        "created_at": holdout.utc_now(),
        "selection": "valid P2 locked DVI roots; P3 eligibility decided mechanically",
        "warm_sha256": holdout.file_sha256(args.warm),
        "sources_sha256": holdout.file_sha256(args.sources),
        "runner_sha256": holdout.file_sha256(Path(__file__)),
        "binary_sha256": holdout.file_sha256(binary),
        "format_sha256": holdout.file_sha256(fmt),
        "selected": len(entries),
        "eligible": len(eligible),
        "valid": len(valid),
        "entries": entries,
    }
    holdout.write_json(args.output, artifact)
    print(f"incremental corpus: {len(valid)}/{len(eligible)} eligible roots exact")
    print(f"results sha256: {holdout.file_sha256(args.output)}")
    return 0 if len(valid) == len(eligible) else 1


if __name__ == "__main__":
    raise SystemExit(main())
