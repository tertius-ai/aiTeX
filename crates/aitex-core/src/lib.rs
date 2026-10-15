//! Shared vocabulary for describing LaTeX compilation problems.
//!
//! Both halves of aiTeX speak this: the engine emits [`Diagnostic`] directly,
//! and the harness produces the same type by parsing a stock `pdflatex` log.
//! Keeping it in a crate of its own is docs/history/PLAN.md decision 6 — the harness must
//! describe an error identically whichever engine produced it, and a shared
//! vocabulary cannot be bolted on later.

use std::fmt;
use std::path::{Path, PathBuf};

mod tool_log;
pub use tool_log::parse_tool_log;

/// A location in a source file. Lines and columns are 1-indexed to match TeX.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: PathBuf,
    pub line: u32,
    /// Column, when known. Log scraping rarely recovers this; the engine can.
    pub col: Option<u32>,
    pub len: Option<u32>,
}

impl Span {
    pub fn new(file: impl Into<PathBuf>, line: u32) -> Self {
        Self {
            file: file.into(),
            line,
            col: None,
            len: None,
        }
    }

    pub fn with_col(mut self, col: u32, len: u32) -> Self {
        self.col = Some(col);
        self.len = Some(len);
        self
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file.display(), self.line)?;
        if let Some(c) = self.col {
            write!(f, ":{c}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Overfull/underfull boxes: not errors, but the harness can act on them.
    BadBox,
    Warning,
    Error,
    /// TeX gave up entirely (capacity exceeded, emergency stop).
    Fatal,
}

/// What kind of problem this is. Drives which deterministic fix rules apply,
/// so it is deliberately coarser than the raw TeX message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Kind {
    UndefinedControlSequence {
        name: String,
    },
    UndefinedReference {
        label: String,
    },
    UndefinedCitation {
        key: String,
    },
    MissingFile {
        name: String,
    },
    MissingPackage {
        name: String,
    },
    /// `\begin{X}` where `X` was never declared. Distinct from a mismatch:
    /// the environment does not exist at all, usually because a `\newtheorem`
    /// or the package declaring it is missing.
    UndefinedEnvironment {
        name: String,
    },
    /// `No counter 'X' defined` — the `\newtheorem` that would create it is
    /// absent, so this travels with [`Kind::UndefinedEnvironment`].
    UndefinedCounter {
        name: String,
    },
    /// An error raised by a package's own handler rather than by TeX, e.g.
    /// `Xy-pic error: ...` or `Package tikz Error: ...`. Carries the package so
    /// fixes can be attributed and looked up.
    PackageError {
        package: String,
    },
    /// `\begin{X} ended by \end{Y}`.
    MismatchedEnvironment {
        began: String,
        ended: String,
    },
    RunawayArgument,
    MissingMathShift,
    MissingBrace,
    ExtraAlignmentTab,
    CapacityExceeded {
        resource: String,
    },
    /// Overfull by `sp` scaled points. TeX's own unit: 65536 sp = 1 pt, and it
    /// is an exact integer, so this stays `Eq`/`Hash` and never drifts.
    OverfullBox {
        sp: i64,
        lines: Option<(u32, u32)>,
    },
    UnderfullBox {
        badness: Option<u32>,
        lines: Option<(u32, u32)>,
    },
    /// Recognized as a bibliography-tool error (from `.blg`, never `.log`).
    Bibliography,
    /// Recognized as an index-tool error (normally from `.ilg`).
    Index,
    /// Recognized as a glossary, acronym, or nomenclature-tool error.
    Glossary,
    RerunNeeded,
    /// Parsed but unclassified. Carries the raw message so nothing is lost.
    Other,
}

impl Kind {
    /// Stable, detail-free name used by diagnostic and benchmark schemas.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::UndefinedControlSequence { .. } => "undefined-control-sequence",
            Kind::UndefinedReference { .. } => "undefined-reference",
            Kind::UndefinedCitation { .. } => "undefined-citation",
            Kind::MissingFile { .. } => "missing-file",
            Kind::MissingPackage { .. } => "missing-package",
            Kind::UndefinedEnvironment { .. } => "undefined-environment",
            Kind::UndefinedCounter { .. } => "undefined-counter",
            Kind::PackageError { .. } => "package-error",
            Kind::MismatchedEnvironment { .. } => "mismatched-environment",
            Kind::RunawayArgument => "runaway-argument",
            Kind::MissingMathShift => "missing-math-shift",
            Kind::MissingBrace => "missing-brace",
            Kind::ExtraAlignmentTab => "extra-alignment-tab",
            Kind::CapacityExceeded { .. } => "capacity-exceeded",
            Kind::OverfullBox { .. } => "overfull-box",
            Kind::UnderfullBox { .. } => "underfull-box",
            Kind::Bibliography => "bibliography",
            Kind::Index => "index",
            Kind::Glossary => "glossary",
            Kind::RerunNeeded => "rerun-needed",
            Kind::Other => "other",
        }
    }
}

/// Program/phase that produced a non-TeX build diagnostic.
///
/// These are separate even where they share an output suffix: BibTeX and Biber
/// both write `.blg`, but their messages, source locations, and repair advice
/// are not interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tool {
    BibTex,
    Biber,
    MakeIndex,
    MakeGlossaries,
    Nomenclature,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::BibTex => "bibtex",
            Tool::Biber => "biber",
            Tool::MakeIndex => "makeindex",
            Tool::MakeGlossaries => "makeglossaries",
            Tool::Nomenclature => "nomenclature",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bibtex" => Some(Tool::BibTex),
            "biber" => Some(Tool::Biber),
            "makeindex" => Some(Tool::MakeIndex),
            "makeglossaries" => Some(Tool::MakeGlossaries),
            "nomenclature" => Some(Tool::Nomenclature),
            _ => None,
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a diagnostic came from. The harness treats `.blg` findings differently
/// from `.log` ones, and forgetting the distinction misdiagnoses every
/// bibliography failure as a citation problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    TexLog,
    ToolLog(Tool),
    Engine,
}

/// One causal step between author source and the diagnostic a producer emits.
///
/// The variants are deliberately producer-neutral: engine errors, generated
/// semantic-tool inputs, and multi-pass build failures can therefore expose
/// the same ordered chain to the terminal, repair protocol, and benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProvenanceKind {
    FailingToken,
    MacroExpansion,
    MacroArgument,
    MacroDefinition,
    Include,
    GeneratedArtifact,
    GeneratedOrigin,
    SemanticTool,
    CompilationPass,
}

impl ProvenanceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailingToken => "failing-token",
            Self::MacroExpansion => "macro-expansion",
            Self::MacroArgument => "macro-argument",
            Self::MacroDefinition => "macro-definition",
            Self::Include => "include",
            Self::GeneratedArtifact => "generated-artifact",
            Self::GeneratedOrigin => "generated-origin",
            Self::SemanticTool => "semantic-tool",
            Self::CompilationPass => "compilation-pass",
        }
    }
}

/// A bounded, source-mapped diagnostic provenance frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceFrame {
    pub kind: ProvenanceKind,
    /// Compact human-readable identity, such as `\\foo`, `main.idx`, or
    /// `TeX pass 2`.
    pub label: String,
    pub span: Option<Span>,
    /// Optional bounded evidence such as the captured macro argument.
    pub detail: Option<String>,
}

impl ProvenanceFrame {
    pub fn new(kind: ProvenanceKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            span: None,
            detail: None,
        }
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn summary(&self) -> String {
        let mut summary = format!("{} · {}", self.kind.as_str(), self.label);
        if let Some(span) = &self.span {
            summary.push_str(" · ");
            summary.push_str(&span.to_string());
        }
        if let Some(detail) = &self.detail
            && !detail.trim().is_empty()
        {
            summary.push_str(" · ");
            summary.push_str(&compact_whitespace(detail));
        }
        summary
    }
}

impl Source {
    /// Stable phase name for serialized diagnostics and compact UI output.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::TexLog => "tex",
            Source::ToolLog(tool) => tool.as_str(),
            Source::Engine => "aitex-engine",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub kind: Kind,
    /// Where the *engine says* the problem is. Frequently not where the cause
    /// is — see [`Diagnostic::cause`].
    pub reported: Option<Span>,
    /// Where the problem actually originates, once something smarter than the
    /// log has weighed in (harness H4). `None` until then.
    pub cause: Option<Span>,
    /// The raw message, verbatim, so no information is lost by classification.
    pub message: String,
    pub source: Source,
    /// TeX's expansion context lines (`l.42 \foo`), when present.
    pub context: Vec<String>,
    /// Ordered causal evidence, nearest the failure first. Producers keep this
    /// bounded; consumers must preserve the order.
    pub provenance: Vec<ProvenanceFrame>,
    /// How many identical diagnostics were collapsed into this one. Clustering
    /// is the first token-saving stage in docs/history/PLAN.md §8.
    pub count: u32,
}

/// The canonical, ANSI-free rendering of a diagnostic.
///
/// Terminal output, model requests, and recorded repair data all use this
/// representation so the useful human-facing summary does not get lost at a
/// serialization boundary. The structured [`Diagnostic`] remains the source
/// of truth; this is a stable presentation derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticPresentation {
    /// Producer, classified kind, and best-known source location.
    pub headline: String,
    /// The producer's message with multiline whitespace compacted.
    pub error: String,
    /// Original reported location when it differs from the identified cause.
    pub reported: Option<String>,
    /// Number of equivalent diagnostics represented by this one.
    pub occurrences: u32,
    /// Canonical compact provenance lines, shared by humans and models.
    pub provenance: Vec<String>,
}

impl DiagnosticPresentation {
    /// Render the same compact block used as model-visible diagnostic context.
    pub fn plain_text(&self) -> String {
        use fmt::Write as _;

        let mut rendered = format!("Diagnose  {}\nError     {}", self.headline, self.error);
        if let Some(reported) = &self.reported {
            let _ = write!(rendered, "\nReported  {reported}");
        }
        if self.occurrences > 1 {
            let _ = write!(rendered, "\nRepeated  {} occurrences", self.occurrences);
        }
        for (index, frame) in self.provenance.iter().enumerate() {
            let label = if index == 0 { "Trace" } else { "" };
            let _ = write!(rendered, "\n{label:<9} {frame}");
        }
        rendered
    }
}

impl Diagnostic {
    pub fn new(severity: Severity, kind: Kind, message: impl Into<String>, source: Source) -> Self {
        Self {
            severity,
            kind,
            reported: None,
            cause: None,
            message: message.into(),
            source,
            context: Vec::new(),
            provenance: Vec::new(),
            count: 1,
        }
    }

    pub fn at(mut self, span: Span) -> Self {
        self.reported = Some(span);
        self
    }

    /// Best known location: the identified cause if we have one, else whatever
    /// the engine reported.
    pub fn location(&self) -> Option<&Span> {
        self.cause.as_ref().or(self.reported.as_ref())
    }

    /// Produce the single compact diagnostic presentation shared by the CLI,
    /// repair protocol, and repair-session records.
    pub fn presentation(&self) -> DiagnosticPresentation {
        let mut headline = format!("{} · {}", self.source.as_str(), self.kind.name());
        if let Some(location) = self.location() {
            headline.push_str(" · ");
            headline.push_str(&location.to_string());
        }
        let reported = match (&self.reported, &self.cause) {
            (Some(reported), Some(cause)) if reported != cause => Some(reported.to_string()),
            _ => None,
        };
        DiagnosticPresentation {
            headline,
            error: compact_whitespace(&self.message),
            reported,
            occurrences: self.count,
            provenance: self
                .provenance
                .iter()
                .take(12)
                .map(ProvenanceFrame::summary)
                .collect(),
        }
    }

    /// Distance between where the error was reported and where it truly is.
    ///
    /// This is the metric the corpus is stratified on: log-only parsing
    /// degrades as it grows, and it is how we test whether the AST layer
    /// earns its complexity (docs/history/PLAN.md §9).
    pub fn gap(&self) -> Gap {
        match (&self.reported, &self.cause) {
            (Some(r), Some(c)) if r.file == c.file => Gap::Lines(r.line.abs_diff(c.line)),
            (Some(_), Some(_)) => Gap::DifferentFile,
            _ => Gap::Unknown,
        }
    }

    /// Stable key for deduplication and for the fix cache. Deliberately ignores
    /// line numbers so the same error at 200 sites collapses to one entry.
    pub fn signature(&self) -> String {
        format!(
            "{:?}|{:?}|{}",
            self.severity,
            self.kind,
            normalize_message(&self.message)
        )
    }
}

fn compact_whitespace(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "(no diagnostic message)".into()
    } else {
        compact
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    Lines(u32),
    DifferentFile,
    Unknown,
}

impl Gap {
    /// Bucket used for reporting. Every harness metric is reported per bucket,
    /// because an average hides exactly the behavior we care about.
    pub fn bucket(self) -> &'static str {
        match self {
            Gap::Lines(0) => "gap=0",
            Gap::Lines(n) if n <= 50 => "0<gap<=50",
            Gap::Lines(_) => "gap>50",
            Gap::DifferentFile => "other-file",
            Gap::Unknown => "unlocalizable",
        }
    }

    pub fn is_non_localized(self) -> bool {
        !matches!(self, Gap::Lines(n) if n <= 50)
    }
}

/// One TeX point, in scaled points. TeX's internal unit throughout.
pub const PT: i64 = 65536;

/// Convert a decimal point measurement (as printed in logs) to scaled points,
/// rounding as TeX does.
pub fn pt_to_sp(pt: f64) -> i64 {
    (pt * PT as f64).round() as i64
}

/// Inverse of [`pt_to_sp`], for display.
pub fn sp_to_pt(sp: i64) -> f64 {
    sp as f64 / PT as f64
}

/// Strip variable detail so that the same error with different specifics
/// shares a signature: quoted names and numbers become placeholders.
pub fn normalize_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut chars = msg.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => {
                out.push_str("`X'");
                for c2 in chars.by_ref() {
                    if c2 == '\'' {
                        break;
                    }
                }
            }
            '0'..='9' => {
                out.push('N');
                while chars
                    .peek()
                    .is_some_and(|d| d.is_ascii_digit() || *d == '.')
                {
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// The outcome of one compilation.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub diagnostics: Vec<Diagnostic>,
    pub succeeded: bool,
    pub pages: Option<u32>,
    pub engine: String,
}

impl Report {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity >= Severity::Error)
    }

    /// First error in reported order — what `-halt-on-error` would surface.
    pub fn first_error(&self) -> Option<&Diagnostic> {
        self.errors().next()
    }

    pub fn bad_boxes(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::BadBox)
    }

    /// Collapse identical diagnostics, summing counts. The first token-saving
    /// stage: one `Undefined control sequence` repeated 200 times becomes one
    /// prompt line.
    pub fn cluster(&mut self) {
        let mut seen: Vec<(String, usize)> = Vec::new();
        let mut out: Vec<Diagnostic> = Vec::new();
        for d in self.diagnostics.drain(..) {
            let sig = d.signature();
            if let Some(&(_, idx)) = seen.iter().find(|(s, _)| *s == sig) {
                out[idx].count += d.count;
            } else {
                seen.push((sig, out.len()));
                out.push(d);
            }
        }
        self.diagnostics = out;
    }
}

/// Is this path inside the user's project, as opposed to a package in the TeX
/// distribution? Errors reported inside a `.sty` name the package, which is
/// almost never where the fix goes.
pub fn is_user_file(path: &Path) -> bool {
    let s = path.to_string_lossy();
    !s.contains("texmf") && !s.ends_with(".sty") && !s.ends_with(".cls") && !s.ends_with(".def")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_variable_detail() {
        assert_eq!(
            normalize_message("LaTeX Error: File `foo.sty' not found."),
            normalize_message("LaTeX Error: File `bar.sty' not found.")
        );
        assert_eq!(
            normalize_message("Overfull \\hbox (12.34pt too wide)"),
            normalize_message("Overfull \\hbox (99.9pt too wide)")
        );
    }

    #[test]
    fn signature_ignores_line_numbers() {
        let mk = |line| {
            Diagnostic::new(
                Severity::Error,
                Kind::UndefinedControlSequence { name: "foo".into() },
                "Undefined control sequence.",
                Source::TexLog,
            )
            .at(Span::new("a.tex", line))
        };
        assert_eq!(mk(10).signature(), mk(9999).signature());
    }

    #[test]
    fn clustering_sums_counts() {
        let mut r = Report::default();
        for line in [1, 50, 900] {
            r.diagnostics.push(
                Diagnostic::new(
                    Severity::Error,
                    Kind::UndefinedControlSequence { name: "x".into() },
                    "Undefined control sequence.",
                    Source::TexLog,
                )
                .at(Span::new("a.tex", line)),
            );
        }
        r.cluster();
        assert_eq!(r.diagnostics.len(), 1);
        assert_eq!(r.diagnostics[0].count, 3);
    }

    #[test]
    fn gap_buckets_match_corpus_definition() {
        let mut d = Diagnostic::new(Severity::Error, Kind::Other, "x", Source::TexLog)
            .at(Span::new("a.tex", 673));
        assert_eq!(d.gap(), Gap::Unknown);
        d.cause = Some(Span::new("a.tex", 101));
        assert_eq!(d.gap(), Gap::Lines(572));
        assert_eq!(d.gap().bucket(), "gap>50");
        assert!(d.gap().is_non_localized());

        d.cause = Some(Span::new("a.tex", 673));
        assert_eq!(d.gap().bucket(), "gap=0");
        assert!(!d.gap().is_non_localized());

        d.cause = Some(Span::new("other.tex", 5));
        assert_eq!(d.gap(), Gap::DifferentFile);
    }

    #[test]
    fn diagnostic_presentation_is_compact_and_ansi_free() {
        let mut d = Diagnostic::new(
            Severity::Error,
            Kind::Index,
            "!! Input index error\n  -- Illegal null field.",
            Source::ToolLog(Tool::MakeIndex),
        )
        .at(Span::new("main.idx", 20));
        d.cause = Some(Span::new("main.tex", 4));
        d.count = 3;

        let presentation = d.presentation();
        assert_eq!(presentation.headline, "makeindex · index · main.tex:4");
        assert_eq!(
            presentation.error,
            "!! Input index error -- Illegal null field."
        );
        assert_eq!(presentation.reported.as_deref(), Some("main.idx:20"));
        assert_eq!(
            presentation.plain_text(),
            "Diagnose  makeindex · index · main.tex:4\n\
             Error     !! Input index error -- Illegal null field.\n\
             Reported  main.idx:20\n\
             Repeated  3 occurrences"
        );
        assert!(!presentation.plain_text().contains('\u{1b}'));
    }

    #[test]
    fn diagnostic_presentation_preserves_typed_provenance() {
        let mut diagnostic =
            Diagnostic::new(Severity::Error, Kind::Other, "failure", Source::Engine)
                .at(Span::new("chapter.tex", 80));
        diagnostic.provenance.push(
            ProvenanceFrame::new(ProvenanceKind::MacroExpansion, "\\outer")
                .at(Span::new("chapter.tex", 80)),
        );
        diagnostic.provenance.push(
            ProvenanceFrame::new(ProvenanceKind::MacroDefinition, "\\outer")
                .at(Span::new("macros.tex", 3))
                .with_detail("#1 -> #1^2"),
        );

        let rendered = diagnostic.presentation().plain_text();
        assert!(rendered.contains("Trace     macro-expansion · \\outer · chapter.tex:80"));
        assert!(rendered.contains("macro-definition · \\outer · macros.tex:3 · #1 -> #1^2"));
    }

    #[test]
    fn presentation_uses_reported_location_when_no_cause_is_known() {
        let d = Diagnostic::new(
            Severity::Error,
            Kind::UndefinedControlSequence { name: "foo".into() },
            "Undefined control sequence.",
            Source::TexLog,
        )
        .at(Span::new("chapter.tex", 12));
        assert_eq!(
            d.presentation().plain_text(),
            "Diagnose  tex · undefined-control-sequence · chapter.tex:12\n\
             Error     Undefined control sequence."
        );
    }

    #[test]
    fn sty_files_are_not_user_files() {
        assert!(is_user_file(Path::new("paper.tex")));
        assert!(!is_user_file(Path::new(
            "/usr/local/texlive/2024/tex/latex/base/article.cls"
        )));
        assert!(!is_user_file(Path::new("hyperref.sty")));
    }
}
