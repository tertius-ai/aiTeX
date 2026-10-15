//! Where the engine's time goes, by phase.
//!
//! There is no `perf` and no `valgrind` on the machine this is developed on,
//! and a `gdb`-driven sampler lands about twenty-five usable samples on a
//! four-hundred-millisecond run — too coarse to apportion anything. So the
//! engine says what it is doing, and the host samples that.
//!
//! [`PHASE`] holds the phase currently running. Entering one is a single
//! relaxed store, and leaving it restores what was there before, so the marker
//! nests exactly as the calls do and always names the *innermost* work. A host
//! thread reads it on a timer; the counts are a statistical profile of self
//! time, in phases rather than functions.
//!
//! The cost is one atomic store per phase transition — per paragraph, per box
//! packed, per page — which is nothing against the work being measured, so
//! this is compiled in unconditionally rather than behind a feature. What it
//! cannot see is *where inside* a phase the time goes; that is what the next
//! layer of guards is for when a phase turns out to be the answer.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// The phase running right now, as one of the `P_*` constants.
pub static PHASE: AtomicU8 = AtomicU8::new(P_OTHER);
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable phase markers for a host sampler. Production runs leave them off;
/// per-character markers otherwise perturb the very workload being measured.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Anything not inside a named phase: start-up, file reading, the driver.
pub const P_OTHER: u8 = 0;
/// The main loop: tokenizing, expansion, primitive dispatch.
pub const P_EXPAND: u8 = 1;
/// Breaking a paragraph into lines.
pub const P_LINEBREAK: u8 = 2;
/// Finding hyphenation points, between the first and second break passes.
pub const P_HYPHENATE: u8 = 3;
/// `hpack`/`vpack`: measuring a list and setting its glue.
pub const P_PACK: u8 = 4;
/// The page builder: moving contributions to the page, and firing it.
pub const P_PAGE: u8 = 5;
/// Shipping a finished page: walking it and writing DVI.
pub const P_SHIP: u8 = 6;
/// Building the nodes of a paragraph — characters, ligatures, kerns, glue.
pub const P_BUILD: u8 = 7;
/// Converting a math list to a horizontal one.
pub const P_MATH: u8 = 8;
/// Ending a paragraph: contributing its lines, interline glue, penalties.
pub const P_PARA: u8 = 9;
/// Appending a node to the current list.
pub const P_APPEND: u8 = 10;

/// How many phases there are, for a host's counter array.
pub const PHASES: usize = 11;

/// The name of a phase, for a host's report.
pub fn name(p: u8) -> &'static str {
    match p {
        P_EXPAND => "expand",
        P_LINEBREAK => "linebreak",
        P_HYPHENATE => "hyphenate",
        P_PACK => "pack",
        P_PAGE => "page",
        P_SHIP => "ship",
        P_BUILD => "build",
        P_MATH => "math",
        P_PARA => "para",
        P_APPEND => "append",
        _ => "other",
    }
}

/// Marks its scope as running in `p`, and restores the enclosing phase on the
/// way out. Nesting is what makes the reading a *self* time.
pub struct Phase(Option<u8>);

impl Phase {
    #[inline]
    pub fn enter(p: u8) -> Self {
        if ENABLED.load(Ordering::Relaxed) {
            Phase(Some(PHASE.swap(p, Ordering::Relaxed)))
        } else {
            Phase(None)
        }
    }
}

impl Drop for Phase {
    #[inline]
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            PHASE.store(previous, Ordering::Relaxed);
        }
    }
}
