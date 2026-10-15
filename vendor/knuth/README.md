# `tex.web`

Knuth's TeX, version 3.141592653, unmodified, from
`https://mirrors.ctan.org/systems/knuth/dist/tex/tex.web`.

Every commit in this repository quotes a section of it, and until now those
quotations were from memory. That is not good enough: the line-breaker work in
`§829`'s commit turned on three details that could not be settled by probing —

- §855 records a candidate when it is cheaper **or equal**, so ties go to the
  last node scanned;
- §845 links a new node in *before* the node the walk is standing on;
- §835 flushes a line-number group once the cursor has already moved on to the
  next one, which decides where in the list that node lands.

Two different tie-breaking heuristics were fitted to probes before this file
was read, and each reproduced TeX on one paragraph and not another. Reading
the three sections settled it in minutes.

Sections are numbered by counting `@` at the start of a line, followed by a
space, tab, newline or `*`:

```python
import re
parts = re.split(r'(?m)^@(?=[ \t\n*])', open('vendor/knuth/tex.web').read())
sections = dict(enumerate(parts[1:], start=1))   # 1380 of them
print(sections[855])
```

The copyright notice permits unlimited redistribution of the file so long as
it is not modified, which is why it is here whole and untouched.
