//! Loader and executable oracle for the small, tracked repair corpus.

use crate::StructuredEdit;
use crate::fixes::{Context, Fix};
use crate::patch::{apply_action, unified_diff};
use crate::repair::{ContextProfile, ProjectFile, PromptPacket, build_prompt};
use crate::rules;
use aitex_core::{Diagnostic, Tool};
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

pub const CORPUS_SCHEMA: &str = "aitex-repair-corpus-v1";

#[derive(Debug, Deserialize)]
pub struct Corpus {
    pub schema: String,
    pub cases: Vec<CorpusCase>,
}

#[derive(Debug, Deserialize)]
pub struct CorpusCase {
    pub id: String,
    /// Evaluation stratum: `sanity`, `localized`, or `nonlocal`.
    #[serde(default)]
    pub tier: Option<String>,
    /// User-visible failure surface, such as `bibliography` or `index`.
    #[serde(default)]
    pub surface: Option<String>,
    pub tool: String,
    pub log: PathBuf,
    /// Root document passed to `aitex build` for validation. Older v1
    /// manifests default to `source`.
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// File named by the diagnostic. It may be a generated `.idx`/`.glo`.
    pub source: PathBuf,
    /// Author-controlled source when `source` is a generated tool input.
    pub project_source: Option<PathBuf>,
    pub fixed: PathBuf,
    pub expected_kind: String,
    pub expected_file: Option<PathBuf>,
    pub expected_line: Option<u32>,
    pub cause_file: Option<PathBuf>,
    pub cause_line: Option<u32>,
    pub expected_route: String,
    pub repair: String,
}

#[derive(Debug, Clone)]
pub struct LocalFinding {
    pub rule: &'static str,
    pub fix: Fix,
    /// True only when the strict patch machinery can apply an edit without a semantic choice.
    pub mechanically_fixable: bool,
    pub refusal: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Route {
    Deterministic {
        rule: &'static str,
        fix: Fix,
        patch: String,
    },
    Model(PromptPacket),
}

#[derive(Debug, Clone)]
pub struct PreparedCase {
    pub id: String,
    pub tier: String,
    pub surface: String,
    pub repair: String,
    pub diagnostic: Diagnostic,
    pub route: Route,
    pub local: Option<LocalFinding>,
    pub project_root: PathBuf,
    pub input: PathBuf,
    pub target: PathBuf,
    pub broken: Vec<u8>,
    pub fixed: Vec<u8>,
}

impl PreparedCase {
    pub fn oracle_patch(&self) -> String {
        unified_diff(&self.target, &self.broken, &self.fixed)
    }

    pub fn oracle_edits(&self) -> Vec<StructuredEdit> {
        vec![StructuredEdit {
            path: self.target.clone(),
            old: String::from_utf8(self.broken.clone()).expect("tracked repair fixtures are UTF-8"),
            new: String::from_utf8(self.fixed.clone()).expect("tracked repair fixtures are UTF-8"),
        }]
    }
}

#[derive(Debug)]
pub enum LabError {
    Read { path: PathBuf, message: String },
    Manifest(String),
    Case { id: String, message: String },
}

impl fmt::Display for LabError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabError::Read { path, message } => {
                write!(formatter, "read {}: {message}", path.display())
            }
            LabError::Manifest(message) => write!(formatter, "manifest: {message}"),
            LabError::Case { id, message } => write!(formatter, "case {id}: {message}"),
        }
    }
}

impl std::error::Error for LabError {}

fn read(path: &Path) -> Result<String, LabError> {
    std::fs::read_to_string(path).map_err(|error| LabError::Read {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

impl Corpus {
    pub fn load(path: &Path) -> Result<Self, LabError> {
        let text = read(path)?;
        let corpus: Corpus = serde_json::from_str(&text)
            .map_err(|error| LabError::Manifest(format!("invalid JSON: {error}")))?;
        if corpus.schema != CORPUS_SCHEMA {
            return Err(LabError::Manifest(format!(
                "expected schema {CORPUS_SCHEMA}, found {}",
                corpus.schema
            )));
        }
        if corpus.cases.is_empty() {
            return Err(LabError::Manifest("contains no cases".into()));
        }
        let mut ids = std::collections::BTreeSet::new();
        for case in &corpus.cases {
            if !ids.insert(&case.id) {
                return Err(LabError::Manifest(format!("duplicate case id {}", case.id)));
            }
            if !matches!(case.expected_route.as_str(), "deterministic" | "model") {
                return Err(LabError::Manifest(format!(
                    "case {} has unknown route {}",
                    case.id, case.expected_route
                )));
            }
            if case
                .tier
                .as_deref()
                .is_some_and(|tier| !matches!(tier, "sanity" | "localized" | "nonlocal"))
            {
                return Err(LabError::Manifest(format!(
                    "case {} has unknown tier {}",
                    case.id,
                    case.tier.as_deref().unwrap_or_default()
                )));
            }
            if case
                .surface
                .as_deref()
                .is_some_and(|surface| surface.trim().is_empty())
            {
                return Err(LabError::Manifest(format!(
                    "case {} has an empty surface",
                    case.id
                )));
            }
        }
        Ok(corpus)
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || (a.file_name().is_some() && a.file_name() == b.file_name())
}

impl CorpusCase {
    fn tier(&self) -> String {
        self.tier.clone().unwrap_or_else(|| {
            if self.expected_route == "deterministic" {
                "sanity"
            } else if self.cause_file.is_some() || self.expected_file.is_none() {
                "nonlocal"
            } else {
                "localized"
            }
            .into()
        })
    }

    fn surface(&self) -> String {
        self.surface.clone().unwrap_or_else(|| {
            match self.tool.as_str() {
                "bibtex" | "biber" => "bibliography",
                "makeindex" => "index",
                "makeglossaries" => "glossary",
                "nomenclature" => "nomenclature",
                _ if self.id.contains("missing-graphicx") => "preamble",
                _ => "document-body",
            }
            .into()
        })
    }

    pub fn prepare(
        &self,
        corpus_root: &Path,
        profile: ContextProfile,
    ) -> Result<PreparedCase, LabError> {
        let fail = |message: String| LabError::Case {
            id: self.id.clone(),
            message,
        };
        let log_path = corpus_root.join(&self.log);
        let source_path = corpus_root.join(&self.source);
        let root_path = corpus_root.join(self.root.as_ref().unwrap_or(&self.source));
        let project_root = root_path
            .parent()
            .ok_or_else(|| fail("root document has no project directory".into()))?
            .to_path_buf();
        let input = root_path
            .strip_prefix(&project_root)
            .map_err(|_| fail("root document escapes its project".into()))?
            .to_path_buf();
        let project_source_path = self
            .project_source
            .as_ref()
            .map(|path| corpus_root.join(path));
        let target_path = project_source_path.as_ref().unwrap_or(&source_path);
        let target = target_path
            .strip_prefix(&project_root)
            .map_err(|_| fail("author source is outside the root document's project".into()))?
            .to_path_buf();
        let diagnostic_source = source_path
            .strip_prefix(&project_root)
            .map_err(|_| fail("diagnostic source is outside the root document's project".into()))?
            .to_path_buf();
        let fixed_path = corpus_root.join(&self.fixed);
        let log = read(&log_path)?;
        let source = read(&source_path)?;
        let project_source = project_source_path.as_deref().map(read).transpose()?;
        let broken = project_source
            .as_ref()
            .unwrap_or(&source)
            .as_bytes()
            .to_vec();
        let fixed = read(&fixed_path)?.into_bytes();
        if broken == fixed {
            return Err(fail("broken and fixed sources are identical".into()));
        }
        if !root_path.is_file() {
            return Err(fail(format!(
                "root document does not exist: {}",
                root_path.display()
            )));
        }

        let mut report = if self.tool == "tex" {
            crate::parse_tex_log(&log, &self.source)
        } else {
            let tool = Tool::parse(&self.tool)
                .ok_or_else(|| fail(format!("unknown tool {}", self.tool)))?;
            crate::parse_tool_log(&log, tool)
        };
        report.cluster();
        let mut diagnostic = report
            .first_error()
            .cloned()
            .ok_or_else(|| fail("parser found no error".into()))?;
        if diagnostic.kind.name() != self.expected_kind {
            return Err(fail(format!(
                "expected kind {}, found {}",
                self.expected_kind,
                diagnostic.kind.name()
            )));
        }
        match (&self.expected_file, diagnostic.reported.as_ref()) {
            (Some(expected), Some(found)) if !same_file(expected, &found.file) => {
                return Err(fail(format!(
                    "expected file {}, found {}",
                    expected.display(),
                    found.file.display()
                )));
            }
            (Some(expected), None) => {
                return Err(fail(format!(
                    "expected source file {}, found no location",
                    expected.display()
                )));
            }
            (None, Some(found)) => {
                return Err(fail(format!(
                    "expected no source file, found {}",
                    found.file.display()
                )));
            }
            _ => {}
        }
        let found_line = diagnostic.reported.as_ref().map(|span| span.line);
        if found_line != self.expected_line {
            return Err(fail(format!(
                "expected line {:?}, found {:?}",
                self.expected_line, found_line
            )));
        }
        match (&self.cause_file, self.cause_line) {
            (Some(file), Some(line)) => diagnostic.cause = Some(aitex_core::Span::new(file, line)),
            (None, None) => {}
            _ => {
                return Err(fail(
                    "cause_file and cause_line must either both be set or both be null".into(),
                ));
            }
        }

        let cascade = rules::default_cascade();
        let rule_source = project_source.as_deref().unwrap_or(&source);
        let candidate = rules::run(
            &cascade,
            &diagnostic,
            &Context::default(),
            rules::Source {
                text: Some(rule_source),
            },
        );
        let mut local = None;
        let mut deterministic = None;
        if let Some((rule, fix)) = candidate {
            match apply_action(&fix.action, &diagnostic, &broken) {
                Ok(Some(after)) => {
                    let patch = unified_diff(&target, &broken, &after);
                    local = Some(LocalFinding {
                        rule,
                        fix: fix.clone(),
                        mechanically_fixable: true,
                        refusal: None,
                    });
                    deterministic = Some((rule, fix, patch));
                }
                result => {
                    local = Some(LocalFinding {
                        rule,
                        fix,
                        mechanically_fixable: false,
                        refusal: result.err().map(|error| error.to_string()),
                    });
                }
            }
        }

        let route = if let Some((rule, fix, patch)) = deterministic {
            Route::Deterministic { rule, fix, patch }
        } else {
            let note = local.as_ref().map(|finding| {
                format!(
                    "Local diagnosis from {}: {}; no safe deterministic edit was found.",
                    finding.rule,
                    finding.fix.rationale.trim_end_matches(['.', '!', '?'])
                )
            });
            let mut files = vec![ProjectFile::new(diagnostic_source, source)];
            if let Some(text) = project_source {
                files.push(ProjectFile::new(&target, text));
            }
            let hint = local
                .as_ref()
                .and_then(|finding| finding.fix.action.source_line())
                .map(|line| aitex_core::Span::new(&target, line));
            Route::Model(build_prompt(
                &diagnostic,
                &files,
                profile,
                note.as_deref(),
                hint.as_ref(),
            ))
        };
        let actual_route = match route {
            Route::Deterministic { .. } => "deterministic",
            Route::Model(_) => "model",
        };
        if actual_route != self.expected_route {
            return Err(fail(format!(
                "expected route {}, found {actual_route}",
                self.expected_route
            )));
        }

        Ok(PreparedCase {
            id: self.id.clone(),
            tier: self.tier(),
            surface: self.surface(),
            repair: self.repair.clone(),
            diagnostic,
            route,
            local,
            project_root,
            input,
            target,
            broken,
            fixed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_core::Source;

    fn corpus_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/repair-lab/manifest.json")
    }

    #[test]
    fn every_tracked_case_matches_its_oracle() {
        let path = corpus_path();
        let corpus = Corpus::load(&path).expect("load corpus");
        let root = path.parent().expect("manifest parent");
        assert_eq!(corpus.cases.len(), 8);

        let mut deterministic = 0;
        let mut locally_explained = 0;
        let mut model = 0;
        let mut locally_explained_only = std::collections::BTreeSet::new();
        let mut mechanically_fixed = std::collections::BTreeSet::new();
        let mut external = std::collections::BTreeSet::new();
        let mut tiers = std::collections::BTreeSet::new();
        let mut surfaces = std::collections::BTreeSet::new();
        let mut generated_product_causes = 0;
        for case in &corpus.cases {
            let prepared = case
                .prepare(root, ContextProfile::Compact)
                .unwrap_or_else(|error| panic!("{error}"));
            locally_explained += usize::from(prepared.local.is_some());
            tiers.insert(prepared.tier.clone());
            surfaces.insert(prepared.surface.clone());
            if prepared
                .local
                .as_ref()
                .is_some_and(|finding| !finding.mechanically_fixable)
            {
                locally_explained_only.insert(prepared.id.clone());
            }
            match prepared.route {
                Route::Deterministic { ref patch, .. } => {
                    deterministic += 1;
                    mechanically_fixed.insert(prepared.id.clone());
                    assert_eq!(crate::patch_target(patch).unwrap(), prepared.target);
                    assert_eq!(
                        crate::apply_unified_patch(patch, &prepared.broken).unwrap(),
                        prepared.fixed
                    );
                }
                Route::Model(packet) => {
                    model += 1;
                    assert!(
                        packet.estimated_input_tokens <= 400,
                        "{} used {} estimated tokens",
                        case.id,
                        packet.estimated_input_tokens
                    );
                    if case.id == "tex-unclosed-brace" {
                        assert!(packet.included_source_lines > 0);
                        assert!(
                            packet.envelope["context"]["source"][0]["text"]
                                .as_str()
                                .expect("source excerpt")
                                .contains("3|\\textbf{")
                        );
                    }
                }
            }
            if let Source::ToolLog(tool) = prepared.diagnostic.source {
                external.insert(tool);
            }
            if matches!(prepared.diagnostic.gap(), aitex_core::Gap::DifferentFile) {
                generated_product_causes += 1;
            }
        }
        assert_eq!((deterministic, model), (2, 6));
        assert_eq!(locally_explained, 3);
        assert_eq!(
            locally_explained_only,
            std::collections::BTreeSet::from(["tex-unclosed-brace".into()])
        );
        assert_eq!(
            mechanically_fixed,
            std::collections::BTreeSet::from([
                "bibtex-missing-comma".into(),
                "tex-missing-graphicx".into()
            ])
        );
        assert_eq!(
            external.len(),
            5,
            "all external tool identities are covered"
        );
        assert_eq!(
            generated_product_causes, 3,
            "index/glossary products map back to author source"
        );
        assert_eq!(
            tiers,
            std::collections::BTreeSet::from([
                "localized".into(),
                "nonlocal".into(),
                "sanity".into()
            ])
        );
        assert_eq!(surfaces.len(), 6, "failure surfaces remain stratified");
    }

    #[test]
    fn all_prompt_profiles_stay_inside_pilot_budgets() {
        let path = corpus_path();
        let corpus = Corpus::load(&path).expect("load corpus");
        let root = path.parent().expect("manifest parent");
        for (profile, budget) in [
            // v1.1 adds the exact human-facing failure block. The smallest
            // profile pays for that useful context while source-bearing
            // compact and expanded profiles retain their v1 ceilings.
            (ContextProfile::Diagnostic, 300),
            (ContextProfile::Compact, 400),
            (ContextProfile::Expanded, 750),
        ] {
            for case in &corpus.cases {
                if let Route::Model(packet) = case.prepare(root, profile).expect("prepare").route {
                    assert!(
                        packet.estimated_input_tokens <= budget,
                        "{} {} used {} estimated tokens",
                        case.id,
                        profile,
                        packet.estimated_input_tokens
                    );
                }
            }
        }
    }
}
