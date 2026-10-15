# §897 `reconstitute` does not use the ligature record

**Status:** the node is built; one consumer of it is not written.

§143/§144's ligature node — the character set plus the run it replaced — is
built and displayed as of the §143/§144/§193 commit, and
`tests/showbox/ligatures.tex` pins it.

What remains is the reason the record exists. §897 `reconstitute` takes a word
apart to hyphenate it and re-runs the font's ligature program on the *letters*;
it needs the original run, not the ligature. Our hyphenation works on character
nodes and has no path for a ligature node's contents, so a word containing one
hyphenates from whatever the ligature node looks like rather than from what it
was made of.

The other two consumers named when this was first filed are fine as they stand:
`\unhbox` hands the nodes back and they now carry their record, and §1124's
`\accent` inspects the preceding node, which is still a `Node::Char`.

## The case to write

A word with a ligature in it, at an `\hsize` narrow enough to force a break
inside the word, against real TeX. `tools/typeset_check.sh` is the right gate —
this is a glyph-position question, not a display one, and it is the first
ligature bug that would show in a DVI.
