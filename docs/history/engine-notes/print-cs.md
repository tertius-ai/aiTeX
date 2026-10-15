# §262 `print_cs`: the escape and the trailing space

`print_cs` is not "backslash, then the name". It has three shapes, and the
trailing space is part of the rendering:

```text
procedure print_cs(@!p:integer); {prints a purported control sequence}
begin if p<hash_base then {single character}
  if p>=single_base then
    if p=null_cs then
      begin print_esc("csname"); print_esc("endcsname"); print_char(" ");
      end
    else  begin print_esc(p-single_base);
      if cat_code(p-single_base)=letter then print_char(" ");
      end
  else if p<active_base then print_esc("IMPOSSIBLE.")
  else print(p-active_base)
else if p>=undefined_control_sequence then print_esc("IMPOSSIBLE.")
else if (text(p)<0)or(text(p)>=str_ptr) then print_esc("NONEXISTENT.")
else  begin print_esc(text(p));
  print_char(" ");
  end;
end;
```

- **active character** — `print(p-active_base)`: the bare character. No escape,
  no space.
- **single-character control sequence** — the escape, then a space *only if that
  character's catcode is currently `letter`*.
- **control word** — the escape, then always a space.
- **the null control sequence** (`\csname\endcsname`) — printed as the two
  primitives that build it, then a space.

`sprint_cs` (§263) is the same minus the error checks and minus every trailing
space. It is what `\string` and `\csname` echoing use.

`print_cs` reaches a lot of output: `show_token_list` (§294) is built on it, and
that backs `\tracingmacros`, `\tracingcommands`, `\message`, `\meaning`,
`\detokenize`, `\show`, error contexts, and `\write` — §1370 `write_out` calls
`token_show`.

## Three ways we had it wrong

**The letter test is a live `\catcode` question, not an alphabet question.** We
asked `char::is_ascii_alphabetic`. The two disagree the moment a document
touches `\catcode`, which LaTeX does on essentially every line of every package:
`\makeatletter` makes `\@` a control *word* for printing purposes, so
`\detokenize{\@}` gains a trailing space, and demoting a letter to `other` takes
the space back off. This one is now `Fast::cs_takes_space`, shared by the three
sites that need it.

**The null control sequence printed as a bare escape character.** Its interned
name is empty, so `name.chars().count() != 1` was true and it took the
control-word path: `\ ` where TeX writes `\csname\endcsname `.

**An active character got a trailing space in `\tracingmacros`.** We intern an
active character's name behind `ACTIVE_MARK`, so the raw string is two
characters long and every active character looked like a control word.
`esc_name` already stripped the marker, so only the space was wrong:

```text
ref    <lead byte>->\UTFviii@two@octets <lead byte>
ours   <lead byte> ->\UTFviii@two@octets <lead byte>
```

Every UTF-8 document traces through `\UTFviii@two@octets`, so this was one
wrong line per non-ASCII character in any traced expansion. Found while diffing
a `\tracingmacros` run of `\pdfstringdef` — see
[ifcat-let-to-character.md](ifcat-let-to-character.md).

## Still not right

Four more sites render a cs name with a hardcoded `\` and an unconditional
space, ignoring `\escapechar` and the catcode rule: `arena_str`
(`fast.rs:3998`), `tok_slice_str` (`fast.rs:4042`), and `macros::show`
(`macros.rs:1093`). `do_show` papers over the space with a
`strip_suffix(' ')`. These feed `\show` and error contexts rather than
`\write`/`\message`, so they have not shown up in corpus diffs yet.

`ACTIVE_MARK` also leaks into `detok_one`, `do_string` and `detok_arena_into`
for a token built as `Token::ControlSequence(active_sym(c))` — which happens for
math-active characters (`\mathcode="8000`, `fast.rs:16614` and `fast.rs:21555`).
An active character reached the ordinary way is a `TokKind::Char(c, Active)` and
prints correctly, which is why the battery does not catch it.
