//! TeX `.log` and BibTeX/biber `.blg` parsing.
//!
//! The single most important thing here is not in this file: it is invoking the
//! engine with `max_print_line=1000`. TeX hard-wraps log lines at 79 characters
//! by default, splitting messages and filenames mid-token, and that wrapping is
//! the top source of bugs across every existing log parser. We still unwrap
//! defensively, because logs arrive from elsewhere too.
//!
//! Written from scratch: every existing parser worth copying (texlogsieve,
//! texlab, Overleaf's) is GPL, and we ship permissively. Algorithms are not
//! copyrightable, so the *approach* is informed by them; none of the code is.

use aitex_core::{Diagnostic, Kind, Report, Severity, Source, Span, Tool};
use std::path::{Path, PathBuf};

/// Longest line TeX will emit before wrapping, when `max_print_line` is unset.
const DEFAULT_MAX_PRINT_LINE: usize = 79;

/// Rejoin lines TeX split at `max_print_line`.
///
/// A line of *exactly* the wrap width was almost certainly cut. This is a
/// heuristic and cannot be perfect — a natural line of exactly 79 characters is
/// indistinguishable from a wrapped one — which is why setting
/// `max_print_line=1000` at invocation matters more than this function does.
pub fn unwrap_lines(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let joined = out
            .last()
            .is_some_and(|prev: &String| prev.chars().count() == DEFAULT_MAX_PRINT_LINE);
        if joined {
            out.last_mut().expect("checked non-empty").push_str(line);
        } else {
            out.push(line.to_string());
        }
    }
    out
}

/// Tracks TeX's `(filename ... )` stack so a diagnostic can be attributed to a
/// file even when the message itself does not name one.
///
/// Every `)` pops, including ones inside prose or filenames, so this drifts on
/// adversarial input. `-file-line-error` is authoritative when present; this is
/// the fallback.
#[derive(Debug, Default)]
struct FileStack {
    stack: Vec<PathBuf>,
}

impl FileStack {
    fn feed(&mut self, line: &str) {
        let bytes: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                '(' => {
                    let start = i + 1;
                    let mut j = start;
                    while j < bytes.len() && !matches!(bytes[j], ')' | '(' | ' ' | '[') {
                        j += 1;
                    }
                    let name: String = bytes[start..j].iter().collect();
                    if !name.is_empty() && name.contains('.') {
                        self.stack.push(PathBuf::from(name));
                    } else {
                        // A '(' that opens no file still needs a matching pop.
                        self.stack.push(PathBuf::new());
                    }
                    i = j;
                }
                ')' => {
                    self.stack.pop();
                    i += 1;
                }
                _ => i += 1,
            }
        }
    }

    /// Innermost file that looks like the user's own, not a package.
    fn current_user_file(&self) -> Option<&Path> {
        self.stack
            .iter()
            .rev()
            .find(|p| !p.as_os_str().is_empty() && aitex_core::is_user_file(p))
            .map(|p| p.as_path())
    }
}

/// Pull the offending control sequence out of TeX's expansion context.
///
/// TeX's message is only ever the generic `Undefined control sequence.`; the
/// name appears on the following context line, which it splits at the point of
/// failure:
///
/// ```text
/// ./u.tex:4: Undefined control sequence.
/// l.4 \nosuchmacro
/// ```
///
/// The culprit is the last control sequence *before* the split, so we take the
/// final `\word` on the `l.N` line.
fn undefined_cs_name(context: &[String]) -> String {
    for line in context {
        let body = match context_line_marker(line) {
            Some((_, body)) => body,
            None if line.starts_with('<') => line.as_str(), // <recently read>, <argument>
            None => continue,
        };
        if let Some(pos) = body.rfind('\\') {
            let name: String = body[pos + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            if !name.is_empty() {
                return name;
            }
        }
    }
    String::new()
}

/// TeX's default wrapping can concatenate an exactly-79-character expansion
/// line with the following `l.N` recovery line.  Accept the marker inside that
/// joined context while still requiring decimal digits immediately after it.
fn context_line_marker(line: &str) -> Option<(u32, &str)> {
    for (at, _) in line.rmatch_indices("l.") {
        let numbered = &line[at + 2..];
        let digits = numbered
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if digits.is_empty() {
            continue;
        }
        let line_number = digits.parse().ok()?;
        return Some((line_number, &numbered[digits.len()..]));
    }
    None
}

fn classify(msg: &str, context: &[String]) -> (Severity, Kind) {
    let m = msg.trim();

    if m.starts_with("Undefined control sequence") {
        return (
            Severity::Error,
            Kind::UndefinedControlSequence {
                name: undefined_cs_name(context),
            },
        );
    }
    // Three spellings of the same underlying fault — a macro argument that was
    // never terminated:
    //   * `Runaway argument?`                       — the announcement
    //   * `File ended while scanning use of \foo`   — ran to end of file
    //   * `Paragraph ended before \foo was complete` — a blank line ended it
    //
    // The third is the most common in real documents (37 of 48 initially
    // unclassified corpus logs) because `\par` terminates a non-`\long` macro
    // argument, and an accidental blank line inside `\begin{...}` or a
    // `\cite{...}` does it constantly.
    if m.starts_with("Runaway argument")
        || m.starts_with("File ended while scanning")
        || m.starts_with("Paragraph ended before")
    {
        return (Severity::Error, Kind::RunawayArgument);
    }
    // Display math opened with `$$` but closed with a single `$`, or vice versa.
    if m.starts_with("Display math should end with") {
        return (Severity::Error, Kind::MissingMathShift);
    }
    if m.starts_with("Missing $ inserted") {
        return (Severity::Error, Kind::MissingMathShift);
    }
    // LaTeX and several math packages report the downstream symptom rather
    // than TeX's primitive `Missing $ inserted`.  These all mean that the
    // current math-mode state is wrong, and classifying them together lets the
    // structural pass look for the delimiter that caused the recovery error.
    if m.contains("allowed only in math mode")
        || m.contains("invalid in math mode")
        || m.contains("Bad math environment delimiter")
    {
        return (Severity::Error, Kind::MissingMathShift);
    }
    if m.starts_with("Missing } inserted") || m.starts_with("Missing { inserted") {
        return (Severity::Error, Kind::MissingBrace);
    }
    if m.starts_with("Misplaced alignment tab") || m.contains("Extra alignment tab") {
        return (Severity::Error, Kind::ExtraAlignmentTab);
    }
    if let Some(rest) = m.strip_prefix("TeX capacity exceeded, sorry [") {
        let resource = rest.split('=').next().unwrap_or("unknown").to_string();
        return (Severity::Fatal, Kind::CapacityExceeded { resource });
    }
    if m.starts_with("Emergency stop") || m.starts_with("==> Fatal error") {
        return (Severity::Fatal, Kind::Other);
    }

    // `LaTeX Error: Environment foo undefined.`
    if let Some(rest) = m.strip_prefix("LaTeX Error: Environment ") {
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        return (Severity::Error, Kind::UndefinedEnvironment { name });
    }
    // `LaTeX Error: No counter 'theorem' defined.`
    if m.starts_with("LaTeX Error: No counter") {
        let name = m.split('\'').nth(1).unwrap_or("").to_string();
        return (Severity::Error, Kind::UndefinedCounter { name });
    }
    // `LaTeX Error: Command \foo undefined.` — same fault as an undefined
    // control sequence, reported by LaTeX rather than by TeX.
    if let Some(rest) = m.strip_prefix("LaTeX Error: Command ") {
        let name = rest
            .trim_start_matches('\\')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_start_matches('\\')
            .to_string();
        return (Severity::Error, Kind::UndefinedControlSequence { name });
    }
    // Packages that raise their own errors. Xy-pic uses its own wording;
    // everything built on LaTeX's interface uses `Package <name> Error:`.
    if m.starts_with("Xy-pic error") {
        return (
            Severity::Error,
            Kind::PackageError {
                package: "xy".into(),
            },
        );
    }
    if let Some(rest) = m.strip_prefix("Package ")
        && let Some(idx) = rest.find(" Error:")
    {
        return (
            Severity::Error,
            Kind::PackageError {
                package: rest[..idx].trim().to_string(),
            },
        );
    }

    // `\begin{X} ended by \end{Y}`
    if let Some(idx) = m.find("ended by \\end{") {
        let began = m
            .find("\\begin{")
            .map(|b| m[b + 7..].split('}').next().unwrap_or("").to_string())
            .unwrap_or_default();
        let ended = m[idx + 14..].split('}').next().unwrap_or("").to_string();
        return (
            Severity::Error,
            Kind::MismatchedEnvironment { began, ended },
        );
    }

    if let Some(rest) = m.strip_prefix("LaTeX Error: File `") {
        let name = rest.split('\'').next().unwrap_or("").to_string();
        let kind = if name.ends_with(".sty") || name.ends_with(".cls") {
            Kind::MissingPackage { name }
        } else {
            Kind::MissingFile { name }
        };
        return (Severity::Error, kind);
    }
    if m.contains("File `") && m.contains("' not found") {
        let name = m
            .split("File `")
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .unwrap_or("")
            .to_string();
        return (Severity::Error, Kind::MissingFile { name });
    }

    if m.contains("Citation") && m.contains("undefined") {
        let key = m
            .split('`')
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .unwrap_or("")
            .to_string();
        return (Severity::Warning, Kind::UndefinedCitation { key });
    }
    if m.contains("Reference") && m.contains("undefined") {
        let label = m
            .split('`')
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .unwrap_or("")
            .to_string();
        return (Severity::Warning, Kind::UndefinedReference { label });
    }
    if m.contains("Rerun to get") || m.contains("Label(s) may have changed") {
        return (Severity::Warning, Kind::RerunNeeded);
    }

    if m.starts_with("LaTeX Error") || m.contains("Error:") {
        return (Severity::Error, Kind::Other);
    }
    (Severity::Warning, Kind::Other)
}

/// Classify a message that arrived in an *error context*.
///
/// Both `! msg` and `file:line: msg` are emitted by TeX only for errors —
/// warnings appear as `LaTeX Warning:` lines with neither marker. So an
/// unrecognized message found in one of those positions is an error we have not
/// enumerated, not a warning.
///
/// This matters more than any individual message pattern: TeX has hundreds of
/// error strings and we will never list them all. Flooring the severity by
/// *context* means a message we have never seen still surfaces as an error,
/// with its text intact, instead of being silently downgraded and dropped.
fn classify_in_error_context(msg: &str, context: &[String]) -> (Severity, Kind) {
    let (sev, kind) = classify(msg, context);
    // Fatal must not be weakened; genuine warning kinds keep their severity
    // even here (a `Rerun to get...` can be echoed in an error block).
    let sev = match kind {
        Kind::RerunNeeded | Kind::UndefinedReference { .. } | Kind::UndefinedCitation { .. } => sev,
        _ => sev.max(Severity::Error),
    };
    (sev, kind)
}

fn parse_badbox(line: &str) -> Option<(Severity, Kind)> {
    let lines = || -> Option<(u32, u32)> {
        let at = line
            .find("at lines ")
            .or_else(|| line.find("in paragraph at lines "))?;
        let tail = &line[at..];
        let nums = tail.trim_start_matches(|c: char| !c.is_ascii_digit());
        let mut it = nums.split("--");
        Some((
            it.next()?.trim().parse().ok()?,
            it.next()?.trim().parse().ok()?,
        ))
    };

    if line.starts_with("Overfull \\hbox") || line.starts_with("Overfull \\vbox") {
        let pt: f64 = line
            .split('(')
            .nth(1)
            .and_then(|s| s.split("pt").next())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0.0);
        let sp = aitex_core::pt_to_sp(pt);
        return Some((Severity::BadBox, Kind::OverfullBox { sp, lines: lines() }));
    }
    if line.starts_with("Underfull \\hbox") || line.starts_with("Underfull \\vbox") {
        let badness = line
            .split("badness ")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.trim().parse().ok());
        return Some((
            Severity::BadBox,
            Kind::UnderfullBox {
                badness,
                lines: lines(),
            },
        ));
    }
    None
}

/// Parse a `.log`. `root` names the main source file, used when the log gives
/// no filename of its own.
pub fn parse_tex_log(raw: &str, root: &Path) -> Report {
    let lines = unwrap_lines(raw);
    let mut report = Report {
        engine: "pdflatex".into(),
        ..Default::default()
    };
    let mut stack = FileStack::default();

    for (i, line) in lines.iter().enumerate() {
        stack.feed(line);

        if let Some(p) = line.find("Output written on") {
            report.succeeded = true;
            report.pages = line[p..]
                .split('(')
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok());
        }

        // `file:line: message` — the authoritative form, from -file-line-error.
        if let Some(d) = parse_file_line_error(line) {
            let (file, ln, msg) = d;
            let context = collect_context(&lines, i);
            let (sev, kind) = classify_in_error_context(&msg, &context);
            let mut diag = Diagnostic::new(sev, kind, msg, Source::TexLog).at(Span::new(file, ln));
            diag.context = context;
            report.diagnostics.push(diag);
            continue;
        }

        // `! message` — recover the line from TeX's following `l.N` context.
        // The parenthesized file stack is useful but heuristic: package prose
        // and filenames may contain unmatched parentheses.  Never throw away
        // a real recovery line merely because that stack drifted; the root
        // input is a conservative fallback and later causal enrichment can
        // relocate unique source evidence into an included file.
        if let Some(msg) = line.strip_prefix("! ") {
            let context = collect_context(&lines, i);
            let (sev, kind) = classify_in_error_context(msg, &context);
            let mut diag = Diagnostic::new(sev, kind, msg, Source::TexLog);
            if let Some(ln) = context_line_number(&lines, i) {
                let file = stack.current_user_file().unwrap_or(root);
                diag = diag.at(Span::new(file, ln));
            }
            diag.context = context;
            report.diagnostics.push(diag);
            continue;
        }

        if let Some((sev, kind)) = parse_badbox(line) {
            let mut diag = Diagnostic::new(sev, kind.clone(), line.clone(), Source::TexLog);
            if let (
                Some(f),
                Kind::OverfullBox {
                    lines: Some((a, _)),
                    ..
                },
            )
            | (
                Some(f),
                Kind::UnderfullBox {
                    lines: Some((a, _)),
                    ..
                },
            ) = (stack.current_user_file(), &kind)
            {
                diag = diag.at(Span::new(f, *a));
            }
            report.diagnostics.push(diag);
            continue;
        }

        if line.starts_with("LaTeX Warning:")
            || line.starts_with("Package ") && line.contains("Warning:")
        {
            let msg = line.trim().to_string();
            // The producer's channel is authoritative for severity. Some
            // warning prose contains phrases also used by hard errors (for
            // example, `invalid in math mode`); retain the typed kind without
            // promoting an explicitly emitted warning above a later real
            // error in the same build.
            let (_, kind) = classify(&msg, &[]);
            let mut diag = Diagnostic::new(Severity::Warning, kind, msg, Source::TexLog);
            if let Some(f) = stack.current_user_file() {
                diag = diag.at(Span::new(f, 0));
            } else {
                diag = diag.at(Span::new(root, 0));
            }
            report.diagnostics.push(diag);
        }
    }

    report
}

fn parse_file_line_error(line: &str) -> Option<(PathBuf, u32, String)> {
    // Shortest viable form is `a:1: x`. Scan for the first `:digits:`.
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(c) = bytes.get(i) {
        if *c == b':' {
            let start = i + 1;
            let mut j = start;
            while bytes.get(j).is_some_and(|b| b.is_ascii_digit()) {
                j += 1;
            }
            if j > start && bytes.get(j) == Some(&b':') {
                let file = line[..i].trim_start_matches("./");
                // TeX's box diagnostics print set text after an `[]` marker,
                // including font selectors such as `\T1/ptm/...`.  An ISO
                // timestamp in that text has the same `:digits:` shape as
                // `-file-line-error`; the slash in the font selector must not
                // make the whole diagnostic look like a path.
                if file.is_empty()
                    || file.starts_with("[]")
                    || file.starts_with("\\")
                    || file.contains(' ') && !file.contains('/')
                {
                    return None;
                }
                let ln: u32 = line[start..j].parse().ok()?;
                let msg = line[j + 1..].trim().to_string();
                if msg.is_empty() {
                    return None;
                }
                return Some((PathBuf::from(file), ln, msg));
            }
        }
        i += 1;
    }
    None
}

/// Whether a TeX transcript contains an error-context marker worth fully
/// parsing.
///
/// Clean logs can contain thousands of warnings and bad-box records.  The CLI
/// only needs the full diagnostic model here when LaTeX raised a macro-level
/// hard error that did not increment the engine's primitive error counter.
/// Scan for TeX's two authoritative error channels first so successful
/// compilation does not allocate and enrich every informational record.
pub fn tex_log_has_error_context(raw: &str) -> bool {
    raw.lines()
        .any(|line| line.starts_with("! ") || parse_file_line_error(line).is_some())
}

/// TeX prints the offending source line as `l.42 ...` after an error.
fn context_line_number(lines: &[String], from: usize) -> Option<u32> {
    lines
        .iter()
        .skip(from + 1)
        .take(24)
        .take_while(|line| !line.starts_with("! "))
        .find_map(|line| context_line_marker(line).map(|(number, _)| number))
}

/// The few lines after an error that show expansion context. Kept verbatim —
/// they are what makes an `Undefined control sequence` diagnosable.
fn collect_context(lines: &[String], from: usize) -> Vec<String> {
    lines
        .iter()
        .skip(from + 1)
        .take(6)
        .take_while(|l| !l.starts_with("! "))
        .filter(|l| !l.trim().is_empty())
        .cloned()
        .collect()
}

/// Parse a BibTeX `.blg`. Kept as a compatibility wrapper for the original H1
/// API; new code should retain the exact producer with [`parse_tool_log`].
pub fn parse_bib_log(raw: &str) -> Report {
    parse_tool_log(raw, Tool::BibTex)
}

/// Parse the transcript produced by one external semantic build tool.
pub fn parse_tool_log(raw: &str, tool: Tool) -> Report {
    aitex_core::parse_tool_log(raw, tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_lines_split_at_79_chars() {
        let a = "x".repeat(79);
        let raw = format!("{a}\ncontinued\nshort line\n");
        let out = unwrap_lines(&raw);
        assert_eq!(out[0], format!("{a}continued"));
        assert_eq!(out[1], "short line");
    }

    #[test]
    fn parses_file_line_error() {
        let (f, l, m) = parse_file_line_error("./paper.tex:673: Undefined control sequence.")
            .expect("should parse");
        assert_eq!(f, PathBuf::from("paper.tex"));
        assert_eq!(l, 673);
        assert_eq!(m, "Undefined control sequence.");
    }

    #[test]
    fn an_iso_timestamp_in_box_text_is_not_a_file_line_error() {
        let line = r"[]\T1/ptm/m/n/10.95 is []\T1/pcr/m/n/10.95 2022-02-03T07:03:23.640Z =";
        assert!(parse_file_line_error(line).is_none());
        assert!(!tex_log_has_error_context(line));
    }

    #[test]
    fn hard_error_preflight_ignores_warnings_but_finds_both_error_channels() {
        assert!(!tex_log_has_error_context(
            "LaTeX Warning: Label(s) may have changed.\nOverfull \\hbox (1.0pt too wide)\n"
        ));
        assert!(tex_log_has_error_context(
            "! LaTeX Error: File `missing.sty' not found.\n"
        ));
        assert!(tex_log_has_error_context(
            "./main.tex:7: Undefined control sequence.\n"
        ));
    }

    #[test]
    fn ignores_non_error_colon_lines() {
        assert!(parse_file_line_error("Package: hyperref 2024-01-20").is_none());
        assert!(parse_file_line_error("").is_none());
    }

    #[test]
    fn classifies_undefined_cs_and_extracts_name() {
        let log = "./a.tex:12: Undefined control sequence.\nl.12 \\includegraphic\n";
        let r = parse_tex_log(log, Path::new("a.tex"));
        assert_eq!(r.diagnostics.len(), 1);
        match &r.diagnostics[0].kind {
            Kind::UndefinedControlSequence { name } => assert_eq!(name, "includegraphic"),
            k => panic!("wrong kind: {k:?}"),
        }
    }

    #[test]
    fn classifies_mismatched_environment() {
        let log = "./a.tex:40: LaTeX Error: \\begin{itemize} on input line 12 ended by \\end{document}.\n";
        let r = parse_tex_log(log, Path::new("a.tex"));
        match &r.diagnostics[0].kind {
            Kind::MismatchedEnvironment { began, ended } => {
                assert_eq!(began, "itemize");
                assert_eq!(ended, "document");
            }
            k => panic!("wrong kind: {k:?}"),
        }
    }

    #[test]
    fn classifies_missing_package_vs_missing_file() {
        let r = parse_tex_log(
            "./a.tex:3: LaTeX Error: File `nonesuch.sty' not found.\n",
            Path::new("a.tex"),
        );
        assert!(
            matches!(&r.diagnostics[0].kind, Kind::MissingPackage { name } if name == "nonesuch.sty")
        );
        let r = parse_tex_log(
            "./a.tex:9: LaTeX Error: File `fig1.pdf' not found.\n",
            Path::new("a.tex"),
        );
        assert!(matches!(&r.diagnostics[0].kind, Kind::MissingFile { name } if name == "fig1.pdf"));
    }

    #[test]
    fn capacity_exceeded_is_fatal() {
        let r = parse_tex_log(
            "! TeX capacity exceeded, sorry [main memory size=5000000].\n",
            Path::new("a.tex"),
        );
        assert_eq!(r.diagnostics[0].severity, Severity::Fatal);
        assert!(matches!(
            &r.diagnostics[0].kind,
            Kind::CapacityExceeded { .. }
        ));
    }

    #[test]
    fn an_explicit_latex_warning_never_becomes_a_hard_error() {
        let r = parse_tex_log(
            "LaTeX Warning: Command \\textendash invalid in math mode on input line 12.\n",
            Path::new("a.tex"),
        );
        assert_eq!(r.diagnostics[0].severity, Severity::Warning);
        assert!(matches!(r.diagnostics[0].kind, Kind::MissingMathShift));
        assert!(r.first_error().is_none());
    }

    #[test]
    fn parses_overfull_hbox_with_line_range() {
        let r = parse_tex_log(
            "(./a.tex\nOverfull \\hbox (12.34pt too wide) in paragraph at lines 42--45\n",
            Path::new("a.tex"),
        );
        let bb: Vec<_> = r.bad_boxes().collect();
        assert_eq!(bb.len(), 1);
        match &bb[0].kind {
            Kind::OverfullBox { sp, lines } => {
                assert_eq!(*sp, aitex_core::pt_to_sp(12.34));
                assert!((aitex_core::sp_to_pt(*sp) - 12.34).abs() < 1e-4);
                assert_eq!(*lines, Some((42, 45)));
            }
            k => panic!("wrong kind: {k:?}"),
        }
    }

    #[test]
    fn records_page_count_and_success() {
        let r = parse_tex_log(
            "Output written on a.pdf (12 pages, 345 bytes).\n",
            Path::new("a.tex"),
        );
        assert!(r.succeeded);
        assert_eq!(r.pages, Some(12));
    }

    #[test]
    fn file_stack_prefers_user_file_over_package() {
        let log = "(./paper.tex (/usr/local/texlive/2024/tex/latex/base/article.cls\n! Undefined control sequence.\nl.5 \\foo\n";
        let r = parse_tex_log(log, Path::new("paper.tex"));
        let span = r.diagnostics[0].reported.as_ref().expect("located");
        assert_eq!(span.file, PathBuf::from("./paper.tex"));
        assert_eq!(span.line, 5);
    }

    #[test]
    fn bang_error_keeps_recovery_line_when_file_stack_is_unknown() {
        let log = "! Missing $ inserted.\n<inserted text>\n                $\nl.42 text_\n";
        let r = parse_tex_log(log, Path::new("paper.tex"));
        let diagnostic = r.first_error().expect("error");
        assert_eq!(diagnostic.kind, Kind::MissingMathShift);
        assert_eq!(
            diagnostic.reported.as_ref(),
            Some(&Span::new("paper.tex", 42))
        );
    }

    #[test]
    fn recovery_line_survives_default_width_context_wrapping() {
        let expansion = "x".repeat(DEFAULT_MAX_PRINT_LINE);
        let log = format!("! Undefined control sequence.\n{expansion}\nl.37 \\misspelled\n");
        let report = parse_tex_log(&log, Path::new("paper.tex"));
        let diagnostic = report.first_error().expect("error");
        assert_eq!(
            diagnostic.reported.as_ref(),
            Some(&Span::new("paper.tex", 37))
        );
        assert!(matches!(
            &diagnostic.kind,
            Kind::UndefinedControlSequence { name } if name == "misspelled"
        ));
    }

    #[test]
    fn latex_math_mode_symptoms_are_math_shift_errors() {
        for message in [
            "LaTeX Error: \\mathcal allowed only in math mode.",
            "LaTeX Error: Command \\item invalid in math mode.",
            "LaTeX Error: Bad math environment delimiter.",
        ] {
            let log = format!("! {message}\nl.8 source\n");
            let report = parse_tex_log(&log, Path::new("paper.tex"));
            assert_eq!(
                report.first_error().expect("error").kind,
                Kind::MissingMathShift
            );
        }
    }

    /// The most common runaway spelling in real documents: a blank line ends a
    /// non-`\long` macro argument. 37 of 48 initially-unclassified corpus logs.
    #[test]
    fn paragraph_ended_before_is_a_runaway() {
        let r = parse_tex_log(
            "./a.tex:349: Paragraph ended before \\begin  was complete.\n",
            Path::new("a.tex"),
        );
        assert_eq!(r.diagnostics[0].kind, Kind::RunawayArgument);
        assert_eq!(r.diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn display_math_mismatch_is_a_math_shift_error() {
        let r = parse_tex_log(
            "./a.tex:12: Display math should end with $$.\n",
            Path::new("a.tex"),
        );
        assert_eq!(r.diagnostics[0].kind, Kind::MissingMathShift);
        assert_eq!(r.diagnostics[0].severity, Severity::Error);
    }

    /// TeX has hundreds of error strings and we will never enumerate them all.
    /// A message we have never seen, arriving in an error position, must still
    /// come out as an Error with its text intact — never silently downgraded.
    #[test]
    fn unknown_message_in_error_position_is_still_an_error() {
        let r = parse_tex_log(
            "./a.tex:7: Some future TeX diagnostic nobody has written a rule for.\n",
            Path::new("a.tex"),
        );
        let d = r.first_error().expect("must surface as an error");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.kind, Kind::Other);
        assert!(d.message.contains("nobody has written a rule for"));

        let r = parse_tex_log("! Another unheard-of failure.\n", Path::new("a.tex"));
        assert_eq!(
            r.first_error().expect("bang lines are errors").severity,
            Severity::Error
        );
    }

    /// Genuine warnings must not be inflated by the severity floor.
    #[test]
    fn warnings_are_not_promoted_to_errors() {
        let r = parse_tex_log(
            "LaTeX Warning: Reference `fig:1' on page 1 undefined on input line 5.\n",
            Path::new("a.tex"),
        );
        assert_eq!(r.errors().count(), 0, "a warning must not become an error");
    }

    #[test]
    fn blg_errors_are_tagged_as_bibliography() {
        let blg = "I couldn't open database file nonesuch.bib\n";
        let r = parse_bib_log(blg);
        assert_eq!(r.diagnostics.len(), 1);
        assert_eq!(r.diagnostics[0].source, Source::ToolLog(Tool::BibTex));
        assert_eq!(r.diagnostics[0].kind, Kind::Bibliography);
    }

    #[test]
    fn blg_line_reference_is_extracted() {
        let blg = "I was expecting a `,' or a `}'---line 42 of file refs.bib\n";
        let r = parse_bib_log(blg);
        let span = r.diagnostics[0].reported.as_ref().expect("located");
        assert_eq!(span.line, 42);
        assert_eq!(span.file, PathBuf::from("refs.bib"));
    }

    #[test]
    fn biber_errors_retain_the_distinct_producer_and_location() {
        let log = "ERROR - BibTeX subsystem: /work/refs.bib, line 8, syntax error\n";
        let r = parse_tool_log(log, Tool::Biber);
        let d = r.first_error().expect("error");
        assert_eq!(d.source, Source::ToolLog(Tool::Biber));
        assert_eq!(d.kind, Kind::Bibliography);
        assert_eq!(d.reported.as_ref(), Some(&Span::new("/work/refs.bib", 8)));
    }

    #[test]
    fn makeindex_family_locations_and_kinds_are_recovered() {
        let log = "!! Input index error (file = main.idx, line = 3):\n   -- Illegal null field.\n";
        let index = parse_tool_log(log, Tool::MakeIndex);
        let d = index.first_error().expect("error");
        assert_eq!(d.kind, Kind::Index);
        assert_eq!(d.reported.as_ref(), Some(&Span::new("main.idx", 3)));
        assert!(d.message.contains("Illegal null field"));

        let glossary = parse_tool_log(log, Tool::MakeGlossaries);
        assert_eq!(glossary.first_error().expect("error").kind, Kind::Glossary);
        assert_eq!(
            glossary.first_error().expect("error").source,
            Source::ToolLog(Tool::MakeGlossaries)
        );
    }

    #[test]
    fn makeglossaries_wrapper_failures_are_not_lost() {
        let log = "makeglossaries: The file 'main.aux' doesn't exist.\n";
        let report = parse_tool_log(log, Tool::MakeGlossaries);
        let diagnostic = report.first_error().expect("wrapper error");
        assert_eq!(diagnostic.kind, Kind::Glossary);
        assert!(diagnostic.reported.is_none());
    }
}
