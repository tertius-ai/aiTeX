//! Structured parsing for external semantic-tool transcripts.
//!
//! This lives in the lightweight shared crate so the ordinary build scheduler
//! can reject semantic failures without linking the optional AI/provider stack.

use crate::{
    Diagnostic, Kind, ProvenanceFrame, ProvenanceKind, Report, Severity, Source, Span, Tool,
};

/// Longest line these kpathsea programs normally emit before wrapping.
const DEFAULT_MAX_PRINT_LINE: usize = 79;

fn unwrap_lines(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        if out
            .last()
            .is_some_and(|previous| previous.chars().count() == DEFAULT_MAX_PRINT_LINE)
        {
            out.last_mut().expect("checked non-empty").push_str(line);
        } else {
            out.push(line.to_string());
        }
    }
    out
}

fn tool_kind(tool: Tool) -> Kind {
    match tool {
        Tool::BibTex | Tool::Biber => Kind::Bibliography,
        Tool::MakeIndex => Kind::Index,
        Tool::MakeGlossaries | Tool::Nomenclature => Kind::Glossary,
    }
}

fn trim_path(value: &str) -> &str {
    value.trim_matches(|character: char| {
        character.is_ascii_whitespace()
            || matches!(character, '\'' | '"' | '`' | '(' | ')' | ',' | ':')
    })
}

/// Recover the location forms emitted by the external tools we orchestrate.
fn tool_location(line: &str) -> Option<Span> {
    // BibTeX: `---line 42 of file refs.bib`
    if let Some(rest) = line.split("---line ").nth(1) {
        let number = rest.split_whitespace().next()?.parse().ok()?;
        let file = trim_path(rest.split("of file ").nth(1)?);
        if !file.is_empty() {
            return Some(Span::new(file, number));
        }
    }

    // MakeIndex family: `Input index error (file = main.idx, line = 3):`
    if let Some(rest) = line.split("file = ").nth(1) {
        let file = trim_path(rest.split(',').next()?);
        let line = rest
            .split("line = ")
            .nth(1)?
            .split(|character: char| !character.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        if !file.is_empty() {
            return Some(Span::new(file, line));
        }
    }

    // Biber commonly includes `/path/refs.bib, line 8, ...`.
    if let Some((before, after)) = line.split_once(", line ") {
        let number = after
            .split(|character: char| !character.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        let file = before
            .split_whitespace()
            .rev()
            .map(trim_path)
            .find(|word| {
                word.ends_with(".bib")
                    || word.ends_with(".utf8")
                    || word.ends_with(".idx")
                    || word.ends_with(".glo")
                    || word.ends_with(".nlo")
            })?;
        return Some(Span::new(file, number));
    }

    // Some Biber releases use `line 8 of file 'refs.bib'`.
    if let Some(rest) = line.split("line ").nth(1) {
        let number = rest.split_whitespace().next()?.parse().ok()?;
        if let Some(file) = rest.split("of file ").nth(1).map(trim_path)
            && !file.is_empty()
        {
            return Some(Span::new(file, number));
        }
    }
    None
}

fn tool_line_severity(line: &str, tool: Tool) -> Option<Severity> {
    let text = line.trim();
    match tool {
        Tool::BibTex => {
            if text.starts_with("Warning--") {
                Some(Severity::Warning)
            } else if text.starts_with("I couldn't open")
                || text.starts_with("I was expecting")
                || text.starts_with("Repeated entry")
                || text.contains("---line ")
                || text.starts_with("Sorry---you've exceeded")
            {
                Some(Severity::Error)
            } else {
                None
            }
        }
        Tool::Biber => {
            // The terminal form starts with the level; `.blg` usually prefixes
            // it with a timestamp and module location ending in `> `.
            if text.starts_with("FATAL -") || text.contains("> FATAL -") {
                Some(Severity::Fatal)
            } else if text.starts_with("ERROR -") || text.contains("> ERROR -") {
                Some(Severity::Error)
            } else if text.starts_with("WARN -") || text.contains("> WARN -") {
                Some(Severity::Warning)
            } else {
                None
            }
        }
        Tool::MakeIndex | Tool::MakeGlossaries | Tool::Nomenclature => {
            if text.starts_with("Input index error")
                || text.starts_with("!!")
                || text.starts_with("ERROR")
                || text.starts_with("Unable to")
                || text.starts_with("Cannot ")
                || text.starts_with("Couldn't ")
                || (text.starts_with("makeglossaries:")
                    && (text.contains("doesn't exist") || text.contains("failed")))
            {
                Some(Severity::Error)
            } else if text.starts_with("Warning") {
                Some(Severity::Warning)
            } else {
                None
            }
        }
    }
}

/// Parse the transcript produced by one external semantic build tool.
///
/// The parser keeps original messages while normalizing producer, severity,
/// kind, and source location. Callers must inspect the report even when the
/// child process returned success: MakeIndex may reject entries and exit zero.
pub fn parse_tool_log(raw: &str, tool: Tool) -> Report {
    let mut report = Report {
        engine: tool.as_str().into(),
        ..Default::default()
    };
    let lines = unwrap_lines(raw);
    for (index, line) in lines.iter().enumerate() {
        let text = line.trim();
        let Some(severity) = tool_line_severity(text, tool) else {
            continue;
        };
        let continuation = if text
            .trim_start_matches("!! ")
            .starts_with("Input index error")
        {
            lines
                .get(index + 1)
                .map(|next| next.trim())
                .filter(|next| next.starts_with("--"))
        } else {
            None
        };
        let message =
            continuation.map_or_else(|| text.to_string(), |next| format!("{text} {next}"));
        let mut diagnostic =
            Diagnostic::new(severity, tool_kind(tool), message, Source::ToolLog(tool));
        if let Some(span) = tool_location(text) {
            diagnostic = diagnostic.at(span);
        }
        diagnostic.provenance.push(ProvenanceFrame::new(
            ProvenanceKind::SemanticTool,
            tool.as_str(),
        ));
        if let Some(span) = diagnostic.reported.as_ref()
            && matches!(
                span.file
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some(
                    "aux"
                        | "bbl"
                        | "bcf"
                        | "idx"
                        | "ind"
                        | "ilg"
                        | "glo"
                        | "gls"
                        | "glg"
                        | "acn"
                        | "acr"
                        | "alg"
                        | "nlo"
                        | "nls"
                        | "nlg"
                )
            )
        {
            diagnostic.provenance.push(
                ProvenanceFrame::new(
                    ProvenanceKind::GeneratedArtifact,
                    span.file.to_string_lossy(),
                )
                .at(span.clone()),
            );
        }
        report.diagnostics.push(diagnostic);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn makeindex_exit_zero_error_form_is_structured() {
        let log = "!! Input index error (file = main.idx, line = 3):\n   -- Illegal null field.\n";
        let report = parse_tool_log(log, Tool::MakeIndex);
        let diagnostic = report.first_error().expect("error");
        assert_eq!(diagnostic.kind, Kind::Index);
        assert_eq!(
            diagnostic.reported.as_ref(),
            Some(&Span::new("main.idx", 3))
        );
        assert!(diagnostic.message.contains("Illegal null field"));
    }

    #[test]
    fn bibliography_producers_remain_distinct() {
        let bibtex = parse_tool_log(
            "I was expecting a `,' or a `}'---line 42 of file refs.bib\n",
            Tool::BibTex,
        );
        assert_eq!(
            bibtex.first_error().expect("error").reported.as_ref(),
            Some(&Span::new(PathBuf::from("refs.bib"), 42))
        );
        let biber = parse_tool_log(
            "ERROR - BibTeX subsystem: /work/refs.bib, line 8, syntax error\n",
            Tool::Biber,
        );
        assert_eq!(
            biber.first_error().expect("error").source,
            Source::ToolLog(Tool::Biber)
        );
        let temporary = parse_tool_log(
            "[95] Utils.pm:399> ERROR - BibTeX subsystem: /tmp/biber_tmp/a.utf8, line 9, syntax error\n",
            Tool::Biber,
        );
        assert_eq!(
            temporary.first_error().expect("error").reported.as_ref(),
            Some(&Span::new("/tmp/biber_tmp/a.utf8", 9))
        );
        let prefixed = parse_tool_log(
            "[95] Biber.pm:130> ERROR - Cannot find 'refs.bib'!\n",
            Tool::Biber,
        );
        assert!(prefixed.first_error().is_some());
    }
}
