# Reduce `dvitype -output-level=4` to the parts of a DVI file that determine
# what a driver draws.  Byte offsets, movement opcodes, push/pop choices, font
# numbers and the comment are deliberately absent: two writers may encode the
# same page differently.  Positions, dimensions, page counters, font identity
# and scale are not optional and remain exact DVI-unit integers.

function trim(s) {
  sub(/^[[:space:]]+/, "", s)
  sub(/[[:space:]]+$/, "", s)
  return s
}

function remember_event(s) {
  events[++event_count] = s
}

BEGIN {
  h = v = page_count = 0
  current_font = ""
}

/^numerator\/denominator=/ {
  if (match($0, /^numerator\/denominator=([0-9]+\/[0-9]+)/, m)) {
    unit = m[1]
  }
  next
}

/^magnification=/ {
  if (match($0, /^magnification=([0-9]+)/, m)) {
    magnification = m[1]
  }
  next
}

/^maxv=/ {
  if (match($0, /^maxv=(-?[0-9]+), maxh=(-?[0-9]+), maxstackdepth=[0-9]+, totalpages=([0-9]+)/, m)) {
    max_v = m[1]
    max_h = m[2]
    post_pages = m[3]
  }
  next
}

/^Font [0-9]+:/ {
  if (match($0, /^Font ([0-9]+): (.*)---loaded at size ([0-9]+) DVI units/, m)) {
    id = m[1]
    name = trim(m[2])
    # `trip scaled 2000` and `trip` can denote the same DVI font when their
    # actual scale fields agree.  Drivers use the TFM name and loaded size;
    # the spelling of the source `\font` assignment is not in the DVI.
    sub(/[[:space:]]+scaled[[:space:]]+-?[0-9]+$/, "", name)
    font = name "@" m[3]
    font_by_id[id] = font
    fonts[font] = 1
  } else if ($0 ~ /---not loaded/) {
    invalid = "dvitype could not load a DVI font; run beside the required TFM files"
  } else if ($0 ~ /---loaded at size/) {
    invalid = "could not parse dvitype font summary: " $0
  } else {
    # A fnt_def encountered in the page stream is echoed as `Font N: name`
    # without load information. The postamble summary above already supplied
    # the semantic name/size mapping; this is only the definition's placement.
  }
  next
}

match($0, /^[0-9]+: beginning of page (.*)$/, m) {
  page_count++
  h = v = 0
  # BOP resets h/v/w/x/y/z and the stack, but not DVI's current font.  Real
  # writers commonly omit a redundant font selection at the start of the next
  # page, so clearing it here made valid multi-page files look malformed.
  remember_event("page " page_count " counters=" trim(m[1]))
  next
}

# A `level` line follows push/pop and states the restored machine position.
match($0, /^level [0-9]+:\(h=(-?[0-9]+),v=(-?[0-9]+),/, m) {
  h = m[1]
  v = m[2]
  next
}

# Font selection numbers are local implementation details.  Resolve them to
# the name and actual loaded size collected from the postamble summary.
match($0, /^[0-9]+: fntnum([0-9]+) current font is /, m) {
  current_font = font_by_id[m[1]]
  if (current_font == "") invalid = "selected font " m[1] " has no postamble definition"
}
match($0, /^[0-9]+: fnt[1-4] (-?[0-9]+) current font is /, m) {
  current_font = font_by_id[m[1]]
  if (current_font == "") invalid = "selected font " m[1] " has no postamble definition"
}

{
  old_h = h
  old_v = v

  code = ""
  kind = ""
  if (match($0, /^[0-9]+: setchar([0-9]+)/, m)) {
    kind = "setchar"; code = m[1]
  } else if (match($0, /^[0-9]+: set[1-4] ([0-9]+)/, m)) {
    kind = "setchar"; code = m[1]
  } else if (match($0, /^[0-9]+: putchar([0-9]+)/, m)) {
    kind = "putchar"; code = m[1]
  } else if (match($0, /^[0-9]+: put[1-4] ([0-9]+)/, m)) {
    kind = "putchar"; code = m[1]
  }
  if (kind != "") {
    if (current_font == "") invalid = "character set before a defined font was selected"
    remember_event(kind " page=" page_count " h=" old_h " v=" old_v \
                   " font=" current_font " code=" code)
  }

  if (match($0, /^[0-9]+: (setrule|putrule) height (-?[0-9]+), width (-?[0-9]+)/, m)) {
    # TeX permits non-positive rules as positioning devices.  DVI drivers draw
    # nothing for them, so retaining one would compare structure, not output.
    if ((m[2] + 0) > 0 && (m[3] + 0) > 0) {
      remember_event(m[1] " page=" page_count " h=" old_h " v=" old_v \
                     " height=" m[2] " width=" m[3])
    }
  }

  if (match($0, /^[0-9]+: xxx[1-4]? (.*)$/, m)) {
    remember_event("special page=" page_count " h=" old_h " v=" old_v \
                   " text=" trim(m[1]))
  }

  # Every movement and every set-character result is printed as an assignment
  # by dvitype.  Use its final exact DVI-unit value, never the rounded pixels.
  if (match($0, /h:=[^,]*=(-?[0-9]+),/, m)) h = m[1]
  if (match($0, /v:=[^,]*=(-?[0-9]+),/, m)) v = m[1]
}

END {
  if (unit == "") invalid = "dvitype output has no numerator/denominator"
  if (magnification == "") invalid = "dvitype output has no magnification"
  if (post_pages == "") invalid = "dvitype output has no postamble"
  if (page_count == 0) invalid = "DVI contains no pages"
  if (event_count <= page_count) invalid = "DVI contains no visible glyph, rule, or special events"
  if (post_pages != page_count) invalid = "postamble page count disagrees with parsed pages"
  if (invalid != "") {
    print "dvi semantic normalizer: " invalid > "/dev/stderr"
    exit 3
  }

  print "unit " unit
  print "magnification " magnification
  print "extent maxv=" max_v " maxh=" max_h " pages=" post_pages
  PROCINFO["sorted_in"] = "@ind_str_asc"
  for (font in fonts) print "font " font
  for (i = 1; i <= event_count; i++) print events[i]
}
