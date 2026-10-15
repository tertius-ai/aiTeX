//! Invoking a LaTeX engine.
//!
//! The flags here matter more than any parsing code downstream. `max_print_line`
//! stops TeX wrapping log lines at 79 characters (the top source of parser bugs
//! everywhere else), and `-file-line-error` makes it emit `file:line: message`
//! instead of leaving us to infer location from the `(...)` file stack.
//!
//! `-no-shell-escape` is not optional: TeX executes arbitrary commands via
//! `\write18`, so every compile is treated as hostile.

use aitex_core::Report;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    PdfLatex,
    XeLatex,
    LuaLatex,
}

impl Engine {
    pub fn binary(self) -> &'static str {
        match self {
            Engine::PdfLatex => "pdflatex",
            Engine::XeLatex => "xelatex",
            Engine::LuaLatex => "lualatex",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub engine: Engine,
    /// Stop at the first error. Gives time-to-first-error; not what you want
    /// when collecting a full diagnostic set.
    pub halt_on_error: bool,
    pub timeout: Duration,
    /// Make output reproducible so a PDF hash is a usable oracle. Without this
    /// pdfTeX embeds `/CreationDate` and every run differs.
    pub deterministic: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            engine: Engine::PdfLatex,
            halt_on_error: false,
            timeout: Duration::from_secs(120),
            deterministic: true,
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    pub exit_code: Option<i32>,
    pub duration: Duration,
    pub timed_out: bool,
    pub pdf: Option<PathBuf>,
}

/// Compile `root` in place and parse the resulting `.log` and `.blg`.
///
/// This writes build artifacts next to the source, so callers that must not
/// mutate a source tree should copy it first.
pub fn run(root: &Path, opts: &CompileOptions) -> std::io::Result<Outcome> {
    let dir = root.parent().unwrap_or(Path::new("."));
    let stem = root
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = root.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "root has no file name")
    })?;

    let mut cmd = Command::new(opts.engine.binary());
    cmd.current_dir(dir)
        .arg("-file-line-error")
        .arg("-interaction=nonstopmode")
        .arg("-no-shell-escape");
    if opts.halt_on_error {
        cmd.arg("-halt-on-error");
    }
    cmd.arg(name);

    cmd.env("max_print_line", "1000")
        .env("error_line", "254")
        .env("half_error_line", "238")
        .env("openout_any", "p")
        .env("openin_any", "p");
    if opts.deterministic {
        cmd.env("SOURCE_DATE_EPOCH", "1451606400")
            .env("FORCE_SOURCE_DATE", "1");
    }

    let start = Instant::now();
    let output = cmd.output()?;
    let duration = start.elapsed();
    let timed_out = duration >= opts.timeout;

    let log_path = dir.join(format!("{stem}.log"));
    let log_text =
        read_log(&log_path).unwrap_or_else(|| String::from_utf8_lossy(&output.stdout).into_owned());
    let mut report = parse_all(&log_text, dir, &stem, root);
    report.engine = opts.engine.binary().to_string();

    let pdf = dir.join(format!("{stem}.pdf"));
    Ok(Outcome {
        report,
        exit_code: output.status.code(),
        duration,
        timed_out,
        pdf: pdf.exists().then_some(pdf),
    })
}

/// Read a TeX log, tolerating invalid UTF-8.
///
/// **TeX logs are byte streams, not UTF-8.** Overfull-box reports dump glyphs
/// using the font's own 8-bit encoding, so a perfectly ordinary log can contain
/// bytes like `0xd1` that are not valid UTF-8 anywhere. Measured on our corpus,
/// `read_to_string` rejects **51 of 1000** logs outright — a silent 5% data
/// loss that would have flattered every metric computed downstream.
pub fn read_log(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Parse `.log` plus any `.blg` beside it, merging into one report.
///
/// The `.blg` half is easy to forget and expensive to omit: BibTeX and biber
/// write their errors *only* there, so a log-only harness reports "citation
/// undefined" for what is really a malformed `.bib` entry.
pub fn parse_all(log_text: &str, dir: &Path, stem: &str, root: &Path) -> Report {
    let mut report = crate::log::parse_tex_log(log_text, root);
    let blg = dir.join(format!("{stem}.blg"));
    if let Some(blg_text) = read_log(&blg) {
        report
            .diagnostics
            .extend(crate::log::parse_bib_log(&blg_text).diagnostics);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_core::{Kind, Severity};
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(body.as_bytes()).expect("write");
        p
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitex-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    /// End-to-end against the real engine: a clean document must report success
    /// with no errors.
    #[test]
    fn compiles_a_clean_document() {
        let d = scratch("clean");
        let root = write(
            &d,
            "ok.tex",
            "\\documentclass{article}\n\\begin{document}\nHello.\n\\end{document}\n",
        );
        let out = match run(&root, &CompileOptions::default()) {
            Ok(o) => o,
            Err(_) => return, // no TeX installed; skip rather than fail
        };
        assert!(out.report.succeeded, "should have produced output");
        assert_eq!(out.report.errors().count(), 0);
        assert_eq!(out.report.pages, Some(1));
        assert!(out.pdf.is_some());
    }

    /// The undefined-control-sequence path, which is the single most common
    /// real-world error.
    #[test]
    fn reports_undefined_control_sequence_at_the_right_line() {
        let d = scratch("undef");
        let root = write(
            &d,
            "bad.tex",
            "\\documentclass{article}\n\\begin{document}\nfine\n\\nosuchmacro\n\\end{document}\n",
        );
        let out = match run(&root, &CompileOptions::default()) {
            Ok(o) => o,
            Err(_) => return,
        };
        let e = out.report.first_error().expect("should find an error");
        assert!(
            matches!(&e.kind, Kind::UndefinedControlSequence { name } if name == "nosuchmacro")
        );
        assert_eq!(e.reported.as_ref().expect("located").line, 4);
    }

    /// An unclosed brace is the canonical *unlocalizable* error. The cause is
    /// `\textbf{` on line 3, but TeX only fails once the file ends, and reports
    /// `! File ended while scanning use of \textbf .` with **no line number at
    /// all** — not merely a distant one.
    ///
    /// Measured on the corpus, the `runaway_arg` family lands unlocalizable
    /// 78/88 times. Pinning it here because it is the hardest case the harness
    /// has to solve, and any regression toward "we found a line" would be the
    /// parser inventing one.
    #[test]
    fn unclosed_brace_is_unlocalizable() {
        let d = scratch("gap");
        let mut body = String::from("\\documentclass{article}\n\\begin{document}\n");
        body.push_str("\\textbf{unclosed\n"); // line 3: the actual cause
        for _ in 0..60 {
            body.push_str("filler text here.\n");
        }
        body.push_str("\\end{document}\n");
        let root = write(&d, "gap.tex", &body);
        let out = match run(&root, &CompileOptions::default()) {
            Ok(o) => o,
            Err(_) => return,
        };
        let e = out.report.first_error().expect("should find an error");
        assert_eq!(e.kind, aitex_core::Kind::RunawayArgument);
        assert!(
            e.reported.is_none(),
            "TeX reports no line for this; we must not fabricate one (got {:?})",
            e.reported
        );
        assert_eq!(e.gap(), aitex_core::Gap::Unknown);
        assert!(e.gap().is_non_localized());
    }

    /// `-halt-on-error` must stop at the *first* problem: the second undefined
    /// macro should never be reached.
    #[test]
    fn halt_on_error_stops_at_the_first_problem() {
        let d = scratch("halt");
        let root = write(
            &d,
            "two.tex",
            "\\documentclass{article}\n\\begin{document}\n\\aaa\n\\bbb\n\\end{document}\n",
        );
        let opts = CompileOptions {
            halt_on_error: true,
            ..Default::default()
        };
        let out = match run(&root, &opts) {
            Ok(o) => o,
            Err(_) => return,
        };
        let first = out.report.first_error().expect("should find an error");
        assert!(
            matches!(&first.kind, Kind::UndefinedControlSequence { name } if name == "aaa"),
            "first error should be \\aaa, got {:?}",
            first.kind
        );
        assert!(
            !out.report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("bbb")
                    || matches!(&d.kind, Kind::UndefinedControlSequence { name } if name == "bbb")),
            "halt-on-error should never reach \\bbb"
        );
    }

    #[test]
    fn detects_overfull_boxes() {
        let d = scratch("overfull");
        let root = write(
            &d,
            "of.tex",
            "\\documentclass{article}\n\\begin{document}\n\\hbox to 1pt{aaaaaaaaaaaaaaaaaaaaaaaaaaaa}\n\\end{document}\n",
        );
        let out = match run(&root, &CompileOptions::default()) {
            Ok(o) => o,
            Err(_) => return,
        };
        assert!(
            out.report
                .bad_boxes()
                .any(|d| d.severity == Severity::BadBox)
        );
    }
}
