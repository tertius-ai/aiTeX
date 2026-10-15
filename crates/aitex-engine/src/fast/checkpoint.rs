//! Conservative live-document checkpoints for [`Fast`].
//!
//! This is intentionally outside the token pump. Checkpoint requests add one
//! branch per command in `main_loop`; provenance checks, serialization, source
//! replacement, and cache-policy refusals stay here on the cold path.

use super::*;

/// A live document checkpoint. Kept distinct from a format dump because the
/// two have opposite lifetime rules: a format deliberately forgets the job
/// that built it, while a checkpoint must retain that job exactly.
const CHECKPOINT_MAGIC: &[u8] = b"\xf7aiTeXcheckpoint";

/// Why a live document checkpoint could not be made or resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointError {
    /// The engine is not at one of the deliberately conservative safe points.
    UnsafeBoundary,
    /// A byte offset did not name a UTF-8 boundary in the supplied source.
    InvalidOffset,
    /// Not an aiTeX live-document checkpoint.
    NotACheckpoint,
    /// A checkpoint from a different serialized engine layout.
    WrongVersion { found: u32, expected: u32 },
    /// The checkpoint is truncated or damaged.
    Corrupt,
    /// The replacement source changed before the saved boundary.
    SourcePrefixChanged,
}

impl core::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CheckpointError::UnsafeBoundary => write!(f, "not at a safe document checkpoint"),
            CheckpointError::InvalidOffset => {
                write!(f, "checkpoint offset is not a UTF-8 source boundary")
            }
            CheckpointError::NotACheckpoint => write!(f, "not an aiTeX document checkpoint"),
            CheckpointError::WrongVersion { found, expected } => write!(
                f,
                "checkpoint was built by a different engine ({found:08x}, this one is {expected:08x})"
            ),
            CheckpointError::Corrupt => write!(f, "checkpoint is truncated or damaged"),
            CheckpointError::SourcePrefixChanged => {
                write!(f, "source changed before the checkpoint boundary")
            }
        }
    }
}

/// Source provenance for a conservative paragraph checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointSite {
    /// The TeX-visible file name (empty for the source passed to [`Fast::run`]).
    pub source_name: alloc::string::String,
    /// The engine file slot. This is provenance, not a stable cross-run id.
    pub file_index: usize,
    /// One-past the source prefix already consumed, in Unicode scalar values.
    pub char_offset: usize,
    /// The same boundary in UTF-8 bytes.
    pub byte_offset: usize,
    /// TeX's physical source line at the boundary.
    pub line: u32,
    /// Monotonic number of real paragraphs completed by this job.
    pub paragraph_serial: u64,
    /// Number of pages completely shipped before this boundary.
    pub pages_shipped: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CheckpointRequest {
    pub(super) file: i32,
    pub(super) after_char: usize,
    pub(super) after_paragraph: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CheckpointRefusal {
    NoTypesetParagraph,
    HostOutputEscaped,
    OutputRoutine,
    Conditional,
    Scanner,
    Alignment,
    TypesetterMode,
    NestedConstruct,
    PendingOperation,
    PendingToken,
    ActiveLink,
    ForcedEof,
    NoSourceInput,
}

impl CheckpointRefusal {
    fn label(self) -> &'static str {
        match self {
            Self::NoTypesetParagraph => "no completed typeset paragraph",
            Self::HostOutputEscaped => "page output already escaped to the host",
            Self::OutputRoutine => "output routine is active",
            Self::Conditional => "conditional is open",
            Self::Scanner => "scanner or assignment is incomplete",
            Self::Alignment => "alignment is active",
            Self::TypesetterMode => "typesetter is not between outer paragraphs",
            Self::NestedConstruct => "nested math, box, insertion, or discretionary is active",
            Self::PendingOperation => "multi-command operation is pending",
            Self::PendingToken => "a token is pending or backed up",
            Self::ActiveLink => "PDF link is active",
            Self::ForcedEof => "an end-of-file request is pending",
            Self::NoSourceInput => "no active source file owns the paragraph boundary",
        }
    }
}

/// Per-job values intentionally omitted from reusable format dumps but required
/// for a bit-for-bit continuation of a live job.
#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointRuntime {
    mag_set: i64,
    term_offset: usize,
    term_watermark: usize,
    logonly_since: usize,
    every_job_done: bool,
    halt_on_error: bool,
    halt_requested: bool,
    force_dvi_output: bool,
    retained_page_meta: Vec<ShipoutMeta>,
    long_help_seen: bool,
    job_name_read: bool,
    clock_reads: u8,
}

#[derive(serde::Serialize)]
struct CheckpointWrite<'a> {
    engine: &'a Fast,
    site: &'a CheckpointSite,
    runtime: CheckpointRuntime,
}

#[derive(serde::Deserialize)]
struct CheckpointRead {
    engine: Fast,
    site: CheckpointSite,
    runtime: CheckpointRuntime,
}

impl Fast {
    #[inline]
    pub(super) fn is_null_frame(frame: &Frame) -> bool {
        frame.file == -1
            && frame.start == 0
            && frame.pos == 0
            && frame.end == 0
            && frame.pbase == 0
            && frame.pn == 0
            && frame.acur == 0
            && frame.astart == 0
            && frame.aend == 0
            && !frame.spent_below
            && !frame.utmpl
    }

    /// Pop exhausted token-list frames without reading the next source token.
    /// A `\par` reached through LaTeX's macro wrappers leaves those spent
    /// frames above the outer file until `get_next` is called again. Doing only
    /// the pop half here exposes the same between-command boundary without
    /// consuming or expanding any downstream input.
    pub(super) fn normalize_checkpoint_input(&mut self) {
        while !self.output_active
            && self.cur.file < 0
            && self.cur.pos >= self.cur.end
            && self.cur.acur >= self.cur.aend
            && !self.cur.utmpl
        {
            let Some(next) = self.input.pop() else {
                break;
            };
            self.cur = next;
            self.reclaim_params();
        }
    }

    /// The nearest live file under any macro/token-list frames. Those frames
    /// are part of the serialized semantic state; the file's own cursor is the
    /// conservative source offset because argument scanning advances it before
    /// retaining any tokens derived from that source.
    fn checkpoint_file_index(&self) -> Option<usize> {
        core::iter::once(&self.cur)
            .chain(self.input.iter().rev())
            .find_map(|frame| (frame.file >= 0).then_some(frame.file as usize))
    }

    /// Return source provenance only at a boundary whose entire live state is
    /// represented by `Fast`. This first implementation is intentionally
    /// narrower than the set of boundaries that could eventually be proven
    /// safe. In particular, streamed pages and active nested source files are
    /// refused; serialized macro/token-list frames above the source are safe.
    fn checkpoint_refusal_for(&self, require_paragraph: bool) -> Option<CheckpointRefusal> {
        if !self.typesetting || (require_paragraph && self.paragraph_serial == 0) {
            return Some(CheckpointRefusal::NoTypesetParagraph);
        }
        if self.page_sink.is_some()
            && !self.checkpoint_page_sink
            && (require_paragraph || self.ts.pages_shipped != 0)
        {
            return Some(CheckpointRefusal::HostOutputEscaped);
        }
        // `output_enclosing` is a remembered nesting index whose meaning is
        // gated by `output_active`; it may retain its last value after §1026.
        if self.output_active || self.saved_output_mode.is_some() {
            return Some(CheckpointRefusal::OutputRoutine);
        }
        if self.cond_depth != 0 {
            return Some(CheckpointRefusal::Conditional);
        }
        if self.scanner_status != ScannerStatus::Normal
            || self.global_flag
            || self.pending_long
            || self.pending_outer
            || self.pending_protected
            || self.keep_protected
            || self.pending_immediate
            || self.suppress_trace
            || self.no_expand
            || self.name_in_progress
            || self.scanning_macro.is_some()
        {
            return Some(CheckpointRefusal::Scanner);
        }
        if self.align_state != 1_000_000
            || !self.aligns.is_empty()
            || !self.align_state_stack.is_empty()
            || !self.align_mode.is_empty()
            || self.display_align_pending
        {
            return Some(CheckpointRefusal::Alignment);
        }
        if self.ts.mode_test() != (crate::fastset::Mode::Vertical, false)
            || !self.ts.open.is_empty()
            || self.no_current_list
        {
            return Some(CheckpointRefusal::TypesetterMode);
        }
        if !self.math_shift_at.is_empty()
            || !self.math_group_at.is_empty()
            || !self.decorate_at.is_empty()
            || !self.left_right_at.is_empty()
            || !self.choice_at.is_empty()
            || !self.script_group_at.is_empty()
            || !self.box_group_at.is_empty()
            || !self.vcenter_at.is_empty()
            || !self.semi_simple_at.is_empty()
            || !self.insert_group_at.is_empty()
            || !self.cell_group_at.is_empty()
            || !self.noalign_group_at.is_empty()
            || !self.disc_group_at.is_empty()
            || self.in_eqno.is_some()
            || self.eqno_depth.is_some()
        {
            return Some(CheckpointRefusal::NestedConstruct);
        }
        if self.pending_box.is_some()
            || self.pending_dest.is_some()
            || self.pending_leaders.is_some()
            || self.leader_box.is_some()
            || self.accent_pending.is_some()
            || self.pending_display_dimens.is_some()
            || self.pending_output_normal_paragraph
            || self.display_pre_at.is_some()
            || self.display_post_at.is_some()
            || self.in_char_run
        {
            return Some(CheckpointRefusal::PendingOperation);
        }
        if self.after_token.is_some() || self.peek.is_some() || self.last_read.is_some() {
            return Some(CheckpointRefusal::PendingToken);
        }
        if !self.pdf_link_targets.is_empty() {
            return Some(CheckpointRefusal::ActiveLink);
        }
        if self.force_eof || self.force_eof_target.is_some() {
            return Some(CheckpointRefusal::ForcedEof);
        }
        if self.checkpoint_file_index().is_none() {
            return Some(CheckpointRefusal::NoSourceInput);
        }
        None
    }

    pub(super) fn checkpoint_refusal(&self) -> Option<CheckpointRefusal> {
        self.checkpoint_refusal_for(true)
    }

    pub(super) fn safe_checkpoint_site(&self) -> Option<CheckpointSite> {
        self.checkpoint_refusal().is_none().then_some(())?;
        self.checkpoint_site()
    }

    fn checkpoint_site(&self) -> Option<CheckpointSite> {
        let file_index = self.checkpoint_file_index()?;
        let file = self.files.get(file_index)?;
        if file.pos > file.chars.len() {
            return None;
        }
        let byte_offset = file.chars[..file.pos].iter().map(|ch| ch.len_utf8()).sum();
        Some(CheckpointSite {
            source_name: file.name.clone(),
            file_index,
            char_offset: file.pos,
            byte_offset,
            line: file.line,
            paragraph_serial: self.paragraph_serial,
            pages_shipped: self.ts.pages_shipped,
        })
    }

    /// A post-`\\document` checkpoint precedes the first body paragraph, so
    /// it deliberately relaxes only that paragraph-count condition. Every
    /// scanner, conditional, alignment, typesetter, nesting, pending-token,
    /// source-provenance, and output-prefix condition remains identical to a
    /// body checkpoint. An installed ordinary page sink is harmless only
    /// while no page has reached it; this lets complete-build hosts install
    /// their writer before running the preamble.
    pub(super) fn document_boundary_checkpoint_site(&self) -> Option<CheckpointSite> {
        self.checkpoint_refusal_for(false).is_none().then_some(())?;
        self.checkpoint_site()
    }

    /// The most recent reason a requested paragraph boundary was refused.
    /// This is diagnostic host state and never participates in a checkpoint.
    pub fn checkpoint_refusal_reason(&self) -> Option<&'static str> {
        self.checkpoint_last_refusal.map(CheckpointRefusal::label)
    }

    /// Run a new outer source until the first safe paragraph boundary at or
    /// after `after_byte`. `Ok(None)` means the source finished without such a
    /// boundary. The request adds one command-granularity branch, never a branch
    /// to the token hot path.
    pub fn run_until_checkpoint(
        &mut self,
        source: &str,
        after_byte: usize,
    ) -> Result<Option<CheckpointSite>, CheckpointError> {
        if after_byte > source.len() || !source.is_char_boundary(after_byte) {
            return Err(CheckpointError::InvalidOffset);
        }
        if !Self::is_null_frame(&self.cur) || !self.input.is_empty() {
            return Err(CheckpointError::UnsafeBoundary);
        }
        if (self.page_sink.is_some() && !self.checkpoint_page_sink) || self.files_written != 0 {
            return Err(CheckpointError::UnsafeBoundary);
        }
        let request = CheckpointRequest {
            file: self.files.len() as i32,
            after_char: source[..after_byte].chars().count(),
            after_paragraph: self.paragraph_serial,
        };
        self.checkpoint_request = Some(request);
        self.checkpoint_hit = None;
        self.checkpoint_last_refusal = None;
        self.run(source);
        self.checkpoint_request = None;
        Ok(self.checkpoint_hit.clone())
    }

    /// Run a new outer source until LaTeX's canonical `\\document` macro has
    /// actually finished at a safe engine boundary.
    ///
    /// This observes macro execution rather than guessing from source text.
    /// `Ok(None)` means the source completed without an eligible boundary;
    /// callers must then use an ordinary full pass. The paused engine remains
    /// live and may be cloned in-process before [`Fast::main_loop`] continues.
    pub fn run_until_document_boundary(
        &mut self,
        source: &str,
    ) -> Result<Option<CheckpointSite>, CheckpointError> {
        if !Self::is_null_frame(&self.cur) || !self.input.is_empty() || self.files_written != 0 {
            return Err(CheckpointError::UnsafeBoundary);
        }
        self.document_boundary_request = true;
        self.document_boundary_hit = None;
        self.run(source);
        self.document_boundary_request = false;
        let hit = self.document_boundary_hit.take();
        if hit.is_some() {
            // `get_next_raw` uses this existing one-bit latch to unwind nested
            // expansion without putting another condition on every token
            // read. A boundary is a pause, not `-halt-on-error`, so resume the
            // returned live engine normally.
            self.halt_requested = false;
        }
        Ok(hit)
    }

    /// Continue an already-paused outer source to its next safe paragraph
    /// boundary. This is the collection primitive for a bounded checkpoint
    /// index: callers may serialize selected sites without pushing a second
    /// source file or perturbing the input stack.
    pub fn continue_until_checkpoint(
        &mut self,
        after_byte: usize,
    ) -> Result<Option<CheckpointSite>, CheckpointError> {
        let current = self
            .safe_checkpoint_site()
            .ok_or(CheckpointError::UnsafeBoundary)?;
        let file = self
            .files
            .get(current.file_index)
            .ok_or(CheckpointError::UnsafeBoundary)?;
        let source_bytes: usize = file.chars.iter().map(|ch| ch.len_utf8()).sum();
        if after_byte > source_bytes {
            return Err(CheckpointError::InvalidOffset);
        }
        let mut bytes = 0usize;
        let mut after_char = 0usize;
        for ch in &file.chars {
            if bytes == after_byte {
                break;
            }
            bytes += ch.len_utf8();
            after_char += 1;
            if bytes > after_byte {
                return Err(CheckpointError::InvalidOffset);
            }
        }
        let request = CheckpointRequest {
            file: current.file_index as i32,
            after_char,
            after_paragraph: self.paragraph_serial,
        };
        self.checkpoint_request = Some(request);
        self.checkpoint_hit = None;
        self.checkpoint_last_refusal = None;
        self.main_loop();
        self.checkpoint_request = None;
        Ok(self.checkpoint_hit.clone())
    }

    /// Serialize the exact live job at a safe paragraph boundary.
    ///
    /// Host callbacks are deliberately not serialized. A resumed host must
    /// reattach file, TFM, file-modification-time, and output callbacks before
    /// calling [`Fast::main_loop`]. Ordinary streaming page callbacks are
    /// refused; a checkpoint-aware sink is allowed only under the host's
    /// promise that it captures and restores the matching DVI writer prefix.
    /// Completed file bodies remain engine-owned replayable output-prefix state.
    pub fn checkpoint(&self) -> Result<alloc::vec::Vec<u8>, CheckpointError> {
        let site = self
            .safe_checkpoint_site()
            .ok_or(CheckpointError::UnsafeBoundary)?;
        let runtime = CheckpointRuntime {
            mag_set: self.mag_set,
            term_offset: self.term_offset,
            term_watermark: self.term_watermark,
            logonly_since: self.logonly_since,
            every_job_done: self.every_job_done,
            halt_on_error: self.halt_on_error,
            halt_requested: self.halt_requested,
            force_dvi_output: self.force_dvi_output,
            retained_page_meta: self.retained_page_meta.clone(),
            long_help_seen: self.long_help_seen,
            job_name_read: self.job_name_read,
            clock_reads: self.clock_reads,
        };
        let payload = CheckpointWrite {
            engine: self,
            site: &site,
            runtime,
        };
        let mut out = alloc::vec::Vec::with_capacity(1 << 20);
        out.extend_from_slice(CHECKPOINT_MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(
            &postcard::to_allocvec(&payload).map_err(|_| CheckpointError::Corrupt)?,
        );
        Ok(out)
    }

    /// Serialize complete future-affecting state for reconvergence comparison.
    ///
    /// Already-consumed file prefixes cannot affect later execution. Removing
    /// them (and rebasing each cursor to zero) makes equal downstream state
    /// compare equal after a same-line source edit, while every register,
    /// definition, stack, page-builder node, write, diagnostic stream, RNG and
    /// clock value remains in the ordinary checkpoint payload. Source suffixes
    /// and current line/tokenizer state remain exact. Mutable line contractions
    /// are refused because rebasing their diagnostic coordinates would require
    /// a second semantics model.
    pub fn reconvergence_state(&mut self) -> Result<alloc::vec::Vec<u8>, CheckpointError> {
        self.safe_checkpoint_site()
            .ok_or(CheckpointError::UnsafeBoundary)?;
        let mut saved = alloc::vec::Vec::with_capacity(self.files.len());
        for file in &mut self.files {
            if file.pos > file.chars.len() {
                return Err(CheckpointError::Corrupt);
            }
            let chars = core::mem::take(&mut file.chars);
            let pos = file.pos;
            let contractions = core::mem::take(&mut file.contractions);
            let contraction_lines = core::mem::take(&mut file.contraction_lines);
            // Keep the already-read part of the current physical line: a
            // later error can print it, so it remains future-observable state.
            // Earlier complete lines cannot be consulted again.
            let retain_from = chars[..pos]
                .iter()
                .rposition(|character| matches!(character, '\n' | '\r'))
                .map_or(0, |newline| newline + 1);
            file.chars = chars[retain_from..].to_vec();
            file.pos = pos - retain_from;
            file.contractions = contractions
                .iter()
                .filter_map(|&(start, end, replacement)| {
                    (start >= retain_from).then_some((
                        start - retain_from,
                        end - retain_from,
                        replacement,
                    ))
                })
                .collect();
            file.contraction_lines = contraction_lines
                .iter()
                .filter_map(|&start| (start >= retain_from).then_some(start - retain_from))
                .collect();
            saved.push((chars, pos, contractions, contraction_lines));
        }
        let result = self.checkpoint();
        for (file, (chars, pos, contractions, contraction_lines)) in
            self.files.iter_mut().zip(saved)
        {
            file.chars = chars;
            file.pos = pos;
            file.contractions = contractions;
            file.contraction_lines = contraction_lines;
        }
        result
    }

    fn check_checkpoint_header(bytes: &[u8]) -> Result<&[u8], CheckpointError> {
        let rest = bytes
            .strip_prefix(CHECKPOINT_MAGIC)
            .ok_or(CheckpointError::NotACheckpoint)?;
        let (v, rest) = rest
            .split_at_checked(4)
            .ok_or(CheckpointError::NotACheckpoint)?;
        let found = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
        if found != FORMAT_VERSION {
            return Err(CheckpointError::WrongVersion {
                found,
                expected: FORMAT_VERSION,
            });
        }
        Ok(rest)
    }

    /// Restore a live job and replace only the unconsumed suffix of its outer
    /// source. Any prefix edit is rejected rather than guessed through.
    pub fn resume_checkpoint(
        bytes: &[u8],
        source: &str,
    ) -> Result<(Self, CheckpointSite), CheckpointError> {
        let body = Self::check_checkpoint_header(bytes)?;
        let CheckpointRead {
            mut engine,
            site,
            runtime,
        } = postcard::from_bytes(body).map_err(|_| CheckpointError::Corrupt)?;

        engine.interner.borrow_mut().rebuild_ids();
        engine.mag_set = runtime.mag_set;
        engine.term_offset = runtime.term_offset;
        engine.term_watermark = runtime.term_watermark;
        engine.logonly_since = runtime.logonly_since;
        engine.every_job_done = runtime.every_job_done;
        engine.halt_on_error = runtime.halt_on_error;
        engine.halt_requested = runtime.halt_requested;
        engine.force_dvi_output = runtime.force_dvi_output;
        engine.retained_page_meta = runtime.retained_page_meta;
        engine.long_help_seen = runtime.long_help_seen;
        engine.job_name_read = runtime.job_name_read;
        engine.clock_reads = runtime.clock_reads;
        engine.checkpoint_request = None;
        engine.checkpoint_hit = None;

        // `serde(skip)` uses type defaults, not the engine's off sentinels.
        engine.dbg_catcode_watch = -1;
        engine.dbg_arena_watch = -1;
        engine.dbg_us_depth = -1;
        engine.dbg_trace_win = (u64::MAX, 0);
        engine.dbg_tok_win = (u64::MAX, 0);
        engine.dbg_trace_cap = 4000;
        engine.dbg_catcode_log.clear();
        engine.dbg_on = engine.trace_defs
            || engine.trace_hot
            || engine.dbg_runaway_n != 0
            || engine.dbg_arg_cap != 0
            || engine.dbg_group;

        engine.resume_prepared_checkpoint(site, source)
    }

    /// Rebind an already-decoded live checkpoint to an edited outer source.
    ///
    /// A watch host may prepare one bounded checkpoint between requests so the
    /// next edit avoids postcard decoding. This performs the same complete
    /// provenance check as [`Self::resume_checkpoint`]; only the decoding and
    /// runtime-field restoration have happened in advance.
    pub fn resume_prepared_checkpoint(
        mut self,
        site: CheckpointSite,
        source: &str,
    ) -> Result<(Self, CheckpointSite), CheckpointError> {
        if self.safe_checkpoint_site().as_ref() != Some(&site)
            || site.byte_offset > source.len()
            || !source.is_char_boundary(site.byte_offset)
            || source[..site.byte_offset].chars().count() != site.char_offset
        {
            return Err(CheckpointError::SourcePrefixChanged);
        }
        let file = self
            .files
            .get_mut(site.file_index)
            .ok_or(CheckpointError::Corrupt)?;
        if file.chars.len() < site.char_offset
            || !source[..site.byte_offset]
                .chars()
                .eq(file.chars[..site.char_offset].iter().copied())
        {
            return Err(CheckpointError::SourcePrefixChanged);
        }
        file.chars = source.chars().collect();
        Ok((self, site))
    }
}
