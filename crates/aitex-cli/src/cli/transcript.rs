/// Read a file as TeX reads one: a stream of **bytes**, each of which is one
/// character.
///
/// TeX82 is an 8-bit engine and `latex.ltx` is written for one. Its UTF-8
/// support makes bytes `0x80`–`0xFF` *active* and composes them into accents in
/// macro code — the same machinery pdfTeX runs. Handing it a decoded `char`
/// instead means those active characters never fire: `naïve café` came out
/// `nave caf`, every non-ASCII character silently dropped, because `ï` arrived
/// as one U+00EF that the kernel has no rule for and cmr10 has no glyph for.
///
/// So each byte becomes the `char` of the same value — U+0000..U+00FF, which is
/// exactly Latin-1 — and the engine's existing `source.chars()` then yields one
/// token per byte with no change to the tokenizer, the 256-entry catcode table,
/// or anything downstream. `\catcode` of a byte works because a byte and its
/// `char` have the same numeric value.
///
/// This also removes a panic. `read_to_string` on a Latin-1 `.tex` file — which
/// real TeX reads without complaint — aborted the process with "stream did not
/// contain valid UTF-8", against docs/history/PLAN.md §9's statement that the engine never
/// panics on arbitrary input.
/// `tex.web`'s transcript, as a character stream — the shape of every TeX log.
///
/// §54's `print_char` writes one character at a time against a column
/// (`file_offset`), and §58 ends the line the moment that column reaches
/// `max_print_line`. Everything else about a TeX log follows from that: it is
/// why lines break mid-word, why `\message`s run together, and why §638's `[1]`
/// page echo lands on whatever line was in progress.
///
/// The engine hands the host *logical* lines (`Fast::messages`) and this turns
/// them back into the stream. An entry ends its line the way §57's `print_nl`
/// does — only if there is something on it — while a `\n` *inside* an entry is
/// §57's `print_ln`, which ends the line whether or not it is empty. That
/// distinction is load-bearing: §90 closes every error with two unconditional
/// `print_ln`s, so a context line the wrap has already ended is followed by two
/// blank lines and not one.
fn render_transcript(messages: &[String], mpl: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut off = 0usize;
    for m in messages {
        if m.is_empty() {
            out.push(core::mem::take(&mut line));
            off = 0;
            continue;
        }
        for c in m.chars() {
            if c == '\n' {
                out.push(core::mem::take(&mut line));
                off = 0;
                continue;
            }
            line.push(c);
            off += 1;
            if off == mpl {
                out.push(core::mem::take(&mut line));
                off = 0;
            }
        }
        if off > 0 {
            out.push(core::mem::take(&mut line));
            off = 0;
        }
    }
    if off > 0 {
        out.push(line);
    }
    out
}

thread_local! {
    /// §1333's closing lines — `Output written on …` — which are produced by
    /// the shipper, well before the transcript is flushed. Recorded here so the
    /// log can end the way TeX's does. Thread-local because the whole job runs
    /// on the one large-stack thread `main` spawns.
    static LOG_TAIL: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log_tail_push(s: String) {
    LOG_TAIL.with(|t| t.borrow_mut().push(s));
}

/// Write `\jobname.log`.
///
/// §534's `open_log_file` opens it with the banner and the `**` line naming
/// what the job was asked to do, and §1333's `close_files_and_terminate` closes
/// it with the statistics and `Output written on …`. Between them it is the
/// same stream the terminal gets.
///
/// Until now the engine wrote no `.log` at all, which cost twice: `tools/trip.sh`
/// could not attempt Knuth's `trip.log` comparison — the canonical proof of a
/// TeX — and the harness (docs/history/PLAN.md §5, H1), whose whole first milestone is parsing
/// `.log` files, had none of ours to parse.
fn write_log(
    job: &str,
    input: &str,
    lines: &[String],
    tail: &[String],
    unicode_mode: bool,
) -> std::io::Result<()> {
    let mut s = String::new();
    // Same shape as TeX's own first two lines, under our own name. A log that
    // claimed to be TeX's would be a lie to every tool that reads one.
    //
    // §534 writes the `**` line as the job was *invoked*, not as the file is
    // named: a run against a preloaded format opens `**&trip trip.tex`, and
    // ours said `**trip.tex`, which is the first line of every transcript and
    // so the first line of every diff.
    let fmt = format_argument()
        .map(|p| {
            let stem = std::path::Path::new(&p)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("fmt")
                .to_string();
            format!("&{stem} ")
        })
        .unwrap_or_default();
    s.push_str(&format!(
        "This is aiTeX, Version {} (format=INITEX)\n**{fmt}{input}\n",
        env!("CARGO_PKG_VERSION")
    ));
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    for l in tail {
        s.push_str(l);
        s.push('\n');
    }
    let path = format!("{job}.log");
    std::fs::write(path, tex_text_bytes(&s, unicode_mode))
}

fn read_latin1(path: &str) -> std::io::Result<String> {
    Ok(std::fs::read(path)?.into_iter().map(char::from).collect())
}

/// Unicode-mode inputs are UTF-8. If an installed legacy package contains a
/// non-UTF-8 comment or string, retain classic TeX's byte-faithful reading
/// rather than making the package disappear from `\input` resolution.
fn read_tex_text(path: &str, unicode_mode: bool) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    if unicode_mode {
        match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(error) => Ok(error.into_bytes().into_iter().map(char::from).collect()),
        }
    } else {
        Ok(bytes.into_iter().map(char::from).collect())
    }
}

/// TeX's `input_ln` appends `\endlinechar` to a nonempty final physical line
/// even when the file has no newline byte. The engine's tokenizer performs
/// that substitution when it sees the raw terminator, so give the top-level
/// source the same complete-file boundary that `\input` files receive.
fn complete_file_source(source: &mut String) {
    if !source.is_empty() && !source.ends_with(['\n', '\r']) {
        source.push('\n');
    }
}

/// The inverse of [`read_latin1`]: one byte per character.
///
/// Every character the engine emits to a *file* is a TeX character in
/// 0..=255, and must go out as that byte. Encoding it as UTF-8 instead would
/// write `é` as two bytes into `\jobname.aux`, and the next run — reading it
/// back a byte at a time — would see two characters where one was written, so
/// every `\label` with an accent in it would fail to round-trip.
///
/// Characters above 255 cannot reach here from a byte-faithful source, but
/// `\char` can construct one; it is written as `?`, which is what `tex` does
/// with a character its output encoding cannot represent.
fn latin1_bytes(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect()
}

fn tex_text_bytes(s: &str, unicode_mode: bool) -> Vec<u8> {
    if unicode_mode {
        s.as_bytes().to_vec()
    } else {
        latin1_bytes(s)
    }
}
