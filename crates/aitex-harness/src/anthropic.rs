//! Native Anthropic backend.
//!
//! Hand-rolled against `POST /v1/messages` rather than using a community crate.
//! The wire format is small and stable, and the one thing we cannot afford to
//! have lag behind is `cache_control` placement — a wrapper that puts the
//! breakpoint in the wrong block produces no error, just a silent cache miss at
//! full price.
//!
//! Chosen over a gateway for three measured reasons (docs/history/PLAN.md H3): the Batches
//! API's 50% discount is native-only and matters for 1000-document sweeps;
//! `count_tokens` is native-only and is how we check a prefix clears the cache
//! floor before spending a run discovering it did not; and gateways have a
//! documented class of silently-ignored parameters, which is exactly what
//! poisons a benchmark.

use crate::backend::{
    Backend, BackendError, Block, Request, Response, ResponseMetadata, Role, Usage,
};
use std::io::Read as _;

const API: &str = "https://api.anthropic.com/v1/messages";
const COUNT_API: &str = "https://api.anthropic.com/v1/messages/count_tokens";
const VERSION: &str = "2023-06-01";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Minimum prefix Anthropic will cache, per model.
///
/// **Below this, caching silently does nothing** — no error, just
/// `cache_creation_input_tokens: 0`. Opus 4.8 needs 4096, so a rulebook that
/// caches fine on Sonnet may not cache at all here.
fn min_cacheable(model: &str) -> u64 {
    if model.starts_with("claude-opus-4") || model.starts_with("claude-haiku-4-5") {
        4096
    } else {
        2048
    }
}

pub struct Anthropic {
    api_key: String,
    pub model: String,
    client: reqwest::blocking::Client,
}

impl Anthropic {
    /// Read the key from `ANTHROPIC_API_KEY`.
    pub fn from_env(model: &str) -> Result<Self, BackendError> {
        Self::from_env_with_timeout(model, std::time::Duration::from_secs(120))
    }

    pub fn from_env_with_timeout(
        model: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, BackendError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| BackendError::Transport("ANTHROPIC_API_KEY is not set".into()))?;
        Ok(Self::new_with_timeout(api_key, model, timeout))
    }

    pub fn new(api_key: impl Into<String>, model: &str) -> Self {
        Self::new_with_timeout(api_key, model, std::time::Duration::from_secs(120))
    }

    pub fn new_with_timeout(
        api_key: impl Into<String>,
        model: &str,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()
                .expect("client builds"),
        }
    }

    /// Render a request as the API's JSON.
    ///
    /// Order is significant: the API renders `tools` → `system` → `messages`,
    /// and caching is a prefix match, so the stable rulebook goes in `system`
    /// with the breakpoint and the volatile excerpt goes last in `messages`.
    fn body(&self, req: &Request) -> serde_json::Value {
        use serde_json::json;

        fn blocks(bs: &[Block]) -> Vec<serde_json::Value> {
            bs.iter()
                .map(|b| {
                    let mut v = json!({"type": "text", "text": b.text});
                    if b.cache {
                        v["cache_control"] = json!({"type": "ephemeral"});
                    }
                    v
                })
                .collect()
        }

        json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "system": blocks(&req.system),
            "messages": req.messages.iter().map(|m| json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": blocks(&m.content),
            })).collect::<Vec<_>>(),
        })
    }

    fn send(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, BackendError> {
        let mut resp = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", VERSION)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        let status = resp.status();
        let mut bytes = Vec::new();
        resp.by_ref()
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(BackendError::Malformed(format!(
                "provider response exceeds {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|error| BackendError::Malformed(format!("response is not UTF-8: {error}")))?;
        if !status.is_success() {
            return Err(BackendError::Api {
                status: status.as_u16(),
                message: text,
            });
        }
        serde_json::from_str(&text).map_err(|e| BackendError::Malformed(e.to_string()))
    }
}

/// Pull the normalized [`Usage`] out of a response.
///
/// `input_tokens` excludes both cache reads and writes, so anything that treats
/// it as the total will report a well-cached request as nearly free.
pub fn parse_usage(v: &serde_json::Value) -> Usage {
    let u = &v["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Usage {
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_read: n("cache_read_input_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        reasoning: 0,
    }
}

/// Concatenate the text blocks of a response.
pub fn parse_text(v: &serde_json::Value) -> String {
    v["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

impl Backend for Anthropic {
    fn complete(&mut self, req: &Request) -> Result<Response, BackendError> {
        let v = self.send(API, &self.body(req))?;
        Ok(Response {
            text: parse_text(&v),
            usage: parse_usage(&v),
            model: v["model"].as_str().unwrap_or(&self.model).to_string(),
            metadata: ResponseMetadata {
                provider: Some("Anthropic".into()),
                generation_id: v["id"].as_str().map(str::to_string),
                ..ResponseMetadata::default()
            },
        })
    }

    fn count_tokens(&mut self, req: &Request) -> Option<Result<u64, BackendError>> {
        let mut body = self.body(req);
        // count_tokens rejects generation parameters.
        body.as_object_mut().map(|o| o.remove("max_tokens"));
        Some(self.send(COUNT_API, &body).and_then(|v| {
            v["input_tokens"]
                .as_u64()
                .ok_or_else(|| BackendError::Malformed("no input_tokens".into()))
        }))
    }

    fn min_cacheable_prefix(&self) -> Option<u64> {
        Some(min_cacheable(&self.model))
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Message;
    use serde_json::json;

    fn req() -> Request {
        Request::new(
            vec![Block::new("rulebook").cached()],
            vec![Message::user("the error")],
        )
    }

    #[test]
    fn cache_control_lands_on_the_marked_block_only() {
        let a = Anthropic::new("k", "claude-opus-4-8");
        let b = a.body(&Request::new(
            vec![Block::new("stable"), Block::new("rulebook").cached()],
            vec![Message::user("volatile")],
        ));
        assert!(b["system"][0].get("cache_control").is_none());
        assert_eq!(b["system"][1]["cache_control"]["type"], "ephemeral");
        // The volatile part must never carry a breakpoint: it changes every
        // request, so caching it would write a new entry each time at 1.25x.
        assert!(
            b["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
    }

    #[test]
    fn system_precedes_messages_in_the_rendered_body() {
        let a = Anthropic::new("k", "claude-opus-4-8");
        let b = a.body(&req());
        assert_eq!(b["system"][0]["text"], "rulebook");
        assert_eq!(b["messages"][0]["content"][0]["text"], "the error");
        assert_eq!(b["messages"][0]["role"], "user");
    }

    #[test]
    fn usage_separates_cache_reads_from_fresh_input() {
        let v = json!({"usage": {
            "input_tokens": 50, "output_tokens": 10,
            "cache_read_input_tokens": 4000, "cache_creation_input_tokens": 0
        }});
        let u = parse_usage(&v);
        assert_eq!(u.input, 50);
        assert_eq!(u.cache_read, 4000);
        // The trap: input_tokens is only the uncached remainder.
        assert_eq!(u.total_input(), 4050);
        assert!(u.cache_hit_rate() > 0.98);
    }

    #[test]
    fn a_missing_usage_field_reads_as_zero_not_a_panic() {
        let u = parse_usage(&json!({}));
        assert_eq!(u, Usage::default());
        assert_eq!(u.cache_hit_rate(), 0.0);
    }

    #[test]
    fn text_blocks_are_concatenated_and_others_ignored() {
        let v = json!({"content": [
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "--- a.tex\n"},
            {"type": "text", "text": "+++ a.tex\n"}
        ]});
        assert_eq!(parse_text(&v), "--- a.tex\n+++ a.tex\n");
        assert_eq!(parse_text(&json!({})), "");
    }

    /// Opus 4.8 will not cache a prefix below 4096 tokens, and says nothing
    /// when it declines. A rulebook sized for Sonnet silently costs full price.
    #[test]
    fn cache_floor_is_model_dependent() {
        assert_eq!(min_cacheable("claude-opus-4-8"), 4096);
        assert_eq!(min_cacheable("claude-haiku-4-5-20251001"), 4096);
        assert_eq!(min_cacheable("claude-sonnet-5"), 2048);
        assert_eq!(
            Anthropic::new("k", "claude-opus-4-8").min_cacheable_prefix(),
            Some(4096)
        );
    }

    #[test]
    fn missing_credentials_are_reported_not_panicked() {
        // Only meaningful when the variable is genuinely absent.
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            assert!(matches!(
                Anthropic::from_env("claude-opus-4-8"),
                Err(BackendError::Transport(_))
            ));
        }
    }
}
