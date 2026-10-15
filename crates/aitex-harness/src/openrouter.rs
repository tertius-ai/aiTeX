//! OpenRouter backend and reproducible routing controls.
//!
//! The ordinary product path defaults to privacy-filtered routing. Benchmark
//! campaigns may instead pin one provider slug and disable fallbacks. Both
//! paths retain native usage, charged cost, resolved model/provider identity,
//! and timing metadata rather than estimating them after the fact.

use crate::backend::{Backend, BackendError, Request, Response, ResponseMetadata, Role, Usage};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const API_ROOT: &str = "https://openrouter.ai/api/v1";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODELS_BYTES: usize = 32 * 1024 * 1024;
const MAX_CREDENTIAL_FILE_BYTES: u64 = 64 * 1024;
const OPENROUTER_API_KEY: &str = "OPENROUTER_API_KEY";

fn openrouter_config_path() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Some(directory.join("aitex/env"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|directory| directory.join(".config/aitex/env"))
}

fn parse_secret_value(value: &str, line_number: usize) -> Result<String, BackendError> {
    let value = value.trim();
    let unquoted = match (value.as_bytes().first(), value.as_bytes().last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) if value.len() >= 2 => {
            &value[1..value.len() - 1]
        }
        (Some(b'\'' | b'"'), _) | (_, Some(b'\'' | b'"')) => {
            return Err(BackendError::Transport(format!(
                "OpenRouter credential file line {line_number} has unmatched quotes"
            )));
        }
        _ => value,
    };
    if unquoted.is_empty() {
        return Err(BackendError::Transport(format!(
            "OpenRouter credential file line {line_number} has an empty key"
        )));
    }
    if unquoted.chars().any(char::is_whitespace) {
        return Err(BackendError::Transport(format!(
            "OpenRouter credential file line {line_number} has whitespace inside the key"
        )));
    }
    Ok(unquoted.to_string())
}

fn parse_openrouter_env(contents: &str) -> Result<String, BackendError> {
    let mut key = None;
    for (index, source_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let mut line = source_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export")
            && rest.starts_with(char::is_whitespace)
        {
            line = rest.trim_start();
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != OPENROUTER_API_KEY {
            continue;
        }
        if key.is_some() {
            return Err(BackendError::Transport(format!(
                "OpenRouter credential file defines {OPENROUTER_API_KEY} more than once"
            )));
        }
        key = Some(parse_secret_value(value, line_number)?);
    }
    key.ok_or_else(|| {
        BackendError::Transport(format!(
            "OpenRouter credential file does not define {OPENROUTER_API_KEY}"
        ))
    })
}

fn read_openrouter_config(path: &Path) -> Result<String, BackendError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        BackendError::Transport(format!(
            "cannot read OpenRouter credential file {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        BackendError::Transport(format!(
            "cannot inspect OpenRouter credential file {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(BackendError::Transport(format!(
            "OpenRouter credential path is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(BackendError::Transport(format!(
            "OpenRouter credential file exceeds {MAX_CREDENTIAL_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(BackendError::Transport(format!(
                "refusing OpenRouter credential file {} with permissions {mode:04o}; run chmod 600 '{}'",
                path.display(),
                path.display()
            )));
        }
    }
    let mut contents = Vec::new();
    file.by_ref()
        .take(MAX_CREDENTIAL_FILE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| {
            BackendError::Transport(format!(
                "cannot read OpenRouter credential file {}: {error}",
                path.display()
            ))
        })?;
    if contents.len() as u64 > MAX_CREDENTIAL_FILE_BYTES {
        return Err(BackendError::Transport(format!(
            "OpenRouter credential file exceeds {MAX_CREDENTIAL_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    let contents = String::from_utf8(contents).map_err(|_| {
        BackendError::Transport(format!(
            "OpenRouter credential file is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    parse_openrouter_env(&contents)
}

fn load_openrouter_api_key() -> Result<String, BackendError> {
    match std::env::var(OPENROUTER_API_KEY) {
        Ok(key) => return Ok(key),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(BackendError::Transport(format!(
                "{OPENROUTER_API_KEY} is not valid UTF-8"
            )));
        }
        Err(std::env::VarError::NotPresent) => {}
    }
    let Some(path) = openrouter_config_path() else {
        return Err(BackendError::Transport(format!(
            "{OPENROUTER_API_KEY} is not set and no user configuration directory is available"
        )));
    };
    read_openrouter_config(&path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingPolicy {
    /// OpenRouter's availability/price-aware production routing.
    Routed,
    /// One provider slug, with no fallback.
    Pinned(String),
}

impl RoutingPolicy {
    pub fn label(&self) -> String {
        match self {
            Self::Routed => "routed".into(),
            Self::Pinned(provider) => format!("pinned:{provider}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenRouterOptions {
    pub routing: RoutingPolicy,
    pub ignored_providers: Vec<String>,
    pub seed: Option<u64>,
    pub timeout: Duration,
    pub zero_data_retention: bool,
    /// Maximum accepted endpoint prices in dollars per million tokens. A
    /// campaign sets these from its frozen catalog snapshot so routing cannot
    /// silently choose a more expensive endpoint than its reservation.
    pub max_prompt_price_per_million: Option<f64>,
    pub max_completion_price_per_million: Option<f64>,
}

impl Default for OpenRouterOptions {
    fn default() -> Self {
        Self {
            routing: RoutingPolicy::Routed,
            ignored_providers: Vec::new(),
            seed: None,
            timeout: Duration::from_secs(120),
            zero_data_retention: true,
            max_prompt_price_per_million: None,
            max_completion_price_per_million: None,
        }
    }
}

pub struct OpenRouter {
    api_key: String,
    pub model: String,
    options: OpenRouterOptions,
    client: reqwest::blocking::Client,
}

impl OpenRouter {
    pub fn from_env(model: &str) -> Result<Self, BackendError> {
        Self::from_env_with_options(model, OpenRouterOptions::default())
    }

    pub fn from_env_with_options(
        model: &str,
        options: OpenRouterOptions,
    ) -> Result<Self, BackendError> {
        let api_key = load_openrouter_api_key()?;
        Self::new(api_key, model, options)
    }

    pub fn new(
        api_key: impl Into<String>,
        model: &str,
        options: OpenRouterOptions,
    ) -> Result<Self, BackendError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(BackendError::Transport(
                "the OpenRouter API key is empty".into(),
            ));
        }
        if model.trim().is_empty() {
            return Err(BackendError::Transport(
                "the OpenRouter model name is empty".into(),
            ));
        }
        if let RoutingPolicy::Pinned(provider) = &options.routing
            && provider.trim().is_empty()
        {
            return Err(BackendError::Transport(
                "the pinned OpenRouter provider is empty".into(),
            ));
        }
        if options
            .ignored_providers
            .iter()
            .any(|provider| provider.trim().is_empty())
        {
            return Err(BackendError::Transport(
                "an ignored OpenRouter provider is empty".into(),
            ));
        }
        if [
            options.max_prompt_price_per_million,
            options.max_completion_price_per_million,
        ]
        .into_iter()
        .flatten()
        .any(|price| !price.is_finite() || price < 0.0)
        {
            return Err(BackendError::Transport(
                "an OpenRouter price ceiling is invalid".into(),
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(options.timeout)
            .build()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        Ok(Self {
            api_key,
            model: model.into(),
            options,
            client,
        })
    }

    fn body(&self, request: &Request) -> Value {
        let joined = |blocks: &[crate::backend::Block]| {
            blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut messages = Vec::new();
        if !request.system.is_empty() {
            messages.push(json!({"role": "system", "content": joined(&request.system)}));
        }
        messages.extend(request.messages.iter().map(|message| {
            json!({
                "role": match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
                "content": joined(&message.content)
            })
        }));
        let mut provider = json!({
            "require_parameters": true,
            "data_collection": "deny",
            "zdr": self.options.zero_data_retention,
        });
        if let RoutingPolicy::Pinned(provider_slug) = &self.options.routing {
            provider["order"] = json!([provider_slug]);
            provider["allow_fallbacks"] = json!(false);
        }
        if !self.options.ignored_providers.is_empty() {
            provider["ignore"] = json!(self.options.ignored_providers);
        }
        if self.options.max_prompt_price_per_million.is_some()
            || self.options.max_completion_price_per_million.is_some()
        {
            provider["max_price"] = json!({
                "prompt": self.options.max_prompt_price_per_million,
                "completion": self.options.max_completion_price_per_million,
            });
        }
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            // OpenRouter's catalog advertises endpoint support under the
            // common `max_tokens` spelling. Using that spelling keeps
            // `require_parameters` from excluding otherwise compatible
            // providers, including Gemini endpoints.
            "max_tokens": request.max_tokens,
            "temperature": 0,
            "stream": false,
            "provider": provider
        });
        if let Some(response_schema) = &request.response_schema {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": response_schema.name,
                    "strict": true,
                    "schema": response_schema.schema
                }
            });
        }
        if let Some(seed) = self.options.seed {
            body["seed"] = json!(seed);
        }
        body
    }

    fn chat_request(&self, request: &Request) -> reqwest::blocking::RequestBuilder {
        self.client
            .post(format!("{API_ROOT}/chat/completions"))
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .header("HTTP-Referer", "https://github.com/tertius-ai/aiTeX")
            .header("X-OpenRouter-Title", "aiTeX")
            .header("X-OpenRouter-Metadata", "enabled")
            .header("X-OpenRouter-Cache", "false")
            .json(&self.body(request))
    }

    fn read_response(
        mut response: reqwest::blocking::Response,
        limit: usize,
    ) -> Result<(reqwest::StatusCode, Value), BackendError> {
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        if bytes.len() > limit {
            return Err(BackendError::Malformed(format!(
                "OpenRouter response exceeds {limit} bytes"
            )));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            BackendError::Malformed(format!("OpenRouter response is not valid JSON: {error}"))
        })?;
        Ok((status, value))
    }

    fn api_error(status: reqwest::StatusCode, value: &Value) -> BackendError {
        let message = value["error"]["message"]
            .as_str()
            .or_else(|| value["message"].as_str())
            .unwrap_or("OpenRouter request failed")
            .to_string();
        BackendError::Api {
            status: status.as_u16(),
            message,
        }
    }

    /// Snapshot the exact catalog used to select and price a campaign.
    pub fn models_snapshot(&self) -> Result<Value, BackendError> {
        let response = self
            .client
            .get(format!("{API_ROOT}/models"))
            .bearer_auth(&self.api_key)
            .send()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        let (status, value) = Self::read_response(response, MAX_MODELS_BYTES)?;
        if !status.is_success() {
            return Err(Self::api_error(status, &value));
        }
        if !value["data"].is_array() {
            return Err(BackendError::Malformed(
                "OpenRouter models response has no data array".into(),
            ));
        }
        Ok(value)
    }
}

fn dollars_to_microusd(value: &Value) -> Option<u64> {
    let dollars = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))?;
    if !dollars.is_finite() || dollars < 0.0 || dollars > u64::MAX as f64 / 1_000_000.0 {
        return None;
    }
    Some((dollars * 1_000_000.0).round() as u64)
}

fn selected_provider(value: &Value) -> Option<String> {
    value["openrouter_metadata"]["endpoints"]["available"]
        .as_array()?
        .iter()
        .find(|endpoint| endpoint["selected"].as_bool() == Some(true))?["provider"]
        .as_str()
        .map(str::to_string)
}

fn parse_response(
    value: &Value,
    requested_model: &str,
    routing: &RoutingPolicy,
    ttfb: Duration,
) -> Result<Response, BackendError> {
    let text = value["choices"][0]["message"]["content"]
        .as_str()
        // A successful response can contain a null/absent content field when
        // a model spends its allowance on reasoning or otherwise emits no
        // answer. Preserve the response as an empty, contract-invalid answer
        // so repair scoring retains its authoritative usage, cost, route, and
        // timing instead of turning it into an unaccounted backend failure.
        .unwrap_or_default()
        .to_string();
    let prompt = value["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let cached = value["usage"]["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0)
        .min(prompt);
    let cache_write = value["usage"]["prompt_tokens_details"]["cache_write_tokens"]
        .as_u64()
        .unwrap_or(0)
        .min(prompt.saturating_sub(cached));
    let provider =
        selected_provider(value).or_else(|| value["provider"].as_str().map(str::to_string));
    let requested_provider = match routing {
        RoutingPolicy::Pinned(provider) => Some(provider.clone()),
        RoutingPolicy::Routed => None,
    };
    Ok(Response {
        text,
        usage: Usage {
            input: prompt.saturating_sub(cached).saturating_sub(cache_write),
            output: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            cache_read: cached,
            cache_write,
            reasoning: value["usage"]["completion_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
        },
        model: value["model"]
            .as_str()
            .unwrap_or(requested_model)
            .to_string(),
        metadata: ResponseMetadata {
            provider,
            requested_provider,
            generation_id: value["id"].as_str().map(str::to_string),
            service_tier: value["service_tier"].as_str().map(str::to_string),
            charged_microusd: dollars_to_microusd(&value["usage"]["cost"]),
            upstream_microusd: dollars_to_microusd(
                &value["usage"]["cost_details"]["upstream_inference_cost"],
            ),
            time_to_first_byte_ms: Some(ttfb.as_secs_f64() * 1e3),
            provider_generation_ms: value["openrouter_metadata"]["generation_time"].as_f64(),
            routing: value["openrouter_metadata"]["strategy"]
                .as_str()
                .map(|strategy| {
                    value["openrouter_metadata"]["attempt"]
                        .as_u64()
                        .map_or_else(
                            || strategy.to_string(),
                            |attempt| format!("{strategy}:attempt-{attempt}"),
                        )
                })
                .or_else(|| Some(routing.label())),
            router_metadata: value["openrouter_metadata"]
                .as_object()
                .map(|_| value["openrouter_metadata"].clone()),
        },
    })
}

impl Backend for OpenRouter {
    fn complete(&mut self, request: &Request) -> Result<Response, BackendError> {
        let started = Instant::now();
        let response = self
            .chat_request(request)
            .send()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        let ttfb = started.elapsed();
        let (status, value) = Self::read_response(response, MAX_RESPONSE_BYTES)?;
        if !status.is_success() {
            return Err(Self::api_error(status, &value));
        }
        parse_response(&value, &self.model, &self.options.routing, ttfb)
    }

    fn name(&self) -> &str {
        "openrouter"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Block, Message};

    fn request() -> Request {
        Request::new(vec![Block::new("rules")], vec![Message::user("broken")])
            .with_response_schema("aitex_repair", crate::repair::repair_response_schema())
    }

    #[test]
    fn pinned_body_disables_fallbacks_and_requires_privacy() {
        let backend = OpenRouter::new(
            "secret",
            "vendor/model",
            OpenRouterOptions {
                routing: RoutingPolicy::Pinned("provider/region".into()),
                ignored_providers: vec!["broken-provider".into()],
                seed: Some(42),
                ..OpenRouterOptions::default()
            },
        )
        .unwrap();
        let body = backend.body(&request());
        assert_eq!(body["provider"]["order"], json!(["provider/region"]));
        assert_eq!(body["provider"]["allow_fallbacks"], false);
        assert_eq!(body["provider"]["ignore"], json!(["broken-provider"]));
        assert_eq!(body["provider"]["data_collection"], "deny");
        assert_eq!(body["provider"]["zdr"], true);
        assert_eq!(body["provider"]["require_parameters"], true);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["seed"], 42);
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["response_format"]["type"], "json_schema");
        let request = backend.chat_request(&request()).build().unwrap();
        assert_eq!(request.headers()["X-OpenRouter-Metadata"], "enabled");
        assert_eq!(request.headers()["X-OpenRouter-Cache"], "false");
    }

    #[test]
    fn native_usage_cost_route_and_speed_are_preserved() {
        let value = json!({
            "id": "gen-1",
            "model": "vendor/model-2026",
            "choices": [{"message": {"content": "{}"}}],
            "service_tier": "default",
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 25,
                "prompt_tokens_details": {"cached_tokens": 40, "cache_write_tokens": 10},
                "completion_tokens_details": {"reasoning_tokens": 7},
                "cost": 0.000123,
                "cost_details": {"upstream_inference_cost": 0.0001}
            },
            "openrouter_metadata": {
                "generation_time": 321.5,
                "strategy": "direct",
                "endpoints": {"available": [
                    {"provider": "Provider A", "selected": true}
                ]}
            }
        });
        let response = parse_response(
            &value,
            "vendor/model",
            &RoutingPolicy::Routed,
            Duration::from_millis(12),
        )
        .unwrap();
        assert_eq!(response.model, "vendor/model-2026");
        assert_eq!(response.usage.input, 50);
        assert_eq!(response.usage.cache_read, 40);
        assert_eq!(response.usage.cache_write, 10);
        assert_eq!(response.usage.output, 25);
        assert_eq!(response.usage.reasoning, 7);
        assert_eq!(response.metadata.charged_microusd, Some(123));
        assert_eq!(response.metadata.upstream_microusd, Some(100));
        assert_eq!(response.metadata.provider.as_deref(), Some("Provider A"));
        assert_eq!(response.metadata.generation_id.as_deref(), Some("gen-1"));
        assert_eq!(response.metadata.time_to_first_byte_ms, Some(12.0));
        assert_eq!(response.metadata.provider_generation_ms, Some(321.5));
        assert_eq!(response.metadata.routing.as_deref(), Some("direct"));
        assert_eq!(
            response.metadata.router_metadata.as_ref().unwrap()["strategy"],
            "direct"
        );
    }

    #[test]
    fn an_absent_text_choice_still_preserves_accounting() {
        let value = json!({
            "id": "gen-empty",
            "model": "vendor/model-2026",
            "choices": [{"message": {"content": null}}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 25,
                "completion_tokens_details": {"reasoning_tokens": 25},
                "cost": 0.000123
            }
        });
        let response = parse_response(
            &value,
            "vendor/model",
            &RoutingPolicy::Routed,
            Duration::from_millis(12),
        )
        .unwrap();
        assert_eq!(response.text, "");
        assert_eq!(response.usage.input, 100);
        assert_eq!(response.usage.output, 25);
        assert_eq!(response.usage.reasoning, 25);
        assert_eq!(response.metadata.charged_microusd, Some(123));
        assert_eq!(
            response.metadata.generation_id.as_deref(),
            Some("gen-empty")
        );
    }

    #[test]
    fn invalid_or_fractional_costs_never_wrap() {
        assert_eq!(dollars_to_microusd(&json!(-1)), None);
        assert_eq!(dollars_to_microusd(&json!("NaN")), None);
        assert_eq!(dollars_to_microusd(&json!(0.0000004)), Some(0));
    }

    #[test]
    fn credentials_models_providers_and_price_caps_are_validated() {
        assert!(OpenRouter::new("", "model", OpenRouterOptions::default()).is_err());
        assert!(OpenRouter::new("key", "", OpenRouterOptions::default()).is_err());
        assert!(
            OpenRouter::new(
                "key",
                "model",
                OpenRouterOptions {
                    routing: RoutingPolicy::Pinned(String::new()),
                    ..OpenRouterOptions::default()
                }
            )
            .is_err()
        );
        assert!(
            OpenRouter::new(
                "key",
                "model",
                OpenRouterOptions {
                    max_prompt_price_per_million: Some(f64::NAN),
                    ..OpenRouterOptions::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn credential_file_parser_is_strict_without_being_a_shell() {
        assert_eq!(
            parse_openrouter_env(
                "# private aiTeX credentials\nOTHER=value\nexport OPENROUTER_API_KEY='sk-or-test'\n"
            )
            .unwrap(),
            "sk-or-test"
        );
        assert!(
            parse_openrouter_env("OPENROUTER_API_KEY=first\nOPENROUTER_API_KEY=second\n").is_err()
        );
        assert!(parse_openrouter_env("OPENROUTER_API_KEY='unterminated\n").is_err());
        assert!(parse_openrouter_env("OPENROUTER_API_KEY=key with spaces\n").is_err());
        assert!(parse_openrouter_env("ANTHROPIC_API_KEY=other\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn credential_file_must_not_be_group_or_world_accessible() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "aitex-openrouter-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("env");
        std::fs::write(&path, "export OPENROUTER_API_KEY='sk-or-private'\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_openrouter_config(&path).unwrap(), "sk-or-private");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = read_openrouter_config(&path).unwrap_err().to_string();
        assert!(error.contains("permissions 0644"));
        assert!(!error.contains("sk-or-private"));

        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
