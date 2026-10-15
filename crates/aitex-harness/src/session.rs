//! Versioned repair-session records shared by the human CLI, machine clients,
//! and future benchmark exporters.
//!
//! The local audit is deliberately richer than a leaderboard row: it retains
//! the bounded prompt envelope, raw provider response, normalized patch,
//! validation result, and user decision. That makes a real repair useful as a
//! later corpus candidate without making publication automatic.

use crate::{ContextProfile, Usage};
use aitex_core::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const REPAIR_SESSION_SCHEMA: &str = "aitex-repair-session-v1";
pub const REPAIR_EVENT_SCHEMA: &str = "aitex-repair-event-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepairLimits {
    pub max_attempts: u32,
    #[serde(default = "default_agent_turns")]
    pub max_agent_turns: u32,
    #[serde(default = "default_agent_builds")]
    pub max_agent_builds: u32,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub max_elapsed_seconds: u64,
    pub max_changed_lines: usize,
    pub max_deleted_lines: usize,
    pub max_source_bytes_per_request: usize,
    /// A provider-reported stop threshold between calls. Providers that cannot
    /// report cost are refused; a provider-side key limit is the pre-call cap.
    pub max_cost_microusd: Option<u64>,
}

const fn default_agent_turns() -> u32 {
    12
}

const fn default_agent_builds() -> u32 {
    4
}

impl Default for RepairLimits {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            max_agent_turns: default_agent_turns(),
            max_agent_builds: default_agent_builds(),
            max_input_tokens: 100_000,
            max_output_tokens: 16_384,
            max_elapsed_seconds: 600,
            max_changed_lines: 200,
            max_deleted_lines: 100,
            max_source_bytes_per_request: 16 * 1024,
            max_cost_microusd: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepairChangeRecord {
    pub target: String,
    pub before_sha256: String,
    pub after_sha256: String,
    pub patch: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepairUsage {
    pub model_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub unpriced_model_calls: u32,
    pub cost_microusd: Option<u64>,
}

impl RepairUsage {
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    pub fn charge(&mut self, usage: Usage, cost_microusd: Option<u64>) {
        self.model_calls = self.model_calls.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(usage.input);
        self.output_tokens = self.output_tokens.saturating_add(usage.output);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(usage.cache_read);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(usage.cache_write);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning);
        if cost_microusd.is_none() {
            self.unpriced_model_calls = self.unpriced_model_calls.saturating_add(1);
        }
        self.cost_microusd = match (self.cost_microusd, cost_microusd) {
            (Some(total), Some(cost)) => Some(total.saturating_add(cost)),
            (None, Some(cost)) => Some(cost),
            (known, None) => known,
        };
    }

    pub fn absorb_agent(&mut self, usage: &crate::agent::AgentUsage) {
        self.model_calls = self.model_calls.saturating_add(usage.model_calls);
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.unpriced_model_calls = self
            .unpriced_model_calls
            .saturating_add(usage.unpriced_calls);
        self.cost_microusd = match (self.cost_microusd, usage.cost_microusd) {
            (Some(total), Some(cost)) => Some(total.saturating_add(cost)),
            (None, Some(cost)) => Some(cost),
            (known, None) => known,
        };
    }

    pub fn model_request_allowed(
        &self,
        limits: &RepairLimits,
        estimated_input_tokens: usize,
        estimated_source_bytes: usize,
    ) -> Result<(), String> {
        if self.model_calls >= limits.max_attempts {
            return Err(format!(
                "model-attempt budget exhausted ({}/{})",
                self.model_calls, limits.max_attempts
            ));
        }
        if limits.max_cost_microusd.is_some() && self.unpriced_model_calls > 0 {
            return Err("a prior model call has unknown cost; refusing further spend".into());
        }
        if self
            .total_input_tokens()
            .saturating_add(u64::try_from(estimated_input_tokens).unwrap_or(u64::MAX))
            > limits.max_input_tokens
        {
            return Err(format!(
                "estimated input would exceed the token budget ({} + {} > {})",
                self.total_input_tokens(),
                estimated_input_tokens,
                limits.max_input_tokens
            ));
        }
        if self.output_tokens >= limits.max_output_tokens {
            return Err(format!(
                "output-token budget exhausted ({}/{})",
                self.output_tokens, limits.max_output_tokens
            ));
        }
        if estimated_source_bytes > limits.max_source_bytes_per_request {
            return Err(format!(
                "selected context exceeds the source-upload limit ({} > {} bytes)",
                estimated_source_bytes, limits.max_source_bytes_per_request
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvenanceRecord {
    pub kind: String,
    pub label: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticRecord {
    pub signature: String,
    pub tool: String,
    pub kind: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// Canonical ANSI-free block shown to humans and sent to models in repair
    /// protocol v1.1. Optional so v1 session records remain readable. Protocol
    /// v1.2 appends the structured chain below without changing this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<String>,
    /// Protocol-v1.2 causal chain. Defaulted so every earlier session remains
    /// readable and resumable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceRecord>,
}

impl From<&Diagnostic> for DiagnosticRecord {
    fn from(diagnostic: &Diagnostic) -> Self {
        let location = diagnostic.location();
        Self {
            signature: diagnostic.signature(),
            tool: diagnostic.source.as_str().into(),
            kind: diagnostic.kind.name().into(),
            message: diagnostic.message.clone(),
            file: location.map(|span| span.file.to_string_lossy().into_owned()),
            line: location.map(|span| span.line),
            column: location.and_then(|span| span.col),
            presentation: Some(diagnostic.presentation().plain_text()),
            provenance: diagnostic
                .provenance
                .iter()
                .map(|frame| ProvenanceRecord {
                    kind: frame.kind.as_str().into(),
                    label: frame.label.clone(),
                    file: frame
                        .span
                        .as_ref()
                        .map(|span| span.file.to_string_lossy().into_owned()),
                    line: frame.span.as_ref().map(|span| span.line),
                    column: frame.span.as_ref().and_then(|span| span.col),
                    detail: frame.detail.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyRecord {
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub added_lines: usize,
    pub deleted_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepairAttempt {
    pub id: String,
    pub created_unix_ms: u64,
    pub route: String,
    pub diagnostic: DiagnosticRecord,
    pub context_profile: Option<ContextProfile>,
    pub context_envelope: Option<Value>,
    pub estimated_input_tokens: Option<usize>,
    pub included_source_lines: Option<usize>,
    pub provider: Option<String>,
    pub requested_model: Option<String>,
    pub returned_model: Option<String>,
    pub provider_counted_input_tokens: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    pub cost_microusd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_microusd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_first_byte_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_generation_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_metadata: Option<Value>,
    pub model_ms: Option<f64>,
    pub raw_response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_normalization: Option<String>,
    pub cause: String,
    pub confidence: String,
    pub raw_patch: String,
    pub normalized_patch: Option<String>,
    pub patch_normalization: Option<String>,
    /// Multi-file changes from an agentic repair.  One-shot v1/v2 attempts
    /// continue to use the singular compatibility fields below.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<RepairChangeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_trace: Option<Value>,
    pub target: Option<String>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub safety: Option<SafetyRecord>,
    pub validation: String,
    pub validation_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_bytes: Option<u64>,
    pub next_diagnostic: Option<DiagnosticRecord>,
    pub accepted: Option<bool>,
    pub rejection: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepairSession {
    pub schema: String,
    pub id: String,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub input: String,
    pub input_sha256: String,
    pub compiler: String,
    pub status: String,
    pub limits: RepairLimits,
    pub usage: RepairUsage,
    pub attempts: Vec<RepairAttempt>,
    pub applied_targets: Vec<String>,
    pub final_build_succeeded: bool,
}

impl RepairSession {
    pub fn new(
        id: String,
        created_unix_ms: u64,
        input: String,
        input_sha256: String,
        compiler: String,
        limits: RepairLimits,
    ) -> Self {
        Self {
            schema: REPAIR_SESSION_SCHEMA.into(),
            id,
            created_unix_ms,
            updated_unix_ms: created_unix_ms,
            input,
            input_sha256,
            compiler,
            status: "diagnosing".into(),
            limits,
            usage: RepairUsage::default(),
            attempts: Vec::new(),
            applied_targets: Vec::new(),
            final_build_succeeded: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepairEvent {
    pub schema: String,
    pub sequence: u64,
    pub session: String,
    pub event: String,
    pub data: Value,
}

impl RepairEvent {
    pub fn new(sequence: u64, session: &str, event: &str, data: Value) -> Self {
        Self {
            schema: REPAIR_EVENT_SCHEMA.into(),
            sequence,
            session: session.into(),
            event: event.into(),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitex_core::{Kind, Severity, Source, Span};

    #[test]
    fn default_budget_is_bounded_and_round_trips() {
        let session = RepairSession::new(
            "r-1".into(),
            1,
            "main.tex".into(),
            "abc".into(),
            "aitex-test".into(),
            RepairLimits::default(),
        );
        let encoded = serde_json::to_string(&session).unwrap();
        let decoded: RepairSession = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, session);
        assert_eq!(decoded.limits.max_attempts, 3);
    }

    #[test]
    fn usage_enforces_model_and_source_budgets() {
        let mut usage = RepairUsage::default();
        let limits = RepairLimits {
            max_attempts: 1,
            max_source_bytes_per_request: 10,
            ..RepairLimits::default()
        };
        assert!(usage.model_request_allowed(&limits, 1, 11).is_err());
        usage.charge(
            Usage {
                input: 4,
                output: 2,
                cache_read: 3,
                cache_write: 1,
                reasoning: 1,
            },
            None,
        );
        assert_eq!(usage.total_input_tokens(), 8);
        assert!(usage.model_request_allowed(&limits, 1, 1).is_err());
    }

    #[test]
    fn diagnostic_records_add_the_display_without_breaking_v1_json() {
        let mut diagnostic = Diagnostic::new(
            Severity::Error,
            Kind::UndefinedControlSequence { name: "bad".into() },
            "Undefined control sequence.",
            Source::TexLog,
        )
        .at(Span::new("main.tex", 7));
        diagnostic.provenance.push(
            aitex_core::ProvenanceFrame::new(aitex_core::ProvenanceKind::MacroDefinition, "\\bad")
                .at(Span::new("macros.tex", 2)),
        );
        let current = DiagnosticRecord::from(&diagnostic);
        assert_eq!(
            current.presentation.as_deref(),
            Some(
                "Diagnose  tex · undefined-control-sequence · main.tex:7\n\
                 Error     Undefined control sequence.\n\
                 Trace     macro-definition · \\bad · macros.tex:2"
            )
        );
        assert_eq!(current.provenance[0].kind, "macro-definition");
        assert_eq!(current.provenance[0].file.as_deref(), Some("macros.tex"));

        let old = r#"{"signature":"s","tool":"tex","kind":"other","message":"x","file":null,"line":null,"column":null}"#;
        let decoded: DiagnosticRecord = serde_json::from_str(old).expect("v1 diagnostic record");
        assert_eq!(decoded.presentation, None);
        assert!(decoded.provenance.is_empty());
    }
}
