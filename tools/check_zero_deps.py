#!/usr/bin/env python3
"""Guard aitex-engine's dependency surface (docs/history/PLAN.md decision 8).

The engine's only permitted external runtime dependencies are `serde` and
`postcard`, a deliberate, reasoned exception: they power engine-state
dump/restore, which is core product machinery (preamble snapshot, incremental
re-typesetting -- see docs/ARCHITECTURE.md §4-§5), and both are pure Rust,
no_std, wasm-clean, and permissively licensed. The dependency-free first-party
`aitex-page` vocabulary is also allowed. This check fails if any other direct
runtime dependency appears.
"""
import json, subprocess, sys

# `aitex-page` is a first-party, dependency-free vocabulary crate.  It lets the
# engine and output backends exchange final positioned events without pulling
# the font/parser/PDF dependency graph into the TeX core.
ALLOWED = {"aitex-page", "serde", "postcard"}

m = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                              capture_output=True, text=True, check=True).stdout)
pkg = next(p for p in m["packages"] if p["name"] == "aitex-engine")

runtime = [d["name"] for d in pkg["dependencies"] if d["kind"] is None]
unexpected = sorted(set(runtime) - ALLOWED)

if unexpected:
    print(f"ERROR: aitex-engine may depend only on {sorted(ALLOWED)}; found extra: {unexpected}")
    sys.exit(1)

print(f"ok: aitex-engine's direct runtime deps are within the allowlist {sorted(ALLOWED)} "
      f"(present: {sorted(set(runtime))})")
