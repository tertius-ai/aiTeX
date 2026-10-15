//! The deterministic fix cascade.
//!
//! Each rule is an independent [`Rule`] that either claims a diagnostic or
//! passes. They run in priority order and the first claim wins, so adding a
//! rule means adding a type — never editing a growing `match`.
//!
//! Ordering is not cosmetic. A missing package must be tried *before* spelling
//! correction, or `\includegraphics` gets "corrected" to some near neighbour
//! instead of prompting `\usepackage{graphicx}`. Each rule states its
//! [`Rule::priority`] and the cascade sorts on it.

use crate::fixes::{Action, Confidence, Context, Fix};
use crate::structure;
use aitex_core::{Diagnostic, Kind, Source as DiagnosticSource, Tool};

/// Source text of the file a diagnostic points into, when available.
///
/// Structural rules need it; message-only rules do not, which is why it is
/// optional rather than required.
#[derive(Debug, Default, Clone, Copy)]
pub struct Source<'a> {
    pub text: Option<&'a str>,
}

/// One deterministic repair strategy.
pub trait Rule {
    /// Stable identifier, used in coverage reporting and ablation.
    fn name(&self) -> &'static str;

    /// Lower runs first.
    fn priority(&self) -> u8;

    fn apply(&self, diag: &Diagnostic, ctx: &Context, src: Source<'_>) -> Option<Fix>;
}

// --- scheduling -------------------------------------------------------------

/// Undefined references and citations before the pipeline has converged.
pub struct RerunRule;

impl Rule for RerunRule {
    fn name(&self) -> &'static str {
        "rerun-pipeline"
    }
    fn priority(&self) -> u8 {
        10
    }
    fn apply(&self, diag: &Diagnostic, ctx: &Context, _: Source<'_>) -> Option<Fix> {
        if !matches!(
            diag.kind,
            Kind::UndefinedCitation { .. } | Kind::UndefinedReference { .. } | Kind::RerunNeeded
        ) {
            return None;
        }
        if ctx.pipeline_converged {
            return Some(Fix {
                action: Action::ReportOnly {
                    reason: "still undefined after the pipeline converged; the label or bib \
                             entry does not exist"
                        .into(),
                },
                rationale: "pipeline already at fixpoint".into(),
                confidence: Confidence::Probable,
            });
        }
        Some(Fix {
            action: Action::RerunPipeline { passes: 2 },
            rationale: "undefined refs before convergence are a scheduling artifact".into(),
            confidence: Confidence::Certain,
        })
    }
}

// --- missing files ----------------------------------------------------------

pub struct MissingPackageRule;

impl Rule for MissingPackageRule {
    fn name(&self) -> &'static str {
        "install-package"
    }
    fn priority(&self) -> u8 {
        20
    }
    fn apply(&self, diag: &Diagnostic, _: &Context, _: Source<'_>) -> Option<Fix> {
        let Kind::MissingPackage { name } = &diag.kind else {
            return None;
        };
        Some(Fix {
            action: Action::InstallPackage { file: name.clone() },
            rationale: format!("{name} is absent; tlmgr can resolve which package owns it"),
            confidence: Confidence::Probable,
        })
    }
}

// --- undefined control sequences -------------------------------------------

/// A macro that identifies a package the document has not loaded.
pub struct MacroPackageRule;

impl Rule for MacroPackageRule {
    fn name(&self) -> &'static str {
        "add-package"
    }
    fn priority(&self) -> u8 {
        30
    }
    fn apply(&self, diag: &Diagnostic, ctx: &Context, _: Source<'_>) -> Option<Fix> {
        let Kind::UndefinedControlSequence { name } = &diag.kind else {
            return None;
        };
        let pkg = crate::fixes::macro_to_package(name)?;
        if ctx.loaded_packages.iter().any(|p| p == pkg) {
            return None;
        }
        Some(Fix {
            action: Action::AddPackage {
                package: pkg.into(),
            },
            rationale: format!("\\{name} is defined by {pkg}, which is not loaded"),
            confidence: Confidence::Probable,
        })
    }
}

/// Nearest known macro, when it is unambiguous.
pub struct SpellingRule;

impl Rule for SpellingRule {
    fn name(&self) -> &'static str {
        "spelling"
    }
    fn priority(&self) -> u8 {
        40
    }
    fn apply(&self, diag: &Diagnostic, ctx: &Context, _: Source<'_>) -> Option<Fix> {
        let Kind::UndefinedControlSequence { name } = &diag.kind else {
            return None;
        };
        if name.is_empty() {
            return None;
        }
        crate::fixes::nearest_macro(name, &ctx.known_macros).map(|(d, cand)| Fix {
            action: Action::ReplaceControlSequence {
                from: name.clone(),
                to: cand.clone(),
            },
            rationale: format!("\\{name} is {d} edit(s) from \\{cand}, uniquely"),
            confidence: if d == 1 {
                Confidence::Probable
            } else {
                Confidence::Possible
            },
        })
    }
}

/// BibTeX's canonical missing-field-comma diagnostic is narrow enough to fix
/// without a model when the reported line is a complete braced field and the
/// following line is visibly another field.
pub struct BibtexFieldCommaRule;

fn bibtex_field(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then_some((name, value))
}

impl Rule for BibtexFieldCommaRule {
    fn name(&self) -> &'static str {
        "bibtex-field-comma"
    }

    fn priority(&self) -> u8 {
        45
    }

    fn apply(&self, diag: &Diagnostic, _: &Context, src: Source<'_>) -> Option<Fix> {
        if diag.source != DiagnosticSource::ToolLog(Tool::BibTex)
            || !matches!(diag.kind, Kind::Bibliography)
            || !diag.message.starts_with("I was expecting a `,' or a `}'")
        {
            return None;
        }
        let reported = diag.reported.as_ref()?.line;
        let lines: Vec<&str> = src.text?.lines().collect();
        // BibTeX often reports the following field and explicitly says that
        // the error may be on the previous line. Accept either shape, but only
        // when the missing-comma line is a complete braced field and the next
        // line is independently recognizable as another field.
        let candidates = [reported, reported.checked_sub(1)?];
        let line = candidates.into_iter().find(|line| {
            let Some(current) = lines
                .get(line.saturating_sub(1) as usize)
                .map(|line| line.trim())
            else {
                return false;
            };
            let Some(following) = lines.get(*line as usize).map(|line| line.trim()) else {
                return false;
            };
            bibtex_field(current).is_some_and(|(_, value)| {
                value.starts_with('{')
                    && value.ends_with('}')
                    && !value.ends_with("},")
                    && bibtex_field(following).is_some()
            })
        })?;
        Some(Fix {
            action: Action::AppendToLine {
                line,
                text: ",".into(),
            },
            rationale: format!(
                "BibTeX expected a field separator after the complete braced field on line {line}"
            ),
            confidence: Confidence::Certain,
        })
    }
}

/// `\begin{foo}` where `foo` was never declared.
///
/// Two causes, distinguishable from context: a `\newtheorem{foo}` that is
/// missing or misspelled, or a package that declares the environment and is not
/// loaded. Theorem-like names are by far the common case in mathematical
/// writing, which is what the corpus is made of.
pub struct UndefinedEnvironmentRule;

/// Environments supplied by a package rather than declared in the document.
fn environment_package(name: &str) -> Option<&'static str> {
    Some(match name {
        "align" | "align*" | "gather" | "gather*" | "multline" | "multline*" | "split"
        | "cases" | "aligned" | "pmatrix" | "bmatrix" | "vmatrix" => "amsmath",
        "algorithm" | "algorithmic" => "algorithm",
        "algorithm2e" => "algorithm2e",
        "lstlisting" => "listings",
        "tikzpicture" => "tikz",
        "tikzcd" => "tikz-cd",
        "subfigure" => "subcaption",
        "longtable" => "longtable",
        "tabularx" => "tabularx",
        "wrapfigure" => "wrapfig",
        "theorem" | "lemma" | "corollary" | "proposition" | "definition" | "remark" | "example"
        | "proof" => return None, // document-declared, not a package
        _ => return None,
    })
}

impl Rule for UndefinedEnvironmentRule {
    fn name(&self) -> &'static str {
        "undefined-environment"
    }
    fn priority(&self) -> u8 {
        35
    }
    fn apply(&self, diag: &Diagnostic, ctx: &Context, _: Source<'_>) -> Option<Fix> {
        let name = match &diag.kind {
            Kind::UndefinedEnvironment { name } => name,
            // `No counter 'theorem'` is the same fault seen from the counter
            // side; treat it identically so the two do not double-report.
            Kind::UndefinedCounter { name } => name,
            _ => return None,
        };
        if name.is_empty() {
            return None;
        }

        if let Some(pkg) = environment_package(name)
            && !ctx.loaded_packages.iter().any(|p| p == pkg)
        {
            return Some(Fix {
                action: Action::AddPackage {
                    package: pkg.into(),
                },
                rationale: format!("environment {name} is provided by {pkg}"),
                confidence: Confidence::Probable,
            });
        }

        // A near-miss against a declared environment means the declaration was
        // renamed or misspelled rather than absent.
        if let Some((d, cand)) = crate::fixes::nearest_macro(name, &ctx.known_macros) {
            return Some(Fix {
                action: Action::ReplaceControlSequence {
                    from: name.clone(),
                    to: cand.clone(),
                },
                rationale: format!("environment {name} is {d} edit(s) from declared {cand}"),
                confidence: Confidence::Possible,
            });
        }

        Some(Fix {
            action: Action::Diagnose {
                line: diag.reported.as_ref().map(|s| s.line).unwrap_or(0),
                what: format!("\\begin{{{name}}}"),
                note: format!(
                    "environment {name} is never declared; it needs \\newtheorem{{{name}}}{{...}}                      or the package that provides it"
                ),
            },
            rationale: "no declaration and no near match".into(),
            confidence: Confidence::Probable,
        })
    }
}

/// Errors a package raised itself.
///
/// These carry package-specific semantics we cannot repair generically, but
/// attributing them correctly is what lets the model be given the right context
/// — and stops them being lumped into an undifferentiated `Other`.
pub struct PackageErrorRule;

impl Rule for PackageErrorRule {
    fn name(&self) -> &'static str {
        "package-error"
    }
    fn priority(&self) -> u8 {
        70
    }
    fn apply(&self, diag: &Diagnostic, _: &Context, _: Source<'_>) -> Option<Fix> {
        let Kind::PackageError { package } = &diag.kind else {
            return None;
        };
        Some(Fix {
            action: Action::Diagnose {
                line: diag.reported.as_ref().map(|s| s.line).unwrap_or(0),
                what: format!("{package} error"),
                note: format!(
                    "{package} raised this itself, so the fault is in {package} syntax                      rather than in LaTeX structure"
                ),
            },
            rationale: format!("attributed to {package}"),
            confidence: Confidence::Probable,
        })
    }
}

// --- structural: the locally context-free rules ----------------------------

/// Locate an unclosed brace by bracket matching.
///
/// This is the rule that makes runaway arguments and missing braces tractable.
/// TeX reports them with no line number — but brace nesting is context-free
/// inside a catcode-stable region, so a stack finds the opener exactly.
pub struct UnclosedBraceRule;

impl Rule for UnclosedBraceRule {
    fn name(&self) -> &'static str {
        "unclosed-brace"
    }
    fn priority(&self) -> u8 {
        50
    }
    fn apply(&self, diag: &Diagnostic, _: &Context, src: Source<'_>) -> Option<Fix> {
        if !matches!(diag.kind, Kind::RunawayArgument | Kind::MissingBrace) {
            return None;
        }
        let text = src.text?;
        let unclosed = structure::unclosed_braces(text);
        // Exactly one is actionable; several means the file is badly enough
        // broken that picking one would be a guess.
        let u = match unclosed.as_slice() {
            [only] => only,
            _ => return None,
        };
        let trust = u.trust;
        Some(Fix {
            action: Action::InsertAt {
                line: u.open.line,
                what: "}".into(),
                note: format!("close the group opened at line {}", u.open.line),
            },
            rationale: format!(
                "brace matching locates the unclosed group at line {}, column {} — \
                 TeX itself reports no line for this",
                u.open.line, u.open.col
            ),
            confidence: match trust {
                structure::Trust::Sound => Confidence::Probable,
                structure::Trust::Unreliable => Confidence::Possible,
            },
        })
    }
}

/// Repair `\begin{a}...\end{b}` using the environment stack.
pub struct EnvironmentRule;

impl Rule for EnvironmentRule {
    fn name(&self) -> &'static str {
        "environment"
    }
    fn priority(&self) -> u8 {
        55
    }
    fn apply(&self, diag: &Diagnostic, _: &Context, src: Source<'_>) -> Option<Fix> {
        let Kind::MismatchedEnvironment { began, ended } = &diag.kind else {
            return None;
        };
        if began == ended {
            return None;
        }

        // With the source we can tell the two cases apart. The stack reports a
        // crossed pair in `mismatched` (the opener *was* popped, by the wrong
        // `\end`) and a never-closed one in `unclosed` — so consult both, and
        // report the position of the `\begin` that actually went wrong rather
        // than wherever TeX happened to notice.
        if let Some(text) = src.text {
            let rep = structure::environments(text);

            if let Some((open, opened, closed)) =
                rep.mismatched.iter().find(|(_, opened, _)| opened == began)
            {
                return Some(Fix {
                    action: Action::Diagnose {
                        line: open.line,
                        what: format!("\\begin{{{opened}}}"),
                        note: format!(
                            "\\begin{{{opened}}} at line {} is closed by \\end{{{closed}}};                              the missing \\end belongs somewhere before it",
                            open.line
                        ),
                    },
                    rationale: format!(
                        "environment stack shows {opened} opened at line {} and closed by \
                         \\end{{{closed}}}",
                        open.line
                    ),
                    confidence: Confidence::Probable,
                });
            }

            if let Some(u) = rep
                .unclosed
                .iter()
                .find(|u| u.name.as_deref() == Some(began.as_str()))
            {
                return Some(Fix {
                    action: Action::Diagnose {
                        line: u.open.line,
                        what: format!("\\begin{{{began}}}"),
                        note: format!("\\begin{{{began}}} at line {} is never closed", u.open.line),
                    },
                    rationale: format!(
                        "environment stack shows {began} opened at line {} and never closed",
                        u.open.line
                    ),
                    confidence: Confidence::Probable,
                });
            }
        }

        Some(Fix {
            action: Action::FixEnvironmentDelimiter {
                from: ended.clone(),
                to: began.clone(),
            },
            rationale: format!("\\begin{{{began}}} is closed by \\end{{{ended}}}"),
            confidence: Confidence::Possible,
        })
    }
}

/// Locate an unclosed `$` by parity within the paragraph.
pub struct UnclosedMathRule;

impl Rule for UnclosedMathRule {
    fn name(&self) -> &'static str {
        "unclosed-math"
    }
    fn priority(&self) -> u8 {
        60
    }
    fn apply(&self, diag: &Diagnostic, _: &Context, src: Source<'_>) -> Option<Fix> {
        if !matches!(diag.kind, Kind::MissingMathShift) {
            return None;
        }
        let text = src.text?;
        let m = structure::unclosed_math(text);
        let u = match m.as_slice() {
            [only] => only,
            _ => return None,
        };
        Some(Fix {
            action: Action::Diagnose {
                line: u.open.line,
                what: "$".into(),
                note: format!(
                    "math opened at line {} is never closed; the missing $ belongs \
                     later in the paragraph",
                    u.open.line
                ),
            },
            rationale: format!(
                "odd number of math shifts in the paragraph beginning at line {}",
                u.open.line
            ),
            confidence: match u.trust {
                structure::Trust::Sound => Confidence::Probable,
                structure::Trust::Unreliable => Confidence::Possible,
            },
        })
    }
}

// --- typography and safety --------------------------------------------------

pub struct MicrotypeRule;

impl Rule for MicrotypeRule {
    fn name(&self) -> &'static str {
        "microtype"
    }
    fn priority(&self) -> u8 {
        80
    }
    fn apply(&self, diag: &Diagnostic, ctx: &Context, _: Source<'_>) -> Option<Fix> {
        if !matches!(diag.kind, Kind::OverfullBox { .. }) {
            return None;
        }
        if ctx.loaded_packages.iter().any(|p| p == "microtype") {
            return None;
        }
        Some(Fix {
            action: Action::AddPackage {
                package: "microtype".into(),
            },
            rationale: "protrusion and font expansion remove many overfull boxes with no \
                        change to the text"
                .into(),
            confidence: Confidence::Probable,
        })
    }
}

/// Detect, never repair.
pub struct CapacityRule;

impl Rule for CapacityRule {
    fn name(&self) -> &'static str {
        "capacity"
    }
    fn priority(&self) -> u8 {
        90
    }
    fn apply(&self, diag: &Diagnostic, _: &Context, _: Source<'_>) -> Option<Fix> {
        let Kind::CapacityExceeded { resource } = &diag.kind else {
            return None;
        };
        Some(Fix {
            action: Action::ReportOnly {
                reason: format!(
                    "TeX ran out of {resource}; usually an unterminated or self-recursive \
                     definition. Detected, not repaired."
                ),
            },
            rationale: "auto-repair here risks deleting author intent".into(),
            confidence: Confidence::Certain,
        })
    }
}

/// The standard cascade, in priority order.
pub fn default_cascade() -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![
        Box::new(RerunRule),
        Box::new(MissingPackageRule),
        Box::new(MacroPackageRule),
        Box::new(UndefinedEnvironmentRule),
        Box::new(PackageErrorRule),
        Box::new(SpellingRule),
        Box::new(BibtexFieldCommaRule),
        Box::new(UnclosedBraceRule),
        Box::new(EnvironmentRule),
        Box::new(UnclosedMathRule),
        Box::new(MicrotypeRule),
        Box::new(CapacityRule),
    ];
    rules.sort_by_key(|r| r.priority());
    rules
}

/// Run the cascade; the first rule to claim the diagnostic wins.
pub fn run(
    rules: &[Box<dyn Rule>],
    diag: &Diagnostic,
    ctx: &Context,
    src: Source<'_>,
) -> Option<(&'static str, Fix)> {
    rules
        .iter()
        .find_map(|r| r.apply(diag, ctx, src).map(|f| (r.name(), f)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_core::{Severity, Span};

    fn diag(kind: Kind) -> Diagnostic {
        Diagnostic::new(Severity::Error, kind, "msg", aitex_core::Source::TexLog)
            .at(Span::new("a.tex", 1))
    }

    #[test]
    fn cascade_is_ordered_by_priority() {
        let c = default_cascade();
        assert!(c.windows(2).all(|w| w[0].priority() <= w[1].priority()));
    }

    #[test]
    fn package_rule_beats_spelling_for_the_same_diagnostic() {
        // Without ordering, `\includegraphics` would be "corrected" to a near
        // neighbour instead of prompting the package that defines it.
        let c = default_cascade();
        let ctx = Context {
            known_macros: vec!["includegraphic".into()],
            ..Default::default()
        };
        let d = diag(Kind::UndefinedControlSequence {
            name: "includegraphics".into(),
        });
        let (rule, fix) = run(&c, &d, &ctx, Source::default()).expect("a rule claims it");
        assert_eq!(rule, "add-package");
        assert_eq!(
            fix.action,
            Action::AddPackage {
                package: "graphicx".into()
            }
        );
    }

    #[test]
    fn bibtex_missing_field_comma_is_narrow_and_mechanical() {
        let c = default_cascade();
        let source = "@article{x,\n  title = {Fast}\n  year = {2026}\n}\n";
        let diagnostic = Diagnostic::new(
            Severity::Error,
            Kind::Bibliography,
            "I was expecting a `,' or a `}'---line 2 of file refs.bib",
            DiagnosticSource::ToolLog(Tool::BibTex),
        )
        .at(Span::new("refs.bib", 2));
        let (rule, fix) = run(
            &c,
            &diagnostic,
            &Context::default(),
            Source { text: Some(source) },
        )
        .expect("canonical BibTeX message and fields are safe");
        assert_eq!(rule, "bibtex-field-comma");
        assert_eq!(
            fix.action,
            Action::AppendToLine {
                line: 2,
                text: ",".into()
            }
        );

        let following_line = Diagnostic {
            reported: Some(Span::new("refs.bib", 3)),
            ..diagnostic
        };
        let (_, fix) = run(
            &c,
            &following_line,
            &Context::default(),
            Source { text: Some(source) },
        )
        .expect("BibTeX may report the field after the missing comma");
        assert_eq!(
            fix.action,
            Action::AppendToLine {
                line: 2,
                text: ",".into()
            }
        );
    }

    #[test]
    fn bibtex_comma_rule_declines_lookalikes() {
        let c = default_cascade();
        let base = Diagnostic::new(
            Severity::Error,
            Kind::Bibliography,
            "I was expecting a `,' or a `}'---line 2 of file refs.bib",
            DiagnosticSource::ToolLog(Tool::BibTex),
        )
        .at(Span::new("refs.bib", 2));
        let invalid = "@article{x,\n  title = Fast\n  % year = {2026}\n}\n";
        assert!(
            run(
                &c,
                &base,
                &Context::default(),
                Source {
                    text: Some(invalid)
                }
            )
            .is_none()
        );
        let biber = Diagnostic {
            source: DiagnosticSource::ToolLog(Tool::Biber),
            ..base
        };
        assert!(
            run(
                &c,
                &biber,
                &Context::default(),
                Source {
                    text: Some("@article{x,\n  title = {Fast}\n  year = {2026}\n}\n")
                }
            )
            .is_none()
        );
    }

    /// The headline of this cascade: an error TeX cannot localize at all
    /// becomes a located, deterministic repair.
    #[test]
    fn runaway_argument_is_localized_by_brace_matching() {
        let c = default_cascade();
        let text =
            "\\documentclass{article}\n\\begin{document}\n\\textbf{oops\nmore\n\\end{document}\n";
        let d = diag(Kind::RunawayArgument);
        let (rule, fix) = run(&c, &d, &Context::default(), Source { text: Some(text) })
            .expect("structural rule should claim it");
        assert_eq!(rule, "unclosed-brace");
        match fix.action {
            Action::InsertAt { line, ref what, .. } => {
                assert_eq!(line, 3, "points at the real opener, not where TeX gave up");
                assert_eq!(what, "}");
            }
            ref a => panic!("unexpected action {a:?}"),
        }
    }

    #[test]
    fn runaway_without_source_declines_rather_than_guesses() {
        let c = default_cascade();
        let d = diag(Kind::RunawayArgument);
        assert!(run(&c, &d, &Context::default(), Source::default()).is_none());
    }

    #[test]
    fn ambiguous_structure_declines() {
        // Two unclosed braces: picking one would be a coin flip.
        let c = default_cascade();
        let text = "\\textbf{a\n\\emph{b\n";
        let d = diag(Kind::RunawayArgument);
        assert!(run(&c, &d, &Context::default(), Source { text: Some(text) }).is_none());
    }

    #[test]
    fn catcode_changes_downgrade_confidence_but_still_help() {
        let c = default_cascade();
        let text = "\\catcode`\\@=11\n\\textbf{x\n";
        let d = diag(Kind::RunawayArgument);
        let (_, fix) = run(&c, &d, &Context::default(), Source { text: Some(text) })
            .expect("still worth reporting");
        assert_eq!(
            fix.confidence,
            Confidence::Possible,
            "must not claim certainty"
        );
    }

    #[test]
    fn unclosed_environment_is_distinguished_from_a_wrong_end() {
        let c = default_cascade();
        let text = "\\begin{document}\n\\begin{itemize}\n\\item a\n\\end{document}\n";
        let d = diag(Kind::MismatchedEnvironment {
            began: "itemize".into(),
            ended: "document".into(),
        });
        let (rule, fix) =
            run(&c, &d, &Context::default(), Source { text: Some(text) }).expect("claimed");
        assert_eq!(rule, "environment");
        match fix.action {
            Action::Diagnose { line, ref what, .. } => {
                assert_eq!(line, 2, "points at the \\begin that went unclosed");
                assert_eq!(what, "\\begin{itemize}");
            }
            ref a => panic!("expected a diagnosis, got {a:?}"),
        }
    }

    #[test]
    fn unclosed_math_is_located() {
        let c = default_cascade();
        let text = "text $x + y\n\nnext\n";
        let d = diag(Kind::MissingMathShift);
        let (rule, fix) =
            run(&c, &d, &Context::default(), Source { text: Some(text) }).expect("claimed");
        assert_eq!(rule, "unclosed-math");
        assert!(matches!(fix.action, Action::Diagnose { line: 1, .. }));
    }

    #[test]
    fn capacity_exceeded_still_refuses_to_repair() {
        let c = default_cascade();
        let d = diag(Kind::CapacityExceeded {
            resource: "main memory".into(),
        });
        let (_, fix) = run(&c, &d, &Context::default(), Source::default()).expect("claimed");
        assert!(matches!(fix.action, Action::ReportOnly { .. }));
    }

    #[test]
    fn adding_a_rule_requires_no_edit_to_existing_ones() {
        // The point of the cascade: extension by composition.
        struct Custom;
        impl Rule for Custom {
            fn name(&self) -> &'static str {
                "custom"
            }
            fn priority(&self) -> u8 {
                1
            }
            fn apply(&self, _: &Diagnostic, _: &Context, _: Source<'_>) -> Option<Fix> {
                Some(Fix {
                    action: Action::CleanAuxiliaries,
                    rationale: "test".into(),
                    confidence: Confidence::Possible,
                })
            }
        }
        let mut c = default_cascade();
        c.insert(0, Box::new(Custom));
        let d = diag(Kind::RunawayArgument);
        let (rule, _) = run(&c, &d, &Context::default(), Source::default()).expect("claimed");
        assert_eq!(rule, "custom", "lowest priority runs first");
    }
}
