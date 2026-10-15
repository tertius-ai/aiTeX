// Feature-gated, read-only failure explanation.
//
// Compilation and semantic-tool orchestration remain the ordinary native
// path. This layer begins only after that path returns a structured failure.

fn explain_option<'a>(arguments: &'a [String], option: &str) -> Option<&'a str> {
    arguments
        .iter()
        .position(|argument| argument == option)
        .and_then(|at| arguments.get(at + 1))
        .map(String::as_str)
}

fn model_backend(
    provider: &str,
    model: &str,
    arguments: &[String],
) -> Result<Box<dyn aitex_harness::Backend>, aitex_harness::BackendError> {
    model_backend_with_timeout(
        provider,
        model,
        std::time::Duration::from_secs(120),
        arguments,
    )
}

fn model_backend_with_timeout(
    provider: &str,
    model: &str,
    timeout: std::time::Duration,
    arguments: &[String],
) -> Result<Box<dyn aitex_harness::Backend>, aitex_harness::BackendError> {
    match provider {
        "anthropic" => Ok(Box::new(
            aitex_harness::anthropic::Anthropic::from_env_with_timeout(model, timeout)?,
        )),
        "codex" => Ok(Box::new(
            aitex_harness::codex::Codex::from_env_with_timeout(model, timeout)?,
        )),
        "openrouter" => {
            let routing = explain_option(arguments, "--openrouter-provider").map_or(
                aitex_harness::openrouter::RoutingPolicy::Routed,
                |provider| aitex_harness::openrouter::RoutingPolicy::Pinned(provider.into()),
            );
            let seed = explain_option(arguments, "--openrouter-seed")
                .and_then(|value| value.parse().ok());
            Ok(Box::new(
                aitex_harness::openrouter::OpenRouter::from_env_with_options(
                    model,
                    aitex_harness::openrouter::OpenRouterOptions {
                        routing,
                        ignored_providers: Vec::new(),
                        seed,
                        timeout,
                        zero_data_retention: true,
                        max_prompt_price_per_million: None,
                        max_completion_price_per_million: None,
                    },
                )?,
            ))
        }
        _ => unreachable!("provider arguments are validated at the front door"),
    }
}

fn explain_line(mark: &str, label: &str, detail: impl std::fmt::Display) {
    if cli_present_output() {
        let style = cli_style();
        eprintln!(
            "  {} {} {detail}",
            match mark {
                "✓" => style.green(mark),
                "◆" => style.yellow(mark),
                _ => style.red(mark),
            },
            style.bold(&format!("{label:<9}")),
        );
    } else {
        eprintln!("aitex explain: {label}: {detail}");
    }
}

fn explain_note(label: &str, text: impl std::fmt::Display) {
    if cli_present_output() {
        let style = cli_style();
        eprintln!("    {} {text}", style.cyan(label));
    } else {
        eprintln!("aitex explain: {label}: {text}");
    }
}

fn model_usage_text(response: &aitex_harness::Response, elapsed: std::time::Duration) -> String {
    let cache = if response.usage.cache_read == 0 && response.usage.cache_write == 0 {
        String::new()
    } else {
        format!(
            " ({} cached + {} write)",
            response.usage.cache_read, response.usage.cache_write
        )
    };
    let reasoning = if response.usage.reasoning == 0 {
        String::new()
    } else {
        format!(" ({} reasoning)", response.usage.reasoning)
    };
    format!(
        "{} input{} / {} output{} · {}{}",
        response.usage.total_input(),
        cache,
        response.usage.output,
        reasoning,
        human_duration(elapsed),
        response
            .metadata
            .charged_microusd
            .map(|cost| format!(" · ${:.6}", cost as f64 / 1_000_000.0))
            .unwrap_or_default()
    )
}

fn confidence_name(confidence: aitex_harness::Confidence) -> &'static str {
    match confidence {
        aitex_harness::Confidence::Possible => "possible",
        aitex_harness::Confidence::Probable => "probable",
        aitex_harness::Confidence::Certain => "certain",
    }
}

fn action_text(action: &aitex_harness::Action) -> String {
    use aitex_harness::Action;
    match action {
        Action::RerunPipeline { passes } => format!("rerun the build pipeline ({passes} passes)"),
        Action::CleanAuxiliaries => "remove stale generated auxiliaries, then rebuild".into(),
        Action::InstallPackage { file } => format!("install the TeX package providing {file}"),
        Action::AddPackage { package } => format!("add \\usepackage{{{package}}} to the preamble"),
        Action::ReplaceControlSequence { from, to } => format!("replace \\{from} with \\{to}"),
        Action::AppendToLine { line, text } => format!("append {text} to line {line}"),
        Action::FixEnvironmentDelimiter { from, to } => {
            format!("replace environment delimiter {from} with {to}")
        }
        Action::InsertAt { line, what, note } => {
            format!("insert {what} at line {line} ({note})")
        }
        Action::Diagnose { line, what, note } => {
            format!("inspect {what} at line {line} ({note})")
        }
        Action::ReportOnly { reason } => reason.clone(),
    }
}

fn settled_without_model(action: &aitex_harness::Action) -> bool {
    !matches!(action, aitex_harness::Action::Diagnose { .. })
}

fn needs_log_enrichment(diagnostic: &aitex_core::Diagnostic) -> bool {
    matches!(diagnostic.kind, aitex_core::Kind::Other)
        || matches!(
            &diagnostic.kind,
            aitex_core::Kind::UndefinedControlSequence { name } if name.is_empty()
        )
}

fn first_diagnostic(mut failure: BuildFailure, input: &str) -> aitex_core::Diagnostic {
    failure.report.cluster();
    let structured = failure
        .report
        .first_error()
        .cloned()
        .expect("every BuildFailure contains an error");
    // Engine-native structure is authoritative. Its `.log` is consulted only
    // when the engine has not yet classified enough detail for a useful rule.
    let mut resolved = structured.clone();
    if failure.report.engine == "aitex"
        && needs_log_enrichment(&structured)
        && let Some(path) = failure.transcript_paths.first()
        && let Ok(log) = read_latin1(&path.to_string_lossy())
    {
        let mut parsed = aitex_harness::parse_tex_log(&log, std::path::Path::new(input));
        parsed.cluster();
        if let Some(diagnostic) = parsed.first_error() {
            resolved = diagnostic.clone();
            resolved.provenance = structured.provenance.clone();
        }
    }
    enrich_project_diagnostic(input, &mut resolved);
    resolved
}

fn push_project_file(
    files: &mut Vec<aitex_harness::ProjectFile>,
    root: &std::path::Path,
    path: &std::path::Path,
) {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let Ok(candidate) = std::fs::canonicalize(candidate) else {
        return;
    };
    if !candidate.starts_with(root) {
        return;
    }
    let Ok(relative) = candidate.strip_prefix(root) else {
        return;
    };
    if files.iter().any(|file| file.path == relative) {
        return;
    }
    if let Ok(bytes) = std::fs::read(&candidate) {
        let text = String::from_utf8(bytes.clone())
            .unwrap_or_else(|_| bytes.iter().map(|byte| char::from(*byte)).collect());
        files.push(aitex_harness::ProjectFile::new(relative, text));
    }
}

fn project_files_for(
    diagnostic: &aitex_core::Diagnostic,
    input: &str,
) -> Vec<aitex_harness::ProjectFile> {
    let mut files = Vec::new();
    let input = std::fs::canonicalize(input).unwrap_or_else(|_| std::path::PathBuf::from(input));
    let root = input
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    push_project_file(&mut files, &root, &input);
    if let Some(span) = diagnostic.reported.as_ref() {
        push_project_file(&mut files, &root, &span.file);
    }
    if let Some(span) = diagnostic.cause.as_ref() {
        push_project_file(&mut files, &root, &span.file);
    }
    for frame in &diagnostic.provenance {
        if let Some(span) = frame.span.as_ref() {
            push_project_file(&mut files, &root, &span.file);
        }
    }
    files
}

fn source_for_rule<'a>(
    diagnostic: &aitex_core::Diagnostic,
    files: &'a [aitex_harness::ProjectFile],
) -> Option<&'a aitex_harness::ProjectFile> {
    diagnostic
        .location()
        .and_then(|span| {
            files.iter().find(|file| {
                file.path == span.file
                    || (file.path.file_name().is_some()
                        && file.path.file_name() == span.file.file_name())
            })
        })
        .or_else(|| files.first())
}

fn show_unchanged() {
    explain_line("◆", "Source", "unchanged · explanation is read-only");
    if cli_present_output() {
        eprintln!();
    }
}

fn run_explain() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let input = input_argument(&arguments).expect("explain arguments were validated");
    let started = std::time::Instant::now();
    let Some(failure) = run_native_build() else {
        explain_line("✓", "Explain", "build succeeded · no failure to explain");
        return;
    };
    let diagnostic = first_diagnostic(failure, input);
    let presentation = diagnostic.presentation();
    explain_line("✓", "Diagnose", &presentation.headline);
    explain_note("Error", &presentation.error);
    if let Some(reported) = &presentation.reported {
        explain_note("Reported", reported);
    }
    if presentation.occurrences > 1 {
        explain_note(
            "Repeated",
            format!("{} occurrences", presentation.occurrences),
        );
    }
    for frame in &presentation.provenance {
        explain_note("Trace", frame);
    }

    let files = project_files_for(&diagnostic, input);
    let context = aitex_harness::context_for_project(std::path::Path::new(input));
    let cascade = aitex_harness::rules::default_cascade();
    let local = aitex_harness::rules::run(
        &cascade,
        &diagnostic,
        &context,
        aitex_harness::rules::Source {
            text: source_for_rule(&diagnostic, &files).map(|file| file.text.as_str()),
        },
    );
    if let Some((rule, fix)) = &local
        && settled_without_model(&fix.action)
    {
        explain_line(
            "✓",
            "Local",
            format!("{rule} · {} confidence", confidence_name(fix.confidence)),
        );
        explain_note("Cause", &fix.rationale);
        explain_note("Suggestion", action_text(&fix.action));
        show_unchanged();
        cli_detail!("aitex explain: completed in {}", human_duration(started.elapsed()));
        return;
    }

    let profile = explain_option(&arguments, "--context")
        .and_then(aitex_harness::ContextProfile::parse)
        .unwrap_or(aitex_harness::ContextProfile::Compact);
    let local_note = local
        .as_ref()
        .map(|(rule, fix)| format!("Local rule {rule}: {}", fix.rationale));
    let local_hint = local
        .as_ref()
        .and_then(|(_, fix)| fix.action.source_line())
        .and_then(|line| {
            source_for_rule(&diagnostic, &files)
                .map(|file| aitex_core::Span::new(file.path.clone(), line))
        });
    let packet = aitex_harness::build_prompt(
        &diagnostic,
        &files,
        profile,
        local_note.as_deref(),
        local_hint.as_ref(),
    );
    explain_line(
        "✓",
        "Context",
        format!(
            "{profile} · {} estimated tokens · {} source lines",
            packet.estimated_input_tokens, packet.included_source_lines
        ),
    );

    if arguments.iter().any(|argument| argument == "--prompt-only") {
        println!("{}", packet.request.messages[0].content[0].text);
        explain_line("◆", "AI", "prompt only · no network · no spend");
        show_unchanged();
        return;
    }
    let Some(model) = explain_option(&arguments, "--model") else {
        explain_line(
            "◆",
            "AI",
            "not run · add --model MODEL --allow-source-upload",
        );
        show_unchanged();
        return;
    };
    if !arguments
        .iter()
        .any(|argument| argument == "--allow-source-upload")
    {
        explain_line(
            "◆",
            "AI",
            "not run · --allow-source-upload is required",
        );
        show_unchanged();
        return;
    }

    let provider = explain_option(&arguments, "--provider").unwrap_or("anthropic");
    let mut backend = match model_backend(provider, model, &arguments) {
        Ok(backend) => backend,
        Err(error) => {
            explain_line("×", "AI", &error);
            show_unchanged();
            return;
        }
    };
    let run = match aitex_harness::run_model(backend.as_mut(), &packet) {
        Ok(run) => run,
        Err(error) => {
            explain_line("×", "AI", &error);
            if let aitex_harness::ModelRunError::Response {
                response, elapsed, ..
            } = &error
            {
                explain_note("Usage", model_usage_text(response, *elapsed));
            }
            show_unchanged();
            return;
        }
    };
    let counted = run
        .provider_counted_input_tokens
        .map(|tokens| format!(" · {tokens} counted"))
        .unwrap_or_default();
    explain_line(
        "✓",
        "AI",
        format!(
            "{} · {}{counted}",
            run.response.model,
            model_usage_text(&run.response, run.elapsed),
        ),
    );
    if let Some(provider) = &run.response.metadata.provider {
        explain_note("Provider", provider);
    }
    explain_note("Cause", &run.answer.cause);
    explain_note("Confidence", &run.answer.confidence);
    if !run.answer.proposal.is_empty() {
        explain_note("Edit preview", "");
        eprintln!("{}", run.answer.proposal.preview());
    }
    show_unchanged();
    cli_detail!(
        "aitex explain: completed in {} through {}",
        human_duration(started.elapsed()),
        backend.name()
    );
}

#[cfg(test)]
mod explain_tests {
    use super::{
        BuildFailure, action_text, enrich_project_diagnostic, enrich_project_report,
        first_diagnostic, model_usage_text, needs_log_enrichment, project_files_for,
        settled_without_model, source_for_rule,
    };
    use aitex_harness::Action;

    #[test]
    fn public_explanation_text_never_claims_to_apply_an_action() {
        assert_eq!(
            action_text(&Action::AddPackage {
                package: "graphicx".into()
            }),
            "add \\usepackage{graphicx} to the preamble"
        );
        assert!(!settled_without_model(&Action::Diagnose {
            line: 4,
            what: "x".into(),
            note: "inspect".into(),
        }));
        assert!(settled_without_model(&Action::ReportOnly {
            reason: "unsafe".into()
        }));

        let structured = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::UndefinedControlSequence { name: "foo".into() },
            "Undefined control sequence.",
            aitex_core::Source::Engine,
        );
        assert!(!needs_log_enrichment(&structured));
        let generic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Other,
            "Package-specific error.",
            aitex_core::Source::Engine,
        );
        assert!(needs_log_enrichment(&generic));
    }

    #[test]
    fn model_usage_is_compact_but_keeps_cache_reasoning_time_and_cost() {
        let response = aitex_harness::Response {
            text: String::new(),
            usage: aitex_harness::Usage {
                input: 10,
                output: 7,
                cache_read: 20,
                cache_write: 3,
                reasoning: 2,
            },
            model: "mock".into(),
            metadata: aitex_harness::ResponseMetadata {
                charged_microusd: Some(123),
                ..Default::default()
            },
        };
        assert_eq!(
            model_usage_text(&response, std::time::Duration::from_millis(25)),
            "33 input (20 cached + 3 write) / 7 output (2 reasoning) · 25.0 ms · $0.000123"
        );
    }

    #[test]
    fn structural_hint_stays_with_the_file_that_was_analyzed() {
        let files = [
            aitex_harness::ProjectFile::new("main.tex", "root"),
            aitex_harness::ProjectFile::new("refs.bib", "broken entry"),
        ];
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Bibliography,
            "bad bibliography",
            aitex_core::Source::ToolLog(aitex_core::Tool::Biber),
        );
        diagnostic.reported = Some(aitex_core::Span::new("refs.bib", 7));
        assert_eq!(
            source_for_rule(&diagnostic, &files).map(|file| file.path.as_path()),
            Some(std::path::Path::new("refs.bib"))
        );
    }

    #[test]
    fn generated_index_record_maps_back_to_the_author_command() {
        let base = std::env::temp_dir().join(format!(
            "aitex-generated-origin-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&base).unwrap();
        let main = base.join("main.tex");
        std::fs::write(
            &main,
            "\\documentclass{article}\nfirst\\index{one}\nsecond\\index{}\n",
        )
        .unwrap();
        std::fs::write(base.join("main.idx"), "\\indexentry{one}{1}\n\\indexentry{}{1}\n")
            .unwrap();
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Index,
            "Illegal null field",
            aitex_core::Source::ToolLog(aitex_core::Tool::MakeIndex),
        )
        .at(aitex_core::Span::new("main.idx", 2));

        enrich_project_diagnostic(&main.to_string_lossy(), &mut diagnostic);

        assert_eq!(
            diagnostic.cause,
            Some(aitex_core::Span::new("main.tex", 3))
        );
        assert!(diagnostic.provenance.iter().any(|frame| {
            frame.kind == aitex_core::ProvenanceKind::GeneratedOrigin
                && frame.span.as_ref() == diagnostic.cause.as_ref()
        }));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn build_explain_and_repair_share_one_canonical_diagnostic() {
        let base = std::env::temp_dir().join(format!(
            "aitex-shared-diagnostic-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&base).unwrap();
        let main = base.join("main.tex");
        std::fs::write(
            &main,
            "\\documentclass{article}\n\\title{Broken $ title}\n\\maketitle\n",
        )
        .unwrap();
        let raw = || {
            aitex_core::Diagnostic::new(
                aitex_core::Severity::Error,
                aitex_core::Kind::MissingMathShift,
                "Extra }, or forgotten $.",
                aitex_core::Source::Engine,
            )
            .at(aitex_core::Span::new(&main, 3))
        };

        let mut build_report = aitex_core::Report {
            diagnostics: vec![raw()],
            engine: "aitex".into(),
            ..Default::default()
        };
        enrich_project_report(&main.to_string_lossy(), &mut build_report);
        let build = build_report
            .first_error()
            .unwrap()
            .presentation()
            .plain_text();

        let explain = first_diagnostic(
            BuildFailure {
                report: aitex_core::Report {
                    diagnostics: vec![raw()],
                    engine: "aitex".into(),
                    ..Default::default()
                },
                transcript_paths: Vec::new(),
            },
            &main.to_string_lossy(),
        )
        .presentation()
        .plain_text();

        let mut repair_report = aitex_core::Report {
            diagnostics: vec![raw()],
            engine: "aitex".into(),
            ..Default::default()
        };
        aitex_harness::enrich_report_diagnostics(
            &base,
            std::path::Path::new("main.tex"),
            &mut repair_report,
        );
        let repair = repair_report
            .first_error()
            .unwrap()
            .presentation()
            .plain_text();

        assert_eq!(build, explain);
        assert_eq!(build, repair);
        assert!(build.contains(&format!("{}:2", main.display())));
        assert!(build.contains(&format!("Reported  {}:3", main.display())));
        assert!(build.contains("Trace     failing-token · odd inline math delimiter count"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn model_context_never_reads_a_diagnostic_path_outside_the_project() {
        let base = std::env::temp_dir().join(format!(
            "aitex-context-boundary-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let main = project.join("main.tex");
        std::fs::write(&main, "broken").unwrap();
        std::fs::write(base.join("secret.tex"), "do not upload").unwrap();
        let diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Other,
            "bad include",
            aitex_core::Source::Engine,
        )
        .at(aitex_core::Span::new("../secret.tex", 1));
        let files = project_files_for(&diagnostic, &main.to_string_lossy());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, std::path::Path::new("main.tex"));
        assert_eq!(files[0].text, "broken");
        let _ = std::fs::remove_dir_all(base);
    }
}
