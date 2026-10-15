# `\accent` runs `do_assignments` too, and we do not

`tex.web` §1123 `make_accent`:

    begin scan_char_num; f:=cur_font; p:=new_character(f,cur_val);
    if p<>null then
      begin x:=x_height(f); s:=slant(f)/float_constant(65536);
      a:=char_width(f,char_info(f)(character(p)));
      do_assignments;
      @<Create a character node |q| for the next character,
        but set |q:=null| if problems arise@>;

The accent's own character is made first, then **assignments are read**, and
only then the accented character.  That is how a document changes font between
the two — and it is the other half of §1241's help text:

    Sorry, \setbox is not allowed after \halign in a display,
    or between \accent and an accented character.

§1241's guard landed for the display half (`display_align_pending`).  The
accent half is still open.  Verified against `tex -ini`:

    A\accent`A \setbox3\hbox{w}B
    \par\showbox3

    tex:   ! Improper \setbox.
           Sorry, \setbox is not allowed after \halign in a display,
           > \box3=void
    ours:  > \box3=   (the box was filled)

## Why it is not a one-line fix

Our `Prim::Accent` arm reads the accented character itself, with
`next_nonspace_x`, inside the arm.  TeX reaches it through a `do_assignments`
loop that goes back out to the dispatcher.  To match, the arm has to *defer*
the character the way `display_align_pending` defers the `$`: set a flag, let
`main_control` run the assignment run, and pick the character up when the run
ends.  The `set_box_allowed` guard then reads the same flag.

Worth doing when the accent path is next opened; the shape to copy is
`display_align_pending` at the head of `main_control`'s loop.
