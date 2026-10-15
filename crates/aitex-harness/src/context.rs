//! Assembling the minimal context a model needs to repair an error.
//!
//! This is where docs/history/PLAN.md §8's largest measured lever lives. Sending the log
//! alone yields a 16–28% fix rate; adding a snippet of the offending source
//! takes it to 38–47%. Adding worked examples on top moves it to 38–49% — not
//! worth the tokens, so we do not.
//!
//! The other half is *which* snippet. TeX reports where it noticed the problem,
//! which for 43% of our corpus is not where the problem is. So when the
//! structural rules have located a true cause, we send both windows: the model
//! needs to see the symptom and the cause, and they are often hundreds of lines
//! apart.

use crate::structure;
use aitex_core::{Diagnostic, Kind};
use std::fmt::Write as _;

/// Lines of source to include either side of a point of interest.
///
/// Twenty is from the industrial repair study; larger windows cost tokens
/// without improving fix rates.
pub const WINDOW: u32 = 20;

/// A window of source with its line numbering preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    pub label: String,
    pub first_line: u32,
    pub text: String,
}

/// Take lines `[centre - WINDOW, centre + WINDOW]`, 1-indexed.
pub fn window(source: &str, centre: u32, label: &str) -> Excerpt {
    let lines: Vec<&str> = source.lines().collect();
    let centre = centre.max(1) as usize;
    let hi = (centre + WINDOW as usize).min(lines.len());
    // `lo` must not overrun `hi`: TeX reports lines past the end of a file after
    // a runaway argument, and slicing `lines[lo..hi]` with `lo > hi` panics.
    let lo = centre.saturating_sub(WINDOW as usize + 1).min(hi);
    let mut text = String::new();
    for (i, l) in lines[lo..hi].iter().enumerate() {
        let n = lo + i + 1;
        // Numbering matters: the model must be able to name a line in its fix.
        let _ = writeln!(text, "{n:5} | {l}");
    }
    Excerpt {
        label: label.into(),
        first_line: lo as u32 + 1,
        text,
    }
}

/// The preamble's `\usepackage` lines.
///
/// Cheap and high signal: which packages are loaded decides whether a macro is
/// undefined because of a typo or because of a missing package, and it is a
/// handful of lines rather than the whole preamble.
pub fn package_lines(source: &str) -> String {
    let mut out = String::new();
    for (i, l) in source.lines().enumerate() {
        let t = l.trim_start();
        if t.starts_with("\\usepackage")
            || t.starts_with("\\RequirePackage")
            || t.starts_with("\\documentclass")
        {
            let _ = writeln!(out, "{:5} | {}", i + 1, l);
        }
        if t.starts_with("\\begin{document}") {
            break;
        }
    }
    out
}

/// Everything the model gets for one error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairContext {
    /// The clustered, deduplicated diagnostic text.
    pub error: String,
    /// Source windows: the reported site, and the true cause when it differs.
    pub excerpts: Vec<Excerpt>,
    pub packages: String,
    /// What the deterministic layer already worked out, so the model does not
    /// redo it.
    pub structural_note: Option<String>,
}

impl RepairContext {
    /// Rough token count, for budgeting. Deliberately an estimate: real counts
    /// come from the backend's `count_tokens`, never from a local guess.
    pub fn approx_tokens(&self) -> usize {
        let chars: usize = self.error.len()
            + self.packages.len()
            + self.structural_note.as_ref().map_or(0, |s| s.len())
            + self.excerpts.iter().map(|e| e.text.len()).sum::<usize>();
        chars / 4
    }

    /// Render for the prompt.
    pub fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "## Error\n\n{}\n", self.error.trim());
        if let Some(n) = &self.structural_note {
            let _ = writeln!(s, "## Structural analysis\n\n{n}\n");
        }
        if !self.packages.is_empty() {
            let _ = writeln!(s, "## Preamble\n\n```\n{}```\n", self.packages);
        }
        for e in &self.excerpts {
            let _ = writeln!(s, "## {}\n\n```\n{}```\n", e.label, e.text);
        }
        s
    }
}

/// Build the context for one diagnostic.
pub fn assemble(diag: &Diagnostic, source: &str) -> RepairContext {
    let mut excerpts = Vec::new();
    let mut structural_note = None;

    // Where TeX says the problem is.
    if let Some(sp) = &diag.reported {
        excerpts.push(window(source, sp.line, "Source at the reported line"));
    }

    // Where it actually is, when we know and it differs. This is the whole
    // point of the structural pass: without it the model reads the wrong
    // region and cannot possibly fix the error.
    let cause = diag
        .cause
        .as_ref()
        .map(|c| c.line)
        .or_else(|| match &diag.kind {
            Kind::RunawayArgument | Kind::MissingBrace => {
                match structure::unclosed_braces(source).as_slice() {
                    [only] => {
                        structural_note = Some(format!(
                            "Brace matching finds a group opened at line {} that is never \
                             closed. TeX reports no line for this class of error.",
                            only.open.line
                        ));
                        Some(only.open.line)
                    }
                    _ => None,
                }
            }
            Kind::MissingMathShift => match structure::unclosed_math(source).as_slice() {
                [only] => {
                    structural_note = Some(format!(
                        "An odd number of math shifts appears in the paragraph beginning at \
                         line {}.",
                        only.open.line
                    ));
                    Some(only.open.line)
                }
                _ => None,
            },
            Kind::MismatchedEnvironment { began, .. } => {
                let rep = structure::environments(source);
                rep.mismatched
                    .iter()
                    .find(|(_, opened, _)| opened == began)
                    .map(|(pos, opened, closed)| {
                        structural_note = Some(format!(
                            "\\begin{{{opened}}} at line {} is closed by \\end{{{closed}}}.",
                            pos.line
                        ));
                        pos.line
                    })
            }
            _ => None,
        });

    if let Some(line) = cause {
        let already = diag
            .reported
            .as_ref()
            .is_some_and(|sp| line.abs_diff(sp.line) <= WINDOW);
        if !already {
            excerpts.push(window(source, line, "Source at the likely cause"));
        }
    }

    let mut error = String::new();
    let _ = write!(error, "{}", diag.message.trim());
    if diag.count > 1 {
        let _ = write!(error, "\n\n(this error occurs {} times)", diag.count);
    }
    if let Some(sp) = &diag.reported {
        let _ = write!(error, "\nReported at {}:{}", sp.file.display(), sp.line);
    } else {
        let _ = write!(error, "\nTeX reported no source line for this error.");
    }
    if !diag.context.is_empty() {
        let _ = write!(error, "\n\nExpansion context:\n{}", diag.context.join("\n"));
    }

    RepairContext {
        error,
        excerpts,
        packages: package_lines(source),
        structural_note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_core::{Severity, Source, Span};

    fn src(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn window_is_centred_and_numbered() {
        let e = window(&src(100), 50, "x");
        assert_eq!(e.first_line, 30);
        assert!(e.text.contains("   50 | line 50"));
        assert!(e.text.contains("   30 | line 30"));
        assert!(!e.text.contains("   29 | line 29"));
    }

    #[test]
    fn window_clamps_at_both_ends() {
        let e = window(&src(10), 1, "x");
        assert_eq!(e.first_line, 1);
        assert!(e.text.contains("    1 | line 1"));

        let e = window(&src(10), 10, "x");
        assert!(e.text.contains("   10 | line 10"));

        // A line past the end must not panic — TeX reports such lines after a
        // runaway argument, so this is a real input, not a synthetic one.
        let e = window(&src(5), 9999, "x");
        assert!(e.text.is_empty() || e.text.contains("line 5"));
        assert!(window("", 1, "x").text.is_empty());
        for centre in [0, 1, 2, u32::MAX] {
            let _ = window(&src(3), centre, "x");
        }
    }

    #[test]
    fn packages_are_extracted_and_stop_at_begin_document() {
        let s = "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\
                 \\usepackage{late}\n";
        let p = package_lines(s);
        assert!(p.contains("documentclass"));
        assert!(p.contains("amsmath"));
        assert!(!p.contains("late"), "must stop at \\begin{{document}}");
    }

    /// The case the whole structural pass exists for: TeX gives no line, but
    /// brace matching does, so the model gets a window on the real cause.
    #[test]
    fn unlocalizable_runaway_still_gets_a_cause_window() {
        let mut s = String::from("\\documentclass{article}\n\\begin{document}\n");
        s.push_str("\\textbf{unclosed\n");
        for i in 0..80 {
            s.push_str(&format!("filler {i}\n"));
        }
        s.push_str("\\end{document}\n");

        let d = Diagnostic::new(
            Severity::Error,
            Kind::RunawayArgument,
            "File ended while scanning use of \\textbf.",
            Source::TexLog,
        );
        let c = assemble(&d, &s);
        assert!(
            c.structural_note.is_some(),
            "structural analysis should fire"
        );
        assert!(
            c.structural_note.as_ref().expect("note").contains("line 3"),
            "must name the real opener"
        );
        assert_eq!(
            c.excerpts.len(),
            1,
            "no reported line, so only the cause window"
        );
        assert!(c.excerpts[0].text.contains("\\textbf{unclosed"));
        assert!(c.error.contains("no source line"));
    }

    #[test]
    fn distant_cause_produces_a_second_window() {
        let mut s = String::from("\\documentclass{article}\n\\begin{document}\n");
        s.push_str("\\textbf{unclosed\n");
        for i in 0..200 {
            s.push_str(&format!("filler {i}\n"));
        }
        let d = Diagnostic::new(
            Severity::Error,
            Kind::RunawayArgument,
            "Runaway argument?",
            Source::TexLog,
        )
        .at(Span::new("a.tex", 190));
        let c = assemble(&d, &s);
        assert_eq!(c.excerpts.len(), 2, "symptom and cause are far apart");
        assert!(c.excerpts[0].text.contains("filler"));
        assert!(c.excerpts[1].text.contains("\\textbf{unclosed"));
    }

    #[test]
    fn a_nearby_cause_does_not_duplicate_the_window() {
        // Within one window of the reported line, a second excerpt would be
        // the same text twice at double the token cost.
        let s = "\\textbf{x\nmore\n";
        let d = Diagnostic::new(
            Severity::Error,
            Kind::RunawayArgument,
            "Runaway argument?",
            Source::TexLog,
        )
        .at(Span::new("a.tex", 2));
        assert_eq!(assemble(&d, s).excerpts.len(), 1);
    }

    #[test]
    fn repeated_errors_report_their_count_once() {
        let mut d = Diagnostic::new(
            Severity::Error,
            Kind::UndefinedControlSequence { name: "foo".into() },
            "Undefined control sequence.",
            Source::TexLog,
        )
        .at(Span::new("a.tex", 5));
        d.count = 200;
        let c = assemble(&d, &src(20));
        assert!(c.error.contains("occurs 200 times"));
        // 200 occurrences must not become 200 excerpts.
        assert_eq!(c.excerpts.len(), 1);
    }

    #[test]
    fn context_stays_small() {
        // The point of all this is token cost; a 5000-line document must not
        // produce a 5000-line prompt.
        let d = Diagnostic::new(Severity::Error, Kind::Other, "boom", Source::TexLog)
            .at(Span::new("a.tex", 2500));
        let c = assemble(&d, &src(5000));
        assert!(
            c.approx_tokens() < 1500,
            "context should stay small, got ~{} tokens",
            c.approx_tokens()
        );
        assert!(c.render().contains("2500"));
    }
}
