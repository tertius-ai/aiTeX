//! Cheap structural scan of a project's sources.
//!
//! Supplies the [`Context`](crate::fixes::Context) the fix rules need: which
//! macros this document defines, and which packages it loads. Without it,
//! spelling correction has no candidates and the largest error category —
//! undefined control sequences — goes unrepaired.
//!
//! Deliberately a scanner, not a parser. It reads definitions and
//! `\usepackage` lines, which is a regular-enough sublanguage to handle
//! textually. The real tree (H4) is for *localization*, a different job.

use crate::fixes::Context;
use std::collections::BTreeSet;
use std::path::Path;

/// Names a document defines for itself.
///
/// Covers the forms that actually appear: `\newcommand`, `\renewcommand`,
/// `\providecommand`, `\def`, `\let`, `\DeclareMathOperator`, `\newenvironment`
/// (which defines `\foo` and `\endfoo`), and `\newtheorem`.
pub fn defined_macros(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b: Vec<char> = source.chars().collect();
    let mut i = 0;

    while i < b.len() {
        if b[i] != '\\' {
            i += 1;
            continue;
        }
        let name = read_cs_name(&b, i + 1);
        let after = i + 1 + name.chars().count();
        match name.as_str() {
            "newcommand" | "renewcommand" | "providecommand" | "DeclareRobustCommand" => {
                if let Some(m) = read_braced_cs(&b, after) {
                    out.insert(m);
                }
            }
            "def" | "gdef" | "edef" | "xdef" | "let" => {
                // `\def\foo` — the name follows immediately.
                let j = skip_spaces(&b, after);
                if b.get(j) == Some(&'\\') {
                    let m = read_cs_name(&b, j + 1);
                    if !m.is_empty() {
                        out.insert(m);
                    }
                }
            }
            "DeclareMathOperator" => {
                // May carry a `*`; the name is in the first braced group.
                if let Some(m) = read_braced_cs(&b, after) {
                    out.insert(m);
                }
            }
            "newenvironment" | "renewenvironment" | "newtheorem" => {
                // These take a *name*, not a control sequence, and define both
                // `\name` and `\endname`.
                if let Some(n) = read_braced_word(&b, after) {
                    out.insert(n.clone());
                    out.insert(format!("end{n}"));
                }
            }
            _ => {}
        }
        i = after.max(i + 1);
    }
    out
}

/// Packages named in `\usepackage[...]{a,b}` and `\RequirePackage`.
pub fn loaded_packages(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for marker in ["\\usepackage", "\\RequirePackage"] {
        let mut from = 0;
        while let Some(p) = source[from..].find(marker) {
            let start = from + p + marker.len();
            let b: Vec<char> = source[start..].chars().take(400).collect();
            if let Some(list) = read_braced_word(&b, skip_optional_arg(&b, 0)) {
                for name in list.split(',') {
                    let n = name.trim();
                    if !n.is_empty() {
                        out.insert(n.to_string());
                    }
                }
            }
            from = start;
        }
    }
    out
}

fn read_cs_name(b: &[char], from: usize) -> String {
    let mut s = String::new();
    let mut i = from;
    while i < b.len() && (b[i].is_alphabetic() || b[i] == '@') {
        s.push(b[i]);
        i += 1;
    }
    s
}

fn skip_spaces(b: &[char], mut i: usize) -> usize {
    while i < b.len() && (b[i] == ' ' || b[i] == '\t') {
        i += 1;
    }
    i
}

/// Step over `[...]`, and over a `*`, if present.
fn skip_optional_arg(b: &[char], mut i: usize) -> usize {
    i = skip_spaces(b, i);
    if b.get(i) == Some(&'*') {
        i = skip_spaces(b, i + 1);
    }
    if b.get(i) == Some(&'[') {
        let mut depth = 1;
        i += 1;
        while i < b.len() && depth > 0 {
            match b[i] {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
    }
    skip_spaces(b, i)
}

/// Read `{\foo}` and return `foo`.
fn read_braced_cs(b: &[char], from: usize) -> Option<String> {
    let i = skip_optional_arg(b, from);
    // `\newcommand\foo{...}` — braces around the name are optional.
    if b.get(i) == Some(&'\\') {
        let n = read_cs_name(b, i + 1);
        return (!n.is_empty()).then_some(n);
    }
    if b.get(i) != Some(&'{') {
        return None;
    }
    let j = skip_spaces(b, i + 1);
    if b.get(j) != Some(&'\\') {
        return None;
    }
    let n = read_cs_name(b, j + 1);
    (!n.is_empty()).then_some(n)
}

/// Read `{word}` and return `word`.
fn read_braced_word(b: &[char], from: usize) -> Option<String> {
    let i = skip_optional_arg(b, from);
    if b.get(i) != Some(&'{') {
        return None;
    }
    let mut s = String::new();
    let mut j = i + 1;
    while j < b.len() && b[j] != '}' {
        s.push(b[j]);
        j += 1;
    }
    (b.get(j) == Some(&'}')).then_some(s)
}

/// Scan every `.tex`/`.sty` beside `root` and build a fix [`Context`].
pub fn context_for_project(root: &Path) -> Context {
    let dir = root.parent().unwrap_or(Path::new("."));
    // Start from what LaTeX itself defines: without this, a misspelling of a
    // kernel macro has nothing to be corrected towards.
    let mut macros: BTreeSet<String> = crate::kernel::KERNEL_MACROS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut packages = BTreeSet::new();

    let mut visit = |p: &Path| {
        if let Some(text) = crate::compile::read_log(p) {
            macros.extend(defined_macros(&text));
            packages.extend(loaded_packages(&text));
        }
    };

    visit(root);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p == root {
                continue;
            }
            if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("tex") | Some("sty")
            ) {
                visit(&p);
            }
        }
    }

    Context {
        known_macros: macros.into_iter().collect(),
        loaded_packages: packages.into_iter().collect(),
        pipeline_converged: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_newcommand_in_both_spellings() {
        let m = defined_macros("\\newcommand{\\foo}[2]{x}\n\\newcommand\\bar{y}\n");
        assert!(m.contains("foo"));
        assert!(m.contains("bar"));
    }

    #[test]
    fn finds_def_and_let() {
        let m = defined_macros("\\def\\alpha{a}\\gdef\\beta{b}\\let\\gamma\\alpha");
        for n in ["alpha", "beta", "gamma"] {
            assert!(m.contains(n), "missing {n}");
        }
    }

    #[test]
    fn finds_at_macros() {
        // Package internals use @ in names; \makeatletter makes them legal.
        let m = defined_macros("\\def\\my@helper{x}");
        assert!(m.contains("my@helper"));
    }

    #[test]
    fn newenvironment_defines_both_ends() {
        let m = defined_macros("\\newenvironment{thm}{a}{b}");
        assert!(m.contains("thm"));
        assert!(m.contains("endthm"), "\\end{{thm}} needs \\endthm to exist");
    }

    #[test]
    fn declaremathoperator_handles_the_star_form() {
        let m = defined_macros("\\DeclareMathOperator*{\\argmin}{arg\\,min}");
        assert!(m.contains("argmin"));
    }

    #[test]
    fn reads_package_lists_and_optional_args() {
        let p = loaded_packages(
            "\\usepackage{amsmath}\n\\usepackage[utf8]{inputenc}\n\\usepackage{a,b, c}\n",
        );
        for n in ["amsmath", "inputenc", "a", "b", "c"] {
            assert!(p.contains(n), "missing {n}");
        }
    }

    #[test]
    fn requirepackage_counts_too() {
        let p = loaded_packages("\\RequirePackage{xkeyval}");
        assert!(p.contains("xkeyval"));
    }

    #[test]
    fn ignores_prose_that_merely_mentions_commands() {
        // A scanner must not treat documentation as definitions.
        let m = defined_macros("We use \\emph{foo} and \\textbf{bar} throughout.");
        assert!(m.is_empty(), "got {m:?}");
    }

    #[test]
    fn survives_truncated_and_malformed_input() {
        for src in [
            "\\newcommand{",
            "\\def",
            "\\usepackage[",
            "\\newcommand{\\",
            "\\",
        ] {
            let _ = defined_macros(src);
            let _ = loaded_packages(src);
        }
    }
}
