# An alignment inside an alignment, and where §774 saves `align_state`

`\begin{matrix}` inside an `align*` raised `! Misplaced \omit.` and a cascade
behind it. Every alignment nested inside another alignment failed the same way:

| | |
|---|---|
| matrix / pmatrix / array / cases in `align*` | fail |
| matrix in matrix | fail |
| cases in `gather*` | fail |
| cases in `equation*` or `$$` | ok — not an alignment |
| `align*` alone, `cases` alone | ok |

A matrix inside an `align` is ubiquitous in mathematical papers.
`2608.17265v1` shipped zero pages against pdfTeX's eight.

## The rule

§774 saves `align_state` and drops it to −1000000 **before** `scan_spec` runs,
and `scan_spec` is what reads `to`/`spread` and the opening brace:

```text
push_alignment; align_state:=-1000000;
@<Scan the preamble and record it in the |preamble| list@>;
```

The order is not decoration. `scan_keyword` looks one token ahead and puts it
back when it does not match, and §325's `back_input` *decrements* `align_state`
for a `{`:

```text
if cur_tok<right_brace_limit then
  if cur_tok<left_brace_limit then decr(align_state)
  else incr(align_state);
```

In TeX that dip happens down at −1000000 and nobody can see it. We scanned the
keywords first and saved afterwards, which captured the dip: the `{` that
`scan_keyword("spread")` had pushed back was still pending, so the saved value
was **−1** where it should have been 0.

§800 hands that −1 back to the enclosing alignment when the inner one ends, and
§342's test then never fires:

```text
if cur_cmd<=car_ret then if cur_cmd>=tab_mark then
  if align_state=0 then
  @<Insert the <v_j> template and goto restart@>
```

So the outer row's `&` stopped being a cell boundary and amsmath's
`\math@cr@@@ -> &\omit …` arrived at `main_control`.

The comment beside the old code already said it saved "before §774 `scan_spec`
reads the `{`" — and it did save before the brace, just not before the keyword
lookahead that had already touched the counter. Reading the comment instead of
the order cost two iterations.

## How it was found

Analysis kept producing plausible wrong answers. What worked was
`#[track_caller]` on `back` — and on `back_x` too, since one attribute only
names the immediate caller and `back_x` is a dispatcher. That attributed all
eight offending back-ups to `scan_keyword`'s failure path. Logging the keyword
and the token then showed

```text
scan { d=1 -> 1
scan } d=-1 -> 0
kw "to" fail tt={ align_state=0
back { d=-1 -> -1
```

a `{` backed up between a balanced scan pair — which is normal, and harmless in
TeX, and only fatal here because the save sat in the middle of it.

## Three things that are not the cause

Recorded because each looked right:

1. **§342's counter being conditional.** Both the scan side and the back side
   guard on `!aligns.is_empty()`, which can change between a scan and its
   matching back. Making it unconditional is faithful and changes nothing here.
2. **`align_delta` being lexical.** §342 keys off `cur_cmd`, so `\bgroup` —
   `\let` to `{` — should raise the counter. Making it meaning-aware makes
   things *worse*: `\bgroup` is already counted somewhere and it double-counts.
3. **§324's unguarded u-template reset.** TeX resets only `if
   align_state>500000`, else it is the "interwoven alignment preambles" fatal
   error; we reset unconditionally. Faithful to fix, but it does not fire here.
