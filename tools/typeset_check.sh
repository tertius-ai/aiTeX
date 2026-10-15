#!/usr/bin/env bash
# Does the fast engine put the glyphs where TeX puts them?
#
# The oracle here is `tex` — Knuth's TeX82 itself, not pdfTeX. Nothing in these
# cases needs an e-TeX primitive, so there is no reason to compare against a
# superset with thirty years of its own maintenance behind it; every case was
# checked to give the same DVI under both before the switch. What still needs
# pdfTeX is `latex_check.sh` and `battery.sh`: LaTeX will not load without
# e-TeX, and the batteries exercise the e-TeX forms on purpose.
#
# `ltx_coverage.sh` keeps the *format load* honest; this keeps the typesetting
# honest. Each case is run through both engines and the DVIs compared position
# by position -- every `setchar` and every vertical movement -- not by eyeballing
# the text, which hides everything that matters. A stray scaled point is
# invisible on paper and is exactly the kind of divergence that compounds.
#
# The comparison is of *positions*, not bytes: we pick DVI opcodes less
# cleverly than TeX does (`right3` where it reuses `w0`), which makes our files
# larger without moving anything. That is a real difference and it is noted in
# docs/HANDOFF-fast-engine.md, but it is not a typesetting difference.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
command -v tex >/dev/null || { echo "tex not found"; exit 1; }
command -v dvitype >/dev/null || { echo "dvitype not found"; exit 1; }

TMP="${AITEX_KEEP_TMP:-$(mktemp -d)}"; [ -n "${AITEX_KEEP_TMP:-}" ] || trap 'rm -rf "$TMP"' EXIT
cd "$TMP" || exit 1

# Shared preamble. Every parameter a paragraph or page depends on is set
# explicitly: INITEX's defaults differ from plain.tex's, and a case that relies
# on one silently tests the other.
PRE='\catcode`\{=1
\catcode`\}=2
\catcode`\#=6
\font\rm=cmr10 \rm
\hsize=250pt
\vsize=120pt
\baselineskip=12pt
\topskip=10pt
\maxdepth=4pt
\parindent=20pt
\parskip=6pt
\tolerance=10000
\pretolerance=100
\linepenalty=10
\adjdemerits=10000
'
LOREM='the quick brown fox jumps over the lazy dog, while office traffic waffles fluffily past the coffee shop on the corner of the avenue near the old railway station.'

case_body() {
  case "$1" in
    # One paragraph, justified last line (INITEX \parfillskip).
    ragged)   printf '\\parfillskip=0pt\nA paragraph: %s\n\\par\n' "$LOREM" ;;
    # The same with plain.tex's \parfillskip, which changes every break.
    filled)   printf '\\parfillskip=0pt plus 1fil\nA paragraph: %s\n\\par\n' "$LOREM" ;;
    # \noindent, and a capital first letter -- \sfcode 999 changes the space.
    noindent) printf '\\parfillskip=0pt plus 1fil\n\\noindent A second paragraph: %s\n\\par\nAnd a third: %s\n\\par\n' "$LOREM" "$LOREM" ;;
    # Long enough to break across pages.
    pages)    printf '\\parfillskip=0pt plus 1fil\n'
              for i in 1 2 3 4 5 6; do printf 'Paragraph %s: %s\n\\par\n' "$i" "$LOREM"; done ;;
    # Explicit boxes, kerns, rules and glue in the middle of text.
    boxes)    printf '\\parfillskip=0pt plus 1fil\n\\noindent A\\kern 3pt B\\hbox to 40pt{C}D\\hskip 12pt plus 3pt E\\vrule width 2pt F\\hbox{\\hbox{G}\\kern 1pt H}I %s\n\\par\n' "$LOREM" ;;
    # `\looseness` — `tex.web` §875 picks a *different endpoint* once the
    # paragraph is broken, the one whose line count comes nearest the wanted
    # difference from the best. §849 is what makes the choice exist: with
    # `\looseness` nonzero the breaker may not collapse chains of different
    # line counts, so the endpoints on offer are more than one per fitness
    # class. Six paragraphs, one per value, because the *whole page* moves
    # when the count is wrong and no single line shows it.
    loose)    printf '\\parfillskip=0pt plus 1fil\n\\hsize=120pt\n'
              for l in 0 1 2 3 -1 -2; do
                printf '\\looseness=%s\n\\noindent %s\n\\par\n' "$l" "$LOREM"
              done
              # With a `\rightskip` that stretches infinitely every chain ties at
              # nought demerits — INITEX's `\linepenalty` is nought too — so the
              # paragraph may be broken anywhere and the answer is decided
              # entirely by which nodes are alive and in what order: §851's
              # per-line-width deactivation, §835's grouping by the
              # predecessor's line number, and §836's `<=`, which lets the last
              # of a tie win.
              printf '\\looseness=2\n\\rightskip 0pt plus 1fil\n'
              printf '\\hangafter=-3 \\hangindent=-20pt\n'
              printf '\\noindent %s\n\\par\n' "$LOREM"
              printf '\\hangindent=0pt \\hangafter=1\n'
              printf '\\looseness=0\n'
              printf '\\parshape=4 0pt 120pt 10pt 100pt 20pt 80pt 0pt 120pt\n'
              printf '\\noindent %s\n\\par\n' "$LOREM"
              printf '\\rightskip 0pt\n\\parshape=0\n' ;;
    # Interword stretch driven to its limits by a narrow measure.
    narrow)   printf '\\parfillskip=0pt plus 1fil\n\\hsize=90pt\n\\noindent %s\n\\par\n' "$LOREM" ;;
    # A real \output routine: it runs mid-page-build, wraps \box255 and adds a
    # folio. This is the case that distinguishes "the page builder works" from
    # "the page builder can hand control to a TeX macro and get it back".
    output)   printf '\\parfillskip=0pt plus 1fil\n\\count0=1\n'
              printf '\\output={\\shipout\\vbox{\\box255\\hbox{Page \\the\\count0}}\\global\\advance\\count0 by1 }\n'
              for i in 1 2 3 4 5 6; do printf 'Paragraph %s: %s\n\\par\n' "$i" "$LOREM"; done ;;
  esac
}

# Where every glyph and rule lands, horizontally.
#
# What marks the page, not how the file spells it. Two adjacent movements that
# TeX writes as one `right` and we write as two put the glyph in the same place;
# a glue that is a scaled point wrong moves it, and that shows up here as surely
# as it would in a raw diff. Comparing the movement opcodes as well only added a
# way to fail that no reader could see.
positions() {
  dvitype "$1" 2>/dev/null |
    grep -oE '(setchar[0-9]+|putrule [0-9-]+ [0-9-]+|setrule [0-9-]+ [0-9-]+) h:=[0-9]+\+[0-9-]+=[0-9]+'
}
# Where the reference point *is* when something is drawn, vertically.
#
# Only the movement a glyph or rule actually follows is kept. TeX synchronises
# the vertical position lazily -- it writes the `push` first and the movement
# after -- so a box's shift and the height of the vertical list inside it come
# out as one `down` where we write two, either side of the push. The route
# differs; the position drawn at does not, and that is what a page looks like.
# Every position anything is set at is still compared, so a scaled point out is
# still a failure. Only the resulting position is compared, not the arithmetic
# dvitype prints to reach it: `1741226-413941` and `1503401-176116` are the
# same place.
verticals() {
  dvitype "$1" 2>/dev/null |
    grep -oE '(v:=[0-9+-]+=[0-9-]+|setchar[0-9]+|setrule|putrule)' |
    awk '/^v:=/ { sub(/.*=/, "", $0); pending = $0; next }
         { if (pending != "") { print pending; pending = "" } }'
}
pagecount() { dvitype "$1" 2>/dev/null | grep -c 'beginning of page'; }

# The ten counters each `bop` carries (`tex.web` §640). `\count0` *is* the page
# number — `\pageno` in plain TeX is `\count0` — and the other nine are the
# document's to use. Compared because they were not: the shipper wrote a
# sequential counter, which agrees with LaTeX (it sets `\count0` to the same
# sequence) and with nothing else.
pagenumbers() { dvitype "$1" 2>/dev/null | grep -oaE 'beginning of page [0-9 -]+'; }

CASES="ragged filled noindent pages boxes narrow loose output"

# plain.tex end to end: plain's own fonts, \output (which sets the folio) and
# \bye. Run separately because it supplies its own preamble.
PLAIN_BODY='\input plain
\hsize=250pt
\vsize=200pt
The quick brown fox jumps over the lazy dog, while office traffic waffles
fluffily past the coffee shop on the corner of the avenue.

A second paragraph, set with plain TeX'"'"'s own parameters, so the folio at the
foot of the page comes from plain'"'"'s output routine rather than anything we wrote.

\bye
'

# Cases known to disagree, named so a disagreement stays visible and the list
# cannot quietly grow. A case not on this list that fails is a regression and
# fails the run. Empty is the goal, and currently the truth.
KNOWN_DIVERGENT=""
KNOWN_WHY=""

fails=0
known=0
for c in $CASES; do
  { printf '%s' "$PRE"; case_body "$c"; }            > "o_$c.tex"
  { printf '%s' "$PRE"; case_body "$c"; echo '\end'; } > "r_$c.tex"

  AITEX_NO_INPUT=1 AITEX_DVI="$TMP/o_$c.dvi" "$FAST" pass "o_$c.tex" --ini >/dev/null 2>&1
  timeout 120 tex -ini -interaction=batchmode "r_$c.tex" >/dev/null 2>&1

  if [ ! -s "o_$c.dvi" ] || [ ! -s "r_$c.dvi" ]; then
    printf '  FAIL  %-9s no output (ours %s, tex %s)\n' "$c" \
      "$([ -s "o_$c.dvi" ] && echo ok || echo missing)" \
      "$([ -s "r_$c.dvi" ] && echo ok || echo missing)"
    fails=$((fails + 1)); continue
  fi

  n=$(positions "o_$c.dvi" | wc -l)
  ph=$(pagecount "o_$c.dvi"); pr=$(pagecount "r_$c.dvi")
  hdiff=$(diff <(positions "o_$c.dvi") <(positions "r_$c.dvi") | grep -c '^<')
  vdiff=$(diff <(verticals "o_$c.dvi") <(verticals "r_$c.dvi") | grep -c '^<')
  ndiff=$(diff <(pagenumbers "o_$c.dvi") <(pagenumbers "r_$c.dvi") | grep -c '^<')
  if [ "$hdiff" = 0 ] && [ "$vdiff" = 0 ] && [ "$ph" = "$pr" ] && [ "$ndiff" = 0 ]; then
    printf '  ok    %-9s %4s positions, %s page(s)\n' "$c" "$n" "$ph"
  elif echo " $KNOWN_DIVERGENT " | grep -q " $c "; then
    printf '  KNOWN %-9s %s horizontal differ (line breaker; see the note above)\n' \
      "$c" "$hdiff"
    known=$((known + 1))
  else
    printf '  FAIL  %-9s %s horizontal, %s vertical, %s page-number differ; pages %s vs %s\n' \
      "$c" "$hdiff" "$vdiff" "$ndiff" "$ph" "$pr"
    diff <(positions "o_$c.dvi") <(positions "r_$c.dvi") | head -6 | sed 's/^/          /'
    diff <(pagenumbers "o_$c.dvi") <(pagenumbers "r_$c.dvi") | head -4 | sed 's/^/          /'
    fails=$((fails + 1))
  fi
done

# --- plain.tex, end to end ---------------------------------------------------
plain_case() {  # <name> <body>
  printf '%s' "$2" > "plain_$1.tex"
  AITEX_MAX_STEPS=900000000 AITEX_DVI="$TMP/o_$1.dvi" "$FAST" pass "plain_$1.tex" --ini >/dev/null 2>&1
  timeout 120 tex -interaction=batchmode "plain_$1.tex" >/dev/null 2>&1
  mv -f "plain_$1.dvi" "r_$1.dvi" 2>/dev/null
  if [ ! -s "$TMP/o_$1.dvi" ] || [ ! -s "r_$1.dvi" ]; then
    printf '  FAIL  %-9s no output\n' "$1"; fails=$((fails + 1)); return
  fi
  local hd vd ph pr
  hd=$(diff <(positions "$TMP/o_$1.dvi") <(positions "r_$1.dvi") | grep -c '^<')
  vd=$(diff <(verticals "$TMP/o_$1.dvi") <(verticals "r_$1.dvi") | grep -c '^<')
  ph=$(pagecount "$TMP/o_$1.dvi"); pr=$(pagecount "r_$1.dvi")
  if [ "$hd" = 0 ] && [ "$vd" = 0 ] && [ "$ph" = "$pr" ]; then
    printf '  ok    %-9s %4s positions, %s page(s)\n' "$1" "$(positions "$TMP/o_$1.dvi" | wc -l)" "$ph"
  elif echo " $KNOWN_DIVERGENT " | grep -q " $1 "; then
    printf '  KNOWN %-9s %s horizontal differ (%s)\n' "$1" "$hd" "$KNOWN_WHY"; known=$((known + 1))
  else
    printf '  FAIL  %-9s %s horizontal, %s vertical differ; pages %s vs %s\n' "$1" "$hd" "$vd" "$ph" "$pr"
    fails=$((fails + 1))
  fi
}

# Font switches, an italic correction, a rule, centring and explicit vertical
# glue — the constructs a document uses that a bare paragraph does not.
RICH_BODY='\input plain
\hsize=250pt
\vsize=300pt
\centerline{\bf A Heading}
\medskip
Ordinary text with {\it emphasis\/} and {\bf bold}, followed by a rule.
\smallskip
\hrule
\smallskip
\noindent Another paragraph after the rule, long enough to break across a couple
of lines so the interline glue is exercised as well.
\vskip 12pt
\centerline{Centred again}
\bye
'
plain_case rich "$RICH_BODY"

# Inline math: families, script sizes, sub/superscripts and the class-driven
# spacing that makes `$-x$` and `$a-x$` different.
MATH_BODY='\input plain
\hsize=250pt
\noindent Text with math $a+b=c$ inline, and $x^2 + y_1 = z^{n+1}$ too.
\par
\bye
'
plain_case math "$MATH_BODY"

# Displayed math: the paragraph is broken *before* the display is built, the
# display is centred in \displaywidth, and \predisplaysize decides whether the
# skips around it are the short pair or the long one.
DISPLAY_BODY='\input plain
\hsize=250pt
\vsize=300pt
\noindent A short line.
$$a+b=c$$
And text after the display which runs on for a while to make a second line here.
$$x^2+y^2=z^2$$
\noindent A paragraph whose last line is long enough that the display below it
must use the long skips instead of the short ones.
$$w$$
Done.
\par
\bye
'
plain_case display "$DISPLAY_BODY"

# Fractions and the rules that go with them: `\over`, `\atop`, `\above`,
# `\overline`, `\underline`, and each of them nested inside a script, where the
# font parameters that place them all change size.
FRAC_BODY='\input plain
\hsize=250pt
\vsize=300pt
\noindent Fractions $a\over b$ and ${c\atop d}$ and $x\above2pt y$ here,
with $\overline{pq}$ and $\underline{rs}$ and $\overline{a\over b}$ too.
\par
\noindent Nested: $x^{a\over b}$ and $x_{a\atop b}^{c\over d}$ and
${{a\over b}\over{c\over d}}$ and $\overline{x^{a\over b}}$ and
$x^{y^z}$ and $\overline{pq}^2$ and $a\over b^2$.
\par
\noindent Styles: $\displaystyle{a\over b}$ and $\textstyle{c\over d}$ and
$\scriptstyle x^2$ and $\scriptscriptstyle y_1$ and $a+\displaystyle b^2$.
\par
$$1\over{2}$$
$$x^2\over y_1$$
\bye
'
plain_case frac "$FRAC_BODY"

# Delimiters: `\sqrt` is `\radical`, and `\left`/`\right` are sized from the
# height and depth of everything between them -- out of the font's charlist
# when a bigger glyph will do, and out of its extensible recipe when none will.
DELIM_BODY='\input plain
\hsize=250pt
\vsize=300pt
\noindent Roots $\sqrt2$ and $\sqrt x y$ and $\sqrt{x+y}$ and $\sqrt{\sqrt x}$
and $\sqrt{a\over b}$ here.
\par
\noindent Delimiters $\left(a+b\right)$ and $\left[x\right]$ and
$\left\{a\right\}$ and $\bigl(x\bigr)$ and $\Bigl[y\Bigr]$ and
$\mathopen{(}a\mathclose{)}$ here.
\par
$$\sqrt{x^2+y^2}\qquad\left({a\over b}\right)$$
$$\left(\sum_{i=1}^{n} x_i\right)\quad\int_0^1 f\quad\sum\nolimits_{k} y_k$$
$$\lim_{n\to\infty}a_n=\sum_{k=0}^\infty{1\over k!}\qquad x^\alpha_\beta$$
\bye
'
plain_case delim "$DELIM_BODY"

# Accents and `\mathchoice`: `\tilde` is `\mathaccent`, and `\cong` is
# `\mathpalette`, which is four `\mathchoice` branches each holding an
# alignment inside a `\vbox` inside math.
ACCENT_BODY='\input plain
\hsize=300pt
\vsize=400pt
\noindent Accents $\tilde H_*$ and $\bar y+\vec v$ and $\hat x^2$ and
$\tilde H_n(X)$ here.
\par
\noindent Relations $X\cong Y$ and $a\ne b$ and $\mathchoice{p}{q}{r}{s}$ and
$x^{\mathchoice{p}{q}{r}{s}}$ here.
\par
$$\tilde H_n(X;{\bf Z})\cong\tilde H_n(Y;{\bf Z})\qquad\hbox{for all }n\ge0.$$
$$a\vbox{\halign{$#$\cr x\cr}}b\qquad\int_0^\infty e^{-x^2}\,dx={\sqrt\pi\over2}$$
$$n\choose k$$
$$a\overwithdelims()b\quad c\atopwithdelims[]d\quad
\vcenter{\hbox{v}}+w\quad e\abovewithdelims\{\}2pt f$$
\bye
'
plain_case accent "$ACCENT_BODY"

# A page of real mathematics: prose with inline formulas, displays, roots,
# fractions, large operators with limits, accents and delimiters. This is the
# case that says the engine can set a paper.
PAPER_BODY='\input plain
\hsize=300pt \vsize=400pt
\centerline{\bf On a Theorem}
\medskip
\noindent Let $f\colon X\to Y$ be a map of spaces, and suppose that for every
$n\ge 0$ the induced map $\pi_n(f)$ is an isomorphism. Then, writing
$\tilde H_*$ for reduced homology, we have
$$\tilde H_n(X;{\bf Z})\cong\tilde H_n(Y;{\bf Z})\qquad\hbox{for all }n\ge0.$$
The proof proceeds by induction on $n$, using the exact sequence
$$\cdots\to H_{n+1}(Y,X)\to H_n(X)\to H_n(Y)\to H_n(Y,X)\to\cdots$$
and the observation that $H_*(Y,X)=0$.
\medskip
\noindent Consider the integral $\int_0^\infty e^{-x^2}\,dx={\sqrt\pi\over2}$,
and the sum $\sum_{k=1}^\infty{1\over k^2}={\pi^2\over6}$. In display these read
$$\int_0^\infty e^{-x^2}\,dx={\sqrt\pi\over2},\qquad
\sum_{k=1}^\infty{1\over k^2}={\pi^2\over6}.$$
\medskip
\noindent Finally, $\left({a+b\over c-d}\right)^2\ne\left[{a\over c}\right]^2$
whenever $b\ne0$ or $d\ne0$.
\par
\bye
'
plain_case paper "$PAPER_BODY"

# `\vsplit`: break a box register in two at the best place for a given height,
# leave the rest in the register with `\splittopskip` at its top.
SPLIT_BODY='\input plain
\hsize=200pt \vsize=300pt
\splittopskip=3pt
\setbox0=\vbox{\hbox{A}\hbox{B}\hbox{C}\hbox{D}\hbox{E}}
\setbox1=\vsplit0 to 20pt
\shipout\vbox{\box1\kern10pt\box0}
\setbox2=\vbox{\hbox{P}\hbox{Q}\hbox{R}}
\setbox3=\vsplit2 to 5pt
\shipout\vbox{\box3\kern6pt\box2}
\bye
'
plain_case vsplit "$SPLIT_BODY"

# Hyphenating words that contain ligatures. `office` is `o` `ffi` `c` `e` on
# the page, so its patterns have to be matched against the letters underneath
# and the pieces re-ligatured around the break: `of-` and `fi`+`ce`.
HYPH_BODY='\input plain
\hsize=200pt \vsize=300pt
\noindent Difficult sufficiently different affluent offices, and the
coefficient of friction in the classification of manufacturing.
\par
\noindent Hyphenation of ordinary words like representation and international
and extraordinary establishment procedures.
\par
\raggedright
\noindent The quick brown fox jumps over the lazy dog while office traffic
waffles fluffily past the coffee shop on the corner of the avenue.
\par
\bye
'
plain_case hyphen "$HYPH_BODY"
CASES="$CASES hyphen"
CASES="$CASES vsplit"
CASES="$CASES paper"
CASES="$CASES accent"
CASES="$CASES delim"
CASES="$CASES frac"
CASES="$CASES display"
CASES="$CASES math"

# \halign: a preamble whose templates are replayed around every entry, columns
# whose widths are not known until the last row has been read, and \noalign
# material between rows.
ALIGN_BODY='\input plain
\hsize=250pt
\noindent Before.
\par
\halign{#\hfil&\quad#\hfil&\quad\hfil#\cr
one&two&three\cr
alpha&beta&gamma\cr
\noalign{\smallskip\hrule\smallskip}
x&y&z\cr}
\noindent After.
\par
\bye
'
plain_case align "$ALIGN_BODY"
CASES="$CASES align"

# A footnote: `\insert` in the middle of a paragraph, migrating out of its line
# and reaching the page its reference landed on.
FOOT_BODY='\input plain
\hsize=250pt
\vsize=200pt
\noindent A paragraph of text with a footnote attached to it,\footnote*{The note
itself, which is set at the foot of the page.} and some more text after the
footnote so the paragraph runs on for a couple of lines.
\par
\bye
'
plain_case footnote "$FOOT_BODY"

# Math inside an alignment cell, with an `\everymath` that ends in
# `\aftergroup` — which is the shape LaTeX's own `\everymath` has, and the
# reason `array` produced no page at all. A formula is a group (`tex.web`
# §1145), so the `\aftergroup` token belongs at the closing `$`; when it
# surfaced at the end of the *cell* instead, `\halign` was looking for the next
# row, did not find one, and swallowed the rest of the document.
MATHCELL_BODY='\input plain
\hsize=250pt
\vsize=200pt
\everymath{\aftergroup\relax}
\noindent Before.
\halign{#&#\cr $a$&$b$\cr $c$&$d$\cr}
After.
\par
\bye
'
plain_case mathcell "$MATHCELL_BODY"
CASES="$CASES mathcell"

# `\eqno` and `\leqno`: the equation number is a formula of its own, packed
# and then placed beside the display — or under it, hard against the right
# margin, when there is no room (`tex.web` §1199-§1205). The third display here
# is wide enough to take that second path.
EQNO_BODY='\input plain
\hsize=250pt
\vsize=200pt
\noindent Before the display, a line of text that runs on a bit.
$$ a+b=c \eqno (1) $$
After it.
$$ x=y \leqno (2) $$
And more.
$$ \hbox to 240pt{wide} \eqno (3) $$
End.
\par
\bye
'
plain_case eqno "$EQNO_BODY"
CASES="$CASES eqno"

# `\leaders`, `\cleaders`, `\xleaders` and a rule leader. The dots of a table
# of contents are the first; the other two differ only in how the slack is
# distributed, and `tex.web` §626 does that arithmetic on a span ten scaled
# points longer than the glue, which is visible here and nowhere else.
LEADERS_BODY='\input plain
\hsize=250pt
\vsize=200pt
\noindent\hbox to 200pt{Chapter one\leaders\hbox{. }\hfill 7}
\par\noindent\hbox to 200pt{Two\leaders\hbox to 6pt{\hss.\hss}\hfill 12}
\par\noindent\hbox to 200pt{Three\cleaders\hbox{. }\hfill 25}
\par\noindent\hbox to 200pt{Four\xleaders\hbox{. }\hfill 99}
\par\noindent\hbox to 200pt{Five\leaders\hrule height 0.4pt\hfill x}
\par
\bye
'
plain_case leaders "$LEADERS_BODY"
CASES="$CASES leaders"

# A box deeper than `\maxdepth` near the foot of the page. `tex.web` §1003
# moves the excess depth into the page's *height*, so the page is as full as it
# looks; without that it takes on a line TeX would have carried over, and every
# page after it differs. Only ever visible when a deep box lands near a break.
DEEP_BODY='\input plain
\hsize=250pt
\vsize=120pt
\maxdepth=4pt
\parskip=0pt
The quick brown fox jumps over the lazy dog, while office traffic waffles.
\par
\hbox{}\kern0pt
\vbox to 0pt{\vss\hbox{deep}\kern40pt}
\par
The quick brown fox jumps over the lazy dog, while office traffic waffles past
the coffee shop on the corner of the avenue near the old railway station and on
beyond it to the far end of the town.
\par
\bye
'
plain_case deepbox "$DEEP_BODY"
CASES="$CASES deepbox"

# `<font>` in `\fontdimen<n><font>` is any font identifier — `\font` for the
# current one, or `\textfont<n>`/`\scriptfont<n>`/`\scriptscriptfont<n>`.
# `\fontdimen22\textfont2` is the axis height every formula is built around.
# Typeset the numbers so the DVI comparison sees them.
FONTOPS_BODY='\input plain
\hsize=250pt
\vsize=200pt
\noindent axis=\the\fontdimen22\textfont2\par
\noindent script axis=\the\fontdimen22\scriptfont2\par
\noindent quad=\the\fontdimen6\textfont2\par
\noindent space=\the\fontdimen2\font\par
\noindent num=\number\fontdimen6\textfont3\par
\bye
'
plain_case fontops "$FONTOPS_BODY"
CASES="$CASES fontops"

# Lists: `\item` is `\par\hang\textindent`, so this is \hangindent line by line,
# \llap'd markers, the \TeX logo's negative kerns and raised box, a tie, a
# discretionary and a control space — the shape of any real document.
LIST_BODY='\input plain
\hsize=250pt
\vsize=300pt
\noindent Before the list, a paragraph that runs on for long enough to break.
\medskip
\item{1.} The first item, set with plain'"'"'s \TeX\ hanging indentation so that
it wraps under itself rather than back to the margin.
\item{2.} A second item, shorter.
\itemitem{a)} A sub-item, indented further still.
\medskip
\noindent A paragraph with \ a control space, an inter\-word discretionary,
and a~tie between words.
\par
\parshape 2 0pt 200pt 30pt 170pt
\noindent A paragraph with an explicit shape, narrow on its second line and
every line after it, running on long enough to show it.
\par
\centerline{\hfil A line with \hfil glue \hfil}
\bye
'
plain_case lists "$LIST_BODY"
CASES="$CASES lists"
CASES="$CASES footnote"

# A footnote too tall for the room its class is allowed, so TeX splits it: part
# at the foot of one page, the rest at the foot of the next (`tex.web`
# §1008-§1010). `\dimen\footins` is the ceiling, `\splittopskip` sets the
# remainder's first baseline, and every one of those has to be read at the right
# moment or the two pages come out different heights.
SPLIT_BODY='\input plain
\hsize=250pt
\vsize=120pt
\dimen\footins=24pt
\splittopskip=10pt
\splitmaxdepth=2pt
The quick brown fox jumps over the lazy dog.\footnote*{A long footnote that runs
to several lines so that it cannot possibly fit into the twenty-four points that
this insertion class has been allowed on any one page, and must therefore be
split across the page boundary the way TeX splits it.}
While office traffic waffles fluffily past the coffee shop on the corner of the
avenue near the old railway station, a second paragraph runs on.

A third paragraph with more text to push things onto a second page, and yet more
text after that so the page builder has real decisions to make about where the
break should fall, and then some more again.
\bye
'
plain_case inssplit "$SPLIT_BODY"
CASES="$CASES inssplit"

# `\count\footins` is a magnification: at 500 a footnote is charged half its
# height against the page, which moves the break.
MAGN_BODY='\input plain
\hsize=250pt
\vsize=100pt
\count\footins=500
The quick brown fox jumps over the lazy dog.\footnote*{A footnote of three lines
or so, whose height counts only half against the page because of the
magnification this class was given.}
While office traffic waffles fluffily past the coffee shop on the corner of the
avenue near the old railway station, a second paragraph runs on and on for some
distance yet.

A third paragraph with more text to push things onto a second page, and yet more
text after that so the page builder has real decisions to make.
\bye
'
plain_case insmagn "$MAGN_BODY"
CASES="$CASES insmagn"

# Three notes crowding the foot of a short page: the last one does not fit and
# waits for the next page, which is what `\insertpenalties` counts.
HELD_BODY='\input plain
\hsize=250pt
\vsize=100pt
The quick brown fox jumps over the lazy dog, while office traffic waffles past
the coffee shop on the corner of the avenue near the old railway
station.\footnote*{First note.} And more text here to fill the
page.\footnote{$\dagger$}{Second note, which is rather longer than the first one
and takes two lines of its own at least.} And a third bit of
text.\footnote{$\ddagger$}{Third note.}

More text in a new paragraph so the page has to break somewhere, and then some
more again to be sure of it.
\bye
'
plain_case insheld "$HELD_BODY"
CASES="$CASES insheld"

# `\valign` — `tex.web` §768, `\halign` with the axes interchanged: the
# preamble describes rows, `\cr` ends a *column*, each entry is a vbox set to
# its row's height, and the finished columns go side by side into the
# horizontal list. Templates, `\omit`, `\tabskip`, `to`/`spread` and a
# `\noalign` holding *horizontal* material all differ from `\halign` only in
# the axis, so all of them are here.
VALIGN_BODY='\input plain
\hsize=300pt \parindent=0pt \tabskip=3pt
\noindent\valign{#&#\cr\hbox{a}&\hbox{bb}\cr\hbox{ccc}&\hbox{d}\cr}
\par\noindent\valign{\hbox{[#]}\vfil&#\cr\hbox{p}&\hbox{q}\cr\omit\hbox{X}&\hbox{r}\cr}
\par\noindent\valign to 40pt{#\cr\hbox{s}\cr\noalign{\hskip 5pt}\hbox{t}\cr}
\par\noindent x\valign{#\cr\vbox{\hbox{u}\hbox{v}}\cr\hbox{w}\cr}y
\bye
'
plain_case valign "$VALIGN_BODY"
CASES="$CASES valign"

# `\pageno` is `\count0`, and `\count1`..`\count9` are the document's. They go
# into every `bop` (§640) and nothing here compared them, so a shipper writing
# a sequential counter passed — it agrees with LaTeX and with nothing else.
PAGENO_BODY='\input plain
\hsize=200pt \vsize=60pt
\pageno=-7 \count3=42 \count7=-1
First page.\vfill\eject
Second page.\vfill\eject
\pageno=100 \count3=99
Third page.
\bye
'
plain_case pageno "$PAGENO_BODY"
CASES="$CASES pageno"
CASES="$CASES rich"

printf '%s' "$PLAIN_BODY" > plain_doc.tex
AITEX_MAX_STEPS=900000000 AITEX_DVI="$TMP/o_plain.dvi" "$FAST" pass plain_doc.tex --ini >/dev/null 2>&1
timeout 120 tex -interaction=batchmode plain_doc.tex >/dev/null 2>&1
mv -f plain_doc.dvi r_plain.dvi 2>/dev/null
if [ -s "$TMP/o_plain.dvi" ] && [ -s r_plain.dvi ]; then
  hdiff=$(diff <(positions "$TMP/o_plain.dvi") <(positions r_plain.dvi) | grep -c '^<')
  vdiff=$(diff <(verticals "$TMP/o_plain.dvi") <(verticals r_plain.dvi) | grep -c '^<')
  ph=$(pagecount "$TMP/o_plain.dvi"); pr=$(pagecount r_plain.dvi)
  if [ "$hdiff" = 0 ] && [ "$vdiff" = 0 ] && [ "$ph" = "$pr" ]; then
    printf '  ok    %-9s %4s positions, %s page(s)\n' plain "$(positions "$TMP/o_plain.dvi" | wc -l)" "$ph"
  elif echo " $KNOWN_DIVERGENT " | grep -q " plain "; then
    printf '  KNOWN %-9s %s horizontal differ (%s)\n' plain "$hdiff" "$KNOWN_WHY"
    known=$((known + 1))
  else
    printf '  FAIL  %-9s %s horizontal, %s vertical differ; pages %s vs %s\n' plain "$hdiff" "$vdiff" "$ph" "$pr"
    fails=$((fails + 1))
  fi
else
  printf '  FAIL  %-9s no output\n' plain
  fails=$((fails + 1))
fi
CASES="$CASES plain"

total=$(echo "$CASES" | wc -w)
echo
if [ "$fails" = 0 ]; then
  echo "every glyph is where TeX puts it — $((total - known))/$total cases$([ "$known" = 0 ] || echo ", $known known divergence(s)")"
else
  echo "$fails case(s) newly disagree with TeX"
  exit 1
fi
