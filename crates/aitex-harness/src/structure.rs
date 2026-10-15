//! Locally context-free structure recovery.
//!
//! TeX as a whole is not context-free — `\catcode` mutates at runtime, so you
//! cannot know where token *n+1* begins without processing *1..n*. But that is
//! a statement about the *whole* language. Within a region where the catcodes
//! are stable (which is almost every real document, almost all of the time),
//! the nesting of `{}`, `\begin`/`\end`, and `$` is an ordinary bracket
//! language: unambiguously context-free, and matchable with a stack.
//!
//! That distinction is worth exploiting, because it is exactly the class of
//! error the log cannot localize. When TeX says
//! `! File ended while scanning use of \textbf` with no line number, a bracket
//! matcher can still point at the `{` that was never closed. This turns the
//! largest unlocalizable category into a deterministic, zero-token repair.
//!
//! **Where this stops being valid** is recorded honestly in [`Confidence`]:
//! verbatim environments, `\catcode` changes, and `%`-comments all break the
//! simple reading, so we detect those and downgrade rather than assert.

use std::collections::BTreeSet;

/// A position in a source file: 1-indexed line, 0-indexed column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

/// How much to trust a structural finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trust {
    /// Something in the region defeats simple scanning — a verbatim
    /// environment, or a `\catcode` assignment that could redefine grouping.
    Unreliable,
    /// Ordinary text; the bracket reading is sound.
    Sound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unclosed {
    pub open: Pos,
    /// Environment name, for `\begin`; `None` for a brace or math shift.
    pub name: Option<String>,
    pub trust: Trust,
}

/// Regions where the plain bracket reading does not hold.
///
/// `verbatim`, `lstlisting`, `minted` and friends deliberately change catcodes
/// so their contents are *not* TeX syntax; counting braces inside them is
/// meaningless.
const VERBATIM_ENVS: &[&str] = &[
    "verbatim",
    "Verbatim",
    "lstlisting",
    "minted",
    "alltt",
    "comment",
    "verbatim*",
    "filecontents",
    "filecontents*",
];

/// One line of source, stripped of anything the scanner must not read.
///
/// Removes comments (`%` to end of line, unless escaped) and the *contents* of
/// verbatim regions, while preserving line and column numbering so reported
/// positions still point at the real source.
struct Scanner<'a> {
    src: &'a str,
    /// True once a `\catcode` assignment is seen: after that, any conclusion
    /// about grouping characters is suspect.
    catcode_touched: bool,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            catcode_touched: src.contains("\\catcode"),
        }
    }

    fn trust(&self) -> Trust {
        if self.catcode_touched {
            Trust::Unreliable
        } else {
            Trust::Sound
        }
    }

    /// Yield `(line, col, char)` for every character that is live TeX syntax.
    fn live_chars(&self) -> Vec<(u32, u32, char)> {
        let mut out = Vec::new();
        let mut verbatim_depth = 0usize;

        for (li, line) in self.src.lines().enumerate() {
            let line_no = li as u32 + 1;
            let chars: Vec<char> = line.chars().collect();

            // Verbatim boundaries are recognized even while inside one, since
            // that is how the region ends.
            if let Some(env) = env_delimiter(line, "\\begin")
                && VERBATIM_ENVS.contains(&env.as_str())
            {
                verbatim_depth += 1;
                continue;
            }
            if let Some(env) = env_delimiter(line, "\\end")
                && VERBATIM_ENVS.contains(&env.as_str())
            {
                verbatim_depth = verbatim_depth.saturating_sub(1);
                continue;
            }
            if verbatim_depth > 0 {
                continue;
            }

            let mut i = 0;
            while i < chars.len() {
                let c = chars[i];
                if c == '\\' {
                    // An escaped character is literal: `\{` is not a group.
                    // Skip the escape *and* what it escapes.
                    i += 2;
                    continue;
                }
                if c == '%' {
                    break; // comment to end of line
                }
                out.push((line_no, i as u32, c));
                i += 1;
            }
        }
        out
    }
}

/// Extract `X` from a line containing `\begin{X}` / `\end{X}`.
fn env_delimiter(line: &str, marker: &str) -> Option<String> {
    let p = line.find(marker)?;
    let rest = &line[p + marker.len()..];
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('{')?;
    let end = inner.find('}')?;
    Some(inner[..end].to_string())
}

/// Find brace groups opened but never closed.
///
/// Returns openers in source order. An extra `}` is reported separately by
/// [`extra_close_braces`].
pub fn unclosed_braces(src: &str) -> Vec<Unclosed> {
    let sc = Scanner::new(src);
    let trust = sc.trust();
    let mut stack: Vec<Pos> = Vec::new();

    for (line, col, c) in sc.live_chars() {
        match c {
            '{' => stack.push(Pos { line, col }),
            '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
        .into_iter()
        .map(|open| Unclosed {
            open,
            name: None,
            trust,
        })
        .collect()
}

/// Positions of `}` with no matching `{`.
pub fn extra_close_braces(src: &str) -> Vec<Pos> {
    let sc = Scanner::new(src);
    let mut depth = 0i64;
    let mut extra = Vec::new();
    for (line, col, c) in sc.live_chars() {
        match c {
            '{' => depth += 1,
            '}' => {
                if depth == 0 {
                    extra.push(Pos { line, col });
                } else {
                    depth -= 1;
                }
            }
            _ => {}
        }
    }
    extra
}

/// Environments opened but never closed, and `\end` without a matching
/// `\begin`.
///
/// The stack discipline also catches *crossed* nesting
/// (`\begin{a}\begin{b}\end{a}\end{b}`), which a naive count cannot.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnvReport {
    pub unclosed: Vec<Unclosed>,
    /// `\end{x}` with no open `x`.
    pub unmatched_end: Vec<(Pos, String)>,
    /// `\begin{a}` closed by `\end{b}` — reports the opener and both names.
    pub mismatched: Vec<(Pos, String, String)>,
}

pub fn environments(src: &str) -> EnvReport {
    let sc = Scanner::new(src);
    let trust = sc.trust();
    let mut stack: Vec<(Pos, String)> = Vec::new();
    let mut rep = EnvReport::default();
    let mut verbatim_depth = 0usize;

    for (li, line) in src.lines().enumerate() {
        let line_no = li as u32 + 1;
        // Strip a trailing comment before looking for delimiters.
        let live = match line.find('%') {
            Some(p) if p == 0 || !line[..p].ends_with('\\') => &line[..p],
            _ => line,
        };

        if let Some(env) = env_delimiter(live, "\\begin") {
            if VERBATIM_ENVS.contains(&env.as_str()) {
                verbatim_depth += 1;
                continue;
            }
            if verbatim_depth == 0 {
                let col = live.find("\\begin").unwrap_or(0) as u32;
                stack.push((Pos { line: line_no, col }, env));
            }
            continue;
        }
        if let Some(env) = env_delimiter(live, "\\end") {
            if VERBATIM_ENVS.contains(&env.as_str()) {
                verbatim_depth = verbatim_depth.saturating_sub(1);
                continue;
            }
            if verbatim_depth > 0 {
                continue;
            }
            let col = live.find("\\end").unwrap_or(0) as u32;
            match stack.pop() {
                Some((_open, opened)) if opened == env => {}
                Some((open, opened)) => {
                    rep.mismatched.push((open, opened, env));
                }
                None => rep.unmatched_end.push((Pos { line: line_no, col }, env)),
            }
        }
    }

    rep.unclosed = stack
        .into_iter()
        .map(|(open, name)| Unclosed {
            open,
            name: Some(name),
            trust,
        })
        .collect();
    rep
}

/// Paragraphs containing an odd number of `$`, i.e. math mode left open.
///
/// TeX closes math at the paragraph boundary, so parity per paragraph is the
/// right granularity: an odd count means the opener on that line never closed.
/// `$$` (display math) is counted as a single delimiter.
pub fn unclosed_math(src: &str) -> Vec<Unclosed> {
    let sc = Scanner::new(src);
    let trust = sc.trust();
    let live = sc.live_chars();

    let mut out = Vec::new();
    let mut open: Option<Pos> = None;
    let mut i = 0;
    let mut blank_lines: BTreeSet<u32> = BTreeSet::new();
    for (li, l) in src.lines().enumerate() {
        if l.trim().is_empty() {
            blank_lines.insert(li as u32 + 1);
        }
    }

    while i < live.len() {
        let (line, col, c) = live[i];
        if c == '$' {
            // Collapse `$$` into one delimiter.
            let step = if live
                .get(i + 1)
                .is_some_and(|&(l2, c2, ch)| ch == '$' && l2 == line && c2 == col + 1)
            {
                2
            } else {
                1
            };
            open = match open {
                None => Some(Pos { line, col }),
                Some(_) => None,
            };
            i += step;
            continue;
        }
        // A blank line ends the paragraph: unclosed math cannot cross it.
        if let Some(o) = open
            && blank_lines.contains(&line)
        {
            out.push(Unclosed {
                open: o,
                name: None,
                trust,
            });
            open = None;
        }
        i += 1;
    }
    if let Some(o) = open {
        out.push(Unclosed {
            open: o,
            name: None,
            trust,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_brace_that_was_never_closed() {
        // The case TeX reports with no line number at all.
        let src = "\\documentclass{article}\n\\begin{document}\n\\textbf{unclosed\nmore text\n\\end{document}\n";
        let u = unclosed_braces(src);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].open.line, 3, "must point at the real opener");
        assert_eq!(u[0].trust, Trust::Sound);
    }

    #[test]
    fn balanced_source_reports_nothing() {
        let src = "\\textbf{a} \\emph{b {c} d}\n";
        assert!(unclosed_braces(src).is_empty());
        assert!(extra_close_braces(src).is_empty());
    }

    #[test]
    fn escaped_braces_are_literal_not_grouping() {
        // `\{` is a printed brace; counting it would produce a phantom error.
        let src = "the set \\{1,2,3\\} is finite\n";
        assert!(
            unclosed_braces(src).is_empty(),
            "escaped braces must not count"
        );
    }

    #[test]
    fn comments_are_ignored() {
        let src = "text % this { is not a real brace\nmore\n";
        assert!(unclosed_braces(src).is_empty());
    }

    #[test]
    fn extra_closing_brace_is_located() {
        let src = "a}\n";
        let e = extra_close_braces(src);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].line, 1);
    }

    #[test]
    fn verbatim_contents_do_not_count() {
        // Braces inside verbatim are data, not syntax.
        let src = "\\begin{verbatim}\nif (x) { y\n\\end{verbatim}\ntext\n";
        assert!(
            unclosed_braces(src).is_empty(),
            "verbatim body must be skipped"
        );
    }

    #[test]
    fn catcode_changes_downgrade_trust() {
        // We can still guess, but must not claim certainty.
        let src = "\\catcode`\\@=11\n\\textbf{x\n";
        let u = unclosed_braces(src);
        assert_eq!(u.len(), 1);
        assert_eq!(
            u[0].trust,
            Trust::Unreliable,
            "grouping may have been redefined"
        );
    }

    #[test]
    fn finds_unclosed_environment_with_its_name() {
        let src = "\\begin{document}\n\\begin{itemize}\n\\item a\n\\end{document}\n";
        let r = environments(src);
        // itemize was closed by \end{document}: a mismatch, not an unclosed.
        assert_eq!(r.mismatched.len(), 1);
        let (pos, opened, closed) = &r.mismatched[0];
        assert_eq!(pos.line, 2);
        assert_eq!(opened, "itemize");
        assert_eq!(closed, "document");
    }

    #[test]
    fn detects_a_genuinely_unclosed_environment() {
        let src = "\\begin{itemize}\n\\item a\n";
        let r = environments(src);
        assert_eq!(r.unclosed.len(), 1);
        assert_eq!(r.unclosed[0].name.as_deref(), Some("itemize"));
        assert_eq!(r.unclosed[0].open.line, 1);
    }

    #[test]
    fn detects_end_without_begin() {
        let r = environments("text\n\\end{itemize}\n");
        assert_eq!(r.unmatched_end.len(), 1);
        assert_eq!(r.unmatched_end[0].1, "itemize");
    }

    #[test]
    fn crossed_nesting_is_caught() {
        // A counting approach sees two begins and two ends and is satisfied;
        // a stack sees the crossing.
        let r = environments("\\begin{a}\n\\begin{b}\n\\end{a}\n\\end{b}\n");
        assert!(!r.mismatched.is_empty(), "crossed nesting must be reported");
    }

    #[test]
    fn verbatim_environments_do_not_nest_into_the_stack() {
        let src = "\\begin{document}\n\\begin{verbatim}\n\\begin{itemize}\n\\end{verbatim}\n\\end{document}\n";
        let r = environments(src);
        assert!(r.unclosed.is_empty(), "itemize inside verbatim is not real");
        assert!(r.mismatched.is_empty());
    }

    #[test]
    fn finds_unclosed_math() {
        let src = "text $x + y\n\nnext paragraph\n";
        let m = unclosed_math(src);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].open.line, 1);
    }

    #[test]
    fn balanced_math_reports_nothing() {
        assert!(unclosed_math("$a$ and $b$ and $$c$$\n").is_empty());
    }

    #[test]
    fn escaped_dollar_is_not_math() {
        assert!(unclosed_math("costs \\$5 today\n").is_empty());
    }

    #[test]
    fn never_panics_on_hostile_input() {
        for src in [
            "{",
            "}",
            "$",
            "\\",
            "\\begin{",
            "\\end{}",
            "%",
            "$$$",
            "\\begin{verbatim}",
            "{{{{{{",
            "}}}}}}",
            "",
        ] {
            let _ = unclosed_braces(src);
            let _ = extra_close_braces(src);
            let _ = environments(src);
            let _ = unclosed_math(src);
        }
    }
}
