//! Harness H1: run a LaTeX engine and turn its output into structured
//! diagnostics. No LLM involvement at this milestone by design — the
//! deterministic core has to work, and be measurable, first.

pub mod agent;
#[cfg(feature = "anthropic")]
pub mod anthropic;
pub mod backend;
pub mod codex;
pub mod compile;
pub mod context;
pub mod fixes;
pub mod kernel;
pub mod lab;
pub mod log;
#[cfg(feature = "openrouter")]
pub mod openrouter;
pub mod patch;
pub mod repair;
pub mod rules;
pub mod scan;
pub mod session;
pub mod structure;
pub mod validation;

pub use agent::{
    AGENT_ACTION_SCHEMA, AGENT_PROTOCOL, AGENT_TRACE_SCHEMA, AgentBuildConfig, AgentChange,
    AgentLimits, AgentProgress, AgentRun, AgentStatus, AgentTrace, AgentUsage, diagnostic_delta,
    run_agent, run_agent_with_progress,
};
pub use backend::{
    Backend, BackendError, Block, JsonResponseSchema, Message, Request, Response, ResponseMetadata,
    Usage,
};
pub use compile::{CompileOptions, Engine, Outcome, read_log, run};
pub use fixes::{Action, Confidence, Context, Fix, propose};
pub use log::{
    parse_bib_log, parse_tex_log, parse_tool_log, tex_log_has_error_context, unwrap_lines,
};
pub use patch::{
    PatchError, PreparedPatch, StructuredEdit, add_package, append_to_line, apply_action,
    apply_unified_patch, author_source, inspect_patch_safety, patch_target, prepare_model_patch,
    prepare_structured_edits, replace_once_on_line, structured_edit_target, unified_diff,
};
pub use repair::{
    CONVENTIONAL_LOG_PROTOCOL, ContextProfile, DIAGNOSTIC_SCHEMA, LEGACY_RESPONSE_SCHEMA,
    ModelAnswer, ModelProposal, ModelRun, ModelRunError, ProjectFile, PromptPacket,
    REPAIR_PROTOCOL, RESPONSE_SCHEMA, build_prompt, build_prompt_with_log, diagnostic_presentation,
    parse_answer, repair_response_schema, run_model,
};
pub use scan::context_for_project;
pub use session::{
    DiagnosticRecord, ProvenanceRecord, REPAIR_EVENT_SCHEMA, REPAIR_SESSION_SCHEMA, RepairAttempt,
    RepairChangeRecord, RepairEvent, RepairLimits, RepairSession, RepairUsage, SafetyRecord,
};
pub use validation::{
    RepairWorkspace, ValidationArtifact, ValidationError, ValidationLimits, ValidationRequest,
    ValidationResult, WorkspaceBuild, enrich_diagnostic_origin, enrich_report_diagnostics,
    validate_edit,
};
