#[derive(Clone, Debug, Eq, PartialEq)]
struct LintFinding {
    rule: &'static str,
    kind: &'static str,
    subject: String,
    message: String,
    evidence: String,
    file: Option<std::path::PathBuf>,
    line: Option<u32>,
}

#[derive(Default)]
struct AuxiliaryFacts {
    citations: std::collections::BTreeSet<String>,
    bibcites: std::collections::BTreeSet<String>,
    labels: std::collections::BTreeMap<String, usize>,
}

fn lint_braced_value<'a>(line: &'a str, command: &str) -> Option<&'a str> {
    let rest = line.trim_start().strip_prefix(command)?.trim_start();
    let rest = rest.strip_prefix('{')?;
    Some(rest.split_once('}')?.0)
}

fn collect_auxiliary_facts_at(
    path: &std::path::Path,
    seen: &mut std::collections::BTreeSet<std::path::PathBuf>,
    facts: &mut AuxiliaryFacts,
) {
    if seen.len() >= 512 {
        return;
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(canonical) {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    if bytes.len() > 8 * 1024 * 1024 {
        return;
    }
    let text: String = bytes.into_iter().map(char::from).collect();
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(value) = lint_braced_value(line, "\\citation") {
            facts.citations.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|key| !key.is_empty() && *key != "*")
                    .map(str::to_string),
            );
        }
        if let Some(value) = lint_braced_value(line, "\\bibcite")
            && !value.trim().is_empty()
        {
            facts.bibcites.insert(value.trim().to_string());
        }
        if let Some(value) = lint_braced_value(line, "\\newlabel")
            && !value.trim().is_empty()
        {
            *facts.labels.entry(value.trim().to_string()).or_default() += 1;
        }
        if let Some(value) = lint_braced_value(line, "\\@input") {
            let included = path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(value.trim());
            collect_auxiliary_facts_at(&included, seen, facts);
        }
    }
}

fn collect_auxiliary_facts(path: &std::path::Path) -> AuxiliaryFacts {
    let mut facts = AuxiliaryFacts::default();
    collect_auxiliary_facts_at(path, &mut std::collections::BTreeSet::new(), &mut facts);
    facts
}

fn unwrap_lint_log(raw: &str) -> Vec<String> {
    let mut lines = Vec::<String>::new();
    for line in raw.lines() {
        if lines
            .last()
            .is_some_and(|prior| prior.chars().count() == 79)
        {
            lines.last_mut().expect("checked above").push_str(line);
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn lint_quoted_subject(line: &str, label: &str) -> Option<String> {
    let tail = line.split_once(label)?.1;
    let start = tail.find(['`', '\''])?;
    let tail = &tail[start + 1..];
    let end = tail.find(['\'', '`'])?;
    let subject = tail[..end].trim();
    (!subject.is_empty() && subject.len() <= 256).then(|| subject.to_string())
}

fn lint_input_line(line: &str) -> Option<u32> {
    let tail = line.split_once("input line ")?.1;
    tail.trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn strip_tex_comment(line: &str) -> &str {
    for (at, character) in line.char_indices() {
        if character != '%' {
            continue;
        }
        let slashes = line[..at]
            .chars()
            .rev()
            .take_while(|character| *character == '\\')
            .count();
        if slashes % 2 == 0 {
            return &line[..at];
        }
    }
    line
}

fn lint_project(
    input: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap_or_default())
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
    let requested = std::path::Path::new(input);
    let absolute = std::fs::canonicalize(requested).unwrap_or_else(|_| {
        if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            cwd.join(requested)
        }
    });
    let root = if absolute.starts_with(&cwd) {
        cwd
    } else {
        absolute
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    };
    let display = absolute
        .strip_prefix(&root)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|_| requested.to_path_buf());
    (root, absolute, display)
}

fn lint_author_sources(
    root: &std::path::Path,
    input: &std::path::Path,
    observation: &BuildObservation,
) -> Vec<std::path::PathBuf> {
    let mut sources = std::collections::BTreeSet::new();
    sources.insert(input.to_path_buf());
    for dependency in &observation.dependencies {
        let Some(resolved) = dependency.resolved.as_deref() else {
            continue;
        };
        let path = std::path::Path::new(resolved);
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            }
        });
        if !canonical.starts_with(root) || !canonical.is_file() {
            continue;
        }
        if !matches!(
            canonical.extension().and_then(|extension| extension.to_str()),
            Some("tex" | "ltx" | "sty" | "cls")
        ) {
            continue;
        }
        if sources.len() < 4096 {
            sources.insert(canonical);
        }
    }
    sources.into_iter().collect()
}

fn lint_source_location(
    rule: &str,
    subject: &str,
    root: &std::path::Path,
    sources: &[std::path::PathBuf],
) -> Option<(std::path::PathBuf, u32)> {
    let commands: &[&str] = match rule {
        "label/duplicate@1" => &["\\label"],
        "citation/undefined@1" => &["\\cite"],
        "reference/undefined@1" => {
            &["\\ref", "\\eqref", "\\pageref", "\\autoref", "\\cref", "\\Cref"]
        }
        _ => &[],
    };
    for path in sources {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.len() > 16 * 1024 * 1024 {
            continue;
        }
        let text: String = bytes.into_iter().map(char::from).collect();
        for (index, line) in text.lines().enumerate() {
            let line = strip_tex_comment(line);
            if line.contains(subject) && commands.iter().any(|command| line.contains(command)) {
                let display = path
                    .strip_prefix(root)
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|_| path.to_path_buf());
                return Some((display, index as u32 + 1));
            }
        }
    }
    None
}

fn insert_lint_finding(
    findings: &mut std::collections::BTreeMap<(&'static str, String), LintFinding>,
    rule: &'static str,
    kind: &'static str,
    subject: String,
    message: String,
    evidence: String,
    fallback_line: Option<u32>,
    root: &std::path::Path,
    input_display: &std::path::Path,
    sources: &[std::path::PathBuf],
) {
    let (file, line) = lint_source_location(rule, &subject, root, sources)
        .map(|(file, line)| (Some(file), Some(line)))
        .unwrap_or_else(|| {
            (
                fallback_line.map(|_| input_display.to_path_buf()),
                fallback_line,
            )
        });
    findings
        .entry((rule, subject.clone()))
        .or_insert(LintFinding {
            rule,
            kind,
            subject,
            message,
            evidence,
            file,
            line,
        });
}

fn lint_findings(
    input: &str,
    observation: &BuildObservation,
) -> Vec<LintFinding> {
    let (root, absolute_input, input_display) = lint_project(input);
    let sources = lint_author_sources(&root, &absolute_input, observation);
    let job = absolute_input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("texput");
    let mut findings = std::collections::BTreeMap::new();

    let facts = collect_auxiliary_facts(std::path::Path::new(&format!("{job}.aux")));
    for key in facts.citations.difference(&facts.bibcites) {
        insert_lint_finding(
            &mut findings,
            "citation/undefined@1",
            "undefined-citation",
            key.clone(),
            format!("citation `{key}` remained unresolved after the complete build"),
            format!("the converged auxiliary file cites `{key}` but records no bibliography entry"),
            None,
            &root,
            &input_display,
            &sources,
        );
    }
    for (label, count) in facts.labels.iter().filter(|(_, count)| **count > 1) {
        insert_lint_finding(
            &mut findings,
            "label/duplicate@1",
            "duplicate-label",
            label.clone(),
            format!("label `{label}` was defined more than once"),
            format!("the converged auxiliary tree contains {count} definitions of `{label}`"),
            None,
            &root,
            &input_display,
            &sources,
        );
    }

    if let Ok(bytes) = std::fs::read(format!("{job}.log"))
        && bytes.len() <= 64 * 1024 * 1024
    {
        let text: String = bytes.into_iter().map(char::from).collect();
        for line in unwrap_lint_log(&text) {
            if line.contains("Citation") && line.contains("undefined") {
                if let Some(key) = lint_quoted_subject(&line, "Citation") {
                    insert_lint_finding(
                        &mut findings,
                        "citation/undefined@1",
                        "undefined-citation",
                        key.clone(),
                        format!("citation `{key}` remained unresolved after the complete build"),
                        line.trim().to_string(),
                        lint_input_line(&line),
                        &root,
                        &input_display,
                        &sources,
                    );
                }
            } else if line.contains("Reference") && line.contains("undefined") {
                if let Some(label) = lint_quoted_subject(&line, "Reference") {
                    insert_lint_finding(
                        &mut findings,
                        "reference/undefined@1",
                        "undefined-reference",
                        label.clone(),
                        format!("reference `{label}` remained unresolved after the complete build"),
                        line.trim().to_string(),
                        lint_input_line(&line),
                        &root,
                        &input_display,
                        &sources,
                    );
                }
            } else if line.contains("Label") && line.contains("multiply defined") {
                if let Some(label) = lint_quoted_subject(&line, "Label") {
                    insert_lint_finding(
                        &mut findings,
                        "label/duplicate@1",
                        "duplicate-label",
                        label.clone(),
                        format!("label `{label}` was defined more than once"),
                        line.trim().to_string(),
                        lint_input_line(&line),
                        &root,
                        &input_display,
                        &sources,
                    );
                }
            }
        }
    }
    findings.into_values().collect()
}

fn lint_json(input: &str, findings: &[LintFinding]) -> serde_json::Value {
    serde_json::json!({
        "schema": "aitex-lint-report-v1",
        "input": input,
        "build_succeeded": true,
        "findings": findings.iter().map(|finding| serde_json::json!({
            "rule": finding.rule,
            "kind": finding.kind,
            "severity": "warning",
            "confidence": "high",
            "subject": finding.subject,
            "message": finding.message,
            "evidence": finding.evidence,
            "span": finding.file.as_ref().map(|file| serde_json::json!({
                "file": file.to_string_lossy(),
                "line": finding.line,
            })),
            "suppressible": false,
        })).collect::<Vec<_>>(),
    })
}

fn show_lint_findings(findings: &[LintFinding]) {
    if !cli_present_output() {
        return;
    }
    let style = cli_style();
    for finding in findings {
        let location = finding
            .file
            .as_ref()
            .map(|file| match finding.line {
                Some(line) => format!(" · {}:{line}", file.display()),
                None => format!(" · {}", file.display()),
            })
            .unwrap_or_default();
        eprintln!(
            "  {} {} {}{}",
            style.yellow("◆"),
            style.bold("Lint     "),
            finding.kind,
            style.dim(&location),
        );
        eprintln!("    {} {}", style.bold("Evidence "), finding.evidence);
        eprintln!(
            "    {} {}",
            style.bold("Rule     "),
            style.dim(&format!("{} · high confidence", finding.rule)),
        );
    }
}

fn run_lint() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let input = input_argument(&arguments)
        .expect("lint arguments were validated")
        .to_string();
    let output = output_argument(&arguments);
    let json = arguments.iter().any(|argument| argument == "--json");
    let deny = arguments
        .windows(2)
        .any(|pair| pair[0] == "--deny" && pair[1] == "warnings");
    let started = std::time::Instant::now();
    let (failure, observation) = run_native_build_observed();
    if failure.is_some() {
        return;
    }
    let lint_started = std::time::Instant::now();
    let findings = lint_findings(&input, &observation);
    if json {
        println!(
            "{}",
            serde_json::to_string(&lint_json(&input, &findings))
                .expect("lint report contains only serializable values")
        );
    } else if findings.is_empty() {
        cli_phase("Lint", "clean · 3 high-confidence rules", lint_started.elapsed());
    } else {
        show_lint_findings(&findings);
    }
    if deny && !findings.is_empty() {
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        cli_fail(started.elapsed());
    } else {
        cli_finish(output.as_deref(), started.elapsed(), false);
    }
}

#[cfg(test)]
mod lint_tests {
    use super::{
        collect_auxiliary_facts, lint_input_line, lint_quoted_subject, strip_tex_comment,
        unwrap_lint_log,
    };

    fn temporary(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aitex-lint-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn auxiliary_tree_records_executed_citations_and_duplicate_labels() {
        let root = temporary("aux");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.aux"),
            b"\\citation{known,missing}\n\\bibcite{known}{1}\n\\newlabel{x}{{1}{1}}\n\\@input{part.aux}\n",
        )
        .unwrap();
        std::fs::write(root.join("part.aux"), b"\\newlabel{x}{{2}{2}}\n").unwrap();
        let facts = collect_auxiliary_facts(&root.join("main.aux"));
        assert_eq!(facts.citations.len(), 2);
        assert!(facts.bibcites.contains("known"));
        assert_eq!(facts.labels.get("x"), Some(&2));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn log_warning_fields_are_bounded_and_source_lines_are_recovered() {
        let warning = "LaTeX Warning: Reference `sec:x' on page 1 undefined on input line 42.";
        assert_eq!(lint_quoted_subject(warning, "Reference").as_deref(), Some("sec:x"));
        assert_eq!(lint_input_line(warning), Some(42));
        let split = format!("{}\n{}\n", "x".repeat(79), "tail");
        assert_eq!(unwrap_lint_log(&split), vec![format!("{}tail", "x".repeat(79))]);
    }

    #[test]
    fn escaped_percent_is_source_but_an_unescaped_percent_starts_a_comment() {
        assert_eq!(strip_tex_comment("text % \\ref{x}"), "text ");
        assert_eq!(strip_tex_comment("text \\% \\ref{x}"), "text \\% \\ref{x}");
    }
}
