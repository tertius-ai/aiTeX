//! Compact CLI for the controlled 0.3 repair experiment.

use aitex_harness::anthropic::Anthropic;
use aitex_harness::backend::{Backend, RecordingBackend, Response, ResponseMetadata, Role, Usage};
use aitex_harness::codex::Codex;
use aitex_harness::lab::{Corpus, PreparedCase, Route};
use aitex_harness::openrouter::{OpenRouter, OpenRouterOptions, RoutingPolicy};
use aitex_harness::repair::{
    ContextProfile, ModelAnswer, ModelProposal, REPAIR_PROTOCOL, RESPONSE_SCHEMA, run_model,
};
use aitex_harness::{ValidationArtifact, ValidationRequest, inspect_patch_safety, validate_edit};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SCORE_SCHEMA: &str = "aitex-repair-score-v1";
const MAX_REPLAY_RESPONSE_BYTES: u64 = 256 * 1024;
const PROFILES: [ContextProfile; 3] = [
    ContextProfile::Diagnostic,
    ContextProfile::Compact,
    ContextProfile::Expanded,
];
const USAGE: &str = "Usage: aitex-repair-lab [--profile diagnostic|compact|expanded|all] [--case ID] [--show-prompt ID]\n       aitex-repair-lab --score [--results FILE.jsonl] [--aitex PATH] [--profile ...] [--case ID]\n       aitex-repair-lab --replay-response FILE --case ID --profile PROFILE --score --results FILE.jsonl\n       aitex-repair-lab --provider anthropic|codex|openrouter --model MODEL --case ID --allow-source-upload [--score]\n       aitex-repair-lab --provider openrouter --model MODEL [--openrouter-provider SLUG] [--openrouter-ignore-provider SLUG] [--seed N] [--max-output-tokens N] --all-model-cases --profile all --allow-source-upload --score --results FILE.jsonl";

#[derive(Debug)]
struct Options {
    corpus: PathBuf,
    profiles: Vec<ContextProfile>,
    profile_label: String,
    case: Option<String>,
    all_model_cases: bool,
    show_prompt: Option<String>,
    replay_response: Option<PathBuf>,
    provider: String,
    model: Option<String>,
    allow_source_upload: bool,
    score: bool,
    results: Option<PathBuf>,
    aitex: Option<PathBuf>,
    openrouter_provider: Option<String>,
    openrouter_ignored_providers: Vec<String>,
    seed: Option<u64>,
    max_output_tokens: u32,
    max_prompt_price_per_million: Option<f64>,
    max_completion_price_per_million: Option<f64>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        corpus: PathBuf::from("corpus/repair-lab/manifest.json"),
        profiles: vec![ContextProfile::Compact],
        profile_label: "compact".into(),
        case: None,
        all_model_cases: false,
        show_prompt: None,
        replay_response: None,
        provider: "recording".into(),
        model: None,
        allow_source_upload: false,
        score: false,
        results: None,
        aitex: None,
        openrouter_provider: None,
        openrouter_ignored_providers: Vec::new(),
        seed: None,
        max_output_tokens: 4096,
        max_prompt_price_per_million: None,
        max_completion_price_per_million: None,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut at = 0;
    while at < args.len() {
        let flag = args[at].as_str();
        match flag {
            "--allow-source-upload" => options.allow_source_upload = true,
            "--all-model-cases" => options.all_model_cases = true,
            "--score" => options.score = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--corpus"
            | "--profile"
            | "--case"
            | "--show-prompt"
            | "--replay-response"
            | "--provider"
            | "--model"
            | "--results"
            | "--aitex"
            | "--openrouter-provider"
            | "--openrouter-ignore-provider"
            | "--seed"
            | "--max-output-tokens"
            | "--max-prompt-price"
            | "--max-completion-price" => {
                let value = args
                    .get(at + 1)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                match flag {
                    "--corpus" => options.corpus = PathBuf::from(value),
                    "--profile" if value == "all" => {
                        options.profiles = PROFILES.to_vec();
                        options.profile_label = "all".into();
                    }
                    "--profile" => {
                        let profile = ContextProfile::parse(value)
                            .ok_or_else(|| format!("unknown profile {value}"))?;
                        options.profiles = vec![profile];
                        options.profile_label = value.clone();
                    }
                    "--case" => options.case = Some(value.clone()),
                    "--show-prompt" => options.show_prompt = Some(value.clone()),
                    "--replay-response" => options.replay_response = Some(PathBuf::from(value)),
                    "--provider" => options.provider = value.clone(),
                    "--model" => options.model = Some(value.clone()),
                    "--results" => {
                        options.results = Some(PathBuf::from(value));
                        options.score = true;
                    }
                    "--aitex" => options.aitex = Some(PathBuf::from(value)),
                    "--openrouter-provider" => options.openrouter_provider = Some(value.clone()),
                    "--openrouter-ignore-provider" => {
                        options.openrouter_ignored_providers.push(value.clone())
                    }
                    "--seed" => {
                        options.seed = Some(
                            value
                                .parse()
                                .map_err(|_| "--seed must be an unsigned integer")?,
                        )
                    }
                    "--max-output-tokens" => {
                        options.max_output_tokens = value
                            .parse::<u32>()
                            .ok()
                            .filter(|tokens| *tokens > 0)
                            .ok_or("--max-output-tokens must be positive")?
                    }
                    "--max-prompt-price" => {
                        options.max_prompt_price_per_million = Some(
                            value
                                .parse::<f64>()
                                .ok()
                                .filter(|price| price.is_finite() && *price >= 0.0)
                                .ok_or("--max-prompt-price must be nonnegative")?,
                        )
                    }
                    "--max-completion-price" => {
                        options.max_completion_price_per_million = Some(
                            value
                                .parse::<f64>()
                                .ok()
                                .filter(|price| price.is_finite() && *price >= 0.0)
                                .ok_or("--max-completion-price must be nonnegative")?,
                        )
                    }
                    _ => unreachable!(),
                }
                at += 1;
            }
            unknown => return Err(format!("unknown argument {unknown}")),
        }
        at += 1;
    }
    if !matches!(
        options.provider.as_str(),
        "recording" | "anthropic" | "codex" | "openrouter"
    ) {
        return Err(format!("unknown provider {}", options.provider));
    }
    if options.case.is_some() && options.all_model_cases {
        return Err("use either --case or --all-model-cases, not both".into());
    }
    if (options.openrouter_provider.is_some() || !options.openrouter_ignored_providers.is_empty())
        && options.provider != "openrouter"
    {
        return Err("OpenRouter provider routing requires --provider openrouter".into());
    }
    if options.score && options.show_prompt.is_some() && options.results.is_none() {
        return Err("--show-prompt with stdout scoring requires --results FILE.jsonl".into());
    }
    if options.replay_response.is_some()
        && (options.provider != "recording"
            || options.case.is_none()
            || options.all_model_cases
            || options.profiles.len() != 1
            || !options.score
            || options.results.is_none())
    {
        return Err("--replay-response requires one recording-provider case and profile with --score --results".into());
    }
    if options.provider != "recording" {
        if options.model.is_none() {
            return Err("a live run requires --model MODEL".into());
        }
        if options.case.is_none() && !options.all_model_cases {
            return Err("a live run requires --case ID or --all-model-cases".into());
        }
        if !options.allow_source_upload {
            return Err("a live run requires --allow-source-upload".into());
        }
        if options.all_model_cases && (!options.score || options.results.is_none()) {
            return Err(
                "a live --all-model-cases run requires --score --results FILE.jsonl".into(),
            );
        }
    }
    Ok(options)
}

#[derive(Clone, Copy)]
struct Style {
    color: bool,
}

impl Style {
    fn paint(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.into()
        }
    }

    fn violet(self, text: &str) -> String {
        self.paint("1;35", text)
    }

    fn cyan(self, text: &str) -> String {
        self.paint("1;36", text)
    }

    fn green(self, text: &str) -> String {
        self.paint("1;32", text)
    }

    fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }
}

fn startup(style: Style, options: &Options) {
    eprintln!(
        "{} {}   {}",
        style.violet("▄▀█ █"),
        style.cyan("▀█▀ █▀▀ ▀▄▀"),
        style.dim(&format!(
            "aiTeX {} · repair lab · {}",
            env!("CARGO_PKG_VERSION"),
            options.profile_label
        ))
    );
    eprintln!(
        "{} {}   {}",
        style.violet("█▀█ █"),
        style.cyan(" █  ██▄ █ █"),
        style.dim(&format!(
            "{} · {}",
            options.provider,
            if options.score {
                "isolated scoring"
            } else {
                "read-only"
            }
        ))
    );
    eprintln!();
}

fn row(style: Style, label: &str, detail: impl AsRef<str>) {
    eprintln!(
        "  {} {:<10} {}",
        style.green("✓"),
        label,
        style.dim(detail.as_ref())
    );
}

fn score_progress(style: Style, record: &ScoreRecord) {
    let (mark, result) = if record.build_passed {
        (style.green("✓"), "build passed")
    } else {
        (
            "×".into(),
            record.error.as_deref().unwrap_or("build failed"),
        )
    };
    let normalization = record
        .patch_normalization
        .as_deref()
        .map(|value| format!(" · normalized {value}"))
        .unwrap_or_default();
    eprintln!(
        "  {mark} {:<10} {} · {} · {result}{normalization}",
        "Case", record.case_id, record.profile
    );
}

fn fatal(message: impl AsRef<str>) -> ! {
    eprintln!("  × {}", message.as_ref());
    std::process::exit(1);
}

fn median(values: &mut [usize]) -> usize {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

fn executable(options: &Options) -> Result<PathBuf, String> {
    let candidate = options
        .aitex
        .clone()
        .or_else(|| std::env::var_os("AITEX_BINARY").map(PathBuf::from))
        .or_else(|| {
            Path::new("bin/aitex")
                .is_file()
                .then(|| PathBuf::from("bin/aitex"))
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join("aitex")))
                .filter(|path| path.is_file())
        })
        .ok_or_else(|| "cannot find aiTeX; pass --aitex PATH or set AITEX_BINARY".to_string())?;
    std::fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "cannot resolve aiTeX executable {}: {error}",
            candidate.display()
        )
    })
}

#[derive(Debug)]
struct Job {
    profile: ContextProfile,
    case: PreparedCase,
}

#[derive(Debug, Serialize)]
struct ScoreRecord {
    schema: &'static str,
    protocol: &'static str,
    case_id: String,
    tier: String,
    surface: String,
    repair_intent: String,
    diagnostic_presentation: String,
    diagnostic_presentation_bytes: usize,
    estimated_presentation_tokens: usize,
    diagnostic_gap: &'static str,
    profile: String,
    route: &'static str,
    local_rule: Option<String>,
    locally_explained: bool,
    locally_mechanically_fixable: bool,
    provider: String,
    requested_model: Option<String>,
    requested_provider: Option<String>,
    request_seed: Option<u64>,
    max_output_tokens: u32,
    prompt_envelope: Option<serde_json::Value>,
    returned_model: Option<String>,
    served_provider: Option<String>,
    generation_id: Option<String>,
    service_tier: Option<String>,
    answer_cause: Option<String>,
    answer_confidence: Option<String>,
    proposal_format: Option<String>,
    structured_edits: Option<serde_json::Value>,
    response_normalization: Option<String>,
    patch: Option<String>,
    raw_response: Option<String>,
    normalized_patch: Option<String>,
    patch_normalization: Option<String>,
    estimated_input_tokens: usize,
    provider_counted_input_tokens: Option<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    cost_microusd: Option<u64>,
    upstream_microusd: Option<u64>,
    model_ms: f64,
    time_to_first_byte_ms: Option<f64>,
    provider_generation_ms: Option<f64>,
    router_metadata: Option<serde_json::Value>,
    response_received: bool,
    response_contract_valid: bool,
    proposal_nonempty: bool,
    patch_accepted: bool,
    safety_passed: bool,
    changed_lines: Option<usize>,
    deleted_lines: Option<usize>,
    target_matched: bool,
    build_passed: bool,
    oracle_matched: bool,
    validation_ms: Option<f64>,
    error: Option<String>,
}

impl ScoreRecord {
    fn new(options: &Options, job: &Job, route: &'static str) -> Self {
        let local = job.case.local.as_ref();
        let estimated = match &job.case.route {
            Route::Model(packet) => packet.estimated_input_tokens,
            Route::Deterministic { .. } => 0,
        };
        let prompt_envelope = match &job.case.route {
            Route::Model(packet) => Some(json!({
                "diagnostic_envelope": packet.envelope,
                "system": packet.request.system.iter().map(|block| json!({
                    "text": block.text,
                    "cache": block.cache,
                })).collect::<Vec<_>>(),
                "messages": packet.request.messages.iter().map(|message| json!({
                    "role": match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": message.content.iter().map(|block| json!({
                        "text": block.text,
                        "cache": block.cache,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "max_output_tokens": packet.request.max_tokens,
            })),
            Route::Deterministic { .. } => None,
        };
        let presentation = job.case.diagnostic.presentation().plain_text();
        Self {
            schema: SCORE_SCHEMA,
            protocol: REPAIR_PROTOCOL,
            case_id: job.case.id.clone(),
            tier: job.case.tier.clone(),
            surface: job.case.surface.clone(),
            repair_intent: job.case.repair.clone(),
            diagnostic_presentation_bytes: presentation.len(),
            estimated_presentation_tokens: presentation.len().div_ceil(4),
            diagnostic_presentation: presentation,
            diagnostic_gap: job.case.diagnostic.gap().bucket(),
            profile: job.profile.to_string(),
            route,
            local_rule: local.map(|finding| finding.rule.to_string()),
            locally_explained: local.is_some(),
            locally_mechanically_fixable: local.is_some_and(|finding| finding.mechanically_fixable),
            provider: options.provider.clone(),
            requested_model: options.model.clone(),
            requested_provider: options.openrouter_provider.clone(),
            request_seed: options.seed,
            max_output_tokens: options.max_output_tokens,
            prompt_envelope,
            returned_model: None,
            served_provider: None,
            generation_id: None,
            service_tier: None,
            answer_cause: None,
            answer_confidence: None,
            proposal_format: None,
            structured_edits: None,
            response_normalization: None,
            patch: None,
            raw_response: None,
            normalized_patch: None,
            patch_normalization: None,
            estimated_input_tokens: estimated,
            provider_counted_input_tokens: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cost_microusd: None,
            upstream_microusd: None,
            model_ms: 0.0,
            time_to_first_byte_ms: None,
            provider_generation_ms: None,
            router_metadata: None,
            response_received: false,
            response_contract_valid: false,
            proposal_nonempty: false,
            patch_accepted: false,
            safety_passed: false,
            changed_lines: None,
            deleted_lines: None,
            target_matched: false,
            build_passed: false,
            oracle_matched: false,
            validation_ms: None,
            error: None,
        }
    }
}

fn record_response_facts(
    record: &mut ScoreRecord,
    response: &Response,
    provider_counted_input_tokens: Option<u64>,
    elapsed: Duration,
) {
    record.response_received = true;
    record.returned_model = Some(response.model.clone());
    record.served_provider = response.metadata.provider.clone();
    record.generation_id = response.metadata.generation_id.clone();
    record.service_tier = response.metadata.service_tier.clone();
    record.provider_counted_input_tokens = provider_counted_input_tokens;
    record.input_tokens = response.usage.input;
    record.output_tokens = response.usage.output;
    record.cache_read_tokens = response.usage.cache_read;
    record.cache_write_tokens = response.usage.cache_write;
    record.reasoning_tokens = response.usage.reasoning;
    record.cost_microusd = response.metadata.charged_microusd;
    record.upstream_microusd = response.metadata.upstream_microusd;
    record.model_ms = elapsed.as_secs_f64() * 1e3;
    record.time_to_first_byte_ms = response.metadata.time_to_first_byte_ms;
    record.provider_generation_ms = response.metadata.provider_generation_ms;
    record.router_metadata = response.metadata.router_metadata.clone();
}

fn jobs(corpus: &Corpus, root: &Path, options: &Options) -> Result<Vec<Job>, String> {
    let selected: Vec<_> = corpus
        .cases
        .iter()
        .filter(|case| options.case.as_ref().is_none_or(|id| id == &case.id))
        .collect();
    if selected.is_empty() {
        return Err(format!(
            "case {} not found",
            options.case.as_deref().unwrap_or("")
        ));
    }
    let mut result = Vec::new();
    for case in selected {
        let first_profile = options.profiles[0];
        let mut first = case
            .prepare(root, first_profile)
            .map_err(|error| error.to_string())?;
        if let Route::Model(packet) = &mut first.route {
            packet.request.max_tokens = options.max_output_tokens;
        }
        if options.all_model_cases && !matches!(first.route, Route::Model(_)) {
            continue;
        }
        if options.profiles.len() == 1 || matches!(first.route, Route::Deterministic { .. }) {
            result.push(Job {
                profile: first_profile,
                case: first,
            });
        } else {
            for profile in &options.profiles {
                let mut prepared = case
                    .prepare(root, *profile)
                    .map_err(|error| error.to_string())?;
                if let Route::Model(packet) = &mut prepared.route {
                    packet.request.max_tokens = options.max_output_tokens;
                }
                result.push(Job {
                    profile: *profile,
                    case: prepared,
                });
            }
        }
    }
    if result.is_empty() {
        return Err("selection contains no model-routed cases".into());
    }
    Ok(result)
}

fn canned_responses(jobs: &[Job], options: &Options) -> Result<Vec<Response>, String> {
    if let Some(path) = &options.replay_response {
        if jobs.len() != 1 || !matches!(jobs[0].case.route, Route::Model(_)) {
            return Err("offline response replay requires exactly one model-routed case".into());
        }
        let file = std::fs::File::open(path)
            .map_err(|error| format!("cannot read replay response {}: {error}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_REPLAY_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read replay response {}: {error}", path.display()))?;
        if bytes.len() as u64 > MAX_REPLAY_RESPONSE_BYTES {
            return Err(format!(
                "replay response {} exceeds the {} byte limit",
                path.display(),
                MAX_REPLAY_RESPONSE_BYTES
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("replay response {} is not UTF-8", path.display()))?;
        return Ok(vec![Response {
            text,
            usage: Usage::default(),
            model: "offline-replay".into(),
            metadata: ResponseMetadata::default(),
        }]);
    }
    Ok(jobs
        .iter()
        .filter_map(|job| match job.case.route {
            Route::Model(_) => Some(Response {
                text: json!({
                    "schema": RESPONSE_SCHEMA,
                    "cause": job.case.repair,
                    "edits": job.case.oracle_edits(),
                    "confidence": "high",
                })
                .to_string(),
                usage: Usage::default(),
                model: "recording-oracle".into(),
                metadata: ResponseMetadata::default(),
            }),
            Route::Deterministic { .. } => None,
        })
        .collect())
}

fn score_answer(record: &mut ScoreRecord, job: &Job, answer: &ModelAnswer, executable: &Path) {
    record.response_contract_valid = true;
    record.answer_cause = Some(answer.cause.clone());
    record.answer_confidence = Some(answer.confidence.clone());
    record.proposal_format = Some(answer.proposal.format().into());
    record.proposal_nonempty = !answer.proposal.is_empty();
    record.response_normalization = answer.response_normalization.clone();
    if let ModelProposal::Edits(edits) = &answer.proposal {
        record.structured_edits = Some(json!(edits));
    }
    let target = match answer.proposal.target() {
        Ok(target) => target,
        Err(error) => {
            record.error = Some(error.to_string());
            return;
        }
    };
    if target != job.case.target {
        record.error = Some(format!(
            "patch targets {}, expected {}",
            target.display(),
            job.case.target.display()
        ));
        return;
    }
    record.target_matched = true;
    let prepared = match answer.proposal.prepare(&job.case.broken) {
        Ok(prepared) => prepared,
        Err(error) => {
            record.error = Some(error.to_string());
            return;
        }
    };
    record.patch = Some(prepared.patch.clone());
    record.patch_accepted = true;
    record.patch_normalization = prepared.normalization();
    if record.patch_normalization.is_some() {
        record.normalized_patch = Some(prepared.patch.clone());
    }
    let after = prepared.after;
    let safety = match inspect_patch_safety(&prepared.patch, &job.case.broken, &after, 200, 100) {
        Ok(safety) => safety,
        Err(error) => {
            record.error = Some(format!("unsafe patch: {error}"));
            return;
        }
    };
    record.safety_passed = true;
    record.changed_lines = Some(safety.added_lines + safety.deleted_lines);
    record.deleted_lines = Some(safety.deleted_lines);
    record.oracle_matched = after == job.case.fixed;
    match validate_edit(&ValidationRequest {
        executable,
        project_root: &job.case.project_root,
        input: &job.case.input,
        target: &job.case.target,
        after: &after,
        artifact: ValidationArtifact::Pdf,
        format: None,
        tex82: false,
    }) {
        Ok(validation) => {
            record.build_passed = true;
            record.validation_ms = Some(validation.elapsed.as_secs_f64() * 1e3);
        }
        Err(error) => record.error = Some(error.to_string()),
    }
}

fn score_patch(record: &mut ScoreRecord, job: &Job, patch: &str, executable: &Path) {
    let answer = ModelAnswer {
        cause: record.answer_cause.clone().unwrap_or_default(),
        proposal: ModelProposal::LegacyPatch(patch.into()),
        confidence: record
            .answer_confidence
            .clone()
            .unwrap_or_else(|| "high".into()),
        response_normalization: None,
    };
    score_answer(record, job, &answer, executable);
    record.response_contract_valid = false;
    record.proposal_format = Some("deterministic-unified-diff".into());
}

fn write_results(path: Option<&Path>, records: &[ScoreRecord]) -> Result<(), String> {
    let mut jsonl = String::new();
    for record in records {
        jsonl.push_str(&serde_json::to_string(record).map_err(|error| error.to_string())?);
        jsonl.push('\n');
    }
    if let Some(path) = path {
        std::fs::write(path, jsonl)
            .map_err(|error| format!("cannot write results {}: {error}", path.display()))
    } else {
        print!("{jsonl}");
        Ok(())
    }
}

fn main() {
    let options = parse_options().unwrap_or_else(|error| {
        eprintln!("aitex repair lab: {error}\n{USAGE}");
        std::process::exit(2);
    });
    let style = Style {
        color: std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    startup(style, &options);
    let started = Instant::now();
    let corpus = Corpus::load(&options.corpus).unwrap_or_else(|error| fatal(error.to_string()));
    let root = options.corpus.parent().unwrap_or_else(|| Path::new("."));
    let jobs = jobs(&corpus, root, &options).unwrap_or_else(|error| fatal(error));

    let mut unique = BTreeSet::new();
    let mut tools = BTreeSet::new();
    let mut understood = BTreeSet::new();
    let mut mechanical = BTreeSet::new();
    let mut model_cases = BTreeSet::new();
    for job in &jobs {
        unique.insert(job.case.id.clone());
        tools.insert(job.case.diagnostic.source.as_str());
        if job.case.local.is_some() {
            understood.insert(job.case.id.clone());
        }
        match job.case.route {
            Route::Deterministic { .. } => {
                mechanical.insert(job.case.id.clone());
            }
            Route::Model(_) => {
                model_cases.insert(job.case.id.clone());
            }
        }
    }
    let tool_text = tools
        .iter()
        .map(|tool| tool.to_string())
        .collect::<Vec<_>>()
        .join(" · ");
    row(
        style,
        "Corpus",
        format!(
            "{}/{} cases · {tool_text}",
            unique.len(),
            corpus.cases.len()
        ),
    );
    row(
        style,
        "Routing",
        format!(
            "{} understood · {} mechanical · {} model · 0 source files changed",
            understood.len(),
            mechanical.len(),
            model_cases.len()
        ),
    );

    let canned = canned_responses(&jobs, &options).unwrap_or_else(|error| fatal(error));
    let mut backend: Box<dyn Backend> = match options.provider.as_str() {
        "anthropic" => Box::new(
            Anthropic::from_env(options.model.as_deref().expect("validated model"))
                .unwrap_or_else(|error| fatal(error.to_string())),
        ),
        "codex" => Box::new(
            Codex::from_env(options.model.as_deref().expect("validated model"))
                .unwrap_or_else(|error| fatal(error.to_string())),
        ),
        "openrouter" => Box::new(
            OpenRouter::from_env_with_options(
                options.model.as_deref().expect("validated model"),
                OpenRouterOptions {
                    routing: options
                        .openrouter_provider
                        .as_ref()
                        .map_or(RoutingPolicy::Routed, |provider| {
                            RoutingPolicy::Pinned(provider.clone())
                        }),
                    ignored_providers: options.openrouter_ignored_providers.clone(),
                    seed: options.seed,
                    max_prompt_price_per_million: options.max_prompt_price_per_million,
                    max_completion_price_per_million: options.max_completion_price_per_million,
                    ..OpenRouterOptions::default()
                },
            )
            .unwrap_or_else(|error| fatal(error.to_string())),
        ),
        "recording" => Box::new(RecordingBackend::new(canned)),
        _ => unreachable!("provider was validated"),
    };
    let validator = options
        .score
        .then(|| executable(&options).unwrap_or_else(|error| fatal(error)));

    let mut token_estimates = Vec::new();
    let mut calls = 0usize;
    let mut fresh_input = 0u64;
    let mut cached_input = 0u64;
    let mut cache_writes = 0u64;
    let mut output = 0u64;
    let mut reasoning = 0u64;
    let mut charged = 0u64;
    let mut counted_input = 0u64;
    let mut model_time = Duration::ZERO;
    let mut returned_models = BTreeSet::new();
    let mut records = Vec::new();
    let mut last_live: Option<(String, String, String)> = None;
    let mut shown_prompt = None;

    for job in &jobs {
        let (route, patch) = match &job.case.route {
            Route::Deterministic { patch, .. } => ("deterministic", Some(patch.clone())),
            Route::Model(packet) => {
                token_estimates.push(packet.estimated_input_tokens);
                if options.show_prompt.as_ref() == Some(&job.case.id) && shown_prompt.is_none() {
                    shown_prompt = Some(packet.request.messages[0].content[0].text.clone());
                }
                match run_model(backend.as_mut(), packet) {
                    Ok(run) => {
                        calls += 1;
                        fresh_input += run.response.usage.input;
                        cached_input += run.response.usage.cache_read;
                        cache_writes += run.response.usage.cache_write;
                        output += run.response.usage.output;
                        reasoning += run.response.usage.reasoning;
                        charged += run.response.metadata.charged_microusd.unwrap_or(0);
                        counted_input += run.provider_counted_input_tokens.unwrap_or(0);
                        model_time += run.elapsed;
                        returned_models.insert(run.response.model.clone());
                        if options.provider != "recording" {
                            last_live = Some((
                                run.answer.confidence.clone(),
                                run.answer.cause.clone(),
                                run.answer.proposal.preview(),
                            ));
                        }
                        if options.score {
                            let mut record = ScoreRecord::new(&options, job, "model");
                            record_response_facts(
                                &mut record,
                                &run.response,
                                run.provider_counted_input_tokens,
                                run.elapsed,
                            );
                            record.raw_response = Some(run.response.text.clone());
                            score_answer(
                                &mut record,
                                job,
                                &run.answer,
                                validator.as_deref().expect("score has validator"),
                            );
                            score_progress(style, &record);
                            records.push(record);
                        }
                        ("model", None)
                    }
                    Err(error) if options.score => {
                        let mut record = ScoreRecord::new(&options, job, "model");
                        if let aitex_harness::ModelRunError::Response {
                            response,
                            provider_counted_input_tokens,
                            elapsed,
                            ..
                        } = &error
                        {
                            calls += 1;
                            fresh_input += response.usage.input;
                            cached_input += response.usage.cache_read;
                            cache_writes += response.usage.cache_write;
                            output += response.usage.output;
                            reasoning += response.usage.reasoning;
                            charged += response.metadata.charged_microusd.unwrap_or(0);
                            model_time += *elapsed;
                            returned_models.insert(response.model.clone());
                            record_response_facts(
                                &mut record,
                                response,
                                *provider_counted_input_tokens,
                                *elapsed,
                            );
                            record.raw_response = Some(response.text.clone());
                        }
                        record.error = Some(error.to_string());
                        score_progress(style, &record);
                        records.push(record);
                        ("model", None)
                    }
                    Err(error) => fatal(format!("{}: {error}", job.case.id)),
                }
            }
        };
        if options.score && route == "deterministic" {
            let mut record = ScoreRecord::new(&options, job, route);
            if let Route::Deterministic { fix, patch, .. } = &job.case.route {
                record.answer_cause = Some(fix.rationale.clone());
                record.answer_confidence = Some(format!("{:?}", fix.confidence).to_lowercase());
                record.patch = Some(patch.clone());
            }
            score_patch(
                &mut record,
                job,
                patch.as_deref().expect("deterministic route has patch"),
                validator.as_deref().expect("score has validator"),
            );
            score_progress(style, &record);
            records.push(record);
        }
    }

    let maximum = token_estimates.iter().copied().max().unwrap_or(0);
    let middle = median(&mut token_estimates);
    row(
        style,
        "Context",
        format!(
            "{} · median {middle} estimated tokens · max {maximum}",
            options.profile_label
        ),
    );
    row(
        style,
        "AI",
        if options.provider == "recording" {
            format!("{calls} oracle replays · no network · no model-quality claim")
        } else {
            let returned = returned_models
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",");
            let counted = if counted_input == 0 {
                "preflight unavailable".to_string()
            } else {
                format!("{counted_input} preflight-counted")
            };
            format!(
                "{calls} calls · {returned} · {counted} · {fresh_input} fresh + {cached_input} cached + {cache_writes} cache-write / {output} output ({reasoning} reasoning) · ${:.6} · {:.2} s",
                charged as f64 / 1_000_000.0,
                model_time.as_secs_f64(),
            )
        },
    );
    if options.score {
        let builds = records.iter().filter(|record| record.build_passed).count();
        let oracles = records
            .iter()
            .filter(|record| record.oracle_matched)
            .count();
        row(
            style,
            "Score",
            format!(
                "{builds}/{} build · {oracles}/{} exact oracle",
                records.len(),
                records.len()
            ),
        );
        let model = records
            .iter()
            .filter(|record| record.route == "model")
            .collect::<Vec<_>>();
        if !model.is_empty() {
            let count = |predicate: fn(&ScoreRecord) -> bool| {
                model.iter().filter(|record| predicate(record)).count()
            };
            row(
                style,
                "Funnel",
                format!(
                    "{}/{} response · {} contract · {} nonempty · {} target · {} grounded · {} safe · {} build · {} oracle",
                    count(|record| record.response_received),
                    model.len(),
                    count(|record| record.response_contract_valid),
                    count(|record| record.proposal_nonempty),
                    count(|record| record.target_matched),
                    count(|record| record.patch_accepted),
                    count(|record| record.safety_passed),
                    count(|record| record.build_passed),
                    count(|record| record.oracle_matched),
                ),
            );
            let mut tiers: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
            for record in &records {
                let counts = tiers.entry(record.tier.as_str()).or_default();
                counts.0 += 1;
                counts.1 += usize::from(record.build_passed);
                counts.2 += usize::from(record.oracle_matched);
            }
            row(
                style,
                "Tiers",
                tiers
                    .into_iter()
                    .map(|(tier, (total, builds, oracles))| {
                        format!("{tier} {builds}/{total} build, {oracles}/{total} oracle")
                    })
                    .collect::<Vec<_>>()
                    .join(" · "),
            );
        }
        write_results(options.results.as_deref(), &records).unwrap_or_else(|error| fatal(error));
        if let Some(path) = &options.results {
            row(style, "Results", path.display().to_string());
        }
    }
    row(
        style,
        "Ready",
        format!(
            "{} · score schema v1 · {:.1} ms",
            REPAIR_PROTOCOL,
            started.elapsed().as_secs_f64() * 1e3
        ),
    );

    if let Some((confidence, cause, proposal)) = last_live {
        eprintln!("\n  {} {cause}", style.cyan("Cause"));
        eprintln!("  {} {confidence}", style.cyan("Confidence"));
        if !proposal.is_empty() {
            eprintln!("  {}\n{proposal}", style.cyan("Edit preview"));
        }
    }
    if let Some(prompt) = shown_prompt {
        println!("{prompt}");
    }
    if options.score && records.iter().any(|record| !record.build_passed) {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_harness::parse_answer;

    fn matrix_options() -> Options {
        Options {
            corpus: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus/repair-lab/manifest.json"),
            profiles: PROFILES.to_vec(),
            profile_label: "all".into(),
            case: None,
            all_model_cases: false,
            show_prompt: None,
            replay_response: None,
            provider: "recording".into(),
            model: None,
            allow_source_upload: false,
            score: true,
            results: None,
            aitex: None,
            openrouter_provider: None,
            openrouter_ignored_providers: Vec::new(),
            seed: None,
            max_output_tokens: 4096,
            max_prompt_price_per_million: None,
            max_completion_price_per_million: None,
        }
    }

    #[test]
    fn all_profiles_mean_eighteen_model_calls_and_two_local_edits() {
        let options = matrix_options();
        let corpus = Corpus::load(&options.corpus).expect("load corpus");
        let root = options.corpus.parent().expect("manifest parent");
        let jobs = jobs(&corpus, root, &options).expect("prepare matrix");
        assert_eq!(jobs.len(), 20);
        assert_eq!(
            jobs.iter()
                .filter(|job| matches!(job.case.route, Route::Model(_)))
                .count(),
            18
        );
        assert_eq!(canned_responses(&jobs, &options).unwrap().len(), 18);
    }

    #[test]
    fn recording_replies_are_real_strict_oracle_patches() {
        let options = matrix_options();
        let corpus = Corpus::load(&options.corpus).expect("load corpus");
        let root = options.corpus.parent().expect("manifest parent");
        let jobs = jobs(&corpus, root, &options).expect("prepare matrix");
        let model_jobs = jobs
            .iter()
            .filter(|job| matches!(job.case.route, Route::Model(_)));
        for (job, response) in model_jobs.zip(canned_responses(&jobs, &options).unwrap()) {
            let answer = parse_answer(&response.text).expect("valid response contract");
            assert_eq!(answer.proposal.target().unwrap(), job.case.target);
            assert_eq!(
                answer.proposal.prepare(&job.case.broken).unwrap().after,
                job.case.fixed
            );
        }
    }
}
