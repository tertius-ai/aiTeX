# §509's second test, and the `\or` that never stopped arriving

`tex.web` §509 skips to the case an `\ifcase` selected:

    while n<>0 do
      begin pass_text;
      if cond_ptr=save_cond_ptr then
        if cur_chr=or_code then decr(n)
        else goto common_ending
      else if cur_chr=fi_code then @<Pop the condition stack@>;
      end;
    change_if_limit(or_code,save_cond_ptr);

Two independent tests live in that loop and it is easy to see only one.

`pass_text` steps over whole `\if…\fi` groups met *inside* the text being
skipped — that is its own `l` counter, and we had it.

`cond_ptr = save_cond_ptr` asks something different: **is this `\ifcase`'s node
still the innermost one?** It is not, whenever a conditional opened *while the
case number was being scanned* has yet to see its `\fi`. §498 pushes the
condition node before scanning the operand, so anything the scan expands nests
inside — and a conditional that has merely chosen a branch is still open.

Without that test, a nested conditional's `\fi` is taken for the `\ifcase`'s.
Worse, the node then popped is whichever is on top, not the `\ifcase`'s own, so:

- the nested conditional loses its node, and
- the `\ifcase`'s node stays at `if_code`.

`if_code` is the state §510 reads as *"condition not yet evaluated"*:

    if cur_chr>if_limit then
      if if_limit=if_code then insert_relax {condition not yet evaluated}

so from then on **every** `\or` anywhere is put back with a `\relax` in front of
it (§379 `insert_relax`) — and put back again, and again.

## XY-pic

    \xydef@\rectangleEdge#1{\ifcase#1\expandafter\rectangleEdge@
     \or \expandafter\rectangleUnder@ \or \expandafter\rectangleDist@
     \or \expandafter\rectangleProp@
     \else \relax \fi}

`\ifcase#1\expandafter…` is the shape. Scanning `#1` looks ahead for another
digit, which expands `\expandafter`; that expands the `\or` (§510 correctly puts
it back, first time) and then `\rectangleEdge@`, which is a chain of `\ifdim`s.
Four of them were still open when the number scan ended:

    cond PUSH IfCase id=2798936 base=0
    cond PUSH IfDim  id=2798937 base=0
    cond enter(skip-else)   -> depth 1 nodes 2 [2798936:1 2798937:2]
    cond PUSH IfDim  id=2798938 base=1
    cond enter(true-branch) -> depth 2 nodes 3 [2798936:1 2798937:2 2798938:3]
    cond PUSH IfDim  id=2798939 base=2
    cond enter(skip-else)   -> depth 3 nodes 4 [… 2798939:2]
    cond PUSH IfDim  id=2798940 base=3
    cond enter(skip-else)   -> depth 4 nodes 5 [… 2798940:2]
    cond EVALPOP ifcase id=2798936 n=4
    cond discard(ifcase-no-case) -> depth 4 nodes 4 [2798936:1 2798937:2 2798938:3 2798939:2]

The last line is the bug in one place: the `\ifcase` resolved, and the node that
went was 2798940. 2798936 stayed at limit `1` for the rest of the run.

    \xymatrix{A \ar@{-->}[r] & B}

is enough — 140,840,925 expansions of `\or` out of 300M steps. A diagonal arrow
(`\ar[dr]`) does it too; `\ar[r]` and `\ar[r]^{\sim}` do not.

2608.17821v1: **21p/29982c → 44p/61256c** against pdfTeX's 46p/66923c.

## The same fix already existed, twenty lines up

The *false-branch* skip had this exact guard, with a comment recording what it
cost to learn:

> `cond_depth` counts only the conditionals whose branch has been *entered*, so
> an operand scan that leaves two nodes at `if_code` makes `pending` name a
> level that does not correspond to those nodes, and the skip then pops whatever
> is on top […] That is how `\AddToHook` on a hook holding an arguments-using
> chunk dropped the `\ifx\@currname\@empty` that LaTeX wraps around a whole
> package file, and hyperref's load ended in `! Extra \fi.`

`do_ifcase` was simply never given the same treatment. It now uses the same
`pass_text()` and `pop_the_condition()` helpers as that loop, so there is one
implementation of the rule rather than two.

Worth remembering as a shape: when two code paths implement the same section of
`tex.web`, a fix to one is a fix owed to the other.

## Finding it

1. `AITEX_HOT_CS=1` — a spin puts its own body at the top of the ranking, and
   `\or` at 140M against 97k for the runner-up is not ambiguous.
2. `AITEX_STEP_TRACE` / `AITEX_TOK_TRACE` for the cycle: `\relax \or \relax \or`
   with the arena growing two tokens a turn, which is `insert_relax` exactly.
3. `AITEX_DBG_SEL=1` for the condition stack. It now prints each node as
   `id:limit` rather than only a count — the count says the stack moved, the
   limits say a node was left at `if_code`, and only the second is the bug.

## Negative results

- **The idiom alone is fine.** Reduced to plain TeX and run against `tex -ini`:

      \def\sel#1{\ifcase#1\expandafter\A\or \expandafter\B\or \expandafter\C\else \relax\fi}

  Ours and TeX both print `<sel0=[A]\relax >`, `<sel1=[B]>`, `<sel4=\relax >`.
  The stray-looking `\relax` in the first is what TeX does — not a bug.
- **Not an orphaned `cond_enter`.** It logs `ORPHAN` under `AITEX_DBG_SEL` when
  no node matches its id; zero hits in the failing run. `cond_enter` searches by
  id and was always right; `cond_discard` and `cond_pop` take the top, and that
  is where the asymmetry was.
- **Not `do_ifcase`'s early returns.** Both go through `skip_guard(t, true)`,
  which sets the top node to `fi_code`.

A methodological one: the first attempt to show the new battery case had teeth
neutralised the guard in the *false-branch* skip instead of `do_ifcase`'s —
both lines are textually identical — and the case passed, which read as "no
teeth". Neutralising the right one produces no output at all. When two sites
share a line, patch by line number or by surrounding context, not by the line.
