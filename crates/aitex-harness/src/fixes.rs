//! Deterministic repairs — docs/history/PLAN.md H2.
//!
//! Every fix here runs **before** any model call and costs zero tokens. The
//! target is 40–60% of all errors, and the ordering is by (frequency ×
//! confidence × safety) rather than by how interesting the fix is.
//!
//! Two principles worth stating because they are easy to get backwards:
//!
//! * **Undefined references and citations are a *scheduling* bug, not a source
//!   bug.** The document is fine; the pipeline simply has not been run to a
//!   fixpoint yet. Re-running is the whole fix.
//! * **For overfull boxes the model is the *last* resort, not the first.**
//!   `microtype` is a one-line preamble change with no semantic risk;
//!   rewording changes what the author said and needs consent.

use aitex_core::{Diagnostic, Kind, Severity};
use std::collections::BTreeMap;

/// A proposed repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub action: Action,
    /// Why we believe this. Shown to the user in `suggest` mode.
    pub rationale: String,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Re-run the build pipeline; the source needs no change.
    RerunPipeline { passes: u8 },
    /// Delete auxiliary files, then rebuild.
    CleanAuxiliaries,
    /// Install a missing package from the TeX distribution.
    InstallPackage { file: String },
    /// Insert `\usepackage{..}` into the preamble.
    AddPackage { package: String },
    /// Replace a control sequence at a known location.
    ReplaceControlSequence { from: String, to: String },
    /// Append exact text to a known source line after syntax-specific checks.
    AppendToLine { line: u32, text: String },
    /// Repair `\begin{x}...\end{y}` by correcting one side.
    FixEnvironmentDelimiter { from: String, to: String },
    /// Insert text at a located line — how structural repairs are expressed.
    InsertAt {
        line: u32,
        what: String,
        note: String,
    },
    /// A located *diagnosis* — we know what is wrong and where it started, but
    /// not where the repair belongs. Surfaced to the user or handed to the
    /// model as context; never applied unattended.
    ///
    /// This exists because measurement forced it: the environment and math
    /// rules identify the opener correctly but the *insertion point* is
    /// elsewhere, and claiming `InsertAt` there was wrong 79% and 85% of the
    /// time respectively.
    Diagnose {
        line: u32,
        what: String,
        note: String,
    },
    /// Report only — never auto-applied.
    ReportOnly { reason: String },
}

impl Action {
    /// A source line established by deterministic analysis, even when the
    /// compiler itself reported only end-of-file or a generated file.
    pub fn source_line(&self) -> Option<u32> {
        match self {
            Action::AppendToLine { line, .. }
            | Action::InsertAt { line, .. }
            | Action::Diagnose { line, .. } => Some(*line),
            _ => None,
        }
    }
}

/// How far we trust a fix. `Certain` fixes are safe to apply unattended;
/// `Probable` ones need `--auto-fix`; anything less is a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Possible,
    Probable,
    Certain,
}

/// Control sequences that identify a package, for the commonest case of all:
/// a macro used without loading what defines it.
///
/// Deliberately small and high-confidence. A big speculative table would fire
/// on macros the document defines itself.
pub(crate) fn macro_to_package(name: &str) -> Option<&'static str> {
    Some(match name {
        "includegraphics" | "rotatebox" | "scalebox" | "resizebox" => "graphicx",
        "toprule" | "midrule" | "bottomrule" | "cmidrule" => "booktabs",
        "url" | "href" => "hyperref",
        "text" | "eqref" | "boxed" | "substack" | "dfrac" | "cfrac" => "amsmath",
        "mathbb" | "mathfrak" | "lozenge" | "square" => "amssymb",
        "SI" | "si" | "num" | "qty" => "siunitx",
        "textcolor" | "colorbox" | "definecolor" => "xcolor",
        "multirow" => "multirow",
        "subfigure" | "subfloat" => "subcaption",
        "algorithm" => "algorithm",
        "lstlisting" | "lstinline" => "listings",
        "tikz" | "tikzpicture" => "tikz",
        "todo" => "todonotes",
        "checkmark" => "amssymb",
        "bm" => "bm",
        _ => return None,
    })
}

/// Levenshtein distance, capped: we only care whether it is ≤ `max`.
fn edit_distance_within(a: &str, b: &str, max: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        core::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[b.len()];
    (d <= max).then_some(d)
}

/// Return a unique nearby package from the deliberately small package table
/// aiTeX already trusts for macro-to-package guidance.
pub(crate) fn nearest_known_package(name: &str) -> Option<(usize, &'static str)> {
    const PACKAGES: &[&str] = &[
        "algorithm",
        "amsmath",
        "amssymb",
        "bm",
        "booktabs",
        "graphicx",
        "hyperref",
        "listings",
        "multirow",
        "siunitx",
        "subcaption",
        "tikz",
        "todonotes",
        "xcolor",
    ];
    let mut best: Option<(usize, &'static str)> = None;
    let mut ties = 0usize;
    for candidate in PACKAGES {
        if let Some(distance) = edit_distance_within(name, candidate, 1) {
            match best {
                Some((best_distance, _)) if distance > best_distance => {}
                Some((best_distance, _)) if distance == best_distance => ties += 1,
                _ => {
                    best = Some((distance, candidate));
                    ties = 0;
                }
            }
        }
    }
    match best {
        Some((distance, candidate)) if distance > 0 && ties == 0 => Some((distance, candidate)),
        _ => None,
    }
}

/// What the fixer knows about the document being repaired.
#[derive(Debug, Default)]
pub struct Context {
    /// Control sequences defined in this project or by loaded packages.
    pub known_macros: Vec<String>,
    /// Packages already in the preamble.
    pub loaded_packages: Vec<String>,
    /// Whether a `.bbl`/`.aux` cycle has already been run to fixpoint.
    pub pipeline_converged: bool,
}

/// Propose a fix for one diagnostic, or `None` if no deterministic rule applies
/// and the model will have to decide.
pub fn propose(diag: &Diagnostic, ctx: &Context) -> Option<Fix> {
    match &diag.kind {
        // 1. Scheduling, not source. Highest-frequency "error" that isn't one.
        Kind::UndefinedCitation { .. } | Kind::UndefinedReference { .. } | Kind::RerunNeeded => {
            if ctx.pipeline_converged {
                // Already at a fixpoint, so the reference is genuinely missing.
                return Some(Fix {
                    action: Action::ReportOnly {
                        reason: "reference still undefined after the pipeline converged; \
                                 the label or bib entry does not exist"
                            .into(),
                    },
                    rationale: "pipeline already at fixpoint".into(),
                    confidence: Confidence::Probable,
                });
            }
            Some(Fix {
                action: Action::RerunPipeline { passes: 2 },
                rationale: "undefined refs/citations before the pipeline has converged are a \
                            scheduling artifact, not a source error"
                    .into(),
                confidence: Confidence::Certain,
            })
        }

        // 2. Missing package file: the distribution can supply it.
        Kind::MissingPackage { name } => Some(Fix {
            action: Action::InstallPackage { file: name.clone() },
            rationale: format!("{name} is absent locally; tlmgr can resolve which package owns it"),
            confidence: Confidence::Probable,
        }),

        // 3. Undefined control sequence: two separate rules, package first.
        Kind::UndefinedControlSequence { name } if !name.is_empty() => {
            if let Some(pkg) = macro_to_package(name)
                && !ctx.loaded_packages.iter().any(|p| p == pkg)
            {
                return Some(Fix {
                    action: Action::AddPackage {
                        package: pkg.into(),
                    },
                    rationale: format!("\\{name} is defined by {pkg}, which is not loaded"),
                    confidence: Confidence::Probable,
                });
            }
            nearest_macro(name, &ctx.known_macros).map(|(d, cand)| Fix {
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

        // 4. Mismatched environment: the AST knows which side is wrong; without
        // it we can still say the pair disagrees.
        Kind::MismatchedEnvironment { began, ended } if began != ended => Some(Fix {
            action: Action::FixEnvironmentDelimiter {
                from: ended.clone(),
                to: began.clone(),
            },
            rationale: format!("\\begin{{{began}}} is closed by \\end{{{ended}}}"),
            confidence: Confidence::Possible,
        }),

        // 5. Overfull boxes: microtype before anything semantic.
        Kind::OverfullBox { .. } if !ctx.loaded_packages.iter().any(|p| p == "microtype") => {
            Some(Fix {
                action: Action::AddPackage {
                    package: "microtype".into(),
                },
                rationale: "character protrusion and font expansion remove a large share of \
                            overfull boxes with no change to the text"
                    .into(),
                confidence: Confidence::Probable,
            })
        }

        // 6. Never auto-fix a capacity blowup: the cause is usually a recursive
        // definition, and "fixing" it means deleting the author's macro.
        Kind::CapacityExceeded { resource } => Some(Fix {
            action: Action::ReportOnly {
                reason: format!(
                    "TeX ran out of {resource}; usually an unterminated or self-recursive \
                     definition. Detected, not repaired."
                ),
            },
            rationale: "auto-repair here risks deleting author intent".into(),
            confidence: Confidence::Certain,
        }),

        _ => None,
    }
}

/// Nearest known macro within two edits, only when it is *unique*.
///
/// Ambiguity is a decline, not a coin flip: two candidates at the same distance
/// means we would be guessing, and a wrong auto-fix is worse than none.
pub(crate) fn nearest_macro<'a>(name: &str, known: &'a [String]) -> Option<(usize, &'a String)> {
    let mut best: Option<(usize, &String)> = None;
    let mut ties = 0usize;
    for cand in known {
        if let Some(d) = edit_distance_within(name, cand, 2) {
            match best {
                Some((bd, _)) if d > bd => {}
                Some((bd, _)) if d == bd => ties += 1,
                _ => {
                    best = Some((d, cand));
                    ties = 0;
                }
            }
        }
    }
    match best {
        Some((d, cand)) if ties == 0 && d > 0 => Some((d, cand)),
        _ => None,
    }
}

/// Summary of what the deterministic layer can handle, for the ablation in
/// docs/history/PLAN.md §9.
#[derive(Debug, Default, Clone)]
pub struct Coverage {
    pub total: usize,
    pub fixed: usize,
    pub by_action: BTreeMap<&'static str, usize>,
}

impl Coverage {
    pub fn rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.fixed as f64 / self.total as f64
        }
    }
}

/// Run the rules over a set of diagnostics and report coverage.
pub fn coverage(diags: &[Diagnostic], ctx: &Context) -> Coverage {
    let mut c = Coverage::default();
    for d in diags.iter().filter(|d| d.severity >= Severity::Error) {
        c.total += 1;
        if let Some(f) = propose(d, ctx) {
            let key = match f.action {
                Action::RerunPipeline { .. } => "rerun",
                Action::CleanAuxiliaries => "clean",
                Action::InstallPackage { .. } => "install",
                Action::AddPackage { .. } => "add-package",
                Action::ReplaceControlSequence { .. } => "spelling",
                Action::AppendToLine { .. } => "append-line",
                Action::FixEnvironmentDelimiter { .. } => "env-delimiter",
                Action::InsertAt { .. } => "structural-insert",
                Action::Diagnose { .. } => "structural-diagnose",
                Action::ReportOnly { .. } => "report-only",
            };
            // Report-only is not a repair; it must not inflate the rate.
            if !matches!(f.action, Action::ReportOnly { .. }) {
                c.fixed += 1;
            }
            *c.by_action.entry(key).or_default() += 1;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_core::{Source, Span};

    fn diag(kind: Kind) -> Diagnostic {
        Diagnostic::new(Severity::Error, kind, "msg", Source::TexLog).at(Span::new("a.tex", 1))
    }

    #[test]
    fn undefined_citation_before_convergence_is_a_rerun() {
        let d = diag(Kind::UndefinedCitation {
            key: "knuth84".into(),
        });
        let f = propose(&d, &Context::default()).expect("rule applies");
        assert_eq!(f.action, Action::RerunPipeline { passes: 2 });
        assert_eq!(f.confidence, Confidence::Certain);
    }

    #[test]
    fn undefined_citation_after_convergence_is_genuinely_missing() {
        let d = diag(Kind::UndefinedCitation {
            key: "ghost".into(),
        });
        let ctx = Context {
            pipeline_converged: true,
            ..Default::default()
        };
        let f = propose(&d, &ctx).expect("rule applies");
        assert!(
            matches!(f.action, Action::ReportOnly { .. }),
            "must not loop forever"
        );
    }

    #[test]
    fn known_macro_maps_to_its_package() {
        let d = diag(Kind::UndefinedControlSequence {
            name: "includegraphics".into(),
        });
        let f = propose(&d, &Context::default()).expect("rule applies");
        assert_eq!(
            f.action,
            Action::AddPackage {
                package: "graphicx".into()
            }
        );
    }

    #[test]
    fn package_rule_does_not_fire_when_already_loaded() {
        let d = diag(Kind::UndefinedControlSequence {
            name: "includegraphics".into(),
        });
        let ctx = Context {
            loaded_packages: vec!["graphicx".into()],
            ..Default::default()
        };
        // graphicx is loaded, so this is something else — no package fix.
        let f = propose(&d, &ctx);
        assert!(f.is_none() || !matches!(f.unwrap().action, Action::AddPackage { .. }));
    }

    #[test]
    fn spelling_correction_needs_a_unique_nearest_match() {
        let ctx = Context {
            known_macros: vec!["section".into(), "subsection".into()],
            ..Default::default()
        };
        // One edit from `section`, far from `subsection`: unambiguous.
        let d = diag(Kind::UndefinedControlSequence {
            name: "sectoin".into(),
        });
        let f = propose(&d, &ctx).expect("rule applies");
        assert_eq!(
            f.action,
            Action::ReplaceControlSequence {
                from: "sectoin".into(),
                to: "section".into()
            }
        );

        // Genuinely equidistant: `alph` is one edit from both `alpha` (append
        // 'a') and `aleph` (insert 'e'). With no basis to choose, declining is
        // the right answer — a wrong auto-fix is worse than no fix.
        let ctx2 = Context {
            known_macros: vec!["alpha".into(), "aleph".into()],
            ..Default::default()
        };
        let d2 = diag(Kind::UndefinedControlSequence {
            name: "alph".into(),
        });
        let f2 = propose(&d2, &ctx2);
        assert!(
            f2.is_none() || !matches!(f2.unwrap().action, Action::ReplaceControlSequence { .. }),
            "ambiguous nearest match must not produce a guess"
        );
    }

    #[test]
    fn overfull_box_reaches_for_microtype_not_the_model() {
        let d = Diagnostic::new(
            Severity::BadBox,
            Kind::OverfullBox {
                sp: 800_000,
                lines: Some((10, 12)),
            },
            "Overfull \\hbox",
            Source::TexLog,
        );
        let f = propose(&d, &Context::default()).expect("rule applies");
        assert_eq!(
            f.action,
            Action::AddPackage {
                package: "microtype".into()
            }
        );
    }

    #[test]
    fn capacity_exceeded_is_detected_never_repaired() {
        let d = diag(Kind::CapacityExceeded {
            resource: "main memory size".into(),
        });
        let f = propose(&d, &Context::default()).expect("rule applies");
        assert!(matches!(f.action, Action::ReportOnly { .. }));
    }

    #[test]
    fn report_only_does_not_count_as_a_repair() {
        // Otherwise the coverage number flatters itself: "detected" is not
        // "fixed", and the ablation would be measuring the wrong thing.
        let diags = vec![diag(Kind::CapacityExceeded {
            resource: "mem".into(),
        })];
        let c = coverage(&diags, &Context::default());
        assert_eq!(c.total, 1);
        assert_eq!(c.fixed, 0);
        assert_eq!(c.rate(), 0.0);
    }

    #[test]
    fn edit_distance_is_bounded_and_correct() {
        assert_eq!(edit_distance_within("abc", "abc", 2), Some(0));
        assert_eq!(edit_distance_within("sectoin", "section", 2), Some(2));
        assert_eq!(
            edit_distance_within("a", "abcdef", 2),
            None,
            "length gap short-circuits"
        );
    }

    #[test]
    fn empty_macro_name_yields_no_guess() {
        // The parser leaves the name empty when TeX gave no context line.
        // Guessing from nothing would be worse than declining.
        let d = diag(Kind::UndefinedControlSequence {
            name: String::new(),
        });
        assert!(propose(&d, &Context::default()).is_none());
    }
}
