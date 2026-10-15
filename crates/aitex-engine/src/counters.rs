//! Lightweight, env-gated performance counters for token-traffic analysis.
//!
//! Zero cost in normal runs (a single relaxed atomic add each, and only when
//! `AITEX_COUNTERS` is observed set at startup). These exist to answer one
//! question the CPU profile cannot: how many token reads/writes we perform per
//! macro expansion versus TeX's in-place minimum — i.e. how much the eager
//! expansion materialization amplifies token traffic.

use core::sync::atomic::{AtomicU64, Ordering};

pub static NEXT_TOKEN: AtomicU64 = AtomicU64::new(0);
pub static STREAM_NEXT: AtomicU64 = AtomicU64::new(0);
pub static MACRO_CALLS: AtomicU64 = AtomicU64::new(0);
pub static EXPANDED_TOKENS: AtomicU64 = AtomicU64::new(0);
pub static ARG_TOKENS: AtomicU64 = AtomicU64::new(0);
// Per-call profiling: call counts for the hot functions, so self-time% from
// `sample` converts to ns/call.
pub static MATCH_ARGS: AtomicU64 = AtomicU64::new(0);
pub static SCAN_NUMBER: AtomicU64 = AtomicU64::new(0);
pub static EXPAND_ONE: AtomicU64 = AtomicU64::new(0);
pub static DO_PRIMITIVE: AtomicU64 = AtomicU64::new(0);
pub static CONTROL_SEQ: AtomicU64 = AtomicU64::new(0);
pub static STREAM_REMOVE: AtomicU64 = AtomicU64::new(0);
pub static STREAM_PEEK: AtomicU64 = AtomicU64::new(0);
pub static INTERN_LOOKUP: AtomicU64 = AtomicU64::new(0);
pub static EXPAND_HEAD: AtomicU64 = AtomicU64::new(0);
pub static IS_EXPANDABLE: AtomicU64 = AtomicU64::new(0);
pub static ZERO_ARG: AtomicU64 = AtomicU64::new(0);

// Zero-cost by default: the `fetch_add`s compile away entirely unless the
// `counters` feature is on. Build the diagnostic with
// `cargo build --release -p aitex-cli --features counters`.
#[inline(always)]
pub fn bump(_c: &AtomicU64) {
    #[cfg(feature = "counters")]
    _c.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn add(_c: &AtomicU64, _n: u64) {
    #[cfg(feature = "counters")]
    _c.fetch_add(_n, Ordering::Relaxed);
}

pub fn report() -> alloc::string::String {
    use core::sync::atomic::Ordering::Relaxed;
    alloc::format!(
        "counters: next_token={} stream_next={} stream_remove={} stream_peek={} \
         macro_calls={} expand_head={} match_args={} is_expandable={} scan_number={} \
         expand_one={} do_primitive={} control_seq={} intern_lookup={} \
         expanded_tokens={} arg_tokens={} zero_arg={}",
        NEXT_TOKEN.load(Relaxed),
        STREAM_NEXT.load(Relaxed),
        STREAM_REMOVE.load(Relaxed),
        STREAM_PEEK.load(Relaxed),
        MACRO_CALLS.load(Relaxed),
        EXPAND_HEAD.load(Relaxed),
        MATCH_ARGS.load(Relaxed),
        IS_EXPANDABLE.load(Relaxed),
        SCAN_NUMBER.load(Relaxed),
        EXPAND_ONE.load(Relaxed),
        DO_PRIMITIVE.load(Relaxed),
        CONTROL_SEQ.load(Relaxed),
        INTERN_LOOKUP.load(Relaxed),
        EXPANDED_TOKENS.load(Relaxed),
        ARG_TOKENS.load(Relaxed),
        ZERO_ARG.load(Relaxed),
    )
}
