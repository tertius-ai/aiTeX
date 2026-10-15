# The output routine's mode, and the hang it caused

`tex.web` §1025 fires the output routine, and the mode is not negotiable:

    @<Fire up the user's output routine and |return|@>=
    begin output_active:=true;
    incr(dead_cycles);
    push_nest; mode:=-vmode; prev_depth:=ignore_depth; mode_line:=-line;
    begin_token_list(output_routine,output_text);
    new_save_level(output_group); normal_paragraph;
    scan_left_brace;
    return;
    end

`mode:=-vmode` — internal vertical mode, flatly, whatever mode the page builder
interrupted to get here.

We run the routine on the outer list, saving `prev_depth` and `prev_graf`,
rather than opening a box for it. That is fine as far as it goes, but the
`push_nest` we skip is the only thing that would have replaced the mode, so
`mode_test` was reporting the *interrupted* mode:

    None if self.output_active => (self.mode, true),   // wrong
    None if self.output_active => (Mode::Vertical, true),

## Why it hid for so long

The page builder is normally exercised between paragraphs, where the mode is
vertical anyway — so `self.mode` and `-vmode` agree and nothing shows. It takes
something that fires the output routine from *inside* a paragraph to separate
them, and a display is the obvious one: §1145 breaks the paragraph, contributes
its lines, and the builder runs with the paragraph still open.

## The hang

    \documentclass{article}
    \begin{document}
    Hello.
    \begin{figure}\rule{1pt}{1pt}\end{figure}
    $$ x=1 $$
    \end{document}

Five lines, no packages, and we never shipped a page. Inside `\output`,
`\ifhmode` was true, so when LaTeX's `\@makecol` reached its `\unvbox`, §1094's
`head_for_vmode` took the `mode>0` branch:

    procedure head_for_vmode;
    begin if mode<0 then
      if cur_cmd<>hrule then off_save
      else …
    else  begin back_input; cur_tok:=par_token; back_input; token_type:=inserted;
      end;
    end;

— put the `\unvbox` back and inserted a `\par` in front of it. LaTeX's `\par` is

    \mode_if_inner:F { … \tex_par:D … }

and `\ifinner` *was* true, so `\use_none:n` threw the body away and the `\par`
did nothing at all. The `\unvbox` came round again, and again, at about 179
steps a turn, until the step cap killed the run.

Both halves had to be wrong together for this to loop: hmode sent
`head_for_vmode` down the paragraph branch, and inner made the `\par` it
inserted a no-op. With the mode reported as vertical, `head_for_vmode` is never
reached and `\unvbox` is executed where it stands, which is what it is for.

2608.17233v1: **1 page of 8 → 8p/15518c, exactly pdfTeX's page and character
counts.** (Positions still differ, so the corpus gate still fails it — see
[[2608.17233v1]] if that gets its own note.)

## What it looked like from outside

Nothing. Zero errors, a transcript that ends after `[1]` with no complaint, and
a `.log` indistinguishable from a document that simply stopped early. It was
recorded as "truncates to 1 page" for two iterations.

The tell is on **stderr**, not in the log:

    steps=2000000003 cap=true

Read that line and the error count before opening the `.log`. Of the ten
projects then shipping far fewer pages than pdfTeX, exactly two were hangs
(this one and 2608.17821v1, which has a different cause and still hangs); the
other eight stop at the 100-error limit and are ordinary parse failures. Those
two groups need completely different work and the transcripts do not tell them
apart.

## Finding it

`AITEX_HOT_CS` is the tool for a spin — it ranks control sequences by expansion
count, and a loop puts its own body at the top:

    107629  \par
    107631  \mode_if_horizontal:TF
    107627  \mode_if_inner:F

Then `AITEX_STEP_TRACE=lo,hi` for the cycle order and `AITEX_TOK_TRACE=lo,hi`
for the tokens between two turns of it, which is where the `\unvbox \par` pair
showed up.

## Negative results

Four hypotheses died on the way, all of them plausible:

- **LaTeX's float loop.** The stack shows
  `\@whilesw\if@fcolmade\fi{\@opcol\@startcolumn}` and it is tempting to stop
  there. Everything after a frame's `▶` is *pending*, not executing: a
  `\message` in a redefined `\@startcolumn` never printed once.
- **The page builder re-firing.** It fires twice and stops. `AITEX_TRACE_PAGE`
  had 191 unused entry slots, so the silence was real.
- **Dead cycles.** `\deadcycles` is maintained and raises TeX's "Output loop"
  at `\maxdeadcycles`; it cannot fire, because the output routine is entered
  once and never left.
- **`\lastnodetype`.** LaTeX's `para/end` hook branches on `11=\lastnodetype`
  and inserts `\hskip`, which would re-arm the hook. Measured against
  pdflatex with `\AddToHook{para/end}{\message{[LNT=\the\lastnodetype]}}`:
  both engines say 1.

That last probe is what actually broke it open, by accident — not through the
value, which agreed, but through the *count*. pdflatex printed the message
twice and we printed it once, which said the spin was between the two
paragraphs and did not itself run the hook.
