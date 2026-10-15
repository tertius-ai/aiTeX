// Interactive, validation-gated repair sessions.
//
// Every proposal is tested in a private project fork. A proposal may either
// complete the build or remove the current causal failure and expose a distinct
// next one. Interactive sessions accumulate accepted repairs in that private
// tree and update the real project only after the complete build succeeds.

use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
struct FixEvidence {
    context_profile: Option<aitex_harness::ContextProfile>,
    context_envelope: Option<serde_json::Value>,
    estimated_input_tokens: Option<usize>,
    included_source_lines: Option<usize>,
    provider: Option<String>,
    requested_model: Option<String>,
    returned_model: Option<String>,
    provider_counted_input_tokens: Option<u64>,
    usage: aitex_harness::Usage,
    metadata: aitex_harness::ResponseMetadata,
    model_ms: Option<f64>,
    raw_response: Option<String>,
    response_normalization: Option<String>,
    raw_patch: String,
    normalized_patch: Option<String>,
    patch_normalization: Option<String>,
}

#[derive(Debug)]
struct FixPlan {
    relative: std::path::PathBuf,
    before: Vec<u8>,
    after: Vec<u8>,
    patch: String,
    route: String,
    rationale: String,
    confidence: String,
    evidence: FixEvidence,
}

struct FixUi {
    json: bool,
    interactive: bool,
    sequence: u64,
    session: String,
}

impl FixUi {
    fn emit(&mut self, event: &str, data: serde_json::Value) {
        self.sequence = self.sequence.saturating_add(1);
        if self.json {
            let event = aitex_harness::RepairEvent::new(
                self.sequence,
                &self.session,
                event,
                data,
            );
            if let Ok(line) = serde_json::to_string(&event) {
                println!("{line}");
                let _ = std::io::stdout().flush();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteChoice {
    Local,
    Agent,
    Ai(aitex_harness::ContextProfile),
    Explain,
    ConfigureAi,
    Stop,
}

fn fix_line(mark: &str, label: &str, detail: impl std::fmt::Display) {
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
        eprintln!("aitex fix: {label}: {detail}");
    }
}

fn fix_note(label: &str, text: impl std::fmt::Display) {
    if cli_present_output() {
        let style = cli_style();
        eprintln!("    {} {text}", style.cyan(label));
    } else {
        eprintln!("aitex fix: {label}: {text}");
    }
}

fn fix_unchanged(reason: impl std::fmt::Display) {
    fix_line("◆", "Source", format!("unchanged · {reason}"));
    if cli_present_output() {
        eprintln!();
    }
}

fn fix_build_phases(ui: &FixUi, build: &aitex_harness::WorkspaceBuild) {
    if ui.json || !cli_present_output() {
        return;
    }
    for phase in &build.phases {
        let Some(body) = phase.strip_prefix("  ✓ ") else {
            continue;
        };
        let Some((label, detail)) = body.split_at_checked(9) else {
            continue;
        };
        let style = cli_style();
        let mark = style.green("✓");
        let label = style.bold(label);
        let detail = detail.strip_prefix(' ').unwrap_or(detail);
        let rendered = detail
            .rsplit_once(' ')
            .and_then(|(prefix, unit)| {
                prefix
                    .rsplit_once(' ')
                    .map(|(text, value)| (text, format!("{value} {unit}")))
            })
            .map_or_else(
                || detail.to_string(),
                |(text, duration)| format!("{text} {}", style.dim(&duration)),
            );
        eprintln!("  {mark} {label} {rendered}");
    }
}

fn repair_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn repair_session_id() -> String {
    format!("r-{:x}-{}", repair_now_ms(), std::process::id())
}

fn fix_value<'a>(arguments: &'a [String], option: &str) -> Option<&'a str> {
    explain_option(arguments, option)
}

fn fix_u64(arguments: &[String], option: &str, default: u64) -> u64 {
    fix_value(arguments, option)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn repair_limits(arguments: &[String]) -> aitex_harness::RepairLimits {
    let defaults = aitex_harness::RepairLimits::default();
    aitex_harness::RepairLimits {
        max_attempts: fix_u64(arguments, "--max-attempts", u64::from(defaults.max_attempts))
            .try_into()
            .unwrap_or(u32::MAX),
        max_agent_turns: fix_u64(
            arguments,
            "--max-agent-turns",
            u64::from(defaults.max_agent_turns),
        )
        .try_into()
        .unwrap_or(u32::MAX),
        max_agent_builds: fix_u64(
            arguments,
            "--max-agent-builds",
            u64::from(defaults.max_agent_builds),
        )
        .try_into()
        .unwrap_or(u32::MAX),
        max_input_tokens: fix_u64(
            arguments,
            "--max-input-tokens",
            defaults.max_input_tokens,
        ),
        max_output_tokens: fix_u64(
            arguments,
            "--max-output-tokens",
            defaults.max_output_tokens,
        ),
        max_elapsed_seconds: fix_u64(
            arguments,
            "--max-elapsed",
            defaults.max_elapsed_seconds,
        ),
        max_changed_lines: fix_u64(
            arguments,
            "--max-changed-lines",
            defaults.max_changed_lines as u64,
        )
        .try_into()
        .unwrap_or(usize::MAX),
        max_deleted_lines: fix_u64(
            arguments,
            "--max-deleted-lines",
            defaults.max_deleted_lines as u64,
        )
        .try_into()
        .unwrap_or(usize::MAX),
        max_source_bytes_per_request: fix_u64(
            arguments,
            "--max-source-bytes",
            defaults.max_source_bytes_per_request as u64,
        )
        .try_into()
        .unwrap_or(usize::MAX),
        max_cost_microusd: fix_value(arguments, "--max-cost-usd").and_then(|value| {
            value
                .parse::<f64>()
                .ok()
                .map(|dollars| (dollars * 1_000_000.0).round() as u64)
        }),
    }
}

fn project_layout(input: &str) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let input = std::fs::canonicalize(input)
        .map_err(|error| format!("cannot open input {input}: {error}"))?;
    if !input.is_file() || !aitex_harness::author_source(&input) {
        return Err("the input must be an author-owned TeX source file".into());
    }
    let root = input
        .parent()
        .ok_or_else(|| "the input has no project directory".to_string())?
        .to_path_buf();
    Ok((root, input))
}

fn resolve_source_target(
    root: &std::path::Path,
    candidate: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let target = std::fs::canonicalize(&joined)
        .map_err(|error| format!("cannot resolve patch target {}: {error}", candidate.display()))?;
    if !target.starts_with(root) {
        return Err("patch target escapes the input's project directory".into());
    }
    if !target.is_file() || !aitex_harness::author_source(&target) {
        return Err(format!(
            "{} is not an editable author-source file",
            target.display()
        ));
    }
    Ok(target)
}

fn deterministic_plan(
    root: &std::path::Path,
    input: &std::path::Path,
    diagnostic: &aitex_core::Diagnostic,
    rule: &str,
    fix: &aitex_harness::Fix,
) -> Result<Option<FixPlan>, String> {
    use aitex_harness::Action;
    let target = match &fix.action {
        Action::AddPackage { .. } => input.to_path_buf(),
        Action::ReplaceControlSequence { .. }
        | Action::AppendToLine { .. }
        | Action::FixEnvironmentDelimiter { .. } => {
            let span = diagnostic.location().ok_or_else(|| {
                "the proposed source repair has no author-source location".to_string()
            })?;
            resolve_source_target(root, &span.file)?
        }
        Action::InsertAt { .. } | Action::Diagnose { .. } => return Ok(None),
        _ => return Ok(None),
    };
    let before = std::fs::read(&target)
        .map_err(|error| format!("cannot read {}: {error}", target.display()))?;
    let Some(after) = aitex_harness::apply_action(&fix.action, diagnostic, &before)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if before == after {
        return Err("the proposed edit makes no source change".into());
    }
    let relative = target
        .strip_prefix(root)
        .map_err(|_| "patch target escapes the project".to_string())?
        .to_path_buf();
    let patch = aitex_harness::unified_diff(&relative, &before, &after);
    Ok(Some(FixPlan {
        relative,
        before,
        after,
        patch: patch.clone(),
        route: format!("local:{rule}"),
        rationale: fix.rationale.clone(),
        confidence: confidence_name(fix.confidence).into(),
        evidence: FixEvidence {
            raw_patch: patch,
            ..FixEvidence::default()
        },
    }))
}

fn model_plan(
    root: &std::path::Path,
    answer: &aitex_harness::ModelAnswer,
    route: &str,
    mut evidence: FixEvidence,
) -> Result<FixPlan, (String, FixEvidence)> {
    let result = (|| {
        let relative = answer
            .proposal
            .target()
            .map_err(|error| error.to_string())?;
        let target = resolve_source_target(root, &relative)?;
        let canonical_relative = target
            .strip_prefix(root)
            .map_err(|_| "model patch target escapes the project".to_string())?
            .to_path_buf();
        let before = std::fs::read(&target)
            .map_err(|error| format!("cannot read {}: {error}", target.display()))?;
        let prepared = answer
            .proposal
            .prepare(&before)
            .map_err(|error| error.to_string())?;
        evidence.raw_patch = match &answer.proposal {
            aitex_harness::ModelProposal::LegacyPatch(patch) => patch.clone(),
            aitex_harness::ModelProposal::Edits(_) => prepared.patch.clone(),
        };
        evidence.patch_normalization = prepared.normalization();
        if evidence.patch_normalization.is_some() {
            evidence.normalized_patch = Some(prepared.patch.clone());
        }
        Ok(FixPlan {
            relative: canonical_relative,
            before,
            after: prepared.after,
            patch: prepared.patch,
            route: route.into(),
            rationale: answer.cause.clone(),
            confidence: answer.confidence.clone(),
            evidence: evidence.clone(),
        })
    })();
    result.map_err(|error| (error, evidence))
}

fn validation_settings(
    arguments: &[String],
) -> Result<(
    std::path::PathBuf,
    Option<std::path::PathBuf>,
    aitex_harness::ValidationArtifact,
    bool,
), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the aiTeX executable: {error}"))?;
    let format = format_argument().map(|path| {
        std::fs::canonicalize(&path).unwrap_or_else(|_| std::path::PathBuf::from(path))
    });
    let artifact = if output_argument(arguments)
        .as_deref()
        .is_some_and(|path| path.ends_with(".dvi"))
    {
        aitex_harness::ValidationArtifact::Dvi
    } else {
        aitex_harness::ValidationArtifact::Pdf
    };
    Ok((
        executable,
        format,
        artifact,
        arguments.iter().any(|argument| argument == "--tex82"),
    ))
}

/// Rebuild the real tree from the document directory after applying a
/// validated transaction. `aitex fix path/to/main.tex` may be launched from a
/// parent directory, while TeX resolves relative `\input` paths and writes
/// nested auxiliary files from the process working directory. Calling the
/// in-process build with the original argv therefore validated one directory
/// and rebuilt another. A fresh child also gives the accepted source the same
/// clean process boundary as isolated validation.
#[allow(clippy::too_many_arguments)]
fn rebuild_applied_project(
    arguments: &[String],
    root: &std::path::Path,
    input_relative: &std::path::Path,
    executable: &std::path::Path,
    format: Option<&std::path::Path>,
    tex82: bool,
) -> Result<(), String> {
    let caller = std::env::current_dir()
        .map_err(|error| format!("cannot resolve the caller directory: {error}"))?;
    let output = output_argument(arguments)
        .ok_or_else(|| "cannot determine the rebuilt output path".to_string())?;
    let output = std::path::PathBuf::from(output);
    let output = if output.is_absolute() {
        output
    } else {
        caller.join(output)
    };
    let mut command = std::process::Command::new(executable);
    command
        .current_dir(root)
        .arg("build")
        .arg(input_relative)
        .arg("-o")
        .arg(output)
        .arg("--no-banner")
        .arg("-halt-on-error")
        .arg("-no-shell-escape");
    if let Some(format) = format {
        command.arg("--fmt").arg(format);
    }
    if tex82 {
        command.arg("--tex82");
    }
    if arguments.iter().any(|argument| argument == "--no-cache") {
        command.arg("--no-cache");
    }
    let status = command
        .status()
        .map_err(|error| format!("cannot start the real project rebuild: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("real project rebuild failed with {status}"))
    }
}

fn session_path(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    root.join(".aitex/repairs").join(format!("{id}.json"))
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn load_session(
    root: &std::path::Path,
    id: &str,
) -> Result<aitex_harness::RepairSession, String> {
    if !valid_session_id(id) {
        return Err("repair session id contains unsafe characters".into());
    }
    let path = session_path(root, id);
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("cannot read repair session {}: {error}", path.display()))?;
    let session: aitex_harness::RepairSession = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid repair session {}: {error}", path.display()))?;
    if session.schema != aitex_harness::REPAIR_SESSION_SCHEMA || session.id != id {
        return Err("repair session identity does not match its filename".into());
    }
    Ok(session)
}

fn save_session(
    root: &std::path::Path,
    session: &mut aitex_harness::RepairSession,
) -> Result<std::path::PathBuf, String> {
    session.updated_unix_ms = repair_now_ms();
    let path = session_path(root, &session.id);
    let parent = path
        .parent()
        .ok_or_else(|| "repair record has no parent directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create repair record directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot protect repair record directory: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(session)
        .map_err(|error| format!("cannot serialize repair record: {error}"))?;
    atomic_write(&path, &bytes).map_err(|error| format!("cannot persist repair record: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot protect repair record: {error}"))?;
    }
    Ok(path)
}

fn prompt_line(prompt: &str) -> Result<String, String> {
    eprint!("{prompt}");
    std::io::stderr()
        .flush()
        .map_err(|error| format!("cannot show repair prompt: {error}"))?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("cannot read repair decision: {error}"))?;
    Ok(answer.trim().to_string())
}

fn edit_plan(
    workspace: &aitex_harness::RepairWorkspace,
    plan: &mut FixPlan,
) -> Result<(), String> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| "set VISUAL or EDITOR to edit a repair proposal".to_string())?;

    // Give the editor an expendable fork. Even an editor with project plugins
    // cannot smuggle changes to another file into the accepted transaction:
    // only the selected target is read back, then staged in a new clean fork.
    let edit_workspace = workspace.fork().map_err(|error| error.to_string())?;
    edit_workspace
        .write(&plan.relative, &plan.after)
        .map_err(|error| error.to_string())?;
    let path = edit_workspace.root().join(&plan.relative);
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exec $AITEX_EDITOR \"$1\"")
        .arg("aitex-editor")
        .arg(&path)
        .env("AITEX_EDITOR", &editor)
        .status()
        .map_err(|error| format!("cannot launch {editor}: {error}"))?;
    if !status.success() {
        return Err(format!("editor exited unsuccessfully ({status})"));
    }
    let after = edit_workspace
        .read(&plan.relative)
        .map_err(|error| error.to_string())?;
    if after == plan.after {
        return Err("editor left the proposal unchanged".into());
    }
    if after == plan.before {
        return Err("edited proposal makes no source change".into());
    }
    plan.after = after;
    plan.patch = aitex_harness::unified_diff(&plan.relative, &plan.before, &plan.after);
    plan.evidence.normalized_patch = Some(plan.patch.clone());
    plan.evidence.patch_normalization = Some("human-edited".into());
    if !plan.route.ends_with("+human-edit") {
        plan.route.push_str("+human-edit");
    }
    Ok(())
}

fn configure_ai(provider: &mut String, model: &mut Option<String>) -> Result<(), String> {
    let selected_provider = prompt_line("  Provider [codex]: ")?;
    *provider = if selected_provider.is_empty() {
        "codex".into()
    } else if matches!(selected_provider.as_str(), "codex" | "anthropic") {
        selected_provider
    } else {
        return Err("provider must be codex or anthropic".into());
    };
    let suggested = if provider == "codex" {
        "gpt-5.6-luna"
    } else {
        ""
    };
    let selected_model = prompt_line(&format!(
        "  Model{}: ",
        if suggested.is_empty() {
            ""
        } else {
            " [gpt-5.6-luna]"
        }
    ))?;
    let selected_model = if selected_model.is_empty() {
        suggested.to_string()
    } else {
        selected_model
    };
    if selected_model.is_empty() {
        return Err("a model name is required".into());
    }
    *model = Some(selected_model);
    Ok(())
}

fn choose_route(
    arguments: &[String],
    ui: &mut FixUi,
    local: bool,
    guidance: Option<&str>,
    model: Option<&str>,
    attempted: &BTreeSet<(String, String)>,
    signature: &str,
) -> Result<RouteChoice, String> {
    let available = |route: &str| !attempted.contains(&(signature.to_string(), route.into()));
    let offline = arguments.iter().any(|argument| argument == "--offline");
    let mut choices = Vec::new();
    if local && available("local") {
        choices.push(serde_json::json!({
            "id": "local", "kind": "deterministic", "requires_upload": false
        }));
    }
    if !offline && model.is_some() {
        if available("ai-agent") {
            choices.push(serde_json::json!({
                "id": "ai-agent", "kind": "agentic-model",
                "model": model, "requires_upload": true
            }));
        }
        if available("ai-compact") {
            choices.push(serde_json::json!({
                "id": "ai-compact", "kind": "model", "profile": "compact",
                "model": model, "requires_upload": true
            }));
        }
        if available("ai-expanded") {
            choices.push(serde_json::json!({
                "id": "ai-expanded", "kind": "model", "profile": "expanded",
                "model": model, "requires_upload": true
            }));
        }
        if available("ai-conventional-log")
            && fix_value(arguments, "--route") == Some("ai-conventional-log")
        {
            choices.push(serde_json::json!({
                "id": "ai-conventional-log", "kind": "model-ablation",
                "profile": "conventional-log", "model": model,
                "requires_upload": true
            }));
        }
    }
    choices.push(serde_json::json!({"id": "explain", "kind": "read-only"}));
    choices.push(serde_json::json!({"id": "stop", "kind": "stop"}));
    ui.emit("routes", serde_json::json!({"choices": choices}));

    if let Some(route) = fix_value(arguments, "--route") {
        return match route {
            "local" if local && available("local") => Ok(RouteChoice::Local),
            "ai-agent" if !offline && model.is_some() && available("ai-agent") => {
                Ok(RouteChoice::Agent)
            }
            "ai-compact" if !offline && model.is_some() && available("ai-compact") => {
                Ok(RouteChoice::Ai(aitex_harness::ContextProfile::Compact))
            }
            "ai-expanded" if !offline && model.is_some() && available("ai-expanded") => {
                Ok(RouteChoice::Ai(aitex_harness::ContextProfile::Expanded))
            }
            "ai-conventional-log"
                if !offline && model.is_some() && available("ai-conventional-log") =>
            {
                Ok(RouteChoice::Ai(
                    aitex_harness::ContextProfile::ConventionalLog,
                ))
            }
            "explain" => Ok(RouteChoice::Explain),
            _ => Err(format!("requested repair route {route} is unavailable")),
        };
    }
    if !ui.interactive {
        if local && available("local") {
            return Ok(RouteChoice::Local);
        }
        if !offline && model.is_some() {
            if available("ai-agent") {
                return Ok(RouteChoice::Agent);
            }
            let profile = fix_value(arguments, "--context")
                .and_then(aitex_harness::ContextProfile::parse)
                .unwrap_or(aitex_harness::ContextProfile::Compact);
            let id = format!("ai-{profile}");
            if available(&id) {
                return Ok(RouteChoice::Ai(profile));
            }
            if fix_value(arguments, "--context").is_none()
                && profile == aitex_harness::ContextProfile::Compact
                && available("ai-expanded")
            {
                return Ok(RouteChoice::Ai(aitex_harness::ContextProfile::Expanded));
            }
        }
        return Err(guidance.unwrap_or(
            "no safe local edit; select --model MODEL or use interactive mode",
        )
        .into());
    }

    eprintln!();
    fix_note("Routes", "choose one repair path");
    let mut numbered = Vec::new();
    if local && available("local") {
        numbered.push(RouteChoice::Local);
        eprintln!("    {}  Local deterministic edit · no upload", numbered.len());
    }
    if !offline {
        if let Some(model) = model {
            if available("ai-agent") {
                numbered.push(RouteChoice::Agent);
                eprintln!("    {}  Ask {model} · agentic repair", numbered.len());
            }
            if available("ai-compact") {
                numbered.push(RouteChoice::Ai(aitex_harness::ContextProfile::Compact));
                eprintln!(
                    "    {}  Ask {model} · compact context",
                    numbered.len()
                );
            }
            if available("ai-expanded") {
                numbered.push(RouteChoice::Ai(aitex_harness::ContextProfile::Expanded));
                eprintln!(
                    "    {}  Ask {model} · expanded context",
                    numbered.len()
                );
            }
        } else {
            numbered.push(RouteChoice::ConfigureAi);
            eprintln!("    {}  Configure an AI route", numbered.len());
        }
    }
    eprintln!("    e  Explain this failure");
    eprintln!("    q  Stop with source unchanged");
    if let Some(guidance) = guidance {
        fix_note("Guidance", guidance);
    }
    loop {
        let answer = prompt_line("  Choose: ")?;
        if answer.eq_ignore_ascii_case("e") {
            return Ok(RouteChoice::Explain);
        }
        if answer.eq_ignore_ascii_case("q") {
            return Ok(RouteChoice::Stop);
        }
        if let Ok(index) = answer.parse::<usize>()
            && index > 0
            && let Some(choice) = numbered.get(index - 1)
        {
            return Ok(*choice);
        }
        eprintln!("  Enter a listed number, e, or q.");
    }
}

fn source_upload_bytes(packet: &aitex_harness::PromptPacket) -> usize {
    // Count the complete selected project context, including related author
    // source for diagnostics that point at generated bibliography/index files.
    serde_json::to_vec(&packet.envelope["context"])
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn conventional_log_for(
    root: &std::path::Path,
    input: &std::path::Path,
    diagnostic: &aitex_core::Diagnostic,
) -> String {
    let extensions: &[&str] = match diagnostic.source {
        aitex_core::Source::ToolLog(aitex_core::Tool::BibTex | aitex_core::Tool::Biber) => {
            &["blg"]
        }
        aitex_core::Source::ToolLog(aitex_core::Tool::MakeIndex) => &["ilg"],
        aitex_core::Source::ToolLog(aitex_core::Tool::MakeGlossaries) => &["glg", "alg"],
        aitex_core::Source::ToolLog(aitex_core::Tool::Nomenclature) => &["nlg"],
        aitex_core::Source::TexLog | aitex_core::Source::Engine => &["log"],
    };
    let preferred_stem = diagnostic
        .reported
        .as_ref()
        .and_then(|span| span.file.file_stem())
        .or_else(|| input.file_stem());
    let mut candidates = Vec::new();
    if let Some(stem) = preferred_stem {
        for extension in extensions {
            candidates.push(root.join(stem).with_extension(extension));
        }
    }
    let mut discovered = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| extensions.contains(&value))
            {
                discovered.push(path);
            }
        }
    }
    discovered.sort();
    for path in discovered {
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    candidates.push(root.join(".aitex-repair-stderr"));
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        if text.trim().is_empty() {
            continue;
        }
        // Conventional TeX errors and summaries occur at the end of the log.
        // Keep the tail itself rather than taking a larger tail and then
        // clipping its oldest half in the prompt builder.
        let mut reversed = text.chars().rev().take(4_000).collect::<String>();
        reversed = reversed.chars().rev().collect();
        return reversed;
    }
    diagnostic.message.clone()
}

fn model_proposal(
    arguments: &[String],
    ui: &mut FixUi,
    session: &mut aitex_harness::RepairSession,
    root: &std::path::Path,
    input: &std::path::Path,
    diagnostic: &aitex_core::Diagnostic,
    local: Option<(&str, &aitex_harness::Fix)>,
    provider: &str,
    model: &str,
    profile: aitex_harness::ContextProfile,
    timeout: std::time::Duration,
) -> Result<FixPlan, (String, FixEvidence)> {
    let input_name = input.to_string_lossy();
    let files = project_files_for(diagnostic, &input_name);
    let local_note = local.map(|(rule, fix)| format!("Local rule {rule}: {}", fix.rationale));
    let local_hint = local
        .and_then(|(_, fix)| fix.action.source_line())
        .and_then(|line| {
            source_for_rule(diagnostic, &files)
                .map(|file| aitex_core::Span::new(file.path.clone(), line))
        });
    let conventional_log = (profile == aitex_harness::ContextProfile::ConventionalLog)
        .then(|| conventional_log_for(root, input, diagnostic));
    let mut packet = aitex_harness::build_prompt_with_log(
        diagnostic,
        &files,
        profile,
        local_note.as_deref(),
        local_hint.as_ref(),
        conventional_log.as_deref(),
    );
    let upload_bytes = source_upload_bytes(&packet);
    let base_evidence = FixEvidence {
        context_profile: Some(profile),
        context_envelope: Some(packet.envelope.clone()),
        estimated_input_tokens: Some(packet.estimated_input_tokens),
        included_source_lines: Some(packet.included_source_lines),
        provider: Some(provider.into()),
        requested_model: Some(model.into()),
        ..FixEvidence::default()
    };
    if let Err(error) = session
        .usage
        .model_request_allowed(
            &session.limits,
            packet.estimated_input_tokens,
            upload_bytes,
        )
    {
        return Err((error, base_evidence));
    }
    if session.limits.max_cost_microusd.is_some() && provider != "openrouter" {
        return Err((
            format!(
                "provider {provider} does not report authoritative cost; refusing a request under --max-cost-usd"
            ),
            base_evidence,
        ));
    }
    let upload_allowed = arguments
        .iter()
        .any(|argument| argument == "--allow-source-upload");
    if !upload_allowed {
        if !ui.interactive {
            return Err((
                "--allow-source-upload is required for a noninteractive model route".into(),
                base_evidence,
            ));
        }
        let answer = prompt_line(&format!(
            "  Upload {} selected source lines ({} bytes) to {provider}/{model}? [y/N] ",
            packet.included_source_lines, upload_bytes
        ))
        .map_err(|error| (error, base_evidence.clone()))?;
        if !matches!(answer.as_str(), "y" | "Y" | "yes" | "YES") {
            return Err(("source upload declined".into(), base_evidence));
        }
    }
    let remaining_output = session
        .limits
        .max_output_tokens
        .saturating_sub(session.usage.output_tokens)
        .min(u64::from(u32::MAX));
    packet.request.max_tokens = packet
        .request
        .max_tokens
        .min(remaining_output.try_into().unwrap_or(u32::MAX));
    if packet.request.max_tokens == 0 {
        return Err(("output-token budget exhausted".into(), base_evidence));
    }
    fix_line(
        "✓",
        "Context",
        format!(
            "{profile} · {} estimated tokens · {} source lines",
            packet.estimated_input_tokens, packet.included_source_lines
        ),
    );
    ui.emit(
        "model-request",
        serde_json::json!({
            "provider": provider,
            "model": model,
            "profile": profile,
            "estimated_input_tokens": packet.estimated_input_tokens,
            "included_source_lines": packet.included_source_lines,
            "source_bytes": upload_bytes
        }),
    );

    // Anthropic performs one preflight token-count request before generation;
    // split the remaining wall budget so the pair cannot each consume it all.
    let request_timeout = if provider == "anthropic" {
        (timeout / 2).max(std::time::Duration::from_millis(1))
    } else {
        timeout
    };
    if let (Some(limit), Some(spent)) = (
        session.limits.max_cost_microusd,
        session.usage.cost_microusd,
    ) && spent >= limit
    {
        return Err(("repair cost budget exhausted".into(), base_evidence));
    }
    let mut backend = model_backend_with_timeout(
        provider,
        model,
        request_timeout,
        arguments,
    )
        .map_err(|error| (error.to_string(), base_evidence.clone()))?;
    let run = match aitex_harness::run_model(backend.as_mut(), &packet) {
        Ok(run) => run,
        Err(error) => {
            let mut evidence = base_evidence;
            if let aitex_harness::ModelRunError::Response {
                response,
                provider_counted_input_tokens,
                elapsed,
                ..
            } = &error
            {
                session
                    .usage
                    .charge(response.usage, response.metadata.charged_microusd);
                evidence.returned_model = Some(response.model.clone());
                evidence.provider_counted_input_tokens = *provider_counted_input_tokens;
                evidence.usage = response.usage;
                evidence.metadata = response.metadata.clone();
                evidence.model_ms = Some(elapsed.as_secs_f64() * 1e3);
                evidence.raw_response = Some(response.text.clone());
                fix_line("◆", "Usage", model_usage_text(response, *elapsed));
            }
            return Err((error.to_string(), evidence));
        }
    };
    session.usage.charge(
        run.response.usage,
        run.response.metadata.charged_microusd,
    );
    let evidence = FixEvidence {
        context_profile: Some(profile),
        context_envelope: Some(packet.envelope.clone()),
        estimated_input_tokens: Some(packet.estimated_input_tokens),
        included_source_lines: Some(packet.included_source_lines),
        provider: Some(provider.into()),
        requested_model: Some(model.into()),
        returned_model: Some(run.response.model.clone()),
        provider_counted_input_tokens: run.provider_counted_input_tokens,
        usage: run.response.usage,
        metadata: run.response.metadata.clone(),
        model_ms: Some(run.elapsed.as_secs_f64() * 1e3),
        raw_response: Some(run.response.text.clone()),
        response_normalization: run.answer.response_normalization.clone(),
        ..FixEvidence::default()
    };
    fix_line(
        "✓",
        "AI",
        format!(
            "{} · {}",
            run.response.model,
            model_usage_text(&run.response, run.elapsed),
        ),
    );
    let route = format!("ai-{profile}");
    model_plan(
        root,
        &run.answer,
        &route,
        evidence,
    )
}

fn attempt_record(
    id: String,
    diagnostic: &aitex_core::Diagnostic,
    plan: Option<&FixPlan>,
    evidence: FixEvidence,
    validation: &str,
    validation_ms: Option<f64>,
    artifact_bytes: Option<u64>,
    next: Option<&aitex_core::Diagnostic>,
    safety: Option<aitex_harness::SafetyRecord>,
    accepted: Option<bool>,
    rejection: Option<String>,
) -> aitex_harness::RepairAttempt {
    let (route, cause, confidence, target, before_sha256, after_sha256, patch) =
        plan.map_or_else(
            || {
                (
                    evidence
                        .context_profile
                        .map_or_else(|| "unavailable".into(), |profile| format!("ai-{profile}")),
                    String::new(),
                    String::new(),
                    None,
                    None,
                    None,
                    evidence.raw_patch.clone(),
                )
            },
            |plan| {
                (
                    plan.route.clone(),
                    plan.rationale.clone(),
                    plan.confidence.clone(),
                    Some(plan.relative.to_string_lossy().into_owned()),
                    Some(sha256_hex(&plan.before)),
                    Some(sha256_hex(&plan.after)),
                    plan.evidence.raw_patch.clone(),
                )
            },
        );
    let evidence = plan.map_or(evidence, |plan| FixEvidence {
        context_profile: plan.evidence.context_profile,
        context_envelope: plan.evidence.context_envelope.clone(),
        estimated_input_tokens: plan.evidence.estimated_input_tokens,
        included_source_lines: plan.evidence.included_source_lines,
        provider: plan.evidence.provider.clone(),
        requested_model: plan.evidence.requested_model.clone(),
        returned_model: plan.evidence.returned_model.clone(),
        provider_counted_input_tokens: plan.evidence.provider_counted_input_tokens,
        usage: plan.evidence.usage,
        metadata: plan.evidence.metadata.clone(),
        model_ms: plan.evidence.model_ms,
        raw_response: plan.evidence.raw_response.clone(),
        response_normalization: plan.evidence.response_normalization.clone(),
        raw_patch: plan.evidence.raw_patch.clone(),
        normalized_patch: plan.evidence.normalized_patch.clone(),
        patch_normalization: plan.evidence.patch_normalization.clone(),
    });
    aitex_harness::RepairAttempt {
        id,
        created_unix_ms: repair_now_ms(),
        route,
        diagnostic: diagnostic.into(),
        context_profile: evidence.context_profile,
        context_envelope: evidence.context_envelope,
        estimated_input_tokens: evidence.estimated_input_tokens,
        included_source_lines: evidence.included_source_lines,
        provider: evidence.provider,
        requested_model: evidence.requested_model,
        returned_model: evidence.returned_model,
        provider_counted_input_tokens: evidence.provider_counted_input_tokens,
        input_tokens: evidence.usage.input,
        output_tokens: evidence.usage.output,
        cache_read_tokens: evidence.usage.cache_read,
        cache_write_tokens: evidence.usage.cache_write,
        reasoning_tokens: evidence.usage.reasoning,
        cost_microusd: evidence.metadata.charged_microusd,
        upstream_microusd: evidence.metadata.upstream_microusd,
        served_provider: evidence.metadata.provider,
        requested_provider: evidence.metadata.requested_provider,
        generation_id: evidence.metadata.generation_id,
        service_tier: evidence.metadata.service_tier,
        time_to_first_byte_ms: evidence.metadata.time_to_first_byte_ms,
        provider_generation_ms: evidence.metadata.provider_generation_ms,
        router_metadata: evidence.metadata.router_metadata,
        model_ms: evidence.model_ms,
        raw_response: evidence.raw_response,
        response_normalization: evidence.response_normalization,
        cause,
        confidence,
        raw_patch: if patch.is_empty() {
            evidence.raw_patch
        } else {
            patch
        },
        normalized_patch: evidence.normalized_patch,
        patch_normalization: evidence.patch_normalization,
        changes: Vec::new(),
        agent_trace: None,
        target,
        before_sha256,
        after_sha256,
        safety,
        validation: validation.into(),
        validation_ms,
        artifact_bytes,
        next_diagnostic: next.map(Into::into),
        accepted,
        rejection,
    }
}

fn agent_attempt_record(
    id: String,
    diagnostic: &aitex_core::Diagnostic,
    run: &aitex_harness::AgentRun,
    provider: &str,
    requested_model: &str,
    accepted: Option<bool>,
    rejection: Option<String>,
) -> aitex_harness::RepairAttempt {
    let changes = run
        .changes
        .iter()
        .map(|change| aitex_harness::RepairChangeRecord {
            target: change.path.to_string_lossy().into_owned(),
            before_sha256: sha256_hex(&change.before),
            after_sha256: sha256_hex(&change.after),
            patch: change.patch.clone(),
        })
        .collect::<Vec<_>>();
    let raw_patch = changes
        .iter()
        .map(|change| change.patch.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let singular = (changes.len() == 1).then(|| {
        (
            changes[0].target.clone(),
            changes[0].before_sha256.clone(),
            changes[0].after_sha256.clone(),
        )
    });
    let safety = run.changes.iter().try_fold(
        aitex_harness::SafetyRecord {
            before_bytes: 0,
            after_bytes: 0,
            added_lines: 0,
            deleted_lines: 0,
        },
        |mut total, change| {
            let record = aitex_harness::inspect_patch_safety(
                &change.patch,
                &change.before,
                &change.after,
                usize::MAX,
                usize::MAX,
            )
            .ok()?;
            total.before_bytes = total.before_bytes.saturating_add(record.before_bytes);
            total.after_bytes = total.after_bytes.saturating_add(record.after_bytes);
            total.added_lines = total.added_lines.saturating_add(record.added_lines);
            total.deleted_lines = total.deleted_lines.saturating_add(record.deleted_lines);
            Some(total)
        },
    );
    let model_ms = run
        .trace
        .events
        .iter()
        .map(|event| event.usage.elapsed_ms as f64)
        .sum::<f64>();
    let metadata = &run.last_metadata;
    aitex_harness::RepairAttempt {
        id,
        created_unix_ms: repair_now_ms(),
        route: "ai-agent".into(),
        diagnostic: diagnostic.into(),
        context_profile: None,
        context_envelope: None,
        estimated_input_tokens: None,
        included_source_lines: usize::try_from(run.trace.source_lines_read).ok(),
        provider: Some(provider.into()),
        requested_model: Some(requested_model.into()),
        returned_model: run.returned_model.clone(),
        provider_counted_input_tokens: None,
        input_tokens: run.trace.usage.input_tokens,
        output_tokens: run.trace.usage.output_tokens,
        cache_read_tokens: run.trace.usage.cache_read_tokens,
        cache_write_tokens: run.trace.usage.cache_write_tokens,
        reasoning_tokens: run.trace.usage.reasoning_tokens,
        cost_microusd: run.trace.usage.cost_microusd,
        upstream_microusd: metadata.upstream_microusd,
        served_provider: metadata.provider.clone(),
        requested_provider: metadata.requested_provider.clone(),
        generation_id: metadata.generation_id.clone(),
        service_tier: metadata.service_tier.clone(),
        time_to_first_byte_ms: metadata.time_to_first_byte_ms,
        provider_generation_ms: metadata.provider_generation_ms,
        router_metadata: metadata.router_metadata.clone(),
        model_ms: Some(model_ms),
        raw_response: None,
        response_normalization: None,
        cause: run.cause.clone(),
        confidence: run.confidence.clone(),
        raw_patch,
        normalized_patch: None,
        patch_normalization: None,
        changes,
        agent_trace: serde_json::to_value(&run.trace).ok(),
        target: singular.as_ref().map(|change| change.0.clone()),
        before_sha256: singular.as_ref().map(|change| change.1.clone()),
        after_sha256: singular.map(|change| change.2),
        safety,
        validation: if run.final_build.succeeded {
            "complete"
        } else {
            run.trace.status.as_str()
        }
        .into(),
        validation_ms: Some(run.final_build.elapsed.as_secs_f64() * 1e3),
        artifact_bytes: run.final_build.artifact_bytes,
        next_diagnostic: run
            .final_build
            .diagnostic
            .as_ref()
            .map(aitex_harness::DiagnosticRecord::from),
        accepted,
        rejection,
    }
}

fn agent_usage_text(run: &aitex_harness::AgentRun) -> String {
    let usage = &run.trace.usage;
    let cost = usage.cost_microusd.map_or_else(
        || "cost unavailable".to_string(),
        |cost| format!("${:.6}", cost as f64 / 1_000_000.0),
    );
    format!(
        "{} calls · {} input / {} output · {} · {}",
        usage.model_calls,
        usage.total_input_tokens(),
        usage.output_tokens,
        human_duration(std::time::Duration::from_millis(run.trace.elapsed_ms)),
        cost,
    )
}

fn apply_transaction(
    root: &std::path::Path,
    workspace: &aitex_harness::RepairWorkspace,
    originals: &BTreeMap<std::path::PathBuf, Vec<u8>>,
    session_id: &str,
) -> Result<std::path::PathBuf, String> {
    if originals.is_empty() {
        return Err("repair transaction contains no accepted changes".into());
    }
    for (relative, original) in originals {
        let target = root.join(relative);
        let current = std::fs::read(&target)
            .map_err(|error| format!("cannot re-read {}: {error}", target.display()))?;
        if &current != original {
            return Err(format!(
                "{} changed during the repair session; refusing the transaction",
                relative.display()
            ));
        }
    }
    let backup_root = root.join(".aitex/backups").join(session_id);
    for (relative, original) in originals {
        let backup = backup_root.join(relative);
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create repair backup directory: {error}"))?;
        }
        atomic_write(&backup, original)
            .map_err(|error| format!("cannot write repair backup: {error}"))?;
    }
    let mut written = Vec::new();
    for (relative, original) in originals {
        let target = root.join(relative);
        let after = workspace.read(relative).map_err(|error| error.to_string())?;
        if let Err(error) = atomic_write(&target, &after) {
            for written_relative in written.iter().rev() {
                if let Some(before) = originals.get(written_relative) {
                    let _ = atomic_write(&root.join(written_relative), before);
                }
            }
            let _ = atomic_write(&target, original);
            return Err(format!(
                "cannot atomically apply {}: {error}; written sources were rolled back",
                relative.display()
            ));
        }
        written.push(relative.clone());
    }
    Ok(backup_root)
}

fn rollback_transaction(
    root: &std::path::Path,
    originals: &BTreeMap<std::path::PathBuf, Vec<u8>>,
) {
    for (relative, original) in originals {
        let _ = atomic_write(&root.join(relative), original);
    }
}

fn inspect_transaction_safety(
    workspace: &aitex_harness::RepairWorkspace,
    originals: &BTreeMap<std::path::PathBuf, Vec<u8>>,
    limits: &aitex_harness::RepairLimits,
) -> Result<(), String> {
    let mut changed = 0usize;
    let mut deleted = 0usize;
    for (path, before) in originals {
        let after = workspace.read(path).map_err(|error| error.to_string())?;
        if &after == before {
            continue;
        }
        let patch = aitex_harness::unified_diff(path, before, &after);
        let safety = aitex_harness::inspect_patch_safety(
            &patch,
            before,
            &after,
            limits.max_changed_lines,
            limits.max_deleted_lines,
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
        changed = changed.saturating_add(safety.added_lines + safety.deleted_lines);
        deleted = deleted.saturating_add(safety.deleted_lines);
    }
    if changed > limits.max_changed_lines {
        return Err(format!(
            "repair changes {changed} lines across the project; limit is {}",
            limits.max_changed_lines
        ));
    }
    if deleted > limits.max_deleted_lines {
        return Err(format!(
            "repair deletes {deleted} lines across the project; limit is {}",
            limits.max_deleted_lines
        ));
    }
    Ok(())
}

fn accept_pending_session(
    arguments: &[String],
    root: &std::path::Path,
    input_relative: &std::path::Path,
    executable: &std::path::Path,
    format: Option<&std::path::Path>,
    artifact: aitex_harness::ValidationArtifact,
    tex82: bool,
    mut session: aitex_harness::RepairSession,
    ui: &mut FixUi,
) -> Result<(), String> {
    if session.input != input_relative.to_string_lossy() {
        return Err("repair session belongs to a different root input".into());
    }
    let pending = session
        .attempts
        .iter()
        .enumerate()
        .filter_map(|(index, attempt)| attempt.accepted.is_none().then_some(index))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Err("repair session has no pending validated proposal".into());
    }
    if pending.iter().any(|index| {
        !matches!(
            session.attempts[*index].validation.as_str(),
            "complete" | "progress"
        )
    }) || pending
        .last()
        .is_none_or(|index| session.attempts[*index].validation != "complete")
    {
        return Err("stored repair chain is not complete; refusing a partial source edit".into());
    }

    let mut candidate = aitex_harness::RepairWorkspace::new(
        root,
        aitex_harness::ValidationLimits::default(),
    )
    .map_err(|error| error.to_string())?;
    candidate.set_timeout(
        aitex_harness::ValidationLimits::default()
            .timeout
            .min(std::time::Duration::from_secs(
                session.limits.max_elapsed_seconds,
            )),
    );
    let mut originals = BTreeMap::new();
    let mut targets = Vec::new();
    let mut changed_lines = 0usize;
    let mut deleted_lines = 0usize;
    for index in &pending {
        let attempt = &session.attempts[*index];
        let changes = if attempt.changes.is_empty() {
            vec![aitex_harness::RepairChangeRecord {
                target: attempt
                    .target
                    .clone()
                    .ok_or_else(|| "pending proposal has no source target".to_string())?,
                before_sha256: attempt
                    .before_sha256
                    .clone()
                    .ok_or_else(|| "pending proposal has no input digest".to_string())?,
                after_sha256: attempt
                    .after_sha256
                    .clone()
                    .ok_or_else(|| "pending proposal has no output digest".to_string())?,
                patch: attempt
                    .normalized_patch
                    .clone()
                    .unwrap_or_else(|| attempt.raw_patch.clone()),
            }]
        } else {
            attempt.changes.clone()
        };
        for change in changes {
            let target_relative = std::path::PathBuf::from(&change.target);
            let target = resolve_source_target(root, &target_relative)?;
            if !originals.contains_key(&target_relative) {
                let original = std::fs::read(&target)
                    .map_err(|error| format!("cannot read {}: {error}", target.display()))?;
                originals.insert(target_relative.clone(), original);
            }
            let before = candidate
                .read(&target_relative)
                .map_err(|error| error.to_string())?;
            if change.before_sha256 != sha256_hex(&before) {
                return Err(format!(
                    "{} changed after proposal validation; refusing stale repair",
                    target_relative.display()
                ));
            }
            let after = aitex_harness::apply_unified_patch(&change.patch, &before)
                .map_err(|error| format!("stored repair patch is no longer applicable: {error}"))?;
            if change.after_sha256 != sha256_hex(&after) {
                return Err("stored repair does not match its validated output digest".into());
            }
            let safety = aitex_harness::inspect_patch_safety(
                &change.patch,
                &before,
                &after,
                session.limits.max_changed_lines,
                session.limits.max_deleted_lines,
            )
            .map_err(|error| error.to_string())?;
            changed_lines = changed_lines
                .saturating_add(safety.added_lines.saturating_add(safety.deleted_lines));
            deleted_lines = deleted_lines.saturating_add(safety.deleted_lines);
            candidate
                .write(&target_relative, &after)
                .map_err(|error| error.to_string())?;
            if !targets.iter().any(|target| target == &change.target) {
                targets.push(change.target);
            }
        }
    }
    if changed_lines > session.limits.max_changed_lines {
        return Err(format!(
            "stored repair changes {changed_lines} lines across the project; limit is {}",
            session.limits.max_changed_lines
        ));
    }
    if deleted_lines > session.limits.max_deleted_lines {
        return Err(format!(
            "stored repair deletes {deleted_lines} lines across the project; limit is {}",
            session.limits.max_deleted_lines
        ));
    }
    let build = candidate
        .build(executable, input_relative, artifact, format, tex82)
        .map_err(|error| error.to_string())?;
    fix_build_phases(ui, &build);
    if !build.succeeded {
        return Err(format!(
            "the stored repair chain no longer completes the build ({})",
            build.detail
        ));
    }
    session.status = "validated".into();
    save_session(root, &mut session)?;
    let backup = apply_transaction(root, &candidate, &originals, &session.id)?;
    for index in &pending {
        session.attempts[*index].accepted = Some(true);
    }
    fix_line("✓", "Applied", format!("{} source file(s)", originals.len()));
    fix_note("Backup", backup.display());
    if rebuild_applied_project(
        arguments,
        root,
        input_relative,
        executable,
        format,
        tex82,
    )
    .is_err()
    {
        rollback_transaction(root, &originals);
        session.status = "rolled-back".into();
        save_session(root, &mut session)?;
        return Err("real project rebuild failed; author source was rolled back".into());
    }
    session.status = "applied".into();
    session.final_build_succeeded = true;
    for target in &targets {
        if !session.applied_targets.contains(target) {
            session.applied_targets.push(target.clone());
        }
    }
    let record = save_session(root, &mut session)?;
    ui.emit(
        "complete",
        serde_json::json!({
            "attempts": pending.iter().map(|index| &session.attempts[*index].id).collect::<Vec<_>>(),
            "targets": targets,
            "record": record,
            "artifact_bytes": build.artifact_bytes
        }),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_agent_route(
    arguments: &[String],
    root: &std::path::Path,
    input_relative: &std::path::Path,
    executable: &std::path::Path,
    format: Option<&std::path::Path>,
    artifact: aitex_harness::ValidationArtifact,
    tex82: bool,
    workspace: aitex_harness::RepairWorkspace,
    initial_build: aitex_harness::WorkspaceBuild,
    diagnostic: &aitex_core::Diagnostic,
    provider: &str,
    model: &str,
    session: &mut aitex_harness::RepairSession,
    ui: &mut FixUi,
    started: std::time::Instant,
    dry_run: bool,
    apply: bool,
) {
    let limits = session.limits.clone();
    if limits.max_cost_microusd.is_some() && provider != "openrouter" {
        fix_line(
            "×",
            "Agent",
            format!(
                "provider {provider} does not report authoritative cost; refusing a session under --max-cost-usd"
            ),
        );
        fix_unchanged("agent did not start");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    if limits.max_cost_microusd.is_some() && session.usage.unpriced_model_calls > 0 {
        fix_line(
            "×",
            "Agent",
            "a prior model call has unknown cost; refusing further spend",
        );
        fix_unchanged("agent did not start");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let upload_allowed = arguments
        .iter()
        .any(|argument| argument == "--allow-source-upload");
    if !upload_allowed {
        if !ui.interactive {
            fix_line(
                "×",
                "Agent",
                "--allow-source-upload is required for a noninteractive agent route",
            );
            fix_unchanged("agent did not start");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let answer = match prompt_line(&format!(
            "  Allow {provider}/{model} to inspect up to {} bytes of selected project source over at most {} turns? [y/N] ",
            limits.max_source_bytes_per_request, limits.max_agent_turns,
        )) {
            Ok(answer) => answer,
            Err(error) => {
                fix_line("×", "Agent", error);
                fix_unchanged("agent did not start");
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        };
        if !matches!(answer.as_str(), "y" | "Y" | "yes" | "YES") {
            fix_unchanged("source inspection declined");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    }

    let remaining = std::time::Duration::from_secs(limits.max_elapsed_seconds)
        .saturating_sub(started.elapsed());
    if remaining.is_zero() {
        fix_line("◆", "Budget", "repair time limit reached");
        fix_unchanged("agent did not start");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let mut backend = match model_backend_with_timeout(provider, model, remaining, arguments) {
        Ok(backend) => backend,
        Err(error) => {
            fix_line("×", "Agent", error);
            fix_unchanged("agent backend unavailable");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    let remaining_cost = limits.max_cost_microusd.map(|limit| {
        limit.saturating_sub(session.usage.cost_microusd.unwrap_or_default())
    });
    if remaining_cost == Some(0) {
        fix_line("◆", "Budget", "repair cost limit reached");
        fix_unchanged("agent did not start");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let agent_limits = aitex_harness::AgentLimits {
        max_turns: limits.max_agent_turns,
        max_builds: limits.max_agent_builds,
        max_source_bytes: limits.max_source_bytes_per_request,
        max_changed_lines: limits.max_changed_lines,
        max_deleted_lines: limits.max_deleted_lines,
        max_input_tokens: limits
            .max_input_tokens
            .saturating_sub(session.usage.total_input_tokens()),
        max_output_tokens: limits
            .max_output_tokens
            .saturating_sub(session.usage.output_tokens),
        max_elapsed_seconds: remaining.as_secs().max(1),
        max_cost_microusd: remaining_cost,
        ..aitex_harness::AgentLimits::default()
    };
    if agent_limits.max_input_tokens == 0 || agent_limits.max_output_tokens == 0 {
        fix_line("◆", "Budget", "repair token limit reached");
        fix_unchanged("agent did not start");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    fix_line(
        "✓",
        "Agent",
        format!(
            "{model} · {} turns / {} builds · bounded tools",
            agent_limits.max_turns, agent_limits.max_builds
        ),
    );
    ui.emit(
        "agent-started",
        serde_json::json!({
            "provider": provider,
            "model": model,
            "limits": agent_limits,
            "protocol": aitex_harness::AGENT_PROTOCOL,
        }),
    );
    session.status = "agent-running".into();
    let _ = save_session(root, session);
    let build_config = aitex_harness::AgentBuildConfig {
        executable: executable.to_path_buf(),
        input: input_relative.to_path_buf(),
        artifact,
        format: format.map(std::path::Path::to_path_buf),
        tex82,
    };
    let run = aitex_harness::run_agent_with_progress(
        backend.as_mut(),
        workspace,
        initial_build,
        &build_config,
        agent_limits,
        |progress| {
            let ok = progress
                .data
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let mark = if ok {
                if progress.label == "Agent" { "◆" } else { "✓" }
            } else {
                "×"
            };
            let detail = if progress.detail.is_empty() {
                "bounded operation".to_string()
            } else {
                progress.detail.clone()
            };
            fix_line(mark, progress.label, detail);
            ui.emit(
                "agent-progress",
                serde_json::json!({
                    "label": progress.label,
                    "detail": progress.detail,
                    "ok": ok,
                }),
            );
        },
    );
    session.usage.absorb_agent(&run.trace.usage);
    fix_line("✓", "Usage", agent_usage_text(&run));
    fix_note(
        "Evidence",
        format!(
            "{} source lines · {} bytes · {} searches · {} reads",
            run.trace.source_lines_read,
            run.trace.source_bytes_shared,
            run.trace.searches,
            run.trace.reads,
        ),
    );

    let attempt_id = format!("a-{}", session.attempts.len() + 1);
    if run.status != aitex_harness::AgentStatus::Complete || !run.final_build.succeeded {
        let reason = run
            .trace
            .error
            .clone()
            .unwrap_or_else(|| format!("agent stopped with status {}", run.trace.status));
        fix_line("×", "Agent", &reason);
        session.attempts.push(agent_attempt_record(
            attempt_id,
            diagnostic,
            &run,
            provider,
            model,
            Some(false),
            Some(reason.clone()),
        ));
        session.status = run.trace.status.clone();
        let record = save_session(root, session).ok();
        fix_unchanged("agent did not produce a complete validated build");
        ui.emit(
            "stopped",
            serde_json::json!({"reason": reason, "record": record}),
        );
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    if run.changes.is_empty() {
        let reason = "complete isolated build contained no author-source change".to_string();
        fix_line("×", "Agent", &reason);
        session.attempts.push(agent_attempt_record(
            attempt_id,
            diagnostic,
            &run,
            provider,
            model,
            Some(false),
            Some(reason.clone()),
        ));
        session.status = "proposal-rejected".into();
        let _ = save_session(root, session);
        fix_unchanged(reason);
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    fix_line(
        "✓",
        "Validate",
        format!(
            "complete isolated {} build · {} files · {}",
            artifact.label(),
            run.changes.len(),
            human_duration(run.final_build.elapsed),
        ),
    );
    for change in &run.changes {
        fix_note("Patch", change.path.display());
        eprintln!("{}", change.patch);
    }

    if dry_run {
        session.attempts.push(agent_attempt_record(
            attempt_id.clone(),
            diagnostic,
            &run,
            provider,
            model,
            None,
            None,
        ));
        session.status = "proposal-ready".into();
        let record = match save_session(root, session) {
            Ok(record) => record,
            Err(error) => {
                fix_line("×", "Record", error);
                fix_unchanged("validated agent repair was not persisted");
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        };
        fix_unchanged(format!(
            "validated agent repair · accept with --session {} --apply",
            session.id
        ));
        fix_note("Record", record.display());
        ui.emit(
            "decision-required",
            serde_json::json!({
                "attempt": attempt_id,
                "choices": ["accept", "reject"],
                "record": record,
            }),
        );
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }

    let accepted = if ui.interactive {
        match prompt_line("  Accept this validated multi-file repair? [y/N] ") {
            Ok(answer) => matches!(answer.as_str(), "y" | "Y" | "yes" | "YES"),
            Err(error) => {
                fix_line("×", "Decision", error);
                false
            }
        }
    } else {
        apply
    };
    session.attempts.push(agent_attempt_record(
        attempt_id.clone(),
        diagnostic,
        &run,
        provider,
        model,
        Some(accepted),
        (!accepted).then(|| "user declined the proposal".into()),
    ));
    if !accepted {
        session.status = "proposal-declined".into();
        let record = save_session(root, session).ok();
        fix_unchanged("validated agent repair was declined");
        ui.emit(
            "decision",
            serde_json::json!({"attempt": attempt_id, "accepted": false, "record": record}),
        );
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }

    let originals = run
        .changes
        .iter()
        .map(|change| (change.path.clone(), change.before.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Err(error) = inspect_transaction_safety(&run.workspace, &originals, &limits) {
        fix_line("×", "Safety", &error);
        if let Some(attempt) = session.attempts.last_mut() {
            attempt.accepted = Some(false);
            attempt.rejection = Some(error);
        }
        session.status = "proposal-rejected".into();
        let _ = save_session(root, session);
        fix_unchanged("validated transaction exceeds the source safety limit");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    session.status = "validated".into();
    if let Err(error) = save_session(root, session) {
        fix_line("×", "Record", error);
        fix_unchanged("the transaction was not applied without an audit record");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let backup = match apply_transaction(root, &run.workspace, &originals, &session.id) {
        Ok(backup) => backup,
        Err(error) => {
            fix_line("×", "Apply", error);
            fix_unchanged("transaction refused");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    fix_line("✓", "Applied", format!("{} source file(s)", originals.len()));
    fix_note("Backup", backup.display());
    if rebuild_applied_project(
        arguments,
        root,
        input_relative,
        executable,
        format,
        tex82,
    )
    .is_err()
    {
        rollback_transaction(root, &originals);
        session.status = "rolled-back".into();
        let _ = save_session(root, session);
        fix_line("×", "Rebuild", "real project build failed after validation");
        fix_note("Rollback", "author sources restored from the repair transaction");
        ui.emit(
            "rolled-back",
            serde_json::json!({"reason": "real project rebuild failed"}),
        );
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    session.status = "applied".into();
    session.final_build_succeeded = true;
    for relative in originals.keys() {
        let target = relative.to_string_lossy().into_owned();
        if !session.applied_targets.contains(&target) {
            session.applied_targets.push(target);
        }
    }
    let record = save_session(root, session).ok();
    fix_line(
        "✓",
        "Complete",
        format!("agent repair applied and rebuilt · {}", human_duration(started.elapsed())),
    );
    if let Some(record) = &record {
        fix_note("Record", record.display());
    }
    ui.emit(
        "complete",
        serde_json::json!({
            "repaired": true,
            "attempt": attempt_id,
            "targets": session.applied_targets,
            "record": record,
        }),
    );
    EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
}

fn run_fix() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let input_name = input_argument(&arguments).expect("fix arguments were validated");
    let started = std::time::Instant::now();
    let json = arguments.iter().any(|argument| argument == "--json");
    let explicit_mode = arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--dry-run" | "--apply"));
    let interactive = !json
        && !explicit_mode
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    if !interactive && !explicit_mode && !json {
        fix_line(
            "×",
            "Fix",
            "noninteractive repair requires --dry-run, --apply, or --json",
        );
        EXIT_CODE.store(2, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let dry_run = arguments.iter().any(|argument| argument == "--dry-run") || json && !explicit_mode;
    let apply = arguments.iter().any(|argument| argument == "--apply");
    let (root, input) = match project_layout(input_name) {
        Ok(layout) => layout,
        Err(error) => {
            fix_line("×", "Fix", error);
            fix_unchanged("planning failed");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    let input_relative = match input.strip_prefix(&root) {
        Ok(path) => path.to_path_buf(),
        Err(_) => {
            fix_line("×", "Fix", "input escapes its project directory");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    let input_bytes = match std::fs::read(&input) {
        Ok(bytes) => bytes,
        Err(error) => {
            fix_line("×", "Fix", error);
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    let requested_session = fix_value(&arguments, "--session");
    let session_id = requested_session
        .map(str::to_string)
        .unwrap_or_else(repair_session_id);
    if !valid_session_id(&session_id) {
        fix_line("×", "Session", "session id contains unsafe characters");
        EXIT_CODE.store(2, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let mut session = if requested_session.is_some() && session_path(&root, &session_id).is_file() {
        match load_session(&root, &session_id) {
            Ok(session) => session,
            Err(error) => {
                fix_line("×", "Session", error);
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        }
    } else {
        aitex_harness::RepairSession::new(
            session_id.clone(),
            repair_now_ms(),
            input_relative.to_string_lossy().into_owned(),
            sha256_hex(&input_bytes),
            format!("aitex {}", env!("CARGO_PKG_VERSION")),
            repair_limits(&arguments),
        )
    };
    if session.input != input_relative.to_string_lossy() {
        fix_line("×", "Session", "repair record belongs to a different root input");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let limits = session.limits.clone();
    let mut ui = FixUi {
        json,
        interactive,
        sequence: 0,
        session: session_id.clone(),
    };
    ui.emit(
        "session-started",
        serde_json::json!({
            "input": session.input,
            "limits": session.limits,
            "record": session_path(&root, &session_id)
        }),
    );
    if let Err(error) = save_session(&root, &mut session) {
        fix_line("×", "Record", error);
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let (executable, format, artifact, tex82) = match validation_settings(&arguments) {
        Ok(settings) => settings,
        Err(error) => {
            fix_line("×", "Fix", error);
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    let pending_ids = session
        .attempts
        .iter()
        .filter(|attempt| attempt.accepted.is_none())
        .map(|attempt| attempt.id.clone())
        .collect::<Vec<_>>();
    if requested_session.is_some() && !pending_ids.is_empty() && !apply {
        let complete_chain = session
            .attempts
            .iter()
            .rfind(|attempt| attempt.accepted.is_none())
            .is_some_and(|attempt| attempt.validation == "complete");
        if complete_chain {
            session.status = "proposal-ready".into();
            let record = save_session(&root, &mut session).ok();
            fix_unchanged(format!(
                "validated repair chain · accept with --session {session_id} --apply"
            ));
            ui.emit(
                "decision-required",
                serde_json::json!({
                    "attempts": pending_ids,
                    "choices": ["accept", "reject"],
                    "record": record
                }),
            );
            EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        } else {
            fix_line(
                "×",
                "Session",
                "stored private chain is incomplete; start a new repair session",
            );
            fix_unchanged("a partial stored chain cannot be applied");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    if requested_session.is_some() && pending_ids.is_empty() && session.final_build_succeeded {
        fix_line("✓", "Session", "repair was already applied and built");
        ui.emit(
            "complete",
            serde_json::json!({"repaired": true, "already_applied": true}),
        );
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    if apply && requested_session.is_some() && !pending_ids.is_empty() {
        match accept_pending_session(
            &arguments,
            &root,
            &input_relative,
            &executable,
            format.as_deref(),
            artifact,
            tex82,
            session,
            &mut ui,
        ) {
            Ok(()) => EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed),
            Err(error) => {
                fix_line("×", "Accept", error);
                fix_unchanged("stored proposal was not applied");
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        return;
    }
    let validation_limits = aitex_harness::ValidationLimits {
        timeout: aitex_harness::ValidationLimits::default()
            .timeout
            .min(std::time::Duration::from_secs(limits.max_elapsed_seconds)),
        ..aitex_harness::ValidationLimits::default()
    };
    let mut workspace = match aitex_harness::RepairWorkspace::new(&root, validation_limits) {
        Ok(workspace) => workspace,
        Err(error) => {
            fix_line("×", "Isolate", error);
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    fix_line("✓", "Isolate", "private project workspace");
    let initial = match workspace.build(
        &executable,
        &input_relative,
        artifact,
        format.as_deref(),
        tex82,
    ) {
        Ok(build) => build,
        Err(error) => {
            fix_line("×", "Build", error);
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    fix_build_phases(&ui, &initial);
    if initial.succeeded {
        session.status = "already-succeeded".into();
        session.final_build_succeeded = true;
        let record = save_session(&root, &mut session).ok();
        fix_line("✓", "Fix", "build succeeded · no repair needed");
        ui.emit(
            "complete",
            serde_json::json!({"repaired": false, "record": record}),
        );
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let Some(mut diagnostic) = initial.diagnostic.clone() else {
        fix_line("×", "Diagnose", initial.detail);
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    };
    let mut current_build = initial;
    let mut originals = BTreeMap::new();
    let mut attempted = BTreeSet::new();
    let mut provider = fix_value(&arguments, "--provider")
        .map(str::to_string)
        .or_else(|| std::env::var("AITEX_PROVIDER").ok())
        .unwrap_or_else(|| "anthropic".into());
    let mut model = fix_value(&arguments, "--model")
        .map(str::to_string)
        .or_else(|| std::env::var("AITEX_MODEL").ok());
    let mut complete = false;

    'repair: while session.attempts.len() < limits.max_attempts as usize
        && started.elapsed() < std::time::Duration::from_secs(limits.max_elapsed_seconds)
    {
        let presentation = diagnostic.presentation();
        fix_line("✓", "Diagnose", &presentation.headline);
        fix_note("Error", &presentation.error);
        if let Some(reported) = &presentation.reported {
            fix_note("Reported", reported);
        }
        if presentation.occurrences > 1 {
            fix_note(
                "Repeated",
                format!("{} occurrences", presentation.occurrences),
            );
        }
        for frame in &presentation.provenance {
            fix_note("Trace", frame);
        }
        ui.emit(
            "diagnostic",
            serde_json::to_value(aitex_harness::DiagnosticRecord::from(&diagnostic))
                .unwrap_or(serde_json::Value::Null),
        );

        let workspace_input = workspace.root().join(&input_relative);
        let workspace_input_name = workspace_input.to_string_lossy();
        let files = project_files_for(&diagnostic, &workspace_input_name);
        let context = aitex_harness::context_for_project(&workspace_input);
        let cascade = aitex_harness::rules::default_cascade();
        let local = aitex_harness::rules::run(
            &cascade,
            &diagnostic,
            &context,
            aitex_harness::rules::Source {
                text: source_for_rule(&diagnostic, &files).map(|file| file.text.as_str()),
            },
        );
        let mut local_plan = local.as_ref().and_then(|(rule, proposed)| {
            match deterministic_plan(
                workspace.root(),
                &workspace_input,
                &diagnostic,
                rule,
                proposed,
            ) {
                Ok(plan) => plan,
                Err(error) => {
                    fix_note("Local", error);
                    None
                }
            }
        });
        let guidance = local.as_ref().map(|(_, fix)| action_text(&fix.action));
        let signature = diagnostic.signature();
        let choice = match choose_route(
            &arguments,
            &mut ui,
            local_plan.is_some(),
            guidance.as_deref(),
            model.as_deref(),
            &attempted,
            &signature,
        ) {
            Ok(choice) => choice,
            Err(error) => {
                fix_line("◆", "Plan", error);
                break;
            }
        };
        match choice {
            RouteChoice::ConfigureAi => {
                if let Err(error) = configure_ai(&mut provider, &mut model) {
                    fix_line("×", "AI", error);
                }
                continue;
            }
            RouteChoice::Explain => {
                if let Some((rule, fix)) = &local {
                    fix_note("Local", format!("{rule} · {}", fix.rationale));
                    fix_note("Suggestion", action_text(&fix.action));
                }
                ui.emit(
                    "explanation",
                    serde_json::json!({
                        "message": diagnostic.message,
                        "local": local.as_ref().map(|(rule, fix)| serde_json::json!({
                            "rule": rule, "rationale": fix.rationale,
                            "suggestion": action_text(&fix.action)
                        }))
                    }),
                );
                if !interactive {
                    break;
                }
                continue;
            }
            RouteChoice::Stop => break,
            _ => {}
        }

        if choice == RouteChoice::Agent {
            let Some(model_name) = model.as_deref() else {
                fix_line("×", "Agent", "a model is required");
                break;
            };
            run_agent_route(
                &arguments,
                &root,
                &input_relative,
                &executable,
                format.as_deref(),
                artifact,
                tex82,
                workspace,
                current_build,
                &diagnostic,
                &provider,
                model_name,
                &mut session,
                &mut ui,
                started,
                dry_run,
                apply,
            );
            return;
        }

        let route_id = match choice {
            RouteChoice::Local => "local".to_string(),
            RouteChoice::Ai(profile) => format!("ai-{profile}"),
            _ => unreachable!(),
        };
        attempted.insert((signature.clone(), route_id.clone()));
        let attempt_id = format!("a-{}", session.attempts.len() + 1);
        let mut plan = match choice {
            RouteChoice::Local => local_plan.take().expect("local route has a plan"),
            RouteChoice::Ai(profile) => {
                let Some(model_name) = model.as_deref() else {
                    fix_line("×", "AI", "a model is required");
                    continue;
                };
                match model_proposal(
                    &arguments,
                    &mut ui,
                    &mut session,
                    workspace.root(),
                    &workspace_input,
                    &diagnostic,
                    local.as_ref().map(|(rule, fix)| (*rule, fix)),
                    &provider,
                    model_name,
                    profile,
                    std::time::Duration::from_secs(limits.max_elapsed_seconds)
                        .saturating_sub(started.elapsed()),
                ) {
                    Ok(plan) => plan,
                    Err((error, evidence)) => {
                        fix_line("×", "AI", &error);
                        let record = attempt_record(
                            attempt_id,
                            &diagnostic,
                            None,
                            evidence,
                            "rejected",
                            None,
                            None,
                            None,
                            None,
                            Some(false),
                            Some(error.clone()),
                        );
                        session.attempts.push(record);
                        session.status = "proposal-rejected".into();
                        let _ = save_session(&root, &mut session);
                        ui.emit("proposal-rejected", serde_json::json!({"reason": error}));
                        if !interactive {
                            break;
                        }
                        continue;
                    }
                }
            }
            _ => unreachable!(),
        };

        let (candidate, validation, safety, accepted) = 'review: loop {
            let safety = match aitex_harness::inspect_patch_safety(
                &plan.patch,
                &plan.before,
                &plan.after,
                limits.max_changed_lines,
                limits.max_deleted_lines,
            ) {
                Ok(safety) => safety,
                Err(error) => {
                    let message = error.to_string();
                    fix_line("×", "Safety", &message);
                    let record = attempt_record(
                        attempt_id,
                        &diagnostic,
                        Some(&plan),
                        FixEvidence::default(),
                        "rejected",
                        None,
                        None,
                        None,
                        None,
                        Some(false),
                        Some(message.clone()),
                    );
                    session.attempts.push(record);
                    session.status = "proposal-rejected".into();
                    let _ = save_session(&root, &mut session);
                    ui.emit("proposal-rejected", serde_json::json!({"reason": message}));
                    continue 'repair;
                }
            };
            let remaining = std::time::Duration::from_secs(limits.max_elapsed_seconds)
                .saturating_sub(started.elapsed());
            if remaining.is_zero() {
                fix_line("◆", "Budget", "repair time limit reached");
                break 'repair;
            }
            let mut candidate = match workspace.fork() {
                Ok(candidate) => candidate,
                Err(error) => {
                    fix_line("×", "Isolate", error);
                    break 'repair;
                }
            };
            candidate.set_timeout(
                aitex_harness::ValidationLimits::default()
                    .timeout
                    .min(remaining),
            );
            if let Err(error) = candidate.write(&plan.relative, &plan.after) {
                fix_line("×", "Patch", error);
                break 'repair;
            }
            fix_line(
                "✓",
                "Plan",
                format!("{} · {} confidence", plan.route, plan.confidence),
            );
            fix_note("Cause", &plan.rationale);
            fix_note("Patch", plan.relative.display());
            eprintln!("{}", plan.patch);
            let validation = match candidate.build(
                &executable,
                &input_relative,
                artifact,
                format.as_deref(),
                tex82,
            ) {
                Ok(build) => build,
                Err(error) => {
                    let message = error.to_string();
                    fix_line("×", "Validate", &message);
                    let record = attempt_record(
                        attempt_id,
                        &diagnostic,
                        Some(&plan),
                        FixEvidence::default(),
                        "validation-error",
                        None,
                        None,
                        None,
                        Some(safety),
                        Some(false),
                        Some(message.clone()),
                    );
                    session.attempts.push(record);
                    session.status = "proposal-rejected".into();
                    let _ = save_session(&root, &mut session);
                    ui.emit("proposal-rejected", serde_json::json!({"reason": message}));
                    continue 'repair;
                }
            };
            fix_build_phases(&ui, &validation);
            let next = validation.diagnostic.as_ref();
            let progressed = validation.succeeded
                || next.is_some_and(|next| next.signature() != diagnostic.signature());
            if !progressed {
                let reason = format!("current causal failure remains: {}", validation.detail);
                fix_line("×", "Validate", &reason);
                let record = attempt_record(
                    attempt_id,
                    &diagnostic,
                    Some(&plan),
                    FixEvidence::default(),
                    "rejected",
                    Some(validation.elapsed.as_secs_f64() * 1e3),
                    validation.artifact_bytes,
                    next,
                    Some(safety),
                    Some(false),
                    Some(reason.clone()),
                );
                session.attempts.push(record);
                session.status = "proposal-rejected".into();
                let _ = save_session(&root, &mut session);
                ui.emit("proposal-rejected", serde_json::json!({"reason": reason}));
                continue 'repair;
            }
            let validation_kind = if validation.succeeded {
                "complete"
            } else {
                "progress"
            };
            fix_line(
                "✓",
                "Validate",
                if validation.succeeded {
                    format!(
                        "complete isolated {} build · {} bytes · {}",
                        artifact.label(),
                        validation.artifact_bytes.unwrap_or(0),
                        human_duration(validation.elapsed)
                    )
                } else {
                    format!(
                        "current error removed · next: {} · {}",
                        next.map(|value| value.kind.name()).unwrap_or("unknown"),
                        human_duration(validation.elapsed)
                    )
                },
            );
            ui.emit(
                "proposal-validated",
                serde_json::json!({
                    "attempt": attempt_id,
                    "route": plan.route,
                    "target": plan.relative,
                    "patch": plan.patch,
                    "validation": validation_kind,
                    "validation_ms": validation.elapsed.as_secs_f64() * 1e3,
                    "artifact_bytes": validation.artifact_bytes,
                    "model_usage": {
                        "input_tokens": plan.evidence.usage.total_input(),
                        "output_tokens": plan.evidence.usage.output,
                        "reasoning_tokens": plan.evidence.usage.reasoning,
                        "cost_microusd": plan.evidence.metadata.charged_microusd,
                        "model_ms": plan.evidence.model_ms,
                        "provider": plan.evidence.metadata.provider,
                    },
                    "next_diagnostic": next.map(aitex_harness::DiagnosticRecord::from)
                }),
            );

            if dry_run {
                let record = attempt_record(
                    attempt_id.clone(),
                    &diagnostic,
                    Some(&plan),
                    FixEvidence::default(),
                    validation_kind,
                    Some(validation.elapsed.as_secs_f64() * 1e3),
                    validation.artifact_bytes,
                    next,
                    Some(safety),
                    None,
                    None,
                );
                session.attempts.push(record);
                session.status = if validation.succeeded {
                    "proposal-ready"
                } else {
                    "proposal-chain"
                }
                .into();
                let record_path = match save_session(&root, &mut session) {
                    Ok(path) => path,
                    Err(error) => {
                        fix_line("×", "Record", error);
                        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                };
                if !validation.succeeded {
                    ui.emit(
                        "decision-deferred",
                        serde_json::json!({
                            "attempt": attempt_id,
                            "reason": "repair chain is not complete",
                            "record": record_path
                        }),
                    );
                    current_build = validation.clone();
                    workspace = candidate;
                    diagnostic = next.expect("progress has a next diagnostic").clone();
                    continue 'repair;
                }
                fix_unchanged(format!(
                    "validated repair chain · accept with --session {session_id} --apply"
                ));
                fix_note("Record", record_path.display());
                ui.emit(
                    "decision-required",
                    serde_json::json!({
                        "attempt": attempt_id,
                        "attempts": session.attempts.iter().filter(|attempt| attempt.accepted.is_none()).map(|attempt| &attempt.id).collect::<Vec<_>>(),
                        "choices": ["accept", "reject", "retry"],
                        "record": record_path
                    }),
                );
                EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
                return;
            }

            let decision = if interactive {
                loop {
                    let answer = match prompt_line(
                        "  Accept this validated repair? [y/N/e to edit] ",
                    ) {
                        Ok(answer) => answer,
                        Err(error) => {
                            fix_line("×", "Decision", error);
                            break Some(false);
                        }
                    };
                    match answer.as_str() {
                        "y" | "Y" | "yes" | "YES" => break Some(true),
                        "" | "n" | "N" | "no" | "NO" => break Some(false),
                        "e" | "E" | "edit" | "EDIT" => break None,
                        _ => eprintln!("  Enter y, n, or e."),
                    }
                }
            } else {
                Some(apply)
            };
            if let Some(accepted) = decision {
                break 'review (candidate, validation, safety, accepted);
            }
            match edit_plan(&workspace, &mut plan) {
                Ok(()) => {
                    fix_line("✓", "Edit", "human revision staged; validating again");
                    ui.emit(
                        "proposal-edited",
                        serde_json::json!({
                            "attempt": attempt_id,
                            "target": plan.relative,
                            "patch": plan.patch
                        }),
                    );
                }
                Err(error) => fix_line("×", "Edit", error),
            }
        };
        let next = validation.diagnostic.as_ref();
        let validation_kind = if validation.succeeded {
            "complete"
        } else {
            "progress"
        };
        let record = attempt_record(
            attempt_id.clone(),
            &diagnostic,
            Some(&plan),
            FixEvidence::default(),
            validation_kind,
            Some(validation.elapsed.as_secs_f64() * 1e3),
            validation.artifact_bytes,
            next,
            Some(safety),
            Some(accepted),
            (!accepted).then(|| "user declined the proposal".into()),
        );
        session.attempts.push(record);
        session.status = if accepted {
            "proposal-accepted-private"
        } else {
            "proposal-declined"
        }
        .into();
        if let Err(error) = save_session(&root, &mut session) {
            fix_line("×", "Record", error);
            fix_unchanged("the accepted private state was not persisted");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if !accepted {
            ui.emit(
                "decision",
                serde_json::json!({"attempt": attempt_id, "accepted": false}),
            );
            if !interactive {
                break;
            }
            continue;
        }
        if !originals.contains_key(&plan.relative) {
            let original = match std::fs::read(root.join(&plan.relative)) {
                Ok(original) => original,
                Err(error) => {
                    fix_line("×", "Source", format!("cannot retain rollback bytes: {error}"));
                    break;
                }
            };
            originals.insert(plan.relative.clone(), original);
        }
        current_build = validation.clone();
        workspace = candidate;
        ui.emit(
            "decision",
            serde_json::json!({"attempt": attempt_id, "accepted": true}),
        );
        if validation.succeeded {
            complete = true;
            break;
        }
        diagnostic = next.expect("progress has a next diagnostic").clone();
    }

    if !complete {
        session.status = if started.elapsed()
            >= std::time::Duration::from_secs(limits.max_elapsed_seconds)
        {
            "elapsed-budget-exhausted"
        } else if session.attempts.len() >= limits.max_attempts as usize {
            "attempt-budget-exhausted"
        } else {
            "stopped"
        }
        .into();
        let record = save_session(&root, &mut session).ok();
        fix_unchanged("session ended before a complete validated build");
        ui.emit(
            "stopped",
            serde_json::json!({"status": session.status, "record": record}),
        );
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }

    if let Err(error) = inspect_transaction_safety(&workspace, &originals, &limits) {
        session.status = "proposal-rejected".into();
        let _ = save_session(&root, &mut session);
        fix_line("×", "Safety", error);
        fix_unchanged("complete chain exceeds the source safety limit");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    session.status = "validated".into();
    if let Err(error) = save_session(&root, &mut session) {
        fix_line("×", "Record", error);
        fix_unchanged("the transaction was not applied without an audit record");
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let backup = match apply_transaction(&root, &workspace, &originals, &session_id) {
        Ok(backup) => backup,
        Err(error) => {
            fix_line("×", "Apply", error);
            fix_unchanged("transaction refused");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    fix_line(
        "✓",
        "Applied",
        format!("{} source file(s)", originals.len()),
    );
    fix_note("Backup", backup.display());
    if rebuild_applied_project(
        &arguments,
        &root,
        &input_relative,
        &executable,
        format.as_deref(),
        tex82,
    )
    .is_err()
    {
        rollback_transaction(&root, &originals);
        session.status = "rolled-back".into();
        let _ = save_session(&root, &mut session);
        fix_line("×", "Rebuild", "real project build failed after validation");
        fix_note("Rollback", "author sources restored from the repair transaction");
        ui.emit(
            "rolled-back",
            serde_json::json!({"reason": "real project rebuild failed"}),
        );
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    session.status = "applied".into();
    session.final_build_succeeded = true;
    for relative in originals.keys() {
        let target = relative.to_string_lossy().into_owned();
        if !session.applied_targets.contains(&target) {
            session.applied_targets.push(target);
        }
    }
    let record = save_session(&root, &mut session).ok();
    fix_line(
        "✓",
        "Complete",
        format!("repaired and rebuilt · {}", human_duration(started.elapsed())),
    );
    if let Some(record) = &record {
        fix_note("Record", record.display());
    }
    ui.emit(
        "complete",
        serde_json::json!({
            "repaired": true,
            "targets": session.applied_targets,
            "record": record
        }),
    );
    EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod fix_tests {
    use super::{
        FixEvidence, FixPlan, FixUi, RouteChoice, choose_route, conventional_log_for,
        model_plan, rebuild_applied_project, repair_limits, save_session, session_path,
        valid_session_id,
    };
    use std::collections::BTreeSet;

    #[test]
    fn preview_plan_retains_a_machine_patch() {
        let plan = FixPlan {
            relative: "main.tex".into(),
            before: b"a\nb\n".to_vec(),
            after: b"a\nB\n".to_vec(),
            patch: "--- a/main.tex\n+++ b/main.tex\n@@ -2 +2 @@\n-b\n+B\n".into(),
            route: "local".into(),
            rationale: "test".into(),
            confidence: "certain".into(),
            evidence: FixEvidence::default(),
        };
        assert!(plan.patch.contains("-b\n+B"));
    }

    #[test]
    fn structured_model_plan_becomes_a_replayable_machine_patch() {
        let root = std::env::temp_dir().join(format!(
            "aitex-structured-plan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.tex"), b"before\\widgitize{x}").unwrap();
        let answer = aitex_harness::ModelAnswer {
            cause: "typo".into(),
            proposal: aitex_harness::ModelProposal::Edits(vec![aitex_harness::StructuredEdit {
                path: "main.tex".into(),
                old: "\\widgitize".into(),
                new: "\\widgetize".into(),
            }]),
            confidence: "high".into(),
            response_normalization: None,
        };
        let plan = model_plan(&root, &answer, "ai-compact", FixEvidence::default()).unwrap();
        assert_eq!(plan.after, b"before\\widgetize{x}");
        assert_eq!(
            aitex_harness::apply_unified_patch(&plan.patch, &plan.before).unwrap(),
            plan.after
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limits_and_record_paths_are_stable() {
        let args = vec![
            "fix".into(),
            "main.tex".into(),
            "--max-attempts".into(),
            "7".into(),
        ];
        assert_eq!(repair_limits(&args).max_attempts, 7);
        assert_eq!(
            session_path(std::path::Path::new("/project"), "r-1"),
            std::path::Path::new("/project/.aitex/repairs/r-1.json")
        );
        assert!(valid_session_id("r-abcd_1"));
        assert!(!valid_session_id("../escape"));
    }

    #[cfg(unix)]
    #[test]
    fn repair_records_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!(
            "aitex-private-repair-record-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut session = aitex_harness::RepairSession::new(
            "r-private".into(),
            1,
            "main.tex".into(),
            "digest".into(),
            "test".into(),
            Default::default(),
        );
        let path = save_session(&root, &mut session).unwrap();
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn applied_nested_projects_rebuild_from_the_document_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = std::env::temp_dir().join(format!(
            "aitex-nested-repair-rebuild-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = temporary.join("project/nested");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.tex"), b"source").unwrap();
        let observed = temporary.join("observed");
        let executable = temporary.join("fake-aitex");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\npwd > '{}'\nprintf '%s\\n' \"$@\" >> '{}'\n",
                observed.display(),
                observed.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();

        rebuild_applied_project(
            &["fix".into(), "nested/main.tex".into()],
            &root,
            std::path::Path::new("main.tex"),
            &executable,
            None,
            false,
        )
        .unwrap();
        let lines = std::fs::read_to_string(&observed).unwrap();
        assert_eq!(lines.lines().next(), Some(root.to_string_lossy().as_ref()));
        assert!(lines.lines().any(|line| line == "build"));
        assert!(lines.lines().any(|line| line == "main.tex"));
        assert!(!lines.lines().any(|line| line == "nested/main.tex"));
        std::fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn unattended_model_route_escalates_from_compact_to_expanded_once() {
        let arguments = vec!["fix".into(), "main.tex".into()];
        let mut ui = FixUi {
            json: false,
            interactive: false,
            sequence: 0,
            session: "r-test".into(),
        };
        assert_eq!(
            choose_route(
                &arguments,
                &mut ui,
                false,
                None,
                Some("model"),
                &BTreeSet::new(),
                "failure",
            )
            .unwrap(),
            RouteChoice::Agent
        );
        let attempted = BTreeSet::from([
            ("failure".into(), "ai-agent".into()),
            ("failure".into(), "ai-compact".into()),
        ]);
        assert_eq!(
            choose_route(
                &arguments,
                &mut ui,
                false,
                None,
                Some("model"),
                &attempted,
                "failure",
            )
            .unwrap(),
            RouteChoice::Ai(aitex_harness::ContextProfile::Expanded)
        );
    }

    #[test]
    fn conventional_log_uses_a_deterministic_lossy_tail() {
        let root = std::env::temp_dir().join(format!(
            "aitex-conventional-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut bytes = vec![b'x'; 5_000];
        bytes.extend_from_slice(b"\xff\n! selected tail\n");
        std::fs::write(root.join("a.log"), bytes).unwrap();
        std::fs::write(root.join("z.log"), "wrong log").unwrap();
        let diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Other,
            "fallback",
            aitex_core::Source::Engine,
        );

        let log = conventional_log_for(&root, &root.join("main.tex"), &diagnostic);
        assert!(log.ends_with("! selected tail\n"));
        assert!(log.contains('\u{fffd}'));
        assert!(log.chars().count() <= 4_000);
        assert!(!log.contains("wrong log"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
