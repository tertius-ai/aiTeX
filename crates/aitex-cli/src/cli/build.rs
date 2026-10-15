// Native full-document convergence.
//
// aiTeX owns the fixed-point loop and keeps its resolver and immutable asset
// caches alive between TeX passes. Bibliography and index processors are still
// separate TeX Live algorithms; they run only when a content fingerprint says
// that their exact inputs or outputs changed.

#[derive(Default)]
struct BuildToolState {
    current: std::collections::BTreeMap<String, (String, String)>,
}

#[derive(Default)]
struct BuildObservation {
    dependencies: Vec<CacheDependency>,
    written_files: Vec<String>,
}

impl BuildObservation {
    fn record_outcome(&mut self, outcome: &RunOutcome) {
        self.dependencies.extend(outcome.dependencies.iter().cloned());
        self.written_files.extend(outcome.written_files.iter().cloned());
    }

    fn record_tool(&mut self, tool: &BuildTool) {
        self.dependencies.extend(tool.inputs.iter().map(|path| {
            let request = path.to_string_lossy();
            let resolved = path.is_file().then_some(request.as_ref());
            CacheDependency::from_resolution(&request, resolved)
        }));
        self.written_files.extend(
            tool.outputs
                .iter()
                .chain(&tool.logs)
                .map(|path| path.to_string_lossy().into_owned()),
        );
    }

    fn finish(mut self) -> Self {
        let mut latest = std::collections::BTreeMap::new();
        for dependency in self.dependencies.drain(..) {
            latest.insert(
                (dependency.request.clone(), dependency.resolved.clone()),
                dependency,
            );
        }
        self.dependencies = normalized_dependencies(&latest.into_values().collect::<Vec<_>>());
        self.written_files.sort();
        self.written_files.dedup();
        self
    }
}

struct BuildTool {
    key: String,
    label: &'static str,
    program: &'static str,
    program_env: &'static str,
    arguments: Vec<String>,
    semantic_identity: Vec<u8>,
    inputs: Vec<std::path::PathBuf>,
    outputs: Vec<std::path::PathBuf>,
    logs: Vec<std::path::PathBuf>,
    diagnostic_tool: aitex_core::Tool,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
struct BuildFailure {
    report: aitex_core::Report,
    transcript_paths: Vec<std::path::PathBuf>,
}

/// Resolve the project boundary once and feed every CLI path through the same
/// bounded source-level diagnostic enrichment used by the repair workspace.
/// Absolute inputs outside the current directory use their containing
/// directory as the project root; relative and in-tree inputs retain the
/// current working directory so included author files remain discoverable.
#[cfg(feature = "ai")]
fn diagnostic_project(input: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap_or_default())
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
    let path = std::path::Path::new(input);
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    });
    if let Ok(relative) = absolute.strip_prefix(&cwd) {
        return (cwd, relative.to_path_buf());
    }
    let root = absolute
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| cwd.clone());
    let relative = absolute
        .strip_prefix(&root)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf());
    (root, relative)
}

fn enrich_project_report(input: &str, report: &mut aitex_core::Report) {
    #[cfg(feature = "ai")]
    {
        let (root, input) = diagnostic_project(input);
        aitex_harness::enrich_report_diagnostics(&root, &input, report);
    }
    #[cfg(not(feature = "ai"))]
    let _ = (input, report);
}

#[cfg(feature = "ai")]
fn enrich_project_diagnostic(input: &str, diagnostic: &mut aitex_core::Diagnostic) {
    let (root, input) = diagnostic_project(input);
    aitex_harness::enrich_diagnostic_origin(&root, &input, diagnostic);
}

fn braced_value<'a>(line: &'a str, command: &str) -> Option<&'a str> {
    let rest = line.trim_start().strip_prefix(command)?.trim_start();
    let rest = rest.strip_prefix('{')?;
    Some(rest.split_once('}')?.0)
}

fn resolve_build_input(name: &str, extension: &str) -> std::path::PathBuf {
    let direct = std::path::PathBuf::from(name);
    if direct.is_file() {
        return direct;
    }
    if !extension.is_empty() {
        let extended = std::path::PathBuf::from(format!("{name}{extension}"));
        if extended.is_file() {
            return extended;
        }
    }
    resolve_path_hot(name, extension)
        .map(std::path::PathBuf::from)
        .unwrap_or(direct)
}

fn collect_bibtex_inputs(
    auxiliary: &std::path::Path,
    seen: &mut std::collections::BTreeSet<std::path::PathBuf>,
    identity: &mut Vec<u8>,
    files: &mut Vec<std::path::PathBuf>,
) -> bool {
    let auxiliary = auxiliary.to_path_buf();
    let seen_key = std::fs::canonicalize(&auxiliary).unwrap_or_else(|_| auxiliary.clone());
    if seen.len() >= 4096 {
        key_field(identity, "aux-limit", b"4096");
        return false;
    }
    if !seen.insert(seen_key) {
        return false;
    }
    let Ok(bytes) = std::fs::read(&auxiliary) else {
        key_field(identity, "missing-aux", auxiliary.to_string_lossy().as_bytes());
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut has_bibliography = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("\\citation{") {
            key_field(identity, "citation", trimmed.as_bytes());
        }
        if let Some(value) = braced_value(trimmed, "\\bibdata") {
            has_bibliography = true;
            key_field(identity, "bibdata", value.as_bytes());
            for database in value.split(',').map(str::trim).filter(|name| !name.is_empty()) {
                files.push(resolve_build_input(database, ".bib"));
            }
        }
        if let Some(value) = braced_value(trimmed, "\\bibstyle") {
            key_field(identity, "bibstyle", value.as_bytes());
            files.push(resolve_build_input(value.trim(), ".bst"));
        }
        if let Some(value) = braced_value(trimmed, "\\@input") {
            let included = auxiliary
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(value);
            key_field(identity, "aux-input", included.to_string_lossy().as_bytes());
            has_bibliography |= collect_bibtex_inputs(&included, seen, identity, files);
        }
    }
    has_bibliography
}

fn biber_data_sources(control: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(text) = std::fs::read_to_string(control) else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<bcf:datasource") {
        rest = &rest[start..];
        let Some(open_end) = rest.find('>') else {
            break;
        };
        rest = &rest[open_end + 1..];
        let Some(close) = rest.find("</bcf:datasource>") else {
            break;
        };
        let name = rest[..close].trim();
        if !name.is_empty() {
            sources.push(resolve_build_input(name, ".bib"));
        }
        rest = &rest[close + "</bcf:datasource>".len()..];
    }
    sources
}

fn preserve_prebuilt_bibliography(
    bbl: &std::path::Path,
    inputs: &[std::path::PathBuf],
) -> bool {
    let nonempty_bbl = bbl.metadata().is_ok_and(|metadata| metadata.len() > 0);
    let project = if bbl.is_absolute() {
        bbl.parent().unwrap_or_else(|| std::path::Path::new("/"))
            .to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(bbl.parent().unwrap_or_else(|| std::path::Path::new("")))
    };
    let project = std::fs::canonicalize(project).ok();
    let has_database = inputs.iter().any(|path| {
        path.extension().and_then(|value| value.to_str()) == Some("bib")
            && path.is_file()
            && project.as_ref().is_some_and(|project| {
                std::fs::canonicalize(path)
                    .is_ok_and(|path| path.starts_with(project))
            })
    });
    // Source archives, especially arXiv submissions, routinely publish the
    // generated BBL but cannot redistribute the original database. The BBL is
    // source input in that situation. Re-running BibTeX/Biber would only turn
    // a valid archival build into an artificial missing-file error.
    nonempty_bbl && !has_database
}

fn generated_with_extension(written: &[String], extension: &str) -> Vec<std::path::PathBuf> {
    let mut paths = written
        .iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some(extension))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn makeglossaries_protocol_declared(auxiliary: &str) -> bool {
    auxiliary
        .lines()
        .map(str::trim_start)
        .any(|line| line.starts_with("\\@istfilename{"))
}

fn build_tools(job_name: &str, written: &[String]) -> Vec<BuildTool> {
    let mut tools = Vec::new();
    let aux = std::path::PathBuf::from(format!("{job_name}.aux"));
    let aux_text = std::fs::read_to_string(&aux).unwrap_or_default();
    let bcf = std::path::PathBuf::from(format!("{job_name}.bcf"));
    let bbl = std::path::PathBuf::from(format!("{job_name}.bbl"));
    let mut bibtex_identity = Vec::new();
    let mut bibtex_inputs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let has_bibtex = collect_bibtex_inputs(
        &aux,
        &mut seen,
        &mut bibtex_identity,
        &mut bibtex_inputs,
    );
    let biber_sources = biber_data_sources(&bcf);
    if bcf.is_file()
        && aux_text.contains("\\abx@aux@")
        && !has_bibtex
        && !preserve_prebuilt_bibliography(&bbl, &biber_sources)
    {
        let mut inputs = vec![bcf.clone()];
        inputs.extend(biber_sources);
        tools.push(BuildTool {
            key: "biber".to_string(),
            label: "Biber",
            program: "biber",
            program_env: "AITEX_BIBER",
            arguments: vec![job_name.to_string()],
            semantic_identity: Vec::new(),
            inputs,
            outputs: vec![bbl.clone()],
            logs: vec![std::path::PathBuf::from(format!("{job_name}.blg"))],
            diagnostic_tool: aitex_core::Tool::Biber,
        });
    } else if has_bibtex && !preserve_prebuilt_bibliography(&bbl, &bibtex_inputs) {
        tools.push(BuildTool {
            key: "bibtex".to_string(),
            label: "BibTeX",
            program: "bibtex",
            program_env: "AITEX_BIBTEX",
            arguments: vec![job_name.to_string()],
            semantic_identity: bibtex_identity,
            inputs: bibtex_inputs,
            outputs: vec![bbl],
            logs: vec![std::path::PathBuf::from(format!("{job_name}.blg"))],
            diagnostic_tool: aitex_core::Tool::BibTex,
        });
    }

    for idx in generated_with_extension(written, "idx") {
        let ind = idx.with_extension("ind");
        let ilg = idx.with_extension("ilg");
        tools.push(BuildTool {
            key: format!("makeindex:{}", idx.to_string_lossy()),
            label: "MakeIndex",
            program: "makeindex",
            program_env: "AITEX_MAKEINDEX",
            arguments: vec![idx.to_string_lossy().into_owned()],
            semantic_identity: Vec::new(),
            inputs: vec![idx],
            outputs: vec![ind],
            logs: vec![ilg],
            diagnostic_tool: aitex_core::Tool::MakeIndex,
        });
    }

    let mut glossary_inputs = written
        .iter()
        .map(std::path::PathBuf::from)
        .filter(|path| {
            matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("glo" | "acn")
            )
        })
        .collect::<Vec<_>>();
    // A `.glo`/`.acn` stream alone is not a request to run makeglossaries.
    // The glossaries package can open an empty stream even when the document
    // never prints a glossary, and older packages use similarly named files
    // for unrelated protocols. makeglossaries itself requires the
    // `\@istfilename{...}` declaration written to the main AUX; without it the
    // tool exits 255. Schedule it only when both halves of its protocol exist.
    if !glossary_inputs.is_empty() && makeglossaries_protocol_declared(&aux_text) {
        // MakeGlossaries uses the AUX protocol declaration and the generated
        // MakeIndex/Xindy style in addition to the term streams themselves.
        glossary_inputs.push(aux.clone());
        for extension in ["ist", "xdy"] {
            let path = std::path::PathBuf::from(format!("{job_name}.{extension}"));
            if path.is_file() {
                glossary_inputs.push(path);
            }
        }
        glossary_inputs.sort();
        glossary_inputs.dedup();
        let outputs = glossary_inputs
            .iter()
            .filter_map(|path| match path.extension().and_then(|value| value.to_str()) {
                Some("glo") => Some(path.with_extension("gls")),
                Some("acn") => Some(path.with_extension("acr")),
                _ => None,
            })
            .collect();
        let logs = glossary_inputs
            .iter()
            .filter_map(|path| match path.extension().and_then(|value| value.to_str()) {
                Some("glo") => Some(path.with_extension("glg")),
                Some("acn") => Some(path.with_extension("alg")),
                _ => None,
            })
            .collect();
        tools.push(BuildTool {
            key: "makeglossaries".to_string(),
            label: "MakeGlossaries",
            program: "makeglossaries",
            program_env: "AITEX_MAKEGLOSSARIES",
            arguments: vec![job_name.to_string()],
            semantic_identity: Vec::new(),
            inputs: glossary_inputs,
            outputs,
            logs,
            diagnostic_tool: aitex_core::Tool::MakeGlossaries,
        });
    }

    for nlo in generated_with_extension(written, "nlo") {
        let nls = nlo.with_extension("nls");
        let nlg = nlo.with_extension("nlg");
        tools.push(BuildTool {
            key: format!("nomenclature:{}", nlo.to_string_lossy()),
            label: "MakeIndex (nomenclature)",
            program: "makeindex",
            program_env: "AITEX_MAKEINDEX",
            arguments: vec![
                "-s".to_string(),
                "nomencl.ist".to_string(),
                "-o".to_string(),
                nls.to_string_lossy().into_owned(),
                nlo.to_string_lossy().into_owned(),
            ],
            semantic_identity: Vec::new(),
            inputs: vec![nlo, resolve_build_input("nomencl.ist", "")],
            outputs: vec![nls],
            logs: vec![nlg],
            diagnostic_tool: aitex_core::Tool::Nomenclature,
        });
    }
    tools
}

fn paths_fingerprint(paths: &[std::path::PathBuf]) -> String {
    let mut identity = Vec::new();
    for path in paths {
        key_field(&mut identity, "path", path.to_string_lossy().as_bytes());
        match std::fs::read(path) {
            Ok(bytes) => key_field(&mut identity, "contents", &bytes),
            Err(_) => key_field(&mut identity, "missing", b""),
        }
    }
    sha256_hex(&identity)
}

fn tool_identity(tool: &BuildTool, program: &std::ffi::OsStr) -> String {
    let mut identity = Vec::new();
    key_field(&mut identity, "schema", b"1");
    key_field(
        &mut identity,
        "tool",
        program.to_string_lossy().as_bytes(),
    );
    for argument in &tool.arguments {
        key_field(&mut identity, "argument", argument.as_bytes());
    }
    key_field(&mut identity, "semantics", &tool.semantic_identity);
    key_field(
        &mut identity,
        "inputs",
        paths_fingerprint(&tool.inputs).as_bytes(),
    );
    sha256_hex(&identity)
}

fn build_state_path(root: &std::path::Path, job_name: &str, tool: &BuildTool) -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut identity = Vec::new();
    key_field(&mut identity, "cwd", cwd.to_string_lossy().as_bytes());
    key_field(&mut identity, "job", job_name.as_bytes());
    key_field(&mut identity, "tool", tool.key.as_bytes());
    root.join("build-tools")
        .join(format!("{}.state", sha256_hex(&identity)))
}

fn persisted_tool_state(
    root: Option<&std::path::Path>,
    job_name: &str,
    tool: &BuildTool,
) -> Option<(String, String)> {
    let text = std::fs::read_to_string(build_state_path(root?, job_name, tool)).ok()?;
    let mut lines = text.lines();
    (lines.next()? == "aitex-build-tool-v1").then_some((
        lines.next()?.strip_prefix("input=")?.to_string(),
        lines.next()?.strip_prefix("output=")?.to_string(),
    ))
}

fn write_tool_state(
    root: Option<&std::path::Path>,
    job_name: &str,
    tool: &BuildTool,
    input: &str,
    output: &str,
) {
    let Some(root) = root else { return };
    let path = build_state_path(root, job_name, tool);
    let Some(parent) = path.parent() else { return };
    if let Err(error) = std::fs::create_dir_all(parent).and_then(|()| {
        atomic_write(
            &path,
            format!("aitex-build-tool-v1\ninput={input}\noutput={output}\n").as_bytes(),
        )
    }) {
        cli_detail!("aitex build: could not persist {} state: {error}", tool.label);
    }
}

fn report_build_problem(label: &str, detail: impl std::fmt::Display) {
    if cli_present_output() {
        let style = cli_style();
        eprintln!(
            "  {} {} {detail}",
            style.red("×"),
            style.bold(&format!("{label:<9}")),
        );
    } else {
        eprintln!("aitex build: {label} {detail}");
    }
}

fn report_build_error(tool: &BuildTool, detail: impl std::fmt::Display) {
    report_build_problem(tool.label, detail);
}

fn kind_for_tool(tool: aitex_core::Tool) -> aitex_core::Kind {
    match tool {
        aitex_core::Tool::BibTex | aitex_core::Tool::Biber => aitex_core::Kind::Bibliography,
        aitex_core::Tool::MakeIndex => aitex_core::Kind::Index,
        aitex_core::Tool::MakeGlossaries | aitex_core::Tool::Nomenclature => {
            aitex_core::Kind::Glossary
        }
    }
}

fn generic_tool_failure(tool: &BuildTool, message: String) -> BuildFailure {
    let mut diagnostic = aitex_core::Diagnostic::new(
        aitex_core::Severity::Error,
        kind_for_tool(tool.diagnostic_tool),
        message,
        aitex_core::Source::ToolLog(tool.diagnostic_tool),
    );
    diagnostic.provenance.push(aitex_core::ProvenanceFrame::new(
        aitex_core::ProvenanceKind::SemanticTool,
        tool.diagnostic_tool.as_str(),
    ));
    BuildFailure {
        report: aitex_core::Report {
            engine: tool.diagnostic_tool.as_str().to_string(),
            diagnostics: vec![diagnostic],
            ..Default::default()
        },
        transcript_paths: tool.logs.clone(),
    }
}

fn tool_transcript(tool: &BuildTool, stdout: &[u8], stderr: &[u8]) -> String {
    fn append(transcript: &mut String, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if !transcript.is_empty() && !transcript.ends_with('\n') {
            transcript.push('\n');
        }
        transcript.push_str(&String::from_utf8_lossy(bytes));
    }

    let mut transcript = String::new();
    append(&mut transcript, stdout);
    append(&mut transcript, stderr);
    for path in &tool.logs {
        if let Ok(bytes) = std::fs::read(path) {
            append(&mut transcript, &bytes);
        }
    }
    transcript
}

fn show_tool_diagnostic(tool: &BuildTool, diagnostic: &aitex_core::Diagnostic) {
    let presentation = diagnostic.presentation();
    let location = diagnostic
        .location()
        .map(|span| format!(" · {span}"))
        .unwrap_or_default();
    report_build_error(tool, format_args!("{}{location}", presentation.error));
    show_diagnostic_details(diagnostic);
    if cli_compact_output()
        && let Some(path) = tool.logs.iter().find(|path| path.is_file())
    {
        eprintln!(
            "    {}",
            cli_style().dim(&format!(
                "full {} transcript in {}",
                tool.diagnostic_tool,
                path.display()
            ))
        );
    }
}

fn show_diagnostic_details(diagnostic: &aitex_core::Diagnostic) {
    if cli_compact_output() {
        let presentation = diagnostic.presentation();
        if let Some(reported) = &presentation.reported {
            eprintln!(
                "    {}",
                cli_style().dim(&format!("↳ reported · {reported}"))
            );
        }
        let mut frames = presentation.provenance.iter().take(5).collect::<Vec<_>>();
        if presentation.provenance.len() > 5
            && let Some(causal) = presentation.provenance.last()
        {
            frames.push(causal);
        }
        for frame in frames {
            eprintln!("    {}", cli_style().dim(&format!("↳ {frame}")));
        }
    }
}

fn replay_tool_output(stdout: &[u8], stderr: &[u8]) {
    if cli_compact_output() {
        return;
    }
    use std::io::Write as _;
    let _ = std::io::stdout().write_all(stdout);
    let _ = std::io::stderr().write_all(stderr);
}

fn show_tool_tail(transcript: &str) {
    if !cli_compact_output() {
        return;
    }
    for line in transcript
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        eprintln!("    {line}");
    }
    eprintln!("    rerun with --verbose for the complete tool output");
}

fn configure_build_tool_command(command: &mut std::process::Command, tool: &BuildTool) {
    // The qualification sandbox restricts TeX source reads with openin_any=p.
    // MakeIndex inherits that kpathsea knob, resolves the trusted packaged
    // nomencl.ist to an absolute TEXMF path, and then rejects its own result.
    // The restriction belongs to the TeX pass, not this fixed semantic-tool
    // invocation; leave MakeIndex's site configuration in force instead.
    if tool.diagnostic_tool == aitex_core::Tool::Nomenclature {
        command.env_remove("openin_any");
    }
}

fn run_build_tool(
    tool: &BuildTool,
    job_name: &str,
    input_path: &str,
    root: Option<&std::path::Path>,
    state: &mut BuildToolState,
    pass: usize,
) -> Result<bool, BuildFailure> {
    let program = std::env::var_os(tool.program_env)
        .unwrap_or_else(|| std::ffi::OsString::from(tool.program));
    let input = tool_identity(tool, &program);
    let output = paths_fingerprint(&tool.outputs);
    let prior = state
        .current
        .get(&tool.key)
        .cloned()
        .or_else(|| persisted_tool_state(root, job_name, tool));
    if prior.as_ref() == Some(&(input.clone(), output))
        && tool.outputs.iter().all(|path| path.is_file())
    {
        return Ok(false);
    }

    cli_detail!("aitex build: running {}", tool.label);
    let started = std::time::Instant::now();
    let mut command = std::process::Command::new(&program);
    command.args(&tool.arguments);
    configure_build_tool_command(&mut command, tool);
    // Always capture, even in verbose/script mode: some semantic tools emit a
    // parse failure only on stdout while still returning success. Replay the
    // bytes below when the caller expects the traditional child transcript.
    let result = command
        .output()
        .map(|output| (output.status, Some((output.stdout, output.stderr))));
    let (status, captured) = match result {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let message = format!("{} is required but was not found in PATH", tool.program);
            report_build_error(tool, &message);
            return Err(generic_tool_failure(tool, message));
        }
        Err(error) => {
            let message = format!("cannot run {}: {error}", tool.program);
            report_build_error(tool, &message);
            return Err(generic_tool_failure(tool, message));
        }
    };
    let (stdout, stderr) = captured.unwrap_or_default();
    replay_tool_output(&stdout, &stderr);
    let transcript = tool_transcript(tool, &stdout, &stderr);
    let mut report = aitex_core::parse_tool_log(&transcript, tool.diagnostic_tool);
    report.cluster();
    for diagnostic in &mut report.diagnostics {
        diagnostic.provenance.push(aitex_core::ProvenanceFrame::new(
            aitex_core::ProvenanceKind::CompilationPass,
            format!("after TeX pass {pass}"),
        ));
    }
    enrich_project_report(input_path, &mut report);
    if let Some(diagnostic) = report.first_error() {
        show_tool_diagnostic(tool, diagnostic);
        return Err(BuildFailure {
            report,
            transcript_paths: tool.logs.clone(),
        });
    }
    if !status.success() {
        let message = format!("{} failed with {status}", tool.label);
        report_build_error(tool, &message);
        show_tool_tail(&transcript);
        return Err(generic_tool_failure(tool, message));
    }
    if let Some(missing) = tool.outputs.iter().find(|path| !path.is_file()) {
        let message = format!("did not produce {}", missing.display());
        report_build_error(tool, &message);
        return Err(generic_tool_failure(tool, message));
    }
    let output = paths_fingerprint(&tool.outputs);
    state
        .current
        .insert(tool.key.clone(), (input.clone(), output.clone()));
    write_tool_state(root, job_name, tool, &input, &output);
    cli_phase(tool.label, job_name, started.elapsed());
    Ok(true)
}

fn tex_build_failure(
    job_name: &str,
    input: &str,
    outcome: RunOutcome,
    pass: usize,
) -> BuildFailure {
    let mut diagnostics = outcome.diagnostics;
    if diagnostics.is_empty() {
        diagnostics.push(
            aitex_core::Diagnostic::new(
                aitex_core::Severity::Error,
                aitex_core::Kind::Other,
                "TeX pass failed; inspect the transcript for the complete diagnostic",
                aitex_core::Source::Engine,
            )
            .at(aitex_core::Span::new(input, 1)),
        );
    }
    for diagnostic in &mut diagnostics {
        diagnostic.provenance.push(aitex_core::ProvenanceFrame::new(
            aitex_core::ProvenanceKind::CompilationPass,
            format!("TeX pass {pass}"),
        ));
    }
    let mut report = aitex_core::Report {
        diagnostics,
        engine: "aitex".into(),
        ..Default::default()
    };
    enrich_project_report(input, &mut report);
    BuildFailure {
        report,
        transcript_paths: vec![std::path::PathBuf::from(format!("{job_name}.log"))],
    }
}

#[cfg(not(feature = "ai"))]
fn fallback_tex_log_report(log: &str) -> aitex_core::Report {
    let mut diagnostics = Vec::new();
    for message in log.lines().filter_map(|line| line.strip_prefix("! ")) {
        let kind = if let Some(rest) = message.strip_prefix("LaTeX Error: File `") {
            let name = rest.split('\'').next().unwrap_or("").to_string();
            if name.ends_with(".sty") || name.ends_with(".cls") {
                aitex_core::Kind::MissingPackage { name }
            } else {
                aitex_core::Kind::MissingFile { name }
            }
        } else {
            aitex_core::Kind::Other
        };
        diagnostics.push(aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            kind,
            message,
            aitex_core::Source::TexLog,
        ));
    }
    aitex_core::Report {
        diagnostics,
        engine: "aitex-log".into(),
        ..Default::default()
    }
}

/// LaTeX can report a macro-level error without reaching TeX's primitive
/// error machinery. Missing `\input` files are the important example: the
/// kernel prints a conventional `! LaTeX Error` block and asks for another
/// filename. In non-interactive mode the job can then finish a partial PDF.
/// Treating that pass as successful both hid the failure and admitted a bad
/// completed-output cache entry.
fn tex_log_error_report_at(
    path: &std::path::Path,
    _input: &str,
) -> Option<aitex_core::Report> {
    let bytes = std::fs::read(&path).ok()?;
    let log = String::from_utf8_lossy(&bytes);
    #[cfg(feature = "ai")]
    if !aitex_harness::tex_log_has_error_context(&log) {
        return None;
    }
    #[cfg(not(feature = "ai"))]
    if !log.lines().any(|line| line.starts_with("! ")) {
        return None;
    }
    #[cfg(feature = "ai")]
    let mut report = aitex_harness::parse_tex_log(&log, std::path::Path::new(_input));
    #[cfg(not(feature = "ai"))]
    let mut report = fallback_tex_log_report(&log);
    enrich_project_report(_input, &mut report);
    report.cluster();
    report
        .diagnostics
        .retain(|diagnostic| diagnostic.severity >= aitex_core::Severity::Error);
    if report.diagnostics.is_empty() {
        return None;
    }
    report.engine = "aitex-log".into();
    Some(report)
}

fn tex_log_failure_at(
    path: std::path::PathBuf,
    input: &str,
    pass: usize,
) -> Option<BuildFailure> {
    let mut report = tex_log_error_report_at(&path, input)?;
    for diagnostic in &mut report.diagnostics {
        diagnostic.provenance.push(aitex_core::ProvenanceFrame::new(
            aitex_core::ProvenanceKind::CompilationPass,
            format!("TeX pass {pass}"),
        ));
    }
    let diagnostic = report.first_error().expect("report was checked non-empty");
    let presentation = diagnostic.presentation();
    let location = diagnostic
        .location()
        .map(|span| format!(" · {span}"))
        .unwrap_or_default();
    report_build_problem("TeX", format_args!("{}{location}", presentation.error));
    show_diagnostic_details(diagnostic);
    Some(BuildFailure {
        report,
        transcript_paths: vec![path],
    })
}

fn tex_log_failure(job_name: &str, input: &str, pass: usize) -> Option<BuildFailure> {
    tex_log_failure_at(
        std::path::PathBuf::from(format!("{job_name}.log")),
        input,
        pass,
    )
}

fn run_native_build() -> Option<BuildFailure> {
    run_native_build_inner(false).0
}

fn run_native_build_observed() -> (Option<BuildFailure>, BuildObservation) {
    run_native_build_inner(true)
}

fn run_native_build_inner(observe: bool) -> (Option<BuildFailure>, BuildObservation) {
    const MAX_BUILD_PASSES: usize = 8;
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let front_door = classify_front_door(&arguments);
    let input = input_argument(&arguments).expect("build arguments were validated");
    let output = output_argument(&arguments);
    let started = std::time::Instant::now();
    let job_name = std::path::Path::new(input)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a validated input has a file stem")
        .to_string();
    let cache_root = (!arguments.iter().any(|argument| argument == "--no-cache"))
        .then(cache_root_argument)
        .flatten()
        .map(std::path::PathBuf::from);
    let mut tools = BuildToolState::default();
    let mut observation = BuildObservation::default();
    if observe {
        observation.written_files.push(format!("{job_name}.log"));
    }

    for pass in 1..=MAX_BUILD_PASSES {
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        cli_detail!("aitex build: TeX pass {pass}/{MAX_BUILD_PASSES}");
        let outcome = run();
        if observe {
            observation.record_outcome(&outcome);
        }
        if EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            cli_fail_complete(&front_door, started.elapsed());
            return (
                Some(tex_build_failure(&job_name, input, outcome, pass)),
                observation.finish(),
            );
        }
        if let Some(failure) = tex_log_failure(&job_name, input, pass) {
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            cli_fail_complete(&front_door, started.elapsed());
            return (Some(failure), observation.finish());
        }

        let mut external_work = false;
        for tool in build_tools(&job_name, &outcome.written_files) {
            if observe {
                observation.record_tool(&tool);
            }
            match run_build_tool(
                &tool,
                &job_name,
                input,
                cache_root.as_deref(),
                &mut tools,
                pass,
            ) {
                Ok(ran) => external_work |= ran,
                Err(mut failure) => {
                    for diagnostic in &mut failure.report.diagnostics {
                        if !diagnostic.provenance.iter().any(|frame| {
                            frame.kind == aitex_core::ProvenanceKind::CompilationPass
                        }) {
                            diagnostic.provenance.push(aitex_core::ProvenanceFrame::new(
                                aitex_core::ProvenanceKind::CompilationPass,
                                format!("after TeX pass {pass}"),
                            ));
                        }
                    }
                    EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                    cli_fail_complete(&front_door, started.elapsed());
                    return (Some(failure), observation.finish());
                }
            }
        }
        if !outcome.rerun_needed && !external_work {
            cli_detail!("aitex build: converged after {pass} TeX pass(es)");
            if !matches!(front_door, FrontDoor::Lint) {
                cli_finish(output.as_deref(), started.elapsed(), false);
            }
            return (None, observation.finish());
        }
    }
    report_build_problem(
        "Build",
        format_args!("inputs did not stabilize after {MAX_BUILD_PASSES} TeX passes"),
    );
    EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
    cli_fail_complete(&front_door, started.elapsed());
    (
        Some(BuildFailure {
            report: aitex_core::Report {
                diagnostics: vec![aitex_core::Diagnostic::new(
                    aitex_core::Severity::Error,
                    aitex_core::Kind::Other,
                    format!("inputs did not stabilize after {MAX_BUILD_PASSES} TeX passes"),
                    aitex_core::Source::Engine,
                )],
                engine: "aitex-build".into(),
                ..Default::default()
            },
            transcript_paths: vec![std::path::PathBuf::from(format!("{job_name}.log"))],
        }),
        observation.finish(),
    )
}

#[cfg(test)]
mod build_tests {
    use super::{
        BuildTool, BuildToolState, braced_value, collect_bibtex_inputs,
        configure_build_tool_command,
        generated_with_extension, makeglossaries_protocol_declared, paths_fingerprint,
        preserve_prebuilt_bibliography, run_build_tool, tex_log_failure_at,
    };

    #[test]
    fn generated_auxiliary_commands_are_parsed_strictly() {
        assert_eq!(braced_value("  \\bibdata{one,two}", "\\bibdata"), Some("one,two"));
        assert_eq!(braced_value("\\bibstyle { amsplain }", "\\bibstyle"), Some(" amsplain "));
        assert_eq!(braced_value("% \\bibdata{ignored}", "\\bibdata"), None);
    }

    #[test]
    fn makeglossaries_requires_its_auxiliary_protocol_declaration() {
        assert!(makeglossaries_protocol_declared(
            "\\relax\n  \\@istfilename{main.ist}\n"
        ));
        assert!(!makeglossaries_protocol_declared(
            "\\relax\n% \\@istfilename{ignored.ist}\n"
        ));
        assert!(!makeglossaries_protocol_declared(
            "\\relax\n\\newlabel{glossary}{{1}{2}}\n"
        ));
    }

    #[test]
    fn a_missing_build_input_has_a_stable_fingerprint() {
        let missing = std::env::temp_dir().join(format!(
            "aitex-missing-build-input-{}",
            std::process::id()
        ));
        assert_eq!(paths_fingerprint(&[missing.clone()]), paths_fingerprint(&[missing]));
    }

    #[test]
    fn nomenclature_style_lookup_does_not_inherit_tex_source_confinement() {
        let tool = BuildTool {
            key: "nomenclature:main.nlo".into(),
            label: "MakeIndex (nomenclature)",
            program: "makeindex",
            program_env: "AITEX_MAKEINDEX",
            arguments: Vec::new(),
            semantic_identity: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            logs: Vec::new(),
            diagnostic_tool: aitex_core::Tool::Nomenclature,
        };
        let mut command = std::process::Command::new("makeindex");
        command.env("openin_any", "p");
        configure_build_tool_command(&mut command, &tool);
        assert!(command.get_envs().any(|(key, value)| {
            key == std::ffi::OsStr::new("openin_any") && value.is_none()
        }));
    }

    #[cfg(unix)]
    #[test]
    fn a_semantic_error_beats_a_successful_tool_exit_status() {
        let tool = BuildTool {
            key: "exit-zero-error".into(),
            label: "MakeIndex",
            program: "/bin/sh",
            program_env: "AITEX_TEST_MAKEINDEX_DO_NOT_SET",
            arguments: vec![
                "-c".into(),
                "printf '%s\\n' '!! Input index error (file = main.idx, line = 2):' '   -- Illegal null field.'".into(),
            ],
            semantic_identity: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            logs: Vec::new(),
            diagnostic_tool: aitex_core::Tool::MakeIndex,
        };
        let failure = run_build_tool(
            &tool,
            "main",
            "main.tex",
            None,
            &mut BuildToolState::default(),
            1,
        )
            .expect_err("MakeIndex rejection must fail even when /bin/sh exits zero");
        let diagnostic = failure.report.first_error().expect("structured error");
        assert_eq!(diagnostic.kind, aitex_core::Kind::Index);
        assert_eq!(
            diagnostic.reported.as_ref(),
            Some(&aitex_core::Span::new("main.idx", 2))
        );
    }

    #[test]
    fn a_macro_reported_tex_error_beats_a_successful_engine_status() {
        let directory = std::env::temp_dir().join(format!(
            "aitex-macro-log-error-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let log = directory.join("main.log");
        std::fs::write(
            &log,
            "! LaTeX Error: File `chapters/missing.tex' not found.\n",
        )
        .unwrap();
        let failure = tex_log_failure_at(log, "main.tex", 2)
            .expect("macro-reported LaTeX error must fail the pass");
        let diagnostic = failure.report.first_error().expect("structured error");
        assert_eq!(
            diagnostic.kind,
            aitex_core::Kind::MissingFile {
                name: "chapters/missing.tex".into()
            }
        );
        assert!(diagnostic.provenance.iter().any(|frame| {
            frame.kind == aitex_core::ProvenanceKind::CompilationPass
                && frame.label == "TeX pass 2"
        }));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn bibtex_identity_ignores_unrelated_auxiliary_changes() {
        use std::collections::BTreeSet;

        let directory = std::env::temp_dir().join(format!(
            "aitex-bibtex-identity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let auxiliary = directory.join("main.aux");
        let bibliography = directory.join("refs.bib");
        std::fs::write(&bibliography, b"@book{one,title={One}}\n").unwrap();
        let aux_text = |citation: &str, label: &str| {
            format!(
                "\\relax\n\\citation{{{citation}}}\n\\bibdata{{{}}}\n\\bibstyle{{plain}}\n\\newlabel{{section}}{{{{{label}}}{{1}}}}\n",
                bibliography.with_extension("").display()
            )
        };
        std::fs::write(&auxiliary, aux_text("one", "1")).unwrap();

        let collect = || {
            let mut identity = Vec::new();
            let mut inputs = Vec::new();
            assert!(collect_bibtex_inputs(
                &auxiliary,
                &mut BTreeSet::new(),
                &mut identity,
                &mut inputs,
            ));
            (identity, inputs)
        };
        let first = collect();
        std::fs::write(&auxiliary, aux_text("one", "changed label")).unwrap();
        let label_change = collect();
        assert_eq!(first, label_change);
        std::fs::write(&auxiliary, aux_text("two", "changed label")).unwrap();
        assert_ne!(first.0, collect().0);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_prebuilt_bbl_is_source_when_its_database_is_unavailable() {
        let directory = std::env::temp_dir().join(format!(
            "aitex-prebuilt-bbl-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let bbl = directory.join("paper.bbl");
        let bib = directory.join("paper.bib");
        std::fs::write(&bbl, b"\\begin{thebibliography}{1}\\end{thebibliography}\n").unwrap();
        assert!(preserve_prebuilt_bibliography(
            &bbl,
            std::slice::from_ref(&bib)
        ));
        std::fs::write(&bib, b"@misc{example,title={Example}}\n").unwrap();
        assert!(!preserve_prebuilt_bibliography(&bbl, &[bib]));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn generated_index_streams_are_sorted_and_deduplicated() {
        let written = vec![
            "terms.idx".to_string(),
            "main.aux".to_string(),
            "names.idx".to_string(),
            "terms.idx".to_string(),
        ];
        assert_eq!(
            generated_with_extension(&written, "idx"),
            ["names.idx", "terms.idx"]
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        );
    }
}
