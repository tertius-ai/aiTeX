#!/usr/bin/env bash
# Does a real LaTeX *document* come out where pdfTeX puts it?
#
# `ltx_coverage.sh` keeps the kernel *load* honest and `typeset_check.sh` keeps
# plain-TeX typesetting honest. Neither exercises the thing in between: a
# `\documentclass{article}` … `\begin{document}` … `\end{document}` run, where
# the kernel, the class, expl3's deferred code and LaTeX's output routine all
# have to work at once.
#
# Both engines are fed the *same* file: latex.ltx read as ordinary input (there
# is no format to dump), then the document. The oracle is pdfTeX with `-etex`
# and not `tex`, and it has to be: LaTeX refuses to load under TeX82 —
# "LaTeX requires e-TeX" — so there is no TeX82 answer to compare against.
# `typeset_check.sh` and most of `battery.sh` do use `tex` itself.
# The last four lines of latex.ltx (`\makeatother \errorstopmode \dump`) must
# go, or it dumps a format instead of running.
#
# The comparison is of glyph *positions*, as in typeset_check.sh: every setchar
# with the h and v it was drawn at, plus the page count.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
LTX="$(kpsewhich latex.ltx)"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
[ -n "$LTX" ] || { echo "latex.ltx not found"; exit 1; }
command -v pdftex  >/dev/null || { echo "pdftex not found"; exit 1; }
command -v dvitype >/dev/null || { echo "dvitype not found"; exit 1; }

# A fixed clock for both engines, so the `today` case is reproducible and can
# never straddle midnight. pdfTeX takes `\year` and friends from
# SOURCE_DATE_EPOCH only when FORCE_SOURCE_DATE is also set; ours does the same.
export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1

TMP="$(mktemp -d)"
if [ "${KEEP_TMP:-0}" = 1 ]; then
  echo "latex check work directory: $TMP"
else
  trap 'rm -rf "$TMP"' EXIT
fi
cd "$TMP" || exit 1

# latex.ltx minus its trailing `\makeatother \errorstopmode \dump`.
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > kernel.tex
mkdir -p ref
cp kernel.tex ref/kernel.tex

# FMT=1 also checks the *format dump*: the kernel is loaded once, the engine is
# dumped, and every case is then run from that dump instead of from source. The
# two runs must agree glyph for glyph — a format is only worth having if
# restoring it is indistinguishable from executing what produced it.
# CACHE=1 additionally turns on the preamble snapshot. Each case is run twice
# per pass anyway (for the `.aux`), so the second run of every case is a
# snapshot *hit* — which is exactly the path that must not change the output.
CACHEDIR=""
if [ "${CACHE:-0}" = 1 ]; then CACHEDIR="$TMP/snap"; mkdir -p "$CACHEDIR"; fi
FMTFILE=""
if [ "${FMT:-0}" = 1 ]; then
  printf '\\input kernel\n' > mkfmt.tex
  AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 "$FAST" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1
  FMTFILE="$TMP/fmt.afmt"
  [ -s "$FMTFILE" ] || { echo "format dump failed"; exit 1; }
fi

SUBFILE='A sentence from an included file, long enough to need breaking across the measure it is set in.'
LOREM='The quick brown fox jumps over the lazy dog, while office traffic waffles fluffily past the coffee shop on the corner of the avenue near the old railway station.'

case_body() {
  case "$1" in
    hello)  printf 'Hello, world.\n' ;;
    para)   printf '%s\n' "$LOREM" ;;
    twopar) printf '%s\n\n%s\n' "$LOREM" "$LOREM" ;;
    emph)   printf 'A line with \\emph{emphasis} and \\textbf{bold} in it.\n' ;;
    math)   printf 'An inline formula $a^2+b^2=c^2$ and a display\n\\[ \\int_0^1 x^2\\,dx = \\frac{1}{3}. \\]\nafter it.\n' ;;
    itemize) printf '\\begin{itemize}\\item One\\item Two\\end{itemize}\n' ;;
    section) printf '\\section{A section}\n%s\n' "$LOREM" ;;
    # Frontiers' section style passes unexpandable paragraph parameters through
    # `\MakeUppercase`. expl3 classifies each token with
    # `\ifx\noexpand#1#1`; a named parameter that does not compare equal to
    # itself is repeatedly expanded and reinserted instead of being stored.
    uppersection) printf '\\makeatletter\n\\def\\section{\\@startsection{section}{1}{\\z@}{-10\\p@ plus -3\\p@}{3\\p@}{\\reset@font\\raggedright\\fontsize{13}{13}\\selectfont\\MakeUppercase}}\n\\makeatother\n\\section{Introduction}\nBody.\n' ;;
    # A measure narrow enough that TeX must hyphenate to set the paragraph at
    # all. LaTeX's patterns arrive through babel's `hyphen.cfg`, which walks
    # `language.dat` with `\openin`/`\read` — so this case tests the whole path
    # from reading a data file to a break inside a word.
    hyphen) printf '\\hsize=120pt \\linewidth=120pt \\columnwidth=120pt\n\\noindent Incomprehensibility and antidisestablishmentarianism are extraordinarily complicated demonstrations of typographical hyphenation behaviour.\n' ;;
    # TeX's hyphenation scanner fixes the font at the first apparent letter.
    # The opening brace is a CMSY glyph whose slot happens to have the code for
    # `f`; the following text-font word must end that candidate, never be
    # reconstituted in CMSY when the narrow measure asks for hyphenation.
    fontboundary) printf '\\hsize=75pt \\linewidth=75pt \\columnwidth=75pt\n\\noindent Prefix text \\{capabilities, safety, both, neither\\} after.\n' ;;
    footnote) printf 'A line of text with a note\\footnote{The note itself.} in it.\n' ;;
    quote)   printf '%s\n\\begin{quote}\n%s\n\\end{quote}\n' "$LOREM" "$LOREM" ;;
    enum)    printf '\\begin{enumerate}\\item One\\item Two\\end{enumerate}\n' ;;
    tabular) printf '\\begin{tabular}{ll}\nA & B \\\\\nC & D \\\\\n\\end{tabular}\n' ;;
    verb)    printf 'Some \\texttt{typewriter} and \\verb|verbatim| text.\n' ;;
    # Three pages of prose. The page builder's break decisions are only visible
    # in a document long enough to make several of them, and they turn on the
    # penalties §890 leaves between the lines of a paragraph.
    manypages) for i in $(seq 1 40); do printf 'Paragraph %s: %s\n\n' "$i" "$LOREM"; done ;;
    # `\label`/`\ref` and a float, which reach the page through very different
    # machinery from ordinary text.
    ref)     printf '\\section{First}\\label{sec:a}\nText here.\n\\section{Second}\nSee section~\\ref{sec:a} on page~\\pageref{sec:a}.\n' ;;
    float)   printf '\\begin{figure}\\centering A figure body\\caption{Cap}\\end{figure}\nText after.\n' ;;
    # `\today` reads `\day`/`\month`/`\year`, which TeX takes from the system
    # clock (tex.web §241) and we take from the host. Both engines see the same
    # fixed clock here, so the two dates have to agree glyph for glyph.
    today)   printf 'Dated \\today.\n' ;;
    # `array` sets every cell in math mode, from a template that opens and
    # closes `$` across the u/v boundary. It is the most demanding thing in the
    # kernel for the alignment code.
    array)   printf 'A matrix:\n\\[ \\begin{array}{cc} a & b \\\\ c & d \\end{array} \\]\nafter it.\n' ;;
    # Nested lists, and a document long enough to space many headings: both are
    # `\addvspace`, which begins by reading `\lastskip` — and on the main
    # vertical list that is whatever the page builder last moved, not what is
    # left on the list (tex.web 424/991).
    nested)  printf '\\begin{enumerate}\\item A\\begin{itemize}\\item inner one\\item inner two\\end{itemize}\\item B\\end{enumerate}\n' ;;
    title)   printf '\\title{A Paper}\\author{A. Person}\\date{January 1970}\n\\maketitle\n\\begin{abstract}\n%s\n\\end{abstract}\n%s\n' "$LOREM" "$LOREM" ;;
    # Four independent holdout papers exposed the same 10,174sp displacement
    # in an 11pt article title.  The shared cause is microtype's interaction
    # with the centered title hbox, so keep that mechanism as one reducer.
    microtitle) printf '\\title{Common Geodesics Do Not Guarantee Fisher Consistency\\\\A Systematic Study}\n\\author{A. Person}\n\\date{}\n\\maketitle\n\\begin{abstract}\n%s\n\\end{abstract}\n%s\n' "$LOREM" "$LOREM" ;;
    # Theorem/list headings behind a hyperref anchor exposed a different
    # microtype edge walk: pdfTeX protrudes the initial T past the leading
    # penalty and nested zero-width driver box, but the otherwise similar title
    # anchor is a barrier.  Keep both shapes in one document so neither half of
    # that distinction can regress independently.
    microtheorem) printf '\\title{A Short Title}\n\\author{A. Person}\n\\date{}\n\\maketitle\n\\section{First}\n\\begin{thm}\\label{thm:first}\n%s\n\\end{thm}\n\\begin{rem}\\label{rem:first}\\hfill\n\\begin{itemize}\n\\item The first point.\n\\item The second point.\n\\end{itemize}\n\\end{rem}\nSee Theorem~\\ref{thm:first} and Remark~\\ref{rem:first}.\n' "$LOREM" ;;
    micromatrix) printf '\\section{First}\n\\begin{thm}\nLet $R=\\begin{pmatrix}0&-1\\\\1&-1\\end{pmatrix},\\, U=\\begin{pmatrix}-1&1\\\\-1&2\\end{pmatrix},\\, V=\\begin{pmatrix}2&-1\\\\3&-1\\end{pmatrix},\\, W=\\begin{pmatrix}1&0\\\\1&1\\end{pmatrix}$.\n\\end{thm}\n' ;;
    # A `\mathalpha` accent carries family 7 in its 15-bit code, which means
    # “use the current `\fam`”. A math alphabet therefore changes both the
    # nucleus and accent font in `\mathbf{\hat v}`.
    micromathaccent) printf 'The selected vector is $\\mathbf{\\hat{v}}_{i_p}$ and remains bold under its accent.\n' ;;
    microrunin) printf '\\section{The nested Hilbert scheme}\n\\subsection{A nef cone decomposition}\nFix a basis $H_1,\\dots,H_\\rho$ of $N^1(S)_\\mathbb{R}$. Every divisor class on $\\mathcal{X}=S^{[n,n+1]}$ admits a unique expression.\n' ;;
    microruninlm) printf '\\setcounter{section}{6}\\setcounter{subsection}{3}\n\\subsection{Constant modes and optimality}\nConstant-term factorization reduces the correction equation to a hyperbolic ODE; interpolation then shows that the next correction is nonzero.\n' ;;
    linequote) printf '\\fontsize{10.03749pt}{12pt}\\selectfont\n\\hsize=372pt \\linewidth=372pt \\columnwidth=372pt\n\\noindent The pattern\x27s core mechanism --- placing ``what may drift\x27\x27 and ``what must be traced\x27\x27 in different domains --- is an instance of \\textit{change isolation}, whose classical statement is that modules should be decomposed so that a change to one aspect of the system does not require changes elsewhere.\n' ;;
    # `verbatim` sets `\sfcode` inside its own group, so it catches a code
    # table that does not come back at the group end — and `\ttfamily` reads
    # `\fontdimen2\font`.
    # `equation` is `$$…\eqno\hbox{…}$$`, and LaTeX's `\eqno` is a macro that
    # defers an `\ignorespaces` to the end of the number's own group.
    mathdisp) printf 'Text.\n\\begin{equation}\n\\sum_{i=0}^{n} x_i^2 = \\frac{n(n+1)}{2}\n\\end{equation}\nMore text.\n\\[ \\lim_{x\\to0}\\frac{\\sin x}{x}=1 \\]\n' ;;
    # amsmath leaves bookkeeping material after the user's last noad. It is
    # transparent to TeX's inter-atom spacing: punctuation at the visible end
    # of either display must not acquire a trailing `\thinmuskip`, since that
    # widens the natural box and moves the whole centred formula by 1.5mu.
    amspunct) printf 'Before.\n\\[ \\left\\{x\\,\\middle|\\,\\lim_{n\\to\\infty} a_n < a\\right\\}. \\]\n\\[ \\left\\{x\\,\\middle|\\, -y < a\\right\\}. \\]\nAfter.\n' ;;
    # `\marginpar` is a `\vtop` placed by the output routine at a float
    # penalty, flattened with `\global\dp…\z@` so it takes no vertical room.
    # A table of contents: written on one pass, read back on the next, and set
    # with dot leaders. It also needs the `.aux` re-read at `\end{document}`,
    # which is where `\@writefile` fills the `.toc` in the first place.
    # Two columns. The narrow measure is what makes it interesting: a line that
    # will not fit at all is set overfull, and `tex.web` §854 still *classifies*
    # it — which is what the next line's `\adjdemerits` is measured against.
    twocol)  printf '\\twocolumn\n'; for i in $(seq 1 12); do printf 'Paragraph %s: %s\n\n' "$i" "$LOREM"; done ;;
    toc)     printf '\\tableofcontents\n\\section{First}\n%s\n\\section{Second}\n%s\n\\subsection{A sub}\n%s\n' "$LOREM" "$LOREM" "$LOREM" ;;
    marginpar) printf '%s\\marginpar{A note in the margin.}\n%s\n' "$LOREM" "$LOREM" ;;
    verbatim) printf '\\begin{verbatim}\n  int main(void) { return 0; }\n  x = y + 1;\n\\end{verbatim}\n%s\n' "$LOREM" ;;
    longdoc) for i in $(seq 1 12); do printf '\\section{Section %s}\n%s\n\n%s\n\n\\begin{itemize}\\item point one\\item point two\\end{itemize}\n' "$i" "$LOREM" "$LOREM"; done ;;
    # `book` and `report`, which `article` does not reach: `\chapter`, and with
    # it `\@makechapterhead`'s `\vspace*` (a zero-height *rule*, which is what
    # takes `\topskip` at the top of a page) and `book`'s default `headings`
    # page style, whose running heads are `\mark` nodes read back by
    # `\leftmark`/`\rightmark`. Both were broken and neither was covered.
    # Shapes real papers use that nothing here reached: theorem environments
    # and aligned display math (amsmath is in nearly every paper), a figure
    # with a caption, nested lists, a long table that breaks, and `\input` of
    # a second file — which exercises the file stack the format snapshot also
    # depends on.
    amsthm)  printf '\\begin{thm}\n%s\n\\end{thm}\n%s\n' "$LOREM" "$LOREM" ;;
    align)   printf '\\begin{align}\n a &= b + c \\\\\n   &= d\n\\end{align}\n%s\n' "$LOREM" ;;
    amseq)   printf '\\begin{equation}\\label{e}\n\\sum_{i=1}^{n} x_i^2\n\\end{equation}\n'
             printf 'See \\eqref{e}.\n%s\n' "$LOREM" ;;
    figure)  printf '\\begin{figure}[h]\n\\rule{40pt}{20pt}\n\\caption{A rule.}\\label{f}\n\\end{figure}\n'
             printf 'Figure~\\ref{f} on page~\\pageref{f}.\n%s\n' "$LOREM" ;;
    nestlist) printf '\\begin{enumerate}\\item one\n\\begin{itemize}\\item inner\\item also\n\\begin{enumerate}\\item deep\\end{enumerate}\\end{itemize}\n\\item two\\end{enumerate}\n%s\n' "$LOREM" ;;
    longtable) printf '\\begin{tabular}{lrr}\n'
             for i in $(seq 1 30); do printf 'row %s & %s & %s \\\\\n' "$i" "$((i*7))" "$((i*13))"; done
             printf '\\end{tabular}\n%s\n' "$LOREM" ;;
    # listings implements `lastline` by making the physical endline active and
    # expanding it to `\endinput`. TeX must stop before the next physical line;
    # retaining it made the active endline repeat and consumed the whole file.
    listingrange) printf '\\lstinputlisting[firstline=2,lastline=3]{listing-source.txt}\n' ;;
    # xcolor deliberately leaves its outermost initial `color push` open.
    # `\aftergroup` at TeX's bottom level is dormant; firing it immediately
    # inserts a visually significant unmatched `color pop` before the body.
    xcolorbody) printf 'Black body text.\n' ;;
    # breqn repeatedly packs and unpacks trial equation lines. pdfTeX removes
    # the old line's protrusion kerns at each `\unhbox`; retaining them moves
    # the final one-line formula even though all its glyph metrics are right.
    breqnpower) printf '\\begin{dmath}\n(1.07)^{25} = 5.427\n\\end{dmath}\n' ;;
    # hyperxmp converts pdfTeX's byte-oriented UTF-16 metadata to UTF-8 through
    # the `\pdfescapehex`/`\pdfunescapehex` string primitives. Glyph-only checks
    # cannot see a corrupt XMP packet, so this case is compared as a complete
    # semantic DVI stream below.
    hyperxmpmeta) printf 'A document with machine-readable metadata.\n' ;;
    multifile) printf 'Before.\n\\input{sub}\nAfter.\n%s\n' "$LOREM" ;;
    book|report)
      printf '\\tableofcontents\n'
      for i in 1 2 3 4 5; do
        printf '\\chapter{Chapter %s}\n' "$i"
        for j in 1 2 3; do printf '\\section{Section %s.%s}\n%s\n\n%s\n\n' "$i" "$j" "$LOREM" "$LOREM"; done
      done
      printf '\\appendix\n\\chapter{An appendix}\n%s\n' "$LOREM" ;;
  esac
}

# What a case puts in the *preamble*. `\usepackage` belongs before
# `\begin{document}`, and putting it after produces a document where LaTeX
# typesets the package name and neither engine is measuring what was intended.
case_preamble() {
  case "$1" in
    amsthm) printf '\\usepackage{amsmath,amsthm}\n\\newtheorem{thm}{Theorem}\n' ;;
    align|amseq) printf '\\usepackage{amsmath}\n' ;;
    amspunct) printf '\\usepackage{mathtools}\n\\mathtoolsset{showonlyrefs=true}\n' ;;
    microtitle) printf '\\usepackage{microtype}\n' ;;
    microtheorem) printf '\\usepackage[T1]{fontenc}\n\\usepackage{lmodern}\n\\usepackage{microtype}\n\\usepackage{amsmath,amsthm}\n\\usepackage{hyperref}\n\\setlength{\\parskip}{4pt}\n\\setlength{\\parindent}{0pt}\n\\newtheorem{thm}{Theorem}[section]\n\\theoremstyle{remark}\n\\newtheorem{rem}[thm]{Remark}\n' ;;
    micromatrix) printf '\\usepackage[T1]{fontenc}\n\\usepackage{lmodern}\n\\usepackage{microtype}\n\\usepackage{amsmath,amsthm,array}\n\\usepackage{hyperref}\n\\newtheorem{thm}{Theorem}[section]\n' ;;
    microrunin) printf '\\usepackage{amsmath,amssymb}\n\\usepackage[colorlinks]{hyperref}\n\\usepackage[final]{microtype}\n\\usepackage[T1]{fontenc}\n' ;;
    microruninlm) printf '\\usepackage[T1]{fontenc}\n\\usepackage{lmodern}\n\\usepackage[protrusion=true,expansion=false]{microtype}\n\\usepackage{amsmath,amssymb}\n' ;;
    linequote) printf '\\usepackage[T1]{fontenc}\n' ;;
    listingrange) printf '\\usepackage{listings}\n' ;;
    xcolorbody) printf '\\usepackage{xcolor}\n' ;;
    breqnpower) printf '\\usepackage[T1]{fontenc}\n\\usepackage{breqn}\n\\usepackage[protrusion=true,expansion=false]{microtype}\n' ;;
    hyperxmpmeta) printf '\\usepackage{hyperref}\n\\usepackage{hyperxmp}\n\\hypersetup{pdftitle={Metadata Test},pdfauthor={Alice Example},pdfcopyright={CC BY 4.0},pdflicenseurl={https://creativecommons.org/licenses/by/4.0/}}\n' ;;
  esac
}

case_class_options() {
  case "$1" in
    microtitle) printf '[11pt]' ;;
    microtheorem|micromatrix) printf '[12pt]' ;;
    microruninlm) printf '[11pt]' ;;
    breqnpower) printf '[12pt]' ;;
  esac
}

# Which class a case is set in. Everything is `article` unless it needs more.
case_class() {
  case "$1" in
    book|breqnpower) printf 'book' ;;
    report) printf 'report' ;;
    microrunin|microruninlm) printf 'amsart' ;;
    *)      printf 'article' ;;
  esac
}

# The position each glyph and rule is *drawn* at — horizontal and vertical
# both. dvitype prints the arithmetic (`h:=a+b=c`); we keep the answer and carry
# the vertical position forward. A `pop` restores it on a `level … (v=…)` line,
# which is just as semantic as an explicit movement and must update the cursor.
positions() {
  dvitype -output-level=4 -page-start='*' "$1" 2>/dev/null |
    sed -nE 's/^[0-9]+: .*v:=[^=]*=([0-9-]+),.*/V \1/p;
             s/^level [0-9]+:\(h=[0-9-]+,v=([0-9-]+),.*/V \1/p;
             s/^[0-9]+: (setchar[0-9]+|set[0-9]|put[0-9]|setrule|putrule) .*h:=[^=]*=([0-9-]+),.*/H \1 \2/p;
             s/^[0-9]+: (setchar[0-9]+) *$/H \1 -/p;
             s/^[0-9]+: eop.*/B/p' |
    awk '$1=="V" { v = $2; next }
         $1=="B" { print "0 --- page ---"; v=0; next }
         { print v " " $0 }'
}

CASES="${*:-hello para twopar emph math itemize section uppersection hyphen fontboundary footnote quote enum tabular verb manypages ref float today array nested title microtitle microtheorem micromatrix micromathaccent microrunin microruninlm breqnpower hyperxmpmeta linequote longdoc verbatim mathdisp amspunct marginpar toc twocol book report amsthm align amseq figure nestlist longtable listingrange xcolorbody multifile}"
# Nothing is known-divergent. `align` was, for a long time, and what it took
# is worth keeping: six separate faults, each hidden behind the one before —
# §783's repeating preamble, §1079's `\lastbox` voiding, §774/§799's
# `\everycr`, §230's scoping of token-list *parameters*, §807's interline
# parameters, and §812's handling of a `\halign` that finishes inside a
# display. The last of those is three rules in one: the rows go on the
# enclosing vertical list, the first of them is spaced by the `\baselineskip`
# from *outside* the display rather than the one `\openup\jot` has since
# changed, and the display's penalties and skips — always the long ones,
# never the short — go around the rows, with the closing `$$` contributing
# nothing at all.
#
# Overridable from the environment so a case can be looked at with
# `KNOWN_DIVERGENT=<case> tools/latex_check.sh <case>` without editing the
# harness — which is how a known divergence quietly becomes a permanent one.
KNOWN_DIVERGENT="${KNOWN_DIVERGENT-}"
fail=0; offset=0; total=0; known=0
for c in $CASES; do
  total=$((total + 1))
  # Every generated file is prefixed. A case named `hyphen` would otherwise
  # write `hyphen.tex` into the working directory and shadow TeX's own — which
  # is where the hyphenation patterns live, so every later case lost them.
  # `\input{sub}` needs the file to exist for both engines.
  printf '%s\n' "$SUBFILE" > sub.tex
  printf '%s\n' "$SUBFILE" > ref/sub.tex
  printf 'DROP-ONE\nKEEP-TWO\nKEEP-THREE\nDROP-FOUR\nDROP-FIVE\n' > listing-source.txt
  printf 'DROP-ONE\nKEEP-TWO\nKEEP-THREE\nDROP-FOUR\nDROP-FIVE\n' > ref/listing-source.txt
  f="doc-$c"
  { # A run from a format starts *past* the kernel, so it must not load it again.
    [ -z "$FMTFILE" ] && printf '\\input kernel\n'
    printf '\\documentclass%s{%s}\n' "$(case_class_options "$c")" "$(case_class "$c")"
    case_preamble "$c"
    printf '\\begin{document}\n'
    case_body "$c"
    printf '\\end{document}\n'; } > "$f.tex"
  # pdfTeX always reads the kernel as source — it has no format either. Building
  # its file by copying ours was wrong the moment ours stopped loading the
  # kernel: the reference run then had no LaTeX at all.
  { printf '\\input kernel\n'
    printf '\\documentclass%s{%s}\n' "$(case_class_options "$c")" "$(case_class "$c")"
    case_preamble "$c"
    printf '\\begin{document}\n'
    case_body "$c"
    printf '\\end{document}\n'; } > "ref/$f.tex"
  # The `.toc` accumulates too: `\@starttoc` appends, so a leftover from an
  # earlier configuration is read back and set twice.
  rm -f "$f.aux" "$f.toc" "ref/$f.aux" "ref/$f.toc"

  # Twice, as a real LaTeX run is: the first pass writes the `.aux` (and any
  # `.toc`), the second reads it back. `\ref`, `\pageref` and every page number
  # in a table of contents are only right on the second.
  for pass in 1 2; do
    # A run from a format must fit in the *default* budgets: the caps are
    # per-run anti-hang valves, and a restored engine that starts with the
    # kernel's step count already on the clock trips them before executing
    # anything — which truncates the document silently. Raising them here is
    # what hid that for a whole afternoon, so format runs get no help.
    if [ -n "$FMTFILE" ]; then
      AITEX_DVI="${f}_o.dvi" "$FAST" pass "$f.tex" --fmt "$FMTFILE" \
        ${CACHEDIR:+--cache "$CACHEDIR"} > "$c.ours" 2>&1
    else
      AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
        AITEX_DVI="${f}_o.dvi" "$FAST" pass "$f.tex" --ini > "$c.ours" 2>&1
    fi
    ( cd ref && timeout 300 pdftex -ini -etex -interaction=nonstopmode -output-format=dvi "$f.tex" \
      </dev/null ) > "$c.ref" 2>&1
  done

  # `tex.web` §642's wording, which our closing line now uses:
  #   Output written on x.dvi (3 pages, 1234 bytes).
  op=$(sed -nE 's/^Output written on .* \(([0-9]+) page.*/\1/p' "$c.ours" | head -1)
  rp=$(sed -nE 's/.*\(([0-9]+) pages?,.*/\1/p' "$c.ref" | head -1)
  und=$(sed -nE 's/.*undef=([0-9]+).*/\1/p' "$c.ours" | head -1)

  positions "${f}_o.dvi" > "$c.ov"
  positions "ref/$f.dvi" > "$c.rv"
  semantic_ok=1
  if { [ "$c" = xcolorbody ] || [ "$c" = hyperxmpmeta ]; } &&
     ! "$ROOT/tools/dvi_semantic.sh" "ref/$f.dvi" "${f}_o.dvi" > "$c.semantic"; then
    semantic_ok=0
  fi
  # A purely *vertical* disagreement is reported separately from a real one: the
  # glyphs and their horizontal positions still have to match exactly, and
  # saying how many sit at a different height keeps the one known defect from
  # hiding others. See docs/HANDOFF-fast-engine.md.
  off=$(paste "$c.rv" "$c.ov" |
        awk 'NF==0 { next }
             bad { next }
             { if ($2 != $6 || $3 != $7 || $4 != $8) { bad = 1; next }
               d = $5 - $1; if (d < 0) d = -d
               if (d > 0) { n++; if (d > worst) worst = d } }
             END { if (bad) print "MIXED"; else printf "%d/%d", n+0, NR }')

  if [ "${op:-0}" = "${rp:-1}" ] && [ "${und:-1}" = "0" ] &&
     [ "$semantic_ok" = 1 ] && cmp -s "$c.ov" "$c.rv"; then
    printf '  ok      %-9s %4s glyph positions, %s page(s)\n' "$c" "$(grep -c . "$c.rv")" "${rp:-?}"
  elif [ "${op:-0}" = "${rp:-1}" ] && [ "${und:-1}" = "0" ] &&
       [ "$semantic_ok" = 1 ] && [ "$off" != "MIXED" ]; then
    offset=$((offset + 1))
    printf '  vshift  %-9s %4s glyph positions, %s page(s) — %s sit at a different height\n' \
      "$c" "$(grep -c . "$c.rv")" "${rp:-?}" "$off"
  elif echo " $KNOWN_DIVERGENT " | grep -q " $c "; then
    known=$((known + 1))
    printf '  KNOWN   %-9s differs — see the note beside KNOWN_DIVERGENT\n' "$c"
  else
    fail=$((fail + 1))
    printf '  FAIL    %-9s pages ours=%s ref=%s undef=%s\n' "$c" "${op:-?}" "${rp:-?}" "${und:-?}"
    [ "$semantic_ok" = 1 ] || sed 's/^/          /' "$c.semantic"
    # DIFFLINES=n to see more of it; eight is enough to recognise a failure
    # and never enough to diagnose one.
    diff "$c.rv" "$c.ov" | head -"${DIFFLINES:-8}" | sed 's/^/          /'
  fi
done

echo
if [ "$fail" = 0 ] && [ "$offset" = 0 ]; then
  if [ "$known" != 0 ]; then
    echo "a LaTeX document sets where pdftex sets it — $((total - known))/$total cases ($known known-divergent)"
  else
    echo "a LaTeX document sets where pdftex sets it — $total/$total cases"
  fi
elif [ "$fail" = 0 ]; then
  echo "$((total - offset))/$total exact; $offset differ only in vertical position"
  exit 1
else
  echo "$fail of $total cases differ"
  exit 1
fi
