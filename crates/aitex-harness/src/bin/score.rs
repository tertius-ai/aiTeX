//! Score the H1 log parser against the broken corpus.
//!
//! Reads the `.log` files the corpus tooling already produced and asks: on how
//! many mutants does the parser recover a classified error, and how does that
//! hold up as the localization gap grows? Everything is reported per gap bucket,
//! because an average hides exactly the behavior we care about.
//!
//! Usage: `score <dir-of-logs>` where each file is `<id>.log`.

use aitex_core::{Kind, Severity};
use std::collections::BTreeMap;
use std::path::Path;

/// Fix context for the project a log came from, cached per project.
///
/// Without the project's own macro definitions, spelling correction has no
/// candidates and the largest error category goes unrepaired.
/// The mutated source the log came from, so structural rules have text to
/// match brackets in. Materialized alongside the logs by `tools/dump_logs.py`.
fn mutant_source(log_name: &str) -> Option<String> {
    let stem = std::path::Path::new(log_name).file_stem()?.to_str()?;
    let dir = std::env::var("SRC_DIR").unwrap_or_else(|_| "corpus/sources".into());
    let p = std::path::Path::new(&dir).join(format!("{stem}.tex"));
    aitex_harness::read_log(&p)
}

fn ctx_for(log_name: &str) -> aitex_harness::fixes::Context {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BTreeMap<String, ()>>> = OnceLock::new();
    let _ = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));

    // `corpus/logs/<project>__<n>.log` -> the project's source directory.
    let stem = std::path::Path::new(log_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let project = stem.split("__").next().unwrap_or("");
    let dir = std::path::Path::new("corpus/meta/ctx").join(project);
    let root = dir.join("root.tex");
    if root.exists() {
        aitex_harness::scan::context_for_project(&root)
    } else {
        aitex_harness::fixes::Context::default()
    }
}

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => d,
        None => {
            eprintln!("usage: score <dir-of-logs>");
            std::process::exit(2);
        }
    };
    let dir = Path::new(&dir);

    let mut total = 0usize;
    let mut classified = 0usize;
    let mut located = 0usize;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut collected: Vec<(Kind, Severity, String)> = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot read {}: {e}", dir.display());
            std::process::exit(1);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        // Lossy: TeX logs carry raw 8-bit font bytes and are not valid UTF-8.
        // Strict reading silently dropped 51/1000 of this corpus.
        let Some(text) = aitex_harness::read_log(&path) else {
            continue;
        };
        total += 1;

        let report = aitex_harness::parse_tex_log(&text, &path);
        if std::env::var("SCORE_DEBUG").is_ok() && report.errors().next().is_none() {
            let marker = text
                .lines()
                .find(|l| l.starts_with("! ") || l.contains(".tex:"))
                .unwrap_or("<none>");
            eprintln!(
                "UNCLASSIFIED {}: {}",
                path.display(),
                &marker[..marker.len().min(90)]
            );
        }
        let Some(err) = report.errors().next() else {
            *kinds.entry("<no error found>".into()).or_default() += 1;
            continue;
        };
        collected.push((err.kind.clone(), err.severity, path.display().to_string()));
        classified += 1;
        if err.reported.is_some() {
            located += 1;
        }
        let key = match &err.kind {
            Kind::UndefinedControlSequence { .. } => "UndefinedControlSequence",
            Kind::MissingPackage { .. } => "MissingPackage",
            Kind::MissingFile { .. } => "MissingFile",
            Kind::MismatchedEnvironment { .. } => "MismatchedEnvironment",
            Kind::UndefinedEnvironment { .. } => "UndefinedEnvironment",
            Kind::UndefinedCounter { .. } => "UndefinedCounter",
            Kind::PackageError { .. } => "PackageError",
            Kind::RunawayArgument => "RunawayArgument",
            Kind::MissingMathShift => "MissingMathShift",
            Kind::MissingBrace => "MissingBrace",
            Kind::ExtraAlignmentTab => "ExtraAlignmentTab",
            Kind::CapacityExceeded { .. } => "CapacityExceeded",
            Kind::Bibliography => "Bibliography",
            Kind::Index => "Index",
            Kind::Glossary => "Glossary",
            other => {
                if err.severity == Severity::Fatal {
                    "Fatal(unclassified)"
                } else {
                    let _ = other;
                    "Other"
                }
            }
        };
        *kinds.entry(key.into()).or_default() += 1;
    }

    // --- H2 deterministic coverage -------------------------------------
    // Measured, not assumed. The plan targets 40-60% at zero token cost, and
    // this is the number that claim has to survive contact with.
    let mut fix_total = 0usize;
    let mut fix_hit = 0usize;
    let mut by_action: BTreeMap<&'static str, usize> = BTreeMap::new();
    let cascade = aitex_harness::rules::default_cascade();
    for (kind, sev, name) in &collected {
        let d = aitex_core::Diagnostic::new(*sev, kind.clone(), "", aitex_core::Source::TexLog);
        fix_total += 1;
        let ctx = ctx_for(name);
        let src_text = mutant_source(name);
        let src = aitex_harness::rules::Source {
            text: src_text.as_deref(),
        };
        if let Some((rule_name, f)) = aitex_harness::rules::run(&cascade, &d, &ctx, src) {
            if !matches!(
                f.action,
                aitex_harness::fixes::Action::Diagnose { .. }
                    | aitex_harness::fixes::Action::ReportOnly { .. }
            ) {
                fix_hit += 1;
            }
            *by_action.entry(rule_name).or_default() += 1;
            if std::env::var("SCORE_JSONL").is_ok() {
                let line = match &f.action {
                    aitex_harness::fixes::Action::InsertAt { line, .. }
                    | aitex_harness::fixes::Action::Diagnose { line, .. } => *line as i64,
                    _ => -1,
                };
                println!(
                    "#JSONL {{\"id\":\"{}\",\"rule\":\"{}\",\"line\":{}}}",
                    std::path::Path::new(name)
                        .file_stem()
                        .unwrap()
                        .to_string_lossy(),
                    rule_name,
                    line
                );
            }
        }
        if false {
            let f = aitex_harness::fixes::Fix {
                action: aitex_harness::fixes::Action::CleanAuxiliaries,
                rationale: String::new(),
                confidence: aitex_harness::fixes::Confidence::Possible,
            };
            let key = match f.action {
                aitex_harness::fixes::Action::RerunPipeline { .. } => "rerun",
                aitex_harness::fixes::Action::CleanAuxiliaries => "clean",
                aitex_harness::fixes::Action::InstallPackage { .. } => "install-package",
                aitex_harness::fixes::Action::AddPackage { .. } => "add-package",
                aitex_harness::fixes::Action::ReplaceControlSequence { .. } => "spelling",
                aitex_harness::fixes::Action::AppendToLine { .. } => "append-line",
                aitex_harness::fixes::Action::FixEnvironmentDelimiter { .. } => "env-delimiter",
                aitex_harness::fixes::Action::InsertAt { .. } => "structural-insert",
                aitex_harness::fixes::Action::Diagnose { .. } => "structural-diagnose",
                aitex_harness::fixes::Action::ReportOnly { .. } => "report-only(not a fix)",
            };
            let _ = (key, f);
        }
        let _ = name;
    }

    println!("logs parsed              : {total}");
    if total == 0 {
        return;
    }
    println!(
        "error recovered          : {classified} ({:.1}%)",
        100.0 * classified as f64 / total as f64
    );
    println!(
        "  ...with a line number  : {located} ({:.1}% of all logs)",
        100.0 * located as f64 / total as f64
    );
    println!(
        "  ...unlocalizable       : {} ({:.1}% of all logs)",
        classified - located,
        100.0 * (classified - located) as f64 / total as f64
    );
    println!(
        "\nH2 deterministic coverage: {fix_hit}/{fix_total} ({:.1}%) at zero token cost",
        100.0 * fix_hit as f64 / fix_total.max(1) as f64
    );
    for (k, n) in &by_action {
        println!("  {n:5}  {k}");
    }

    println!("\nfirst-error classification:");
    let mut rows: Vec<_> = kinds.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (k, n) in rows {
        println!("  {n:5}  {k}");
    }
}
