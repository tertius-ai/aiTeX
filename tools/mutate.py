#!/usr/bin/env python3
"""Generate labelled broken LaTeX documents from a clean corpus.

Each mutation is stored as a *patch record*, not a copied tree -- 1000 copies of
real projects would be gigabytes, and the patch is the ground truth anyway:

    {project, file, line, col, span, family, original, replacement}

`line` is the TRUE cause location. After compiling we record where pdflatex
*claims* the error is, and

    gap = |reported_line - true_line|      (inf if reported in another file)

is what the corpus is stratified on. Families are chosen to span the gap range;
the largest gaps come from separating a macro's definition from its use.
"""
import argparse
import json
import random
import re
from pathlib import Path

# Families and their expected gap class (see plan §6).
FAMILIES = [
    ("newcommand_arity", "very_large"),
    ("drop_usepackage", "very_large"),
    ("rename_macro_def", "very_large"),
    ("delete_close_brace", "large"),
    ("delete_end_env", "large"),
    ("unclosed_math", "large"),
    ("runaway_arg", "large"),
    ("break_tabular", "medium"),
    ("recursive_def", "medium"),
    ("misspell_cs", "zero"),
    ("break_input_path", "zero"),
    # package-specific
    ("xymatrix_cell", "large"),
    ("tikzcd_arrow", "medium"),
    ("amscd_arrow", "medium"),
    ("align_tabs", "medium"),
    ("verbatim_unclosed", "very_large"),
    ("theorem_undefined", "very_large"),
]

PROTECTED_PKGS = {"inputenc", "fontenc", "babel", "amsmath", "amssymb"}


def tex_files(projdir, root):
    """Root plus anything it plausibly \\inputs, restricted to the project."""
    out = [projdir / root]
    for t in sorted(projdir.rglob("*.tex")):
        if t not in out and not any(p in {"drafts", "old", "archive"} for p in t.parts):
            out.append(t)
    return [t for t in out if t.exists()]


def lines_of(p):
    try:
        return p.read_text("utf-8", "replace").splitlines(keepends=True)
    except Exception:
        return None


def _rec(proj, path, projdir, li, col, span, fam, gapclass, orig, repl, note=""):
    return {
        "project": proj,
        "file": str(path.relative_to(projdir)),
        "line": li + 1,          # 1-indexed, matches TeX
        "col": col,
        "span": span,
        "family": fam,
        "expected_gap": gapclass,
        "original": orig,
        "replacement": repl,
        "note": note,
    }


# --- individual mutators: each returns a patch record or None -----------------

def m_newcommand_arity(proj, projdir, files, rng):
    """Preamble cause, error at every use site far below."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.search(r"\\(?:new|renew)command\*?\{?\\(\w+)\}?\[(\d)\]", l)]
                 if m]
        if cands:
            i, m = rng.choice(cands)
            old = m.group(2)
            new = str(int(old) + 1)
            col = m.start(2)
            return _rec(proj, p, projdir, i, col, 1, "newcommand_arity", "very_large",
                        old, new, f"arity of \\{m.group(1)}: {old}->{new}")
    return None


def m_drop_usepackage(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = []
        for i, l in enumerate(L):
            m = re.match(r"\s*\\usepackage(?:\[[^\]]*\])?\{([^}]+)\}\s*$", l)
            if m and not (set(x.strip() for x in m.group(1).split(",")) & PROTECTED_PKGS):
                cands.append((i, l, m.group(1)))
        if cands:
            i, l, pkg = rng.choice(cands)
            return _rec(proj, p, projdir, i, 0, len(l), "drop_usepackage", "very_large",
                        l, "", f"dropped package(s): {pkg}")
    return None


def m_rename_macro_def(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.search(r"\\newcommand\*?\{?\\(\w{3,})\}?", l)] if m]
        if cands:
            i, m = rng.choice(cands)
            name = m.group(1)
            return _rec(proj, p, projdir, i, m.start(1), len(name),
                        "rename_macro_def", "very_large",
                        name, name + "X", f"\\{name} defined but never used under that name")
    return None


def m_delete_close_brace(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, l.rfind("}")) for i, l in enumerate(L)
                 if l.count("}") >= 1 and l.count("{") == l.count("}")
                 and not l.lstrip().startswith("%") and len(l.strip()) > 12]
        cands = [c for c in cands if c[1] >= 0]
        if cands:
            i, col = rng.choice(cands[len(cands)//4:] or cands)
            return _rec(proj, p, projdir, i, col, 1, "delete_close_brace", "large",
                        "}", "", "unbalanced brace; surfaces at next structural boundary")
    return None


def m_delete_end_env(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, l) for i, l in enumerate(L)
                 if re.match(r"\s*\\end\{(?!document)(\w+)\}\s*$", l)]
        if cands:
            i, l = rng.choice(cands)
            return _rec(proj, p, projdir, i, 0, len(l), "delete_end_env", "large",
                        l, "", "environment never closed")
    return None


def m_unclosed_math(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = []
        for i, l in enumerate(L):
            if l.lstrip().startswith("%"):
                continue
            pos = [j for j, ch in enumerate(l)
                   if ch == "$" and (j == 0 or l[j-1] != "\\")]
            if len(pos) >= 2:
                cands.append((i, pos[-1]))
        if cands:
            i, col = rng.choice(cands)
            return _rec(proj, p, projdir, i, col, 1, "unclosed_math", "large",
                        "$", "", "math mode runs to next $ or paragraph end")
    return None


def m_runaway_arg(proj, projdir, files, rng):
    """Delete the CLOSING brace of a sectioning argument -> genuine runaway.

    Deleting the *opening* brace makes TeX fail immediately at the same line
    (measured: 24/24 at gap=0). Deleting the closer makes the argument swallow
    everything until TeX gives up, which is the real-world failure we want.
    """
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = []
        for i, l in enumerate(L):
            m = re.search(r"\\(?:section|subsection|textbf|emph|caption)\*?\{", l)
            if not m or l.lstrip().startswith("%"):
                continue
            depth, j = 1, m.end()
            while j < len(l) and depth:
                if l[j] == "{" and l[j-1] != "\\":
                    depth += 1
                elif l[j] == "}" and l[j-1] != "\\":
                    depth -= 1
                    if not depth:
                        break
                j += 1
            if depth == 0 and j < len(l):
                cands.append((i, j))
        if cands:
            i, col = rng.choice(cands)
            return _rec(proj, p, projdir, i, col, 1, "runaway_arg", "large",
                        "}", "", "argument runs away to end of file")
    return None


def m_break_tabular(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.match(r"(\s*\\begin\{tabular\}\{)([^}]+)(\})", l)] if m]
        if cands:
            i, m = rng.choice(cands)
            spec = m.group(2)
            cols = re.sub(r"[^lcrp]", "", spec)
            if len(cols) >= 2:
                new = spec.replace(cols[-1], "", 1)
                return _rec(proj, p, projdir, i, m.start(2), len(spec),
                            "break_tabular", "medium", spec, new,
                            "column spec short by one -> extra alignment tab")
    return None


def m_recursive_def(proj, projdir, files, rng):
    """Insert a self-recursive macro just after \\begin{document}."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        for i, l in enumerate(L):
            if "\\begin{document}" in l:
                return _rec(proj, p, projdir, i, len(l), 0, "recursive_def", "medium",
                            "", "\\def\\aitexloop{\\aitexloop}\\aitexloop\n",
                            "TeX capacity exceeded")
    return None


def m_misspell_cs(proj, projdir, files, rng):
    """Localized control: error is reported exactly where the typo is."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.search(r"\\(section|textbf|emph|alpha|beta|frac|label)\b", l)]
                 if m and not l.lstrip().startswith("%")]
        if cands:
            i, m = rng.choice(cands)
            name = m.group(1)
            bad = name[:-1] + "q" + name[-1]
            return _rec(proj, p, projdir, i, m.start(1), len(name),
                        "misspell_cs", "zero", name, bad, "undefined control sequence")
    return None


def m_break_input_path(proj, projdir, files, rng):
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.search(r"\\(?:input|include)\{([^}]+)\}", l)] if m]
        if cands:
            i, m = rng.choice(cands)
            old = m.group(1)
            return _rec(proj, p, projdir, i, m.start(1), len(old),
                        "break_input_path", "zero", old, old + "_missing",
                        "file not found")
    return None



# --- package-specific families ------------------------------------------------
#
# The generic families (braces, environments, math) exercise LaTeX's core
# syntax, but real documents fail inside *package* syntax that has its own
# grammar: xy-pic matrices, tikz-cd arrows, amscd arrows, aligned tab counts.
# These errors read very differently in the log -- often reported by the
# package's own error handler rather than by TeX -- so a corpus without them
# tests only half the problem.

def _find_env(L, env):
    """Line indices of \\begin{env} ... \\end{env} bodies."""
    out, start = [], None
    for i, l in enumerate(L):
        if f"\\begin{{{env}}}" in l:
            start = i
        elif f"\\end{{{env}}}" in l and start is not None:
            if i - start > 1:
                out.append((start, i))
            start = None
    return out


def m_xymatrix_cell(proj, projdir, files, rng):
    """Remove a cell separator inside \\xymatrix -- xy-pic's own grammar breaks."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = []
        for i, l in enumerate(L):
            if "\\xymatrix" in "".join(L[max(0, i - 6):i + 1]) and "&" in l:
                pos = [j for j, ch in enumerate(l) if ch == "&" and (j == 0 or l[j-1] != "\\")]
                if pos:
                    cands.append((i, rng.choice(pos)))
        if cands:
            i, col = rng.choice(cands)
            return _rec(proj, p, projdir, i, col, 1, "xymatrix_cell", "large",
                        "&", "", "xy-pic matrix loses a cell separator")
    return None


def m_tikzcd_arrow(proj, projdir, files, rng):
    """Corrupt a tikz-cd arrow specification."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.search(r"\\(?:arrow|ar)\[([^\]]*)\]", l)] if m]
        if cands:
            i, m = rng.choice(cands)
            spec = m.group(1)
            return _rec(proj, p, projdir, i, m.start(1), len(spec),
                        "tikzcd_arrow", "medium", spec, spec + "{",
                        "unbalanced brace inside a tikz-cd arrow spec")
    return None


def m_amscd_arrow(proj, projdir, files, rng):
    """Break an amscd @>>> / @VVV arrow."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.search(r"@[<>ARVv]{2,}", l)] if m]
        if cands:
            i, m = rng.choice(cands)
            old = m.group(0)
            return _rec(proj, p, projdir, i, m.start(), len(old),
                        "amscd_arrow", "medium", old, old[0] + old[1],
                        "truncated amscd arrow")
    return None


def m_align_tabs(proj, projdir, files, rng):
    """Add an alignment tab inside align/aligned, exceeding the column count."""
    for env in ("align", "align*", "aligned", "array", "cases"):
        for p in files:
            L = lines_of(p)
            if not L:
                continue
            for (a, b) in _find_env(L, env):
                cands = [i for i in range(a + 1, b)
                         if "&" in L[i] and not L[i].lstrip().startswith("%")]
                if cands:
                    i = rng.choice(cands)
                    col = L[i].index("&")
                    return _rec(proj, p, projdir, i, col, 1, "align_tabs", "medium",
                                "&", "&&", f"extra alignment tab in {env}")
    return None


def m_verbatim_unclosed(proj, projdir, files, rng):
    """Delete \\end{verbatim} -- swallows the rest of the file as literal text."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, l) for i, l in enumerate(L)
                 if re.match(r"\s*\\end\{(verbatim|lstlisting|Verbatim)\}\s*$", l)]
        if cands:
            i, l = rng.choice(cands)
            return _rec(proj, p, projdir, i, 0, len(l), "verbatim_unclosed", "very_large",
                        l, "", "verbatim region never ends")
    return None


def m_theorem_undefined(proj, projdir, files, rng):
    """Use a theorem environment that was never declared."""
    for p in files:
        L = lines_of(p)
        if not L:
            continue
        cands = [(i, m) for i, l in enumerate(L)
                 for m in [re.match(r"(\s*\\newtheorem\{)(\w+)(\})", l)] if m]
        if cands:
            i, m = rng.choice(cands)
            name = m.group(2)
            return _rec(proj, p, projdir, i, m.start(2), len(name),
                        "theorem_undefined", "very_large", name, name + "X",
                        f"\\begin{{{name}}} used but the declaration was renamed")
    return None


MUTATORS = {
    "newcommand_arity": m_newcommand_arity,
    "drop_usepackage": m_drop_usepackage,
    "rename_macro_def": m_rename_macro_def,
    "delete_close_brace": m_delete_close_brace,
    "delete_end_env": m_delete_end_env,
    "unclosed_math": m_unclosed_math,
    "runaway_arg": m_runaway_arg,
    "break_tabular": m_break_tabular,
    "recursive_def": m_recursive_def,
    "misspell_cs": m_misspell_cs,
    "break_input_path": m_break_input_path,
    "xymatrix_cell": m_xymatrix_cell,
    "tikzcd_arrow": m_tikzcd_arrow,
    "amscd_arrow": m_amscd_arrow,
    "align_tabs": m_align_tabs,
    "verbatim_unclosed": m_verbatim_unclosed,
    "theorem_undefined": m_theorem_undefined,
}


def apply_patch(projdir, rec):
    """Materialize one mutation in an already-copied tree. Returns True on success."""
    p = projdir / rec["file"]
    L = p.read_text("utf-8", "replace").splitlines(keepends=True)
    i = rec["line"] - 1
    if i >= len(L):
        return False
    line = L[i]
    col, span, orig, repl = rec["col"], rec["span"], rec["original"], rec["replacement"]
    if span == 0 and not orig:                      # pure insertion
        L[i] = line.rstrip("\n") + "\n" + repl
    elif span == len(line) and orig == line:        # whole-line delete/replace
        L[i] = repl
    else:
        if line[col:col + span] != orig:
            return False
        L[i] = line[:col] + repl + line[col + span:]
    p.write_text("".join(L))
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--clean", required=True, help="bench json of clean projects")
    ap.add_argument("--specs", required=True, help="specs json (name->path,root)")
    ap.add_argument("--output", required=True)
    ap.add_argument("--per-project", type=int, default=12)
    ap.add_argument("--seed", type=int, default=20260720)
    a = ap.parse_args()

    rng = random.Random(a.seed)
    clean = {r["name"] for r in json.load(open(a.clean)) if r.get("status") == "clean"}
    specs = {s["name"]: s for s in json.load(open(a.specs))}

    out = []
    for name in sorted(clean):
        s = specs.get(name)
        if not s:
            continue
        projdir = Path(s["path"])
        files = tex_files(projdir, s["root"])
        if not files:
            continue
        # Oversample large-gap families: measured yields skew heavily toward
        # gap=0, but the corpus targets ~1/3 in each bucket.
        weights = {"very_large": 5, "large": 3, "medium": 2, "zero": 1}
        fams = []
        for f, g in FAMILIES:
            fams += [f] * weights[g]
        for k in range(a.per_project):
            rng.shuffle(fams)
            rec = None
            for fam in fams:
                try:
                    rec = MUTATORS[fam](name, projdir, files, rng)
                except Exception:
                    rec = None
                if rec:
                    break
            if rec:
                rec["id"] = f"{name}__{len(out):05d}"
                rec["src_path"] = s["path"]
                rec["src_root"] = s["root"]
                out.append(rec)

    Path(a.output).write_text(json.dumps(out, indent=1))
    from collections import Counter
    c = Counter(r["family"] for r in out)
    print(f"[mutate] {len(out)} candidate mutations from {len(clean)} clean projects")
    for k, v in c.most_common():
        print(f"   {v:5}  {k}")


if __name__ == "__main__":
    main()
