# `#{` and `align_state`: §400's correction, and the three ways to reach it

`tex.web` §400 states the rule and the reason outright:

> A slightly subtle point arises here: When the parameter delimiter ends with
> `\#\{`, the token list will have a left brace both before and after the
> `end_match`. Only one of these should affect the `align_state`, but both will
> be scanned, so we must make a correction.

    @<Advance r; goto found if the parameter delimiter has been fully...@>=
    begin r:=link(r);
    if (info(r)>=match_token)and(info(r)<=end_match_token) then
      begin if cur_tok<left_brace_limit then decr(align_state);
      goto found;
      end
    else goto continue;
    end

§476 puts the `{` of a `#{` parameter text in two places: it delimits the
argument, and it is re-inserted with the body. Both copies get scanned, so
without the correction one brace raises `align_state` twice and its `}` lowers
it once.

Nothing shows until the macro runs inside an alignment, and then §342's

    if cur_cmd<=car_ret then if cur_cmd>=tab_mark then
      if align_state=0 then
      @<Insert the <v_j> template and goto restart@>

never fires. The `\cr` is no longer a row boundary; it reaches `main_control`,
and whatever `\egroup`s follow close a group too many.

## Where the correction goes

The correction is easy to under-apply, because there is only one *rule* but
three *code paths* that consume the brace. TeX has no such split — §400 sits in
the delimiter-matching loop, which every delimited parameter goes through — so
the shape of our `parse_params` output is what multiplies it:

| parameter text     | how the `{` is consumed                        |
|--------------------|------------------------------------------------|
| `\def\hb#{…}`      | `Param::BraceLiteral` standalone               |
| `\def\hd#1#{…}`    | `Param::Arg` + `BraceLiteral`, inline loop     |
| `\def\hg#1x#{…}`   | `Param::Arg` + literal run, `read_delimited`   |

Only the first was corrected at first (commit `626b612`). That was enough for
hyperref's

    \def\Hy@href#{\hyper@normalise\href@}

and for nothing else. The fix now lives where §400 puts it — at the point the
delimiter is fully matched, inside `read_delimited` — plus the one inline loop
that does its own matching.

A `{` can only ever be the *last* token of a delimiter, because §476 ends the
parameter text at one. So "the delimiter just completed and the token that
completed it is a left brace" is exactly "this is a `#{` parameter text", and no
other delimited macro reaches the correction.

## The two papers

**2608.17815v1** took the first path. An `\href` inside `\author` expanded
`\Hy@href`, and the rest of the `tabular` that `\maketitle` builds ran one brace
deep. 14 pages became 0.

**2608.15339v1** took the second, and it is the more alarming one because no
package in the document writes a `#{` macro at all. LaTeX's `\@yargd@f` — the
kernel macro that gives a font command its arguments — builds

    \def\reserved@a#15#2#{…}

so *any* font-shape file read inside an alignment drifts. The reduction is four
lines and mentions neither hyperref nor an alignment:

    \documentclass{article}
    \usepackage[T1]{fontenc}
    \title{A}\author{\texttt{q}}
    \begin{document}\maketitle\end{document}

`\texttt` under T1 has to load `T1+cmtt`, that reads `t1cmtt.fd`, the `.fd` file
runs `\DeclareFontShape` through `\@yargd@f`, and `\endtabular`'s `\crcr`
arrives at `align_state=1`. 34 pages became 0.

Drop the `[T1]`, and OT1's `cmtt` is already known, no file is read, and the
document is fine — which is why this looked like a font-encoding bug and not an
alignment one for the first hour.

## AITEX_ALIGN_TRACE

Three bugs have now come down to `align_state` being off by one at a `\cr`, and
each was found the same way. `cell_boundary_here` is silent about the near
misses, which are the whole story: a `\cr` at `align_state=1` simply *is not* a
boundary, and nothing anywhere says so.

    AITEX_ALIGN_TRACE=1        every tab mark and every \cr an open alignment
                               sees, with the align_state §342 judged it at
    AITEX_ALIGN_TRACE=braces   …and every brace scanned, with the running count

The recipe:

1. `AITEX_ALIGN_TRACE=1` and find the `\cr` that says `not a boundary`.
2. `AITEX_ALIGN_TRACE=braces AITEX_TRACE_CAP=200000`, then walk back from that
   step to the last line where the count was 0. The `{` on the next line is the
   one that was counted twice — or the `}` that never came.
3. `AITEX_TOK_TRACE=<step-70>,<step+50>` to see what was being expanded there.

Step 2 landed on `{` at step 855969 out of ~868000, between a `\def\reserved@a`
and a `\DeclareFontShape`, which named the bug outright.

**Rebuild the format after every engine change.** The format is keyed to the
engine build, and a mismatch kills the run *before* it writes a `.log`, leaving
the previous run's log in place — which reads exactly like "the change did
nothing". That is `tools/mkfmt.sh`, and it has now cost three cycles anyway.

## Negative results

- Reducing to `\author{B\\ C}` under neurips_2026.sty and hyperref does **not**
  reproduce. Neither the class, nor `\\`, nor `\href` is necessary; all three
  were in the failing document and all three are red herrings.
- `xurl`, `microtype`, `booktabs`/`longtable`/`array` and `\hypersetup` are
  likewise not involved, though the failing preamble loads all of them.
- A plain `\input` inside an alignment cell is fine — it is not file reading
  that drifts, it is what `t1cmtt.fd` happens to run.
