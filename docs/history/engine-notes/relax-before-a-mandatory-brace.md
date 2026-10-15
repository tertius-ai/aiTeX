# A `\relax` before a mandatory brace is skipped, not tolerated

`\hbox\relax{AB}` came out 8.75002pt wide where TeX gives 14.58337pt, with a
"Missing { inserted" beside it. The rule is §403:

```text
procedure scan_left_brace; {reads a mandatory |left_brace|}
begin @<Get the next non-blank non-relax non-call token@>;
if cur_cmd<>left_brace then
  begin print_err("Missing { inserted");
...
@ @<Get the next non-blank non-relax non-call token@>=
repeat get_x_token;
until (cur_cmd<>spacer)and(cur_cmd<>relax)
```

§645 `scan_spec` ends with `scan_left_brace`, so every box constructor inherits
it, and §1084 `scan_box` opens with the same three-way skip. We had a correct
`scan_left_brace`, but `\hbox`/`\vbox`/`\vtop` never called it — they did their
own peek through `next_nonspace_x`, which skips spaces only. The engine already
had `next_nonspace_nonrelax_x`; the box path just was not using it.

## Why it matters far more than it looks

expl3 does not write `\relax` by accident. Its box constructors *depend* on the
skip:

```text
\hbox:n #1 -> \tex_hbox:D \scan_stop: {\color_group_begin: #1 \color_group_end: }
\__box_dim_eval:n #1 -> \__box_dim_eval:w #1 \scan_stop:
```

`\scan_stop:` is `\relax` under another name. So hyperref's anchor —
`\box_move_up:nn {\normalbaselineskip} {\hbox:n {…}}` — reaches the engine as

```text
\raise \dimexpr \normalbaselineskip \scan_stop: \hbox \scan_stop: {…}
```

with a `\relax` in *both* mandatory positions. Ours reported "Missing {
inserted", opened the box one token early, and set the real `{…}` as an
ordinary nested group inside it.

## How it was found

`2608.17397v1` differed from pdfTeX by exactly one character in 23237 — the
kind of gap that is usually a rounding disagreement. It was not. Working back:

1. `logdiff.sh … text` put the first divergence at character 3741: we
   hyphenated `sta-ble`, pdfTeX hyphenated `knowl-edge`, and pdfTeX fitted one
   more word on page 2.
2. `TRACE=paragraphs logdiff.sh … par` showed the first badness disagreement,
   `b=15 d=625` against our `b=13 d=529`, on the line beginning `Theorem 1.1.`
3. Immediately above it in our log — and nowhere in pdfTeX's — was `! Missing {
   inserted` with `<to be read again> \scan_stop:`. An error, not a rounding
   difference; the inserted token changed the line's width and with it every
   break after it.
4. Bisecting the body found the trigger was `\begin{theorem}`, and bisecting
   the preamble found it needed **hyperref**. Eight lines reproduce it.

Worth noting for next time: `logdiff.sh` in its default `events` mode reported
"identical for all 0 events" on a file that was raising this error. The error is
not one of the events it extracts, so a clean `events` run does not mean a clean
document.

## Also seen, not fixed

While tracing, `\exp_end_continue_f:w`'s body printed as a raw NUL byte where
pdfTeX prints `` `^^@ ``. §49's `^^X` notation is not reaching macro-body
display. Cosmetic, but it is a transcript difference on every expl3 document.

There is no `scan_box` in this engine at all: `\raise`/`\lower`/`\moveleft`/
`\moveright` and `\setbox<n>=` park a pending value and return to the main
loop, where `\relax` happens to be a no-op. So they behave correctly for
`\relax` by accident, but never produce §1084's "A <box> was supposed to be
here" — `ErrorKind::BoxExpected` is declared and never reported. `\vcenter`
likewise has no brace check at all.
