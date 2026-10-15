# Deterministic performance fixture

`main.tex` deliberately covers a root paragraph, an included file, a
bibliography, cross-references, a preamble definition, and an image. The P1
runner copies this directory to temporary worktrees and makes one controlled
edit at a time. It generates `figure.png` from fixed RGB samples; the EPS is
the equivalent DVI-route asset.

This fixture is small enough for CI. It is not a substitute for the locked
191-paper warm benchmark or the long-document scaling gate.
