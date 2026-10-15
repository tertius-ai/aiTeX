//! The LLM seam.
//!
//! Native Anthropic and OpenRouter are direct API paths and the local Codex CLI
//! is a convenient experimental path, but nothing above this module knows that.
//! Everything goes through [`Backend`], so another provider is a new impl
//! rather than a refactor.
//!
//! The part that must be normalized is **cache accounting**. Our whole token
//! strategy rests on prompt-cache hits, and a silent miss is invisible: it costs
//! full price and raises no error. Providers disagree on the field names —
//! Anthropic returns `cache_read_input_tokens` / `cache_creation_input_tokens`,
//! OpenAI-compatible gateways return `prompt_tokens_details.cached_tokens` — so
//! [`Usage`] is the one shape the harness measures against.
//!
//! No network code lives here and this module has no dependencies; concrete
//! backends arrive at harness H3.

use serde_json::Value;
use std::fmt;

/// A chunk of prompt content. Split out because cache breakpoints attach to
/// block boundaries, not to whole messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub text: String,
    /// Mark this block as the end of a cacheable prefix.
    ///
    /// Caching is a **prefix match**: any byte change at position *N*
    /// invalidates every breakpoint at position ≥ *N*. Render order is
    /// tools → system → messages, so the frozen rulebook goes first with the
    /// marker, and the volatile log excerpt goes last without one.
    pub cache: bool,
}

impl Block {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache: false,
        }
    }

    /// Mark this as a cache breakpoint.
    pub fn cached(mut self) -> Self {
        self.cache = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![Block::new(text)],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Stable prefix: instructions and the LaTeX rulebook. Cache marker belongs
    /// on the last of these.
    pub system: Vec<Block>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    /// Optional provider-enforced JSON response shape.  Keeping this on the
    /// request rather than in a concrete backend lets the one-shot repair and
    /// agentic tool protocols share the same provider-neutral seam.
    pub response_schema: Option<JsonResponseSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonResponseSchema {
    pub name: String,
    pub schema: Value,
}

impl Request {
    pub fn new(system: Vec<Block>, messages: Vec<Message>) -> Self {
        Self {
            system,
            messages,
            max_tokens: 4096,
            response_schema: None,
        }
    }

    pub fn with_response_schema(mut self, name: impl Into<String>, schema: Value) -> Self {
        self.response_schema = Some(JsonResponseSchema {
            name: name.into(),
            schema,
        });
        self
    }

    /// Does any block request caching?
    pub fn has_cache_breakpoint(&self) -> bool {
        self.system.iter().any(|b| b.cache)
            || self
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|b| b.cache)
    }
}

/// Token accounting, normalized across providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens that were **not** served from cache.
    ///
    /// Note this is the *remainder*, not the total — Anthropic's `input_tokens`
    /// excludes both cache reads and cache writes. Use [`Usage::total_input`].
    pub input: u64,
    pub output: u64,
    /// Tokens read from cache. Billed at roughly 0.1× input.
    pub cache_read: u64,
    /// Tokens written to cache. Billed at 1.25× (5-min TTL) or 2× (1-hour).
    pub cache_write: u64,
    /// Reasoning tokens included within `output`, when exposed separately.
    /// This is a subset and must not be added to `output` again.
    pub reasoning: u64,
}

impl Usage {
    /// Every input token, however billed. Summing the three parts is the only
    /// correct way to get this.
    pub fn total_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    /// Fraction of input served from cache. The number to watch: if this stays
    /// at zero across repeated runs, something is silently invalidating the
    /// prefix and the cost model is wrong.
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.total_input();
        if total == 0 {
            0.0
        } else {
            self.cache_read as f64 / total as f64
        }
    }
}

/// Provider facts that do not belong in normalized token accounting. Fields
/// remain absent when a provider does not authoritatively expose them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResponseMetadata {
    pub provider: Option<String>,
    /// Provider slug explicitly requested by a pinned route. This is not an
    /// assertion about an unexposed physical serving endpoint.
    pub requested_provider: Option<String>,
    pub generation_id: Option<String>,
    pub service_tier: Option<String>,
    pub charged_microusd: Option<u64>,
    pub upstream_microusd: Option<u64>,
    pub time_to_first_byte_ms: Option<f64>,
    pub provider_generation_ms: Option<f64>,
    pub routing: Option<String>,
    /// Exact router-authored metadata, when explicitly requested and returned.
    pub router_metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub text: String,
    pub usage: Usage,
    /// Which model actually served this. Providers may route; a benchmark must
    /// record what it really got rather than what it asked for.
    pub model: String,
    pub metadata: ResponseMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Transport(String),
    /// Provider refused: rate limit, overload, bad request.
    Api {
        status: u16,
        message: String,
    },
    /// Response did not match the expected shape.
    Malformed(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Transport(m) => write!(f, "transport error: {m}"),
            BackendError::Api { status, message } => write!(f, "api error {status}: {message}"),
            BackendError::Malformed(m) => write!(f, "malformed response: {m}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// A provider that can complete a [`Request`].
pub trait Backend {
    fn complete(&mut self, req: &Request) -> Result<Response, BackendError>;

    /// Count tokens without spending them.
    ///
    /// Only needed to check that a cacheable prefix clears the provider's
    /// minimum — below it, caching silently does nothing. Returns `None` where
    /// unsupported, which is itself worth knowing: estimating instead is
    /// unreliable, since tokenizers differ by 15–20% on prose and worse on
    /// LaTeX.
    fn count_tokens(&mut self, _req: &Request) -> Option<Result<u64, BackendError>> {
        None
    }

    /// Smallest prefix this provider will cache, in tokens. `None` if unknown.
    ///
    /// Model-dependent and easy to trip over: a rulebook that caches fine on one
    /// model silently will not on another with a higher floor.
    fn min_cacheable_prefix(&self) -> Option<u64> {
        None
    }

    fn name(&self) -> &str;
}

/// Records requests and replays canned responses. Lets the whole repair loop be
/// tested — including cache accounting — without a network or an API key.
#[derive(Debug, Default)]
pub struct RecordingBackend {
    pub seen: Vec<Request>,
    pub replies: Vec<Response>,
    next: usize,
}

impl RecordingBackend {
    pub fn new(replies: Vec<Response>) -> Self {
        Self {
            seen: Vec::new(),
            replies,
            next: 0,
        }
    }

    /// Reply once with `text` and no cache activity.
    pub fn replying(text: &str) -> Self {
        Self::new(vec![Response {
            text: text.to_string(),
            usage: Usage {
                input: 100,
                output: 20,
                ..Default::default()
            },
            model: "recording".into(),
            metadata: ResponseMetadata::default(),
        }])
    }
}

impl Backend for RecordingBackend {
    fn complete(&mut self, req: &Request) -> Result<Response, BackendError> {
        self.seen.push(req.clone());
        let r = self
            .replies
            .get(self.next)
            .cloned()
            .ok_or_else(|| BackendError::Malformed("no canned reply left".into()))?;
        self.next += 1;
        Ok(r)
    }

    fn name(&self) -> &str {
        "recording"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_input_sums_all_three_buckets() {
        // The classic mistake: reading `input` as the total. It is only the
        // uncached remainder, so a well-cached request looks nearly free.
        let u = Usage {
            input: 50,
            output: 10,
            cache_read: 4000,
            cache_write: 0,
            reasoning: 3,
        };
        assert_eq!(u.total_input(), 4050);
        assert_eq!(u.reasoning, 3);
        assert!(u.cache_hit_rate() > 0.98);
    }

    #[test]
    fn zero_cache_reads_are_visible_as_a_zero_hit_rate() {
        let u = Usage {
            input: 4050,
            output: 10,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        assert_eq!(
            u.cache_hit_rate(),
            0.0,
            "a silent cache miss must be measurable"
        );
        assert_eq!(
            Usage::default().cache_hit_rate(),
            0.0,
            "no division by zero"
        );
    }

    #[test]
    fn cache_breakpoints_are_detected_anywhere_in_the_request() {
        let plain = Request::new(vec![Block::new("rules")], vec![Message::user("log")]);
        assert!(!plain.has_cache_breakpoint());

        let cached = Request::new(
            vec![Block::new("rules").cached()],
            vec![Message::user("log")],
        );
        assert!(cached.has_cache_breakpoint());
    }

    #[test]
    fn recording_backend_captures_requests_and_replays_replies() {
        let mut b = RecordingBackend::replying("--- a.tex\n+++ a.tex\n");
        let req = Request::new(
            vec![Block::new("rulebook").cached()],
            vec![Message::user("err")],
        );
        let resp = b.complete(&req).expect("canned reply");
        assert!(resp.text.starts_with("--- a.tex"));
        assert_eq!(b.seen.len(), 1);
        assert!(b.seen[0].has_cache_breakpoint());
        // Exhausted: a second call must fail loudly, not silently repeat.
        assert!(b.complete(&req).is_err());
    }

    #[test]
    fn backends_may_decline_token_counting() {
        let mut b = RecordingBackend::default();
        let req = Request::new(vec![], vec![]);
        assert!(b.count_tokens(&req).is_none());
        assert!(b.min_cacheable_prefix().is_none());
    }
}
