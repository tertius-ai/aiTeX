#!/usr/bin/env python3
"""Measure document-class and option frequency in the locked holdout corpus."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOURCE_LOCK = ROOT / "qualification" / "holdout-200.sources.json"
CORPUS = ROOT / "corpus" / "holdout-200" / "sources"
OUTPUT = ROOT / "qualification" / "holdout-200.classes.json"
REPORT = ROOT / "docs" / "CLASS-FREQUENCY.md"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def skip_space_comments(source: str, offset: int) -> int:
    while True:
        while offset < len(source) and source[offset].isspace():
            offset += 1
        if offset >= len(source) or source[offset] != "%":
            return offset
        newline = source.find("\n", offset)
        offset = len(source) if newline < 0 else newline + 1


def balanced(source: str, offset: int, opening: str,
             closing: str) -> tuple[str, int] | None:
    if offset >= len(source) or source[offset] != opening:
        return None
    depth = 0
    body: list[str] = []
    index = offset
    while index < len(source):
        char = source[index]
        if char == "%":
            newline = source.find("\n", index)
            index = len(source) if newline < 0 else newline + 1
            body.append(" ")
            continue
        if char == "\\" and index + 1 < len(source):
            if depth > 0:
                body.extend(source[index:index + 2])
            index += 2
            continue
        if char == opening:
            depth += 1
            if depth > 1:
                body.append(char)
        elif char == closing:
            depth -= 1
            if depth == 0:
                return "".join(body), index + 1
            if depth < 0:
                return None
            body.append(char)
        elif depth > 0:
            body.append(char)
        index += 1
    return None


def documentclass(source: str) -> tuple[str, str] | None:
    command = "\\documentclass"
    index = 0
    while index < len(source):
        if source[index] == "%":
            newline = source.find("\n", index)
            index = len(source) if newline < 0 else newline + 1
            continue
        if source.startswith(command, index):
            end = index + len(command)
            if end == len(source) or not source[end].isalpha():
                index = skip_space_comments(source, end)
                options = ""
                if index < len(source) and source[index] == "[":
                    parsed = balanced(source, index, "[", "]")
                    if parsed is None:
                        return None
                    options, index = parsed
                    index = skip_space_comments(source, index)
                parsed = balanced(source, index, "{", "}")
                if parsed is None:
                    return None
                class_name, _ = parsed
                normalize = lambda value: re.sub(r"\s+", "", value)
                return normalize(class_name), normalize(options)
        index += 1
    return None


def build(lock_path: Path = SOURCE_LOCK, corpus: Path = CORPUS) -> dict:
    source_lock = json.loads(lock_path.read_text())
    entries = [item for item in source_lock["entries"]
               if item.get("status") == "ready"]
    classes: Counter[str] = Counter()
    combinations: Counter[tuple[str, str]] = Counter()
    options_by_class: dict[str, Counter[str]] = defaultdict(Counter)
    unmatched: list[str] = []
    for item in entries:
        path = corpus / item["id"].replace("/", "_") / item["root"]
        parsed = documentclass(path.read_bytes().decode("latin-1"))
        if parsed is None:
            unmatched.append(item["id"])
            continue
        class_name, options = parsed
        classes[class_name] += 1
        combinations[(class_name, options)] += 1
        options_by_class[class_name][options] += 1

    measured = sum(classes.values())
    return {
        "schema": 1,
        "description": ("Literal document-class and option frequency in the "
                        "locked 191-root holdout corpus"),
        "source_lock": str(lock_path.relative_to(ROOT)),
        "source_lock_sha256": sha256(lock_path),
        "source_lock_created_at": source_lock.get("created_at"),
        "roots": len(entries),
        "measured": measured,
        "unmatched": sorted(unmatched),
        "classes": [
            {
                "class": name,
                "count": count,
                "fraction": round(count / measured, 6) if measured else 0,
                "option_sets": [
                    {"options": options, "count": option_count}
                    for options, option_count in sorted(
                        options_by_class[name].items(),
                        key=lambda item: (-item[1], item[0]))
                ],
            }
            for name, count in sorted(classes.items(),
                                      key=lambda item: (-item[1], item[0]))
        ],
        "class_option_combinations": [
            {"class": name, "options": options, "count": count}
            for (name, options), count in sorted(
                combinations.items(), key=lambda item: (-item[1], item[0]))
        ],
    }


def markdown(result: dict) -> str:
    lines = [
        "# Locked-corpus document-class frequency",
        "",
        ("This report measures literal `\\documentclass` use in the 191-root "
         "locked holdout. It determines what P2 should prewarm; it is not a "
         "claim about the global LaTeX population."),
        "",
        (f"The scanner classified {result['measured']} of {result['roots']} roots; "
         f"{len(result['unmatched'])} had no safely parseable literal command."),
        "",
        "## Classes",
        "",
        "| Class | Roots | Share | Most frequent option sets |",
        "|---|---:|---:|---|",
    ]
    for item in result["classes"]:
        option_sets = ", ".join(
            f"`{option['options'] or '(none)'}` ({option['count']})"
            for option in item["option_sets"][:4])
        lines.append(
            f"| `{item['class']}` | {item['count']} | "
            f"{item['fraction'] * 100:.1f}% | {option_sets} |")
    lines += [
        "",
        "## Prewarm implication",
        "",
        ("Snapshots are built lazily for every safe class/option prefix. An "
         "optional prewarm policy should start with the leading exact "
         "class/option combinations in the versioned JSON artifact, while "
         "retaining lazy fallback for the long tail and local classes."),
        "",
        ("Frequency alone is not an admission rule. On the qualification host "
         "the shared snapshot is roughly 18--20 MiB and costs about 60--80 ms "
         "to write; representative class execution ranged from about 12 ms for "
         "`article` to 270 ms for `acmart`. Ordinary compilation therefore uses "
         "a 25 ms admission threshold, while `aitex cache warm` forces a requested "
         "class/option combination into the cache. A batch prewarm policy should "
         "rank combinations by observed frequency times measured time saved, not "
         "frequency alone."),
        "",
        ("The machine-readable source is "
         "[`../qualification/holdout-200.classes.json`]"
         "(../qualification/holdout-200.classes.json). Regenerate both files "
         "with `python3 tools/class_frequency.py`."),
    ]
    if result["unmatched"]:
        lines += ["", "Unmatched roots: " + ", ".join(
            f"`{item}`" for item in result["unmatched"]) + "."]
    return "\n".join(lines) + "\n"


def self_test() -> None:
    assert documentclass("% \\documentclass{fake}\n\\documentclass [a={b,c}] { article }") == (
        "article", "a={b,c}")
    assert documentclass("\\documentclass% x\n{book}") == ("book", "")
    assert documentclass("plain TeX") is None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-lock", type=Path, default=SOURCE_LOCK)
    parser.add_argument("--corpus", type=Path, default=CORPUS)
    parser.add_argument("--output", type=Path, default=OUTPUT)
    parser.add_argument("--report", type=Path, default=REPORT)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    self_test()
    if args.self_test:
        print("class-frequency self-test: OK")
        return 0
    result = build(args.source_lock, args.corpus)
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    report = markdown(result)
    if args.check:
        if (not args.output.is_file() or args.output.read_text() != encoded
                or not args.report.is_file() or args.report.read_text() != report):
            print("class-frequency artifacts are stale", file=sys.stderr)
            return 1
        print("class-frequency artifacts: current")
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(encoded)
    args.report.write_text(report)
    print(f"classified {result['measured']}/{result['roots']} roots; wrote "
          f"{args.output} and {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
