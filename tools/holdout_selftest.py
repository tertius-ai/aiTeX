#!/usr/bin/env python3
"""Fast invariants for the holdout's semantic comparison policy."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from arxiv_holdout import (
    acceptable_rasterizer_exit,
    canonical_special_text,
    compare_positioned_specials,
    draft_eps_assets,
    effective_cpu_affinity,
)
from holdout_check import (
    DVI_CPU_RATIO_LIMIT,
    PDF_CPU_RATIO_LIMIT,
    ROOT,
    compiler_source_identity_error,
    cpu_ratio_error,
)


def special(page: int, h: int, v: int, text: str) -> bytes:
    return f"special page={page} h={h} v={v} text='{text}'\n".encode()


def main() -> int:
    affinity = effective_cpu_affinity()
    assert affinity is None or affinity == sorted(set(affinity))
    assert PDF_CPU_RATIO_LIMIT == 1.0
    assert DVI_CPU_RATIO_LIMIT == 1.30
    assert cpu_ratio_error("pdf", 0.999) is None
    assert cpu_ratio_error("pdf", 1.0) is not None
    assert cpu_ratio_error("dvi", 1.30) is None
    assert cpu_ratio_error("dvi", 1.301) is not None

    # A hosted hash waiver must include files git has not learned about. A
    # build.rs or Cargo config can alter the executable without appearing in a
    # normal `git diff`.
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, stdout=subprocess.PIPE,
        text=True, check=True,
    ).stdout.strip()
    identity_probes = (
        ROOT / "crates" / "aitex-cli" / ".holdout-identity-probe",
        ROOT / "crates" / "aitex-cli" / "holdout-identity-probe.aux",
    )
    ignored = subprocess.run(
        ["git", "check-ignore", "--quiet", str(identity_probes[1])],
        cwd=ROOT, check=False,
    )
    assert ignored.returncode == 0, "ignored identity probe is not covered by .gitignore"
    for identity_probe in identity_probes:
        if identity_probe.exists():
            raise AssertionError(f"identity self-test path already exists: {identity_probe}")
        identity_probe.write_text("untracked compiler input\n")
        try:
            identity_error = compiler_source_identity_error(head)
            assert identity_error is not None and str(
                identity_probe.relative_to(ROOT)
            ) in identity_error
        finally:
            identity_probe.unlink()

    base = special(1, 100, 200, "ps: currentpoint")
    assert compare_positioned_specials(
        base, special(1, 101, 199, "ps: currentpoint"))["equivalent"]
    assert not compare_positioned_specials(
        base, special(1, 102, 200, "ps: currentpoint"))["equivalent"]
    assert not compare_positioned_specials(
        base, special(1, 100, 200, "ps: different"))["equivalent"]

    extra_noop = special(1, 0, 0, "color pop") + base
    result = compare_positioned_specials(base, extra_noop)
    assert result["equivalent"]
    assert result["candidate_ignored_base_color_pops"] == 1

    nested = (special(1, 0, 0, "color push rgb 1 0 0")
              + special(1, 0, 0, "color pop"))
    result = compare_positioned_specials(b"", nested)
    assert result["equivalent"]
    assert result["candidate_ignored_adjacent_color_pairs"] == 1
    base_black = special(1, 0, 0, "color push gray 0") + base
    result = compare_positioned_specials(base, base_black)
    assert result["equivalent"]
    assert result["candidate_ignored_base_black_pushes"] == 1
    drawing = b"glyph page=1 h=0 v=0 font=1 char=65\n"
    assert not compare_positioned_specials(
        b"", special(1, 0, 0, "color push rgb 1 0 0")
        + drawing + special(1, 0, 0, "color pop"))["equivalent"]
    assert not compare_positioned_specials(
        special(1, 0, 0, "a") + special(1, 0, 0, "b"),
        special(1, 0, 0, "b") + special(1, 0, 0, "a"),
    )["equivalent"]
    icc = b"warning: ICC support is not available\n"
    assert acceptable_rasterizer_exit(1, icc)
    assert not acceptable_rasterizer_exit(1, icc + b"error: malformed page\n")
    assert not acceptable_rasterizer_exit(2, icc)
    eps = special(3, 10, 20, 'PSfile="fig/a plot.eps" llx=0 lly=0 urx=10 ury=20 ')
    eps_result = compare_positioned_specials(eps, eps)
    assert eps_result["reference_eps_assets"] == ["fig/a plot.eps"]
    assert draft_eps_assets("bodyfig/aplot.epscaption", eps_result, "reference") == [
        "fig/a plot.eps"
    ]
    assert not draft_eps_assets("bodycaption", eps_result, "reference")
    ascii_title = b"'ps: [/Title (1.\\040Introduction) /OUT pdfmark'"
    utf16_title = (b"'ps: [/Title (\\376\\377\\0001\\000.\\000\\040"
                   b"\\000I\\000n\\000t\\000r\\000o\\000d\\000u"
                   b"\\000c\\000t\\000i\\000o\\000n) /OUT pdfmark'")
    assert canonical_special_text(ascii_title) == canonical_special_text(utf16_title)
    assert canonical_special_text(ascii_title) != canonical_special_text(
        b"'ps: [/Title (2.\\040Methods) /OUT pdfmark'")
    save = special(1, 0, 0, "ps:: save ")
    restore = special(1, 0, 0, "ps:: restore ")
    path = special(1, 0, 0, "ps:: 0 0 moveto ")
    assert compare_positioned_specials(
        save + path + restore,
        save + save + path + restore + restore,
    )["equivalent"]
    assert not compare_positioned_specials(
        save + path + restore,
        save + b"glyph page=1 h=0 v=0 font=1 char=65\n" + save
        + path + restore + restore,
    )["equivalent"]
    print("holdout self-test: semantic bounds, raster scope, and release policy pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
