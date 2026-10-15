# `\input` is expandable, and `.tex` is a default in two places

Our TRIP transcript stopped 893 lines before TeX's. A structural diff of the two
logs put the whole loss in one block: our log simply ends where TeX carries on.
Three separate defects were stacked behind it.

## 1. `\input` is expandable

```text
@d max_command=100 {the largest command code seen at |big_switch|}
...
@d input=max_command+4 {input a source file ( \input, \endinput )}
```

§210 says of everything above that line that these commands "cannot get through
TeX's scanner to the main control routine". We dispatched `\input` from
`main_control`, so `expand_one` — the one-step expander behind `\expandafter`
and `\expanded` — backed the token up unexpanded instead of running it.

§366 is where that matters:

```text
expand_after: begin get_token; t:=cur_tok; get_token;
  if cur_cmd>max_command then expand @+else back_input;
  cur_tok:=t; back_input;
  end;
```

B is expanded **first**, and only then is A backed up — on top of whatever B
left behind. So `\expandafter\stopinput\input tripos` opens `tripos` and puts
`\stopinput` at the head of the *new file*. Backing `\input` up unexpanded
reverses the two, and `\stopinput` runs before the file opens:

```text
ref    (./b.tex (./sub.tex <STOP> <INSUB>
ours   (./b.tex <STOP> (./sub.tex <INSUB>
```

TRIP line 419 defines `\def\stopinput{\error\let\input\die}`, so the `\let`
meant to fire inside `tripos` fired outside it — or, once the group bug below
is accounted for, not at all.

A side effect worth knowing: §367 traces expandable non-macros when
`\tracingcommands > 1`, which is why `{\input}` appears in trip.log at all. It
looks exactly like a `main_control` trace and is not one.

## 2. `\input` appends `.tex` and does not try the bare name

```text
begin scan_file_name; {set |cur_name| to desired file name}
if cur_ext="" then cur_ext:=".tex";
pack_cur_name;
```

`resolve_path` tried the bare name first and the extended name second. That is
not equivalent even when both files exist and hold the same bytes, because §537
echoes the name it opened: `\input tripos` logged `(./tripos` against TeX's
`(./tripos.tex`. §515 sets `cur_ext` from a `.` in the scanned name, so `\input
foo.bar` keeps `.bar` and gets no `.tex`.

The bare name is still tried, but behind the extended one — it is now reached
only where TeX would have found nothing and prompted.

## 3. `\openout` appends `.tex` too, at open time

This is what actually created the file above. TRIP writes `tripos` itself:

```tex
\openout10=tripos
```

and §1375 gives it the same default:

```text
cur_name:=open_name(p); cur_area:=open_area(p); cur_ext:=open_ext(p);
if cur_ext="" then cur_ext:=".tex";
pack_cur_name;
```

The default belongs at open time, not in the whatsit: §1356 displays the name
as it was *scanned*, so both engines trace `.\openout10=tripos` while the file
on disk is `tripos.tex`. We wrote a bare `tripos`, so `\input tripos` had
nothing with the right name to find.

## What is still wrong

`\stopinput` is defined on TRIP line 419 and never restored in TeX's run. Ours
restores it:

```text
{restoring \rb=undefined}
{restoring \lb=undefined}
{restoring \stopinput=undefined}
{restoring \everycr=}
{restoring \toks256=}
{restoring \pretolerance=0}
{restoring \a=macro:#1->…}
```

— everything set between lines 417 and 420, unwound at once. So we close a
group on line 420 or 421 that TeX does not. The divergence starts one command
earlier, at line 420's `\halign\relax{\span\iffalse}\fi\cr#&\ifnum0=`{\fi\cr\cr}`:

```text
ref    {\halign} {internal vertical mode: \iffalse} {false} \everycr->\noalign {\penalty 97}
ours   {\halign} {math shift character $}
```

That is TRIP's alignment torture, and a thread of its own. Until it is fixed,
line 424 sees `\stopinput` undefined and the three fixes above cannot show
their full effect on the transcript.
