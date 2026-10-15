#!/usr/bin/env python3
"""Print median child CPU milliseconds for repeated command executions."""

from __future__ import annotations

import os
import resource
import statistics
import subprocess
import sys


def usage_seconds() -> float:
    used = resource.getrusage(resource.RUSAGE_CHILDREN)
    return used.ru_utime + used.ru_stime


def timed(command: list[str], devnull: object) -> float:
    cpu = os.environ.get("BENCH_CPU")
    if cpu:
        command = ["taskset", "-c", cpu, *command]
    before = usage_seconds()
    completed = subprocess.run(
        command,
        stdin=devnull,
        stdout=devnull,
        stderr=devnull,
        check=False,
    )
    after = usage_seconds()
    if completed.returncode != 0:
        raise subprocess.CalledProcessError(completed.returncode, command)
    return (after - before) * 1000.0


def main() -> int:
    if len(sys.argv) < 3:
        print(
            f"usage: {sys.argv[0]} REPS COMMAND [ARG ...]\n"
            f"       {sys.argv[0]} --pair REPS COMMAND... --and-- COMMAND...",
            file=sys.stderr,
        )
        return 2
    pair = sys.argv[1] == "--pair"
    if pair:
        reps = int(sys.argv[2])
        split = sys.argv.index("--and--", 3)
        commands = [sys.argv[3:split], sys.argv[split + 1 :]]
        samples: list[list[float]] = [[], []]
    else:
        reps = int(sys.argv[1])
        commands = [sys.argv[2:]]
        samples = [[]]
    with open(os.devnull, "wb") as devnull:
        try:
            for repetition in range(reps):
                order = [0, 1] if pair and repetition % 2 == 0 else list(reversed(range(len(commands))))
                for index in order:
                    samples[index].append(timed(commands[index], devnull))
        except subprocess.CalledProcessError as error:
            print(
                f"cpu_time: command exited with {error.returncode}: "
                + " ".join(error.cmd),
                file=sys.stderr,
            )
            return error.returncode or 1
    print(" ".join(f"{statistics.median(group):.3f}" for group in samples))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
