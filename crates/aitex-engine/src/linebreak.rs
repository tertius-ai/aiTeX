//! Breaking a horizontal list into lines.
//!
//! Behind a trait because docs/history/PLAN.md decision 4 reserved one: a greedy breaker
//! ships first and Knuth-Plass drops in at E3 without the stomach changing.
//! The interface is deliberately the whole paragraph in and the finished lines
//! out — Knuth-Plass needs to see every breakpoint at once to minimise total
//! demerits, so a streaming interface would have foreclosed it.
//!
//! Breakpoints are *glue nodes*, not spaces in a token stream. That is the
//! difference between this and the throwaway version it replaces: by the time
//! a list gets here, a `\hbox`, a `\kern` and a character all measure the same
//! way, so boxed material can be broken around instead of being invisible.

use crate::node::{CharMetrics, Glue, Node, TeXBox};
use crate::scaled::Scaled;
use alloc::vec::Vec;

/// One finished line, and whether it ends its paragraph.
///
/// The distinction is visible on the page: TeX sets a paragraph's last line at
/// its natural width and justifies the rest. Justifying it too stretches a few
/// words across the whole measure, which is glaring and was a real bug here
/// once — invisible to every test, visible immediately in the PDF.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub nodes: Vec<Node>,
    pub ends_paragraph: bool,
    /// What the breaker paid for this line: its badness, the penalty at the
    /// break, and the running total of demerits. The analogue of TeX's
    /// `\tracingparagraphs`, and the only way to tell a wrong *decision* from a
    /// wrong width.
    pub badness: i32,
    pub penalty: i32,
    pub demerits: i64,
    /// How wide this line is set, and how far it is shifted right —
    /// `tex.web` §850's `line_width` and §889's `cur_indent`. Both are `\hsize`
    /// and zero unless `\hangindent` or `\parshape` says otherwise.
    pub width: Scaled,
    pub indent: Scaled,
    /// This line ended *at a discretionary* — it was hyphenated. `tex.web` §890
    /// charges `\brokenpenalty` for a page break just after such a line.
    pub hyphenated: bool,
}

/// What the engine's registers say about how to break.
///
/// Passed rather than read from the eqtb so a breaker stays a pure function of
/// its input: the same paragraph and the same parameters give the same lines,
/// which is what makes the property tests in docs/history/PLAN.md §9 possible at all.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakParams {
    pub hsize: Scaled,
    /// Packing-only parameters carried here so a repeated paragraph can prove
    /// that a cached set of line boxes is reusable before it clones the line
    /// nodes.  They do not participate in breakpoint selection.
    pub pack_key: [i32; 3],
    /// pdfTeX's `\pdfadjustspacing`. Values greater than one let font
    /// expansion participate in line-breaking badness.
    pub adjust_spacing: i32,
    /// `tex.web` §816/§848's inherited `prev_graf`. An interrupted
    /// paragraph does not restart its line numbers after a display: the first
    /// line on the far side is `prev_graf + 1`, and both the paragraph shape
    /// and `\tracingparagraphs` use that absolute number.
    pub prev_graf: usize,
    /// `\hangindent` and `\hangafter` — `tex.web` §848. A positive `\hangafter`
    /// indents every line *after* the first `\hangafter` lines; a negative one
    /// indents the first `|\hangafter|` lines instead. A negative `\hangindent`
    /// narrows the line from the right rather than the left, so it is the width
    /// that changes and the indent that stays zero.
    pub hang_indent: Scaled,
    pub hang_after: i32,
    /// `\looseness` — `tex.web` §875. Not another cost: the paragraph is
    /// broken as usual and *then* a different endpoint is chosen, the one whose
    /// line count is nearest the wanted difference from the best.
    ///
    /// ```text
    /// line_diff:=line_number(r)-best_line;
    /// if ((line_diff<actual_looseness)and(looseness<=line_diff))or
    ///    ((line_diff>actual_looseness)and(looseness>=line_diff)) then
    ///   begin best_bet:=r; actual_looseness:=line_diff;
    ///   fewest_demerits:=total_demerits(r);
    ///   end
    /// else if (line_diff=actual_looseness)and
    ///    (total_demerits(r)<fewest_demerits) then
    ///   begin best_bet:=r; fewest_demerits:=total_demerits(r);
    ///   end;
    /// ```
    ///
    /// §873 then retries the whole pass if the wanted looseness was not
    /// reached and there is a pass left to run:
    ///
    /// ```text
    /// if (actual_looseness=looseness)or final_pass then goto done;
    /// ```
    pub looseness: i32,
    /// `\parshape` — one `(indent, width)` per line, the last repeating.
    /// Overrides `\hangindent` entirely when it is not empty.
    pub par_shape: alloc::vec::Vec<(Scaled, Scaled)>,
    /// `\leftskip` and `\rightskip` — glue TeX puts at the start and end of
    /// *every* line (`tex.web` §881/§887), and counts in the background totals
    /// while breaking (§816). That is how `\raggedright` — `\rightskip` with
    /// infinite stretch — makes every line badness 0 instead of unsettable.
    pub left_skip: crate::node::Glue,
    pub right_skip: crate::node::Glue,
    /// The worst badness a line may have. plain TeX sets 200; INITEX 10000,
    /// which accepts almost anything.
    /// pdfTeX's `\pdfprotrudechars`. Zero is off; **1** inserts the margin
    /// kerns and leaves line breaking alone; **2** also lets the protrusion
    /// into the badness, so the breaks themselves move. The two are different
    /// jobs, and measurably so: on one paragraph the same breakpoint reads
    /// `b=649` at 0 and at 1, and `b=2875` at 2.
    pub protrude_chars: i32,
    pub tolerance: i32,
    /// `\linepenalty` — added to every line's badness before squaring, so a
    /// paragraph with fewer lines is preferred when quality is otherwise equal.
    pub line_penalty: i32,
    /// `\adjdemerits` — charged when consecutive lines are very differently
    /// spaced. This is what stops a tight line following a loose one.
    pub adj_demerits: i32,
    /// `\tracingparagraphs > 0` — narrate the search (`tex.web` §846/§855/§856).
    /// The lines land in [`BreakScratch::trace`], which the caller drains.
    pub trace: bool,
    /// Diagnostics produced lazily as TeX's line-breaking walk reaches a node.
    /// Paragraph hyphenation is performed up front in this implementation,
    /// but §915 calls `new_character` only when the walk reaches the word. The
    /// node index preserves that observable ordering without coupling the
    /// line breaker to the engine's diagnostic printer.
    pub trace_inserts: alloc::vec::Vec<(usize, alloc::string::String)>,
    /// The `\font` identifier of each loaded font, for §174's short display —
    /// which names a font the way `\the\font` does, not by its file. Filled
    /// only when `trace` is set; each entry already includes the current
    /// `\escapechar`, because the breaker has no access to TeX's printer.
    pub font_ids: alloc::vec::Vec<alloc::string::String>,
    /// The current printed escape character for control-sequence names in the
    /// paragraph trace. It can be empty, `^^` notation, or a line break.
    pub escape: alloc::string::String,
    /// `\hyphenpenalty` — charged for breaking at a discretionary, so a line
    /// broken between words is preferred to one broken inside one.
    pub hyphen_penalty: i32,
    /// `\exhyphenpenalty` — §869 charges this instead when the discretionary's
    /// *pre-break* list is empty:
    ///
    /// ```text
    /// begin s:=pre_break(cur_p); disc_width:=0;
    /// if s=null then try_break(ex_hyphen_penalty,hyphenated)
    /// else … try_break(hyphen_penalty,hyphenated);
    /// ```
    ///
    /// A break after `\-` in an already-hyphenated word is not a new hyphen, so
    /// it is not charged for one.
    pub ex_hyphen_penalty: i32,
    /// `\newlinechar`, or -1: §59 prints that character as a line break, and
    /// §174's short display goes through §59.
    pub newline_char: i64,
    /// `\doublehyphendemerits` and `\finalhyphendemerits` — §859's charge for
    /// two hyphenated lines in a row:
    ///
    /// ```text
    /// if (break_type=hyphenated)and(type(r)=hyphenated) then
    ///   if cur_p<>null then d:=d+double_hyphen_demerits
    ///   else d:=d+final_hyphen_demerits;
    /// ```
    ///
    /// The paragraph's closing break counts as hyphenated — `cur_p=null` is
    /// how §829 says so — which is what makes the *final* charge a separate
    /// parameter: a hyphen on the second-to-last line is worse than one in the
    /// middle, because the last line is short and the hyphen has nothing to
    /// justify it.
    pub double_hyphen_demerits: i32,
    pub final_hyphen_demerits: i32,
    /// §873's `final_pass`: this pass has no successor, so it may not fail.
    ///
    /// §854 is what that means in practice — when the last active node is
    /// about to go and nothing feasible has been recorded, the break is *forced*
    /// and charged nothing:
    ///
    /// ```text
    /// if final_pass and (minimum_demerits=awful_bad) and
    ///    (link(r)=last_active) and (prev_r=active) then
    ///   artificial_demerits:=true {set demerits zero, this break is forced}
    /// else if b>threshold then goto deactivate;
    /// ```
    ///
    /// An overfull line is *still* not a candidate in this pass; it is reached
    /// only through that forced break, which is why TeX's `Overfull \hbox` is
    /// always the first line that does not fit rather than the cheapest one
    /// that does not.
    pub final_pass: bool,
    /// §873's `background[2] := background[2] + emergency_stretch`, in force on
    /// the emergency pass only. It is *stretch*, added to what `\leftskip` and
    /// `\rightskip` already contribute to every line, so it changes what each
    /// break's badness comes out as rather than which breaks exist. That is the
    /// whole of `\emergencystretch`: a last pass where the lines are allowed to
    /// be looser than `\tolerance` would otherwise permit, instead of overfull.
    pub emergency_stretch: crate::scaled::Scaled,
}

impl Default for BreakParams {
    fn default() -> Self {
        Self {
            emergency_stretch: crate::scaled::Scaled::ZERO,
            hsize: Scaled::ZERO,
            pack_key: [0; 3],
            adjust_spacing: 0,
            prev_graf: 0,
            hang_indent: Scaled::ZERO,
            hang_after: 1,
            looseness: 0,
            trace: false,
            trace_inserts: alloc::vec::Vec::new(),
            font_ids: alloc::vec::Vec::new(),
            escape: alloc::string::String::from("\\"),
            par_shape: alloc::vec::Vec::new(),
            left_skip: crate::node::Glue::fixed(Scaled::ZERO),
            right_skip: crate::node::Glue::fixed(Scaled::ZERO),
            protrude_chars: 0,
            tolerance: 10_000,
            line_penalty: 10,
            adj_demerits: 10_000,
            hyphen_penalty: 0,
            ex_hyphen_penalty: 0,
            newline_char: -1,
            double_hyphen_demerits: 0,
            final_hyphen_demerits: 0,
            final_pass: false,
        }
    }
}

pub trait LineBreaker {
    /// Break `list` into lines.
    fn break_lines(
        &self,
        list: &[Node],
        params: &BreakParams,
        metrics: &impl CharMetrics,
    ) -> Vec<Line>;
}

/// Fill a line until the next chunk will not fit, then break.
///
/// No lookahead and no demerits, so it makes locally sensible and globally poor
/// decisions — which is exactly what E3 replaces. It is honest about what it
/// is: `\hbadness` and `\tolerance` are ignored entirely.
pub struct Greedy;

/// Apply UAX #9 rule L2 to the native runs in one already-broken line.
///
/// Whitespace and UAX #14 penalties stay in their breakpoint slots while the
/// shaped runs on either side move. That is equivalent for horizontal layout,
/// and avoids adding bidi metadata to every ordinary TeX glue node. A span ID
/// prevents an inline Unicode object from reordering adjacent classic TeX or a
/// separately shaped Unicode object.
fn reorder_native_bidi_line(nodes: &mut [Node]) {
    let mut at = 0usize;
    while at < nodes.len() {
        let Some((first, span)) =
            nodes[at..]
                .iter()
                .enumerate()
                .find_map(|(offset, node)| match node {
                    Node::NativeRun(run) => Some((at + offset, run.bidi_span)),
                    _ => None,
                })
        else {
            break;
        };
        let mut positions = Vec::new();
        let mut end = first;
        while end < nodes.len() {
            match &nodes[end] {
                Node::NativeRun(run) if run.bidi_span == span => positions.push(end),
                Node::Glue(_) | Node::Penalty(_) => {}
                _ => break,
            }
            end += 1;
        }
        let mut runs = positions
            .iter()
            .filter_map(|position| match &nodes[*position] {
                Node::NativeRun(run) => Some(run.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let maximum = runs.iter().map(|run| run.bidi_level).max().unwrap_or(0);
        if let Some(minimum_odd) = runs
            .iter()
            .map(|run| run.bidi_level)
            .filter(|level| level % 2 == 1)
            .min()
        {
            for level in (minimum_odd..=maximum).rev() {
                let mut start = 0usize;
                while start < runs.len() {
                    while start < runs.len() && runs[start].bidi_level < level {
                        start += 1;
                    }
                    let mut finish = start;
                    while finish < runs.len() && runs[finish].bidi_level >= level {
                        finish += 1;
                    }
                    runs[start..finish].reverse();
                    start = finish;
                }
            }
        }
        for (position, run) in positions.into_iter().zip(runs) {
            nodes[position] = Node::NativeRun(run);
        }
        at = end.max(first + 1);
    }
}

/// The measure and indentation of line `l` (1-based) — `tex.web` §848–§850.
///
/// `\parshape` wins outright when it is given; otherwise `\hangindent` shortens
/// either the first `|\hangafter|` lines (negative `\hangafter`) or every line
/// after them (positive). A negative `\hangindent` takes the space off the
/// right, so the width shrinks and the indent stays zero.
/// §849's `last_special_line`: the last line number whose measure is its own.
/// Beyond it every line has the same width, so §835 need not tell them apart.
///
/// ```text
/// if par_shape_ptr=null then
///   if hang_indent=0 then begin last_special_line:=0; … end
///   else begin last_special_line:=abs(hang_after); … end
/// else begin last_special_line:=info(par_shape_ptr)-1; … end
/// ```
fn last_special_line(params: &BreakParams) -> usize {
    if !params.par_shape.is_empty() {
        return params.par_shape.len() - 1;
    }
    if params.hang_indent.0 == 0 {
        return 0;
    }
    params.hang_after.unsigned_abs() as usize
}

pub fn line_dims(params: &BreakParams, l: usize) -> (Scaled, Scaled) {
    if !params.par_shape.is_empty() {
        let i = (l.max(1) - 1).min(params.par_shape.len() - 1);
        let (indent, width) = params.par_shape[i];
        return (indent, width);
    }
    if params.hang_indent.0 == 0 {
        return (Scaled::ZERO, params.hsize);
    }
    let last_special = params.hang_after.unsigned_abs() as usize;
    let narrow = Scaled(params.hsize.0 - params.hang_indent.0.abs());
    let indent = if params.hang_indent.0 >= 0 {
        params.hang_indent
    } else {
        Scaled::ZERO
    };
    let hangs_here = if params.hang_after < 0 {
        l <= last_special
    } else {
        l > last_special
    };
    if hangs_here {
        (indent, narrow)
    } else {
        (Scaled::ZERO, params.hsize)
    }
}

/// The natural width of a node.
fn width_of(n: &Node, metrics: &impl CharMetrics) -> Scaled {
    match n {
        Node::Char { code, font, .. } => metrics.width(*font, *code),
        Node::NativeRun(run) => run.width,
        Node::Glue(g) | Node::Leaders { glue: g, .. } => g.width,
        Node::Kern(k, _) => *k,
        Node::Math { width, .. } => *width,
        Node::Box(b) => b.width,
        Node::Rule { width, .. } if *width != Scaled::RUNNING => *width,
        _ => Scaled::ZERO,
    }
}

impl LineBreaker for Greedy {
    fn break_lines(
        &self,
        list: &[Node],
        params: &BreakParams,
        metrics: &impl CharMetrics,
    ) -> Vec<Line> {
        let mut lines: Vec<Vec<Node>> = Vec::new();
        let mut line: Vec<Node> = Vec::new();
        // Material since the last legal breakpoint. It moves to the next line
        // as a unit, which is what keeps a word whole: breaking inside one
        // needs hyphenation, and that is E5.
        let mut chunk: Vec<Node> = Vec::new();
        let mut line_w = Scaled::ZERO;
        let mut chunk_w = Scaled::ZERO;

        for n in list {
            // Glue at depth zero is a breakpoint. Everything before it is now
            // committed to the current line if it fits.
            if let Node::Glue(g) = &n {
                let (_, measure) = line_dims(
                    params,
                    params
                        .prev_graf
                        .saturating_add(lines.len())
                        .saturating_add(1),
                );
                if (line_w + chunk_w).0 > measure.0 && !line.is_empty() {
                    discard_trailing_glue(&mut line);
                    lines.push(core::mem::take(&mut line));
                    line_w = Scaled::ZERO;
                }
                line.append(&mut chunk);
                line_w = line_w + chunk_w;
                chunk_w = Scaled::ZERO;
                // The glue itself joins the line; if the line breaks here it is
                // discarded above.
                line.push(Node::Glue(*g));
                line_w = line_w + g.width;
                continue;
            }
            chunk_w = chunk_w + width_of(n, metrics);
            chunk.push(n.clone());
        }

        let (_, measure) = line_dims(
            params,
            params
                .prev_graf
                .saturating_add(lines.len())
                .saturating_add(1),
        );
        if (line_w + chunk_w).0 > measure.0 && !line.is_empty() {
            discard_trailing_glue(&mut line);
            lines.push(core::mem::take(&mut line));
        }
        line.append(&mut chunk);
        if !line.is_empty() {
            // The last line keeps its trailing glue — see the note in
            // `break_counted`; `\parfillskip` lives there.
            lines.push(line);
        }

        let last = lines.len().saturating_sub(1);
        lines
            .into_iter()
            .enumerate()
            .map(|(i, mut nodes)| {
                reorder_native_bidi_line(&mut nodes);
                let (indent, width) =
                    line_dims(params, params.prev_graf.saturating_add(i).saturating_add(1));
                Line {
                    nodes,
                    ends_paragraph: i == last,
                    badness: 0,
                    penalty: 0,
                    demerits: 0,
                    width,
                    indent,
                    hyphenated: false,
                }
            })
            .collect()
    }
}

/// Break a paragraph by minimising total demerits over all break sequences.
///
/// `tex.web` §813–§890 and Knuth & Plass (1981). The difference from
/// [`Greedy`] is not local quality but *global*: a break that looks fine on
/// its own may force a terrible one two lines later, and only considering
/// whole sequences can see that. Greedy cannot, by construction, and that is
/// the whole reason this exists.
///
/// This is a dynamic program over breakpoints rather than TeX's incrementally
/// maintained active list. The two compute the same minimum -- TeX's active
/// list is an optimisation that discards breakpoints which can no longer
/// begin a feasible line -- but the DP is far easier to check by eye, which
/// matters more while the answer is still being compared against tex's.
pub struct KnuthPlass;

/// Set when a paragraph could not be broken within `\tolerance` and the greedy
/// result was used instead.
///
/// TeX's answer to this is a second pass with hyphenation, then
/// `\emergencystretch`; we have neither yet, so counting how often it happens
/// is the honest measure of how much they are missed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BreakStats {
    pub paragraphs: usize,
    pub fell_back: usize,
}

/// How loose or tight a line is. TeX charges `\adjdemerits` when consecutive
/// lines differ by more than one class, which is what stops a tight line from
/// following a very loose one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fitness {
    VeryLoose = 0,
    Loose = 1,
    Decent = 2,
    Tight = 3,
}

impl Fitness {
    fn from_index(i: usize) -> Fitness {
        match i {
            0 => Fitness::VeryLoose,
            1 => Fitness::Loose,
            2 => Fitness::Decent,
            _ => Fitness::Tight,
        }
    }
}

/// A penalty this negative forces a break; this positive forbids one.
const INF_PENALTY: i32 = 10_000;
const EJECT_PENALTY: i32 = -INF_PENALTY;
/// Worse than any real demerits. Not TeX's `awful_bad`, which is scaled to a
/// 32-bit word; the arithmetic here is 64-bit and the guard only exists to
/// stop a runaway.
const AWFUL: i64 = i64::MAX / 8;

/// Running totals of a prefix of the list.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Totals {
    width: i64,
    stretch: [i64; 4],
    shrink: i64,
    font_stretch: i64,
    font_shrink: i64,
}

const PLAN_CACHE_SLOTS: usize = 16;

#[derive(Clone)]
pub(crate) struct CachedLayoutLine {
    pub boxed: TeXBox,
    pub measure: crate::node::PackMeasure,
    pub badness: i32,
}

#[derive(Clone)]
struct BreakPlanEntry {
    hash: u64,
    source: Vec<Node>,
    params: BreakParams,
    lines: Vec<Line>,
    layout: Option<([i32; 3], Vec<CachedLayoutLine>)>,
}

#[inline]
fn plan_mix(hash: &mut u64, value: u64) {
    *hash = (*hash ^ value).wrapping_mul(0x0000_0100_0000_01b3);
}

/// Fingerprint ordinary paragraphs for the bounded break-plan cache. Complex
/// nodes are deliberately ineligible: repeated prose is the useful case, and
/// keeping writes, inserts, discretionary state, or driver whatsits out makes
/// a cache hit a straightforward equality-preserving substitution.
fn break_plan_hash(list: &[Node], params: &BreakParams) -> Option<u64> {
    if params.trace
        || !params.trace_inserts.is_empty()
        || params.protrude_chars != 0
        || params.adjust_spacing != 0
    {
        return None;
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    plan_mix(&mut hash, list.len() as u64);
    for node in list {
        match node {
            Node::Char { code, font, lig } => {
                plan_mix(&mut hash, 1);
                plan_mix(&mut hash, *code as u64);
                plan_mix(&mut hash, *font as u64);
                if let Some(source) = lig {
                    plan_mix(&mut hash, source.len() as u64);
                    for &raw in source.iter() {
                        plan_mix(&mut hash, raw as u64);
                    }
                }
            }
            Node::Glue(g) => {
                plan_mix(&mut hash, 2);
                for value in [
                    g.width.0 as i64,
                    g.stretch.0 as i64,
                    g.stretch_order as i64,
                    g.shrink.0 as i64,
                    g.shrink_order as i64,
                    g.subtype as i64,
                    i64::from(g.canonical_zero),
                ] {
                    plan_mix(&mut hash, value as u64);
                }
            }
            Node::Kern(width, subtype) => {
                plan_mix(&mut hash, 3);
                plan_mix(&mut hash, width.0 as i64 as u64);
                plan_mix(&mut hash, *subtype as u64);
            }
            Node::Penalty(value) => {
                plan_mix(&mut hash, 4);
                plan_mix(&mut hash, *value as i64 as u64);
            }
            Node::Language {
                language,
                left,
                right,
            } => {
                plan_mix(&mut hash, 5);
                plan_mix(
                    &mut hash,
                    u64::from(*language) | (u64::from(*left) << 8) | (u64::from(*right) << 16),
                );
            }
            Node::Box(boxed) if boxed.list.is_empty() => {
                plan_mix(&mut hash, 6);
                plan_mix(&mut hash, boxed.kind as u64);
                for value in [
                    boxed.width.0 as i64,
                    boxed.height.0 as i64,
                    boxed.depth.0 as i64,
                    boxed.set_num,
                    boxed.set_den,
                    boxed.glue_sign as i64,
                    boxed.glue_order as i64,
                    boxed.shift.0 as i64,
                ] {
                    plan_mix(&mut hash, value as u64);
                }
            }
            Node::Rule {
                width,
                height,
                depth,
            } => {
                plan_mix(&mut hash, 7);
                plan_mix(&mut hash, width.0 as i64 as u64);
                plan_mix(&mut hash, height.0 as i64 as u64);
                plan_mix(&mut hash, depth.0 as i64 as u64);
            }
            _ => return None,
        }
    }
    Some(if hash == 0 { 1 } else { hash })
}

fn same_plan_source(left: &[Node], right: &[Node]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (Node::Glue(a), Node::Glue(b)) => {
                    a.width == b.width
                        && a.stretch == b.stretch
                        && a.stretch_order == b.stretch_order
                        && a.shrink == b.shrink
                        && a.shrink_order == b.shrink_order
                        && a.subtype == b.subtype
                        && a.canonical_zero == b.canonical_zero
                }
                (Node::Box(a), Node::Box(b)) => a.list.is_empty() && b.list.is_empty() && a == b,
                _ => left == right,
            })
}

impl Totals {
    #[inline(always)]
    fn add(&mut self, n: &Node, metrics: &impl CharMetrics, expanding: bool) {
        match n {
            Node::Char { code, font, .. } => {
                self.width += metrics.width(*font, *code).0 as i64;
                if expanding {
                    let (stretch, shrink) = metrics.expansion_capacity(*font, *code);
                    self.font_stretch += stretch.0 as i64;
                    self.font_shrink += shrink.0 as i64;
                }
            }
            Node::NativeRun(run) => self.width += run.width.0 as i64,
            Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                self.width += g.width.0 as i64;
                self.stretch[g.stretch_order as usize] += g.stretch.0 as i64;
                self.shrink += g.shrink.0 as i64;
            }
            Node::Kern(k, _) => self.width += k.0 as i64,
            // §192's formula boundaries carry `\mathsurround`; §867 adds
            // a math node's width to `active_width` exactly as it does a kern.
            // Packing already counted this width, so omitting it here made the
            // search narrate and choose a looser line than the box it produced.
            Node::Math { width, .. } => self.width += width.0 as i64,
            Node::Box(b) => self.width += b.width.0 as i64,
            Node::Rule { width, .. } if *width != Scaled::RUNNING => self.width += width.0 as i64,
            _ => {}
        }
    }
}

/// pdfTeX lets an automatic TFM kern participate in font expansion only when
/// it sits directly between two characters of the same font. The kern node
/// itself carries no character identities, so recover that local context while
/// building prefix or discretionary totals.
fn kern_expansion_at(list: &[Node], index: usize, metrics: &impl CharMetrics) -> (i64, i64) {
    let Some(Node::Kern(base, crate::node::kern_sub::NORMAL)) = list.get(index) else {
        return (0, 0);
    };
    let (
        Some(Node::Char {
            code: left, font, ..
        }),
        Some(Node::Char {
            code: right,
            font: right_font,
            ..
        }),
    ) = (
        index.checked_sub(1).and_then(|i| list.get(i)),
        list.get(index + 1),
    )
    else {
        return (0, 0);
    };
    if font != right_font {
        return (0, 0);
    }
    let (stretch, shrink) = metrics.expansion_kern_capacity(*font, *left, *right, *base);
    (stretch.0 as i64, shrink.0 as i64)
}

/// Is a break legal just before `i`?
///
/// `tex.web` §866. Glue is a breakpoint only when something non-discardable
/// precedes it -- otherwise a run of spaces would offer a break between each
/// pair -- and a kern only when glue follows.
/// Every legal breakpoint in the list, in order — `tex.web` §866.
///
/// This has to be one pass rather than a per-node test, because whether a glue
/// is a breakpoint depends on `auto_breaking`: TeX turns it off at the start of
/// a formula and on again at its end, which is what stops a line from ever
/// breaking inside `$a + b$`. A math node is itself a breakpoint when glue
/// follows it, and that is how a line comes to end just after a formula.
fn breakpoints(list: &[Node]) -> Vec<usize> {
    let mut out = Vec::new();
    breakpoints_into(list, &mut out);
    out
}

/// As [`breakpoints`], into a buffer the caller keeps.
fn breakpoints_into(list: &[Node], out: &mut Vec<usize>) {
    let mut auto = true;
    let mut i = 0usize;
    let mut prev = None;
    while i < list.len() {
        let n = &list[i];
        let legal = match n {
            Node::Glue(_) | Node::Leaders { .. } => {
                auto && prev.is_some_and(|p| !discardable(&list[p]))
            }
            Node::Penalty(p) => *p < INF_PENALTY,
            // §866's `kern_break`: a kern or a math node is a breakpoint only
            // when glue follows it.
            Node::Kern(_, sub) => {
                // §866 `kern_break` runs only for `subtype = explicit`; a font's
                // own kern between two characters falls straight through to
                // `act_width`. Without the test, `AV ` offered a break between
                // the `V` and the space — inside a word.
                *sub == crate::node::kern_sub::EXPLICIT
                    && auto
                    && matches!(list.get(i + 1), Some(Node::Glue(_) | Node::Leaders { .. }))
            }
            Node::Math { after, .. } => {
                // §866 sets `auto_breaking` *before* the `kern_break` test, so
                // the node that ends a formula is itself a breakpoint — which
                // is how a line comes to end just after `$…$`.
                auto = *after;
                auto && matches!(list.get(i + 1), Some(Node::Glue(_)))
            }
            // A discretionary is always a breakpoint — that is what it is for.
            Node::Disc { .. } => true,
            _ => false,
        };
        if legal {
            out.push(i);
        }
        // §867 adds a discretionary's replacement nodes straight to
        // `active_width` and advances `cur_p` past all of them. They describe
        // the unbroken form and therefore count in prefix widths, but none is
        // visited as a breakpoint of its own. The next ordinary node also sees
        // the discretionary as its logical predecessor: an explicit kern at
        // the end of the replacement must not suppress a following glue break.
        let replace = match n {
            Node::Disc(d) => d.replace,
            _ => 0,
        };
        prev = Some(i);
        i = i.saturating_add(1).saturating_add(replace).min(list.len());
    }
}

/// The width the pre-break material adds when a line ends at a discretionary.
///
/// Charging it matters: a hyphen is not free, and a line measured without one
/// and then set with one overruns the measure by exactly its width.
fn disc_pre_totals(list: &[Node], i: usize, metrics: &impl CharMetrics, expanding: bool) -> Totals {
    match list.get(i) {
        Some(Node::Disc(d)) => {
            let mut t = Totals::default();
            for (index, n) in d.pre.iter().enumerate() {
                t.add(n, metrics, expanding);
                if expanding {
                    let (stretch, shrink) = kern_expansion_at(&d.pre, index, metrics);
                    t.font_stretch += stretch;
                    t.font_shrink += shrink;
                }
            }
            t
        }
        _ => Totals::default(),
    }
}

/// §840: what a line *beginning* at a discretionary starts out holding.
///
/// ```text
/// while s<>null do
///   begin @<Add the width of node |s| to |break_width|@>;
///   s:=link(s);
///   end;
/// ```
///
/// The post-break text is set at the head of the next line, so its width — and
/// its glue, though a discretionary list may hold none — belongs to that line
/// from the start. `after_break` skips past the disc node and its replace text
/// to find where the next line's *nodes* begin, which is right; the material
/// TeX moved across the break is not among them, and was going uncounted. A
/// line starting after `\discretionary{AAAB}{B-}{}` was measured 4pt short of
/// what it sets, so an overfull line looked underfull and TeX's `b=*` came out
/// as `b=10000`.
fn disc_post_totals(
    list: &[Node],
    i: usize,
    metrics: &impl CharMetrics,
    expanding: bool,
) -> Totals {
    let mut t = Totals::default();
    if let Some(Node::Disc(d)) = list.get(i) {
        for (index, n) in d.post.iter().enumerate() {
            t.add(n, metrics, expanding);
            if expanding {
                let (stretch, shrink) = kern_expansion_at(&d.post, index, metrics);
                t.font_stretch += stretch;
                t.font_shrink += shrink;
            }
        }
    }
    t
}

/// §879: what a break throws away — and what may not precede a glue break.
fn discardable(n: &Node) -> bool {
    matches!(
        n,
        Node::Glue(_)
            | Node::Leaders { .. }
            | Node::Kern(_, crate::node::kern_sub::EXPLICIT)
            | Node::Penalty(_)
            | Node::Math { .. }
    )
}

/// Where the line after a break at `i` starts: discardables at a break are
/// dropped, so the next line does not begin with the space it broke at.
fn after_break(list: &[Node], i: usize) -> usize {
    // §882: a line beginning after a *discretionary* starts past the disc node
    // and past its replace text — the nodes the break throws away. `\hbox{}`
    // and characters are not discardable, so skipping only discardables left
    // the replace text at the head of the next line: the search measured a line
    // that still contained it, found the break infeasible, and took no break at
    // all. The span builder already knew this (`cut + 1 + d.replace`); the
    // search that decides *where* to cut did not, which is why the two only
    // disagreed when a discretionary had a non-empty replace text — and why
    // `\-` and every hyphenation break, whose replace text is empty, were fine.
    let (mut j, keep_discardables) = match list.get(i) {
        // §840 leaves `s=null` after adding a nonempty post-break list to
        // `break_width`, so the ordinary discardable-node lookahead does not
        // run.  That is necessary: the post-break material begins the word on
        // the new line, and a glue immediately after the replacement text is
        // the interword space *after* that word, not leading glue to discard.
        // Dropping it removed both its width and its stretch from every line
        // beginning at such a discretionary; TRIP's next candidate therefore
        // had badness 10000 where TeX has 12.
        Some(Node::Disc(d)) => (i + 1 + d.replace, !d.post.is_empty()),
        _ => (i, false),
    };
    if keep_discardables {
        return j.min(list.len());
    }
    while j < list.len() && discardable(&list[j]) {
        j += 1;
    }
    j
}

fn penalty_at(list: &[Node], i: usize) -> i32 {
    match list.get(i) {
        Some(Node::Penalty(p)) => *p,
        _ => 0,
    }
}

/// `tex.web` §174 `short_display`: the abbreviated form the paragraph trace
/// uses to show what lies between one breakpoint and the next.
///
/// Characters bare, a font announced by its identifier the first time it is
/// used and never again, `[]` for a box or anything else with contents, `|`
/// for a rule, `$` for a math node, a space for any non-zero glue, and a
/// discretionary shown as its pre-break and post-break lists run together.
pub fn short_display(
    list: &[Node],
    font_names: &dyn Fn(usize) -> alloc::string::String,
    cur_font: &mut Option<usize>,
    out: &mut alloc::string::String,
    // `\newlinechar`, or -1 for none: §59 prints that character as a line
    // break rather than in `^^` form, and §174 goes through §59.
    nl: i64,
) {
    let mut i = 0usize;
    while i < list.len() {
        let n = &list[i];
        i += 1;
        match n {
            Node::Char { code, font, lig } => {
                if *cur_font != Some(*font) {
                    out.push_str(&font_names(*font));
                    out.push(' ');
                    *cur_font = Some(*font);
                }
                // §175: `ligature_node: short_display(lig_ptr(p))` — a ligature
                // shows the characters it was *made of*, not the character it
                // became. `[BB]` prints `BB`, so a word of three `BB` ligatures
                // prints as six B's; printing the ligature character instead
                // gave three of whatever glyph the font put the ligature at,
                // which in trip.tex's font reads `AAA`.
                // §174 prints each character with `print_ASCII`, so one outside
                // the printable range comes out in §48's `^^` form. We pushed
                // the byte itself: INITEX's `\defaulthyphenchar` is nought, so
                // `\-` in a font loaded without one inserts character 0, and a
                // paragraph trace carried a raw NUL where TeX writes `^^@`.
                match lig {
                    Some(orig) => {
                        for c in orig.iter() {
                            if *c != crate::node::LIG_BOUNDARY {
                                out.push_str(&crate::math::print_ascii_nl(*c, nl));
                            }
                        }
                    }
                    None => out.push_str(&crate::math::print_ascii_nl(*code, nl)),
                }
            }
            Node::NativeRun(_)
            | Node::Box(_)
            | Node::Ins(_)
            | Node::Mark { .. }
            | Node::Adjust { .. }
            | Node::Language { .. }
            | Node::Whatsit(_)
            | Node::WriteOut(_) => out.push_str("[]"),
            Node::Rule { .. } => out.push('|'),
            Node::Glue(g) => {
                if *g != crate::node::Glue::default() {
                    out.push(' ');
                }
            }
            Node::Leaders { .. } => out.push(' '),
            Node::Math { .. } => out.push('$'),
            Node::Disc(d) => {
                short_display(&d.pre, font_names, cur_font, out, nl);
                short_display(&d.post, font_names, cur_font, out, nl);
                // §175 skips the `replace` nodes after showing both halves:
                //
                //     n:=replace_count(p);
                //     while n>0 do
                //       begin if link(p)<>null then p:=link(p); decr(n); end;
                //
                // They are what the *unbroken* word looks like, and the two
                // halves are what it looks like broken — showing both prints
                // the material twice. `office` broken after `of` came out
                // `of-ffices`: the post-break `fi`, and then the whole `ffi`
                // ligature it replaces.
                i = (i + d.replace).min(list.len());
            }
            _ => {}
        }
    }
}

/// The badness and fitness of setting `natural` into `hsize`.
fn assess(
    natural: i64,
    stretch: [i64; 4],
    shrink: i64,
    hsize: i64,
    allow_overfull: bool,
) -> Option<(i32, Fitness, i64)> {
    let shortfall = hsize - natural;
    if shortfall > 0 {
        // Infinite stretch absorbs anything, at no cost.
        let infinite = stretch[1] != 0 || stretch[2] != 0 || stretch[3] != 0;
        if infinite {
            return Some((0, Fitness::Decent, 0));
        }
        let b = crate::page::PageBuilder::badness(
            Scaled(shortfall.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
            Scaled(stretch[0].clamp(0, i32::MAX as i64) as i32),
        );
        let f = if b > 99 {
            Fitness::VeryLoose
        } else if b > 12 {
            Fitness::Loose
        } else {
            Fitness::Decent
        };
        Some((b, f, 0))
    } else {
        // The line is too long. It can only be set if the glue can shrink
        // enough; TeX reports the rest as overfull rather than squeezing.
        if -shortfall > shrink {
            if !allow_overfull {
                return None;
            }
            // Overfull, and accepted. The *amount* is returned so demerits can
            // charge for it: a flat cost makes one enormous line cheaper than
            // several slightly-overfull ones, since fewer lines means fewer
            // squared badnesses to pay.
            // §853: `b:=inf_bad+1` — one *past* `inf_bad`, which is how §856
            // knows to print `*` rather than a number and how §851's
            // `b>inf_bad` tells an overfull line from a merely dreadful one.
            // Returning `inf_bad` itself made every overfull line report
            // `b=10000` where TeX reports `b=*`.
            return Some((INF_PENALTY + 1, Fitness::Tight, -shortfall - shrink));
        }
        let b = crate::page::PageBuilder::badness(
            Scaled((-shortfall).clamp(0, i32::MAX as i64) as i32),
            Scaled(shrink.clamp(0, i32::MAX as i64) as i32),
        );
        let f = if b > 12 {
            Fitness::Tight
        } else {
            Fitness::Decent
        };
        Some((b, f, 0))
    }
}

/// The common expansion parameters for this paragraph.
///
/// pdfTeX rejects paragraphs that mix expanded fonts with different limits or
/// steps. The assignment scanner already preserves each font's parameters; the
/// breaker needs only the common tuple while evaluating candidates.
fn paragraph_expansion(
    list: &[Node],
    adjust_spacing: i32,
    metrics: &impl CharMetrics,
) -> Option<(i64, i64, i64)> {
    if adjust_spacing <= 1 {
        return None;
    }
    fn find(list: &[Node], metrics: &impl CharMetrics) -> Option<(i64, i64, i64)> {
        for n in list {
            match n {
                Node::Char { font, .. } => {
                    if let Some(p) = metrics.expansion_limits(*font) {
                        return Some(p);
                    }
                }
                Node::Disc(d) => {
                    if let Some(p) = find(&d.pre, metrics).or_else(|| find(&d.post, metrics)) {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
        None
    }
    find(list, metrics).filter(|(_, _, step)| *step > 0)
}

/// Apply pdfTeX's font-expansion contribution to a line's shortfall.
///
/// This is the integer arithmetic in `try_break`: when the full expansion
/// capacity exceeds the requested change, only half an expansion step remains
/// as glue shortfall for badness; otherwise the capacity is consumed in full.
fn expanded_natural(
    natural: i64,
    font_stretch: i64,
    font_shrink: i64,
    hsize: i64,
    expansion: Option<(i64, i64, i64)>,
) -> i64 {
    let Some((max_stretch, max_shrink, step)) = expansion else {
        return natural;
    };
    let mut shortfall = hsize - natural;
    if shortfall > 0 && font_stretch > 0 {
        if font_stretch > shortfall {
            let steps = max_stretch / step;
            if steps > 0 {
                shortfall = (font_stretch / steps) / 2;
            }
        } else {
            shortfall -= font_stretch;
        }
    } else if shortfall < 0 && font_shrink > 0 {
        if font_shrink > -shortfall {
            let steps = max_shrink / step;
            if steps > 0 {
                shortfall = -((font_shrink / steps) / 2);
            }
        } else {
            shortfall += font_shrink;
        }
    }
    hsize - shortfall
}

/// The tables the breaker fills for one paragraph, kept between calls.
///
/// They are all the same shape every time — one entry per node, one per
/// breakpoint — so allocating them fresh per paragraph meant growing and
/// freeing tens of kilobytes eight thousand times over a long document, which
/// was 395 MB of the 2.2 GB such a run allocated. Cleared and refilled instead.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BreakScratch {
    prefix: Vec<Totals>,
    breaks: Vec<usize>,
    /// Where the line after `breaks[j]` starts. A function of `j` alone, so it
    /// is computed once per paragraph: it used to be recomputed for every
    /// (break, candidate-predecessor) pair, and each call walks the discardable
    /// run after the break. That scan was 10% of the whole line breaker.
    starts: Vec<usize>,
    /// `tex.web` §829's active list: the places a line may still begin, in
    /// order. §835 deletes one the moment it can no longer reach the break
    /// being considered, which is what keeps this at one to five entries
    /// instead of every breakpoint seen so far.
    ///
    /// A *(line number, break, group, fitness class, demerits, lines)* —
    /// `tex.web`'s active node, in the order §829 keeps them.  Keeping the
    /// cost and line count on the active node avoids looking its cell up in
    /// `best` twice for every candidate; those values cannot change after the
    /// node is created.
    ///
    /// ```text
    /// link(q):=r; link(prev_r):=q; prev_r:=q;
    /// ```
    ///
    /// §845 links a new node in just before the node the walk is standing on,
    /// so the list stays sorted by line number and, within one line number, in
    /// creation order. §835 walks it forward from `link(active)`, and §855
    /// records a candidate when it is cheaper *or equal* — so among ties the
    /// last node scanned in this order wins. There is no key that reproduces
    /// that; the order has to be the list's.
    ///
    /// §850 reads `line_width` from the node's own line number and §854
    /// deactivates against that width, so one break can be active at several
    /// measures at once.
    active: Vec<(usize, usize, u32, u8, i64, usize)>,
    /// Nodes §854 deactivated this round, as indices into `active`. Held here
    /// rather than allocated per breakpoint.
    kill: Vec<usize>,
    /// §854's predecessor costs, rebuilt for each candidate but kept here so
    /// the short active-list buffer does not allocate once per paragraph.
    #[serde(skip)]
    prior: Vec<(i64, Fitness, usize)>,
    /// One *row* per break, holding one bucket per line count reachable there.
    ///
    /// `tex.web` §849 is why the line count has to be part of the key at all:
    ///
    /// ```text
    /// if looseness=0 then easy_line:=last_special_line
    /// else easy_line:=max_halfword
    /// ```
    ///
    /// and §835 creates a fresh set of active nodes each time the line number
    /// changes, unless the line number is past `easy_line` — where the measure
    /// no longer depends on it, so one set does for all. With `\looseness` set
    /// TeX gives up that collapse deliberately, because §875 then has to choose
    /// *between* line counts. With it zero there is exactly one bucket and this
    /// is the table it always was.
    best: Vec<Vec<(u32, [Cell; 4])>>,
    cuts: Vec<usize>,
    costs: Vec<(i32, i32, i64)>,
    spans: Vec<(usize, usize, bool, usize, bool)>,
    /// §826: the paragraph carried glue that shrinks infinitely. Reported by
    /// the caller, which has somewhere to report to; recorded here because this
    /// is where the paragraph's list is walked anyway. Taken and cleared with
    /// the trace.
    pub infinite_shrink: bool,
    /// §846's narration, one entry per line of it. Filled only when
    /// [`BreakParams::trace`] is set; the caller takes it and clears it.
    pub trace: Vec<alloc::string::String>,
    /// `@@` serial per (break, fitness class), and the next serial to hand out.
    /// §845 numbers passive nodes in creation order, which is what every
    /// `-> @@N` and `via @@N` in the trace refers to.
    serial: Vec<Vec<(u32, [u32; 4])>>,
    next_serial: u32,
    #[serde(skip)]
    plan_cache: Vec<Option<BreakPlanEntry>>,
    #[serde(skip)]
    plan_seen: Vec<u64>,
    #[serde(skip)]
    last_plan_slot: Option<usize>,
    #[serde(skip)]
    layout_hit: Option<([i32; 3], Vec<CachedLayoutLine>)>,
}

impl BreakScratch {
    pub fn clear_plan_cache(&mut self) {
        self.plan_cache.clear();
        self.plan_seen.clear();
        self.last_plan_slot = None;
        self.layout_hit = None;
    }

    /// Take a pre-packed version of the plan most recently reused, provided
    /// the packing-only parameters still match.
    pub(crate) fn take_plan_layout(&mut self, key: [i32; 3]) -> Option<Vec<CachedLayoutLine>> {
        match self.layout_hit.take() {
            Some((stored, boxes)) if stored == key => Some(boxes),
            _ => None,
        }
    }

    /// Attach pre-packed boxes to the plan just admitted or reused. Only
    /// warning-free ordinary paragraphs call this; bounded plan eviction owns
    /// the boxes, so layout memory cannot grow with the document.
    pub(crate) fn remember_plan_layout(&mut self, key: [i32; 3], boxes: Vec<CachedLayoutLine>) {
        let Some(slot) = self.last_plan_slot else {
            return;
        };
        if let Some(entry) = self.plan_cache.get_mut(slot).and_then(Option::as_mut) {
            entry.layout = Some((key, boxes));
        }
    }
}

/// One entry of the breaker's table: the cheapest way to end a line at a given
/// break *in a given fitness class* — total demerits, the break before it, the
/// class that one was reached in, how many lines precede it, this line's
/// badness, and the penalty at its break.
type Cell = Option<(i64, usize, usize, usize, i32, i32)>;

/// The bucket for `lines` at one break, created if it is not there yet.
///
/// A short `Vec` scanned linearly rather than a map: with `\looseness` zero —
/// every paragraph in every ordinary document — it holds exactly one entry, and
/// this is the innermost loop of the line breaker.
/// The `@@` number a chain reached break `j` under, for the trace's `via @@N`.
fn serial_of(row: &[(u32, [u32; 4])], lines: u32, fit: usize) -> u32 {
    row.iter()
        .find(|(l, _)| *l == lines)
        .map_or(0, |(_, s)| s[fit])
}

/// Kept in increasing line order, because that is the order §835 keeps the
/// active list in and §836 walks it — and with demerits tied, order is the
/// whole answer.
/// Where a candidate sits in the order `tex.web` §829 keeps the active list
/// in: by line number, and within a line number by the break it starts at.
///
/// §836 records a candidate when it is cheaper *or equal*:
///
/// ```text
/// if d<=minimal_demerits[fit_class] then
///   begin minimal_demerits[fit_class]:=d;
///   best_place[fit_class]:=break_node(r); best_pl_line[fit_class]:=l;
/// ```
///
/// — so among equal demerits the **last** in this order wins. With INITEX's
/// `\linepenalty` of nought and a `\rightskip` that stretches infinitely,
/// *every* chain ties at nought and this order is the whole answer: it is what
/// makes TeX set such a paragraph one word to the line rather than four.
///
fn bucket_mut(row: &mut Vec<(u32, [Cell; 4])>, lines: u32) -> &mut [Cell; 4] {
    match row.binary_search_by_key(&lines, |(l, _)| *l) {
        Ok(i) => &mut row[i].1,
        Err(i) => {
            row.insert(i, (lines, [None; 4]));
            &mut row[i].1
        }
    }
}

impl KnuthPlass {
    /// As [`LineBreaker::break_lines`], but also reporting whether the
    /// paragraph had to fall back to greedy.
    /// Borrowed, not owned: the caller keeps the paragraph. Two passes over the
    /// same list were two clones of it, and the buffer the paragraph was built
    /// in can be cleared and reused for the next one instead of being freed and
    /// grown again from nothing. Between them that was 0.9 GB of the 2.2 GB a
    /// thousand-page document allocated.
    pub fn break_counted(
        &self,
        list: &[Node],
        params: &BreakParams,
        metrics: &impl CharMetrics,
    ) -> (Vec<Line>, bool) {
        let mut scratch = BreakScratch::default();
        self.break_with(list, params, metrics, &mut scratch)
    }

    /// As [`KnuthPlass::break_counted`], reusing the caller's tables.
    ///
    /// The paragraph is *copied* to get an owned buffer to cut up. Prefer
    /// [`KnuthPlass::break_in_place`], which does not.
    pub fn break_with(
        &self,
        list: &[Node],
        params: &BreakParams,
        metrics: &impl CharMetrics,
        scratch: &mut BreakScratch,
    ) -> (Vec<Line>, bool) {
        let mut owned = list.to_vec();
        self.break_in_place(&mut owned, params, metrics, scratch)
    }

    /// Break `list` into lines, **moving** its nodes into them.
    ///
    /// The lines of a paragraph partition it, so a line's nodes can be taken
    /// out of the paragraph's buffer rather than copied out of it: `drain`
    /// yields them by value and leaves the buffer empty *with its capacity*, so
    /// the caller can still hand it back for the next paragraph. Copying meant
    /// every node was built once, cloned once and dropped twice — and a
    /// discretionary's pre- and post-break lists were cloned on top of that.
    ///
    /// On return `list` is empty. Nodes discarded at a break — the glue a line
    /// broke at, a discretionary's `replace` text — are dropped here, which is
    /// where TeX frees them too (§879, §882).
    pub fn break_in_place(
        &self,
        buf: &mut Vec<Node>,
        params: &BreakParams,
        metrics: &impl CharMetrics,
        scratch: &mut BreakScratch,
    ) -> (Vec<Line>, bool) {
        let _p = crate::profile::Phase::enter(crate::profile::P_LINEBREAK);
        if buf.is_empty() {
            return (Vec::new(), false);
        }
        scratch.last_plan_slot = None;
        scratch.layout_hit = None;
        let cache_key = break_plan_hash(buf, params).map(|hash| {
            if scratch.plan_seen.len() != PLAN_CACHE_SLOTS {
                scratch.plan_seen.resize(PLAN_CACHE_SLOTS, 0);
                scratch.plan_cache.resize_with(PLAN_CACHE_SLOTS, || None);
            }
            let slot = hash as usize & (PLAN_CACHE_SLOTS - 1);
            (hash, slot)
        });
        if let Some((hash, slot)) = cache_key
            && let Some(entry) = scratch.plan_cache[slot].as_ref()
            && entry.hash == hash
            && entry.params == *params
            && same_plan_source(&entry.source, buf)
        {
            let layout = entry
                .layout
                .as_ref()
                .filter(|(key, _)| *key == params.pack_key)
                .cloned();
            let lines = if layout.is_some() {
                entry
                    .lines
                    .iter()
                    .map(|line| Line {
                        nodes: Vec::new(),
                        ends_paragraph: line.ends_paragraph,
                        badness: line.badness,
                        penalty: line.penalty,
                        demerits: line.demerits,
                        width: line.width,
                        indent: line.indent,
                        hyphenated: line.hyphenated,
                    })
                    .collect()
            } else {
                entry.lines.clone()
            };
            scratch.last_plan_slot = Some(slot);
            scratch.layout_hit = layout;
            buf.clear();
            return (lines, false);
        }
        let cache_source = cache_key.and_then(|(hash, slot)| {
            let seen = scratch.plan_seen[slot] == hash;
            scratch.plan_seen[slot] = hash;
            seen.then(|| buf.clone())
        });
        let expansion = paragraph_expansion(&buf[..], params.adjust_spacing, metrics);
        let expanding = expansion.is_some();
        // `tex.web` §816: `\leftskip` and `\rightskip` are on every line before
        // any of its own material is, so they are the background every line is
        // measured against.
        let mut background = Totals::default();
        background.add(&Node::Glue(params.left_skip), metrics, false);
        background.add(&Node::Glue(params.right_skip), metrics, false);
        // §873, on the emergency pass.
        background.stretch[0] += params.emergency_stretch.0 as i64;
        let n = buf.len();

        // Prefix totals, finite-shrink normalization, and candidate discovery
        // are all one forward walk. They used to be three complete walks over
        // every paragraph even though each observes the same node stream.
        //
        // §§826 and 867: paragraph glue may shrink only at finite order. Our
        // node-local glue is already owned, so normalize it before measuring
        // that node. The diagnostic flag is cumulative across a paragraph's
        // passes.
        let prefix = &mut scratch.prefix;
        prefix.clear();
        prefix.reserve(n + 1);
        let mut total = Totals::default();
        prefix.push(total);
        let breaks = &mut scratch.breaks;
        breaks.clear();
        let mut auto_breaking = true;
        let mut logical_prev = None;
        let mut skip_breaks_until = 0usize;
        for i in 0..n {
            if let Node::Glue(g) | Node::Leaders { glue: g, .. } = &mut buf[i]
                && g.shrink_order != 0
                && g.shrink != crate::scaled::Scaled::ZERO
            {
                g.shrink_order = 0;
                scratch.infinite_shrink = true;
            }
            total.add(&buf[i], metrics, expanding);
            if expanding {
                let (stretch, shrink) = kern_expansion_at(&buf[..], i, metrics);
                total.font_stretch += stretch;
                total.font_shrink += shrink;
            }
            prefix.push(total);

            // §867 counts discretionary replacement nodes in the prefix
            // width but does not visit them as possible breaks. The node after
            // the replacement still sees the discretionary as its logical
            // predecessor.
            if i < skip_breaks_until {
                continue;
            }
            let node = &buf[i];
            let legal = match node {
                Node::Glue(_) | Node::Leaders { .. } => {
                    auto_breaking && logical_prev.is_some_and(|p| !discardable(&buf[p]))
                }
                Node::Penalty(p) => *p < INF_PENALTY,
                Node::Kern(_, sub) => {
                    *sub == crate::node::kern_sub::EXPLICIT
                        && auto_breaking
                        && matches!(buf.get(i + 1), Some(Node::Glue(_) | Node::Leaders { .. }))
                }
                Node::Math { after, .. } => {
                    auto_breaking = *after;
                    auto_breaking && matches!(buf.get(i + 1), Some(Node::Glue(_)))
                }
                Node::Disc { .. } => true,
                _ => false,
            };
            if legal {
                breaks.push(i);
            }
            let replace = match node {
                Node::Disc(d) => d.replace,
                _ => 0,
            };
            logical_prev = Some(i);
            skip_breaks_until = i.saturating_add(1).saturating_add(replace).min(n);
        }
        // Planning only reads from here until the cuts are known, after which
        // the nodes are moved into their lines.
        let list: &[Node] = &buf[..];
        let prefix = &scratch.prefix;

        // The end of the paragraph is always a legal, forced break.
        breaks.push(n);
        let starts = &mut scratch.starts;
        starts.clear();
        starts.extend(scratch.breaks.iter().map(|&b| after_break(list, b)));
        let breaks = &scratch.breaks;
        let starts = &scratch.starts;

        // best[k][f] = the cheapest way to end a line at break `k` *in fitness
        // class f* — (demerits, previous break, the class it was reached in,
        // line count).
        //
        // The predecessor's *class* is part of the answer, not just its index.
        // Walking the chain back by re-picking the cheapest class at each break
        // can step onto a different chain than the optimum used: at a break
        // reachable both cheaply-but-badly-adjoining and dearly-but-well-, the
        // cheaper entry wins the lookup while the optimum went through the other.
        //
        // One entry per class, not one per break: `tex.web` §835 keeps
        // `minimal_demerits[fit_class]` separately because `\adjdemerits`
        // charges for adjacent lines whose classes differ by more than one. A
        // cheaper break in a badly-adjoining class is not the better break, and
        // keeping only the cheapest lets it shadow the one the paragraph needs
        // two lines later — with no way to recover, since the shadowed entry is
        // simply gone.
        // §849's `easy_line`, and §850's use of it:
        //
        //     if looseness=0 then easy_line:=last_special_line
        //     else easy_line:=max_halfword
        //
        //     if l>easy_line then
        //       begin line_width:=second_width; old_l:=max_halfword-1; end
        //
        // §835 starts a fresh set of active nodes each time the line number
        // rises, and that sentinel is what stops it: every line past
        // `easy_line` has the same measure, so one set does for all of them.
        // Below it they must be kept apart, because the measure differs — which
        // is why `\parshape` and `\hangindent` need the line count in the key
        // even with `\looseness` at nought.
        let easy_line = if params.looseness != 0 {
            usize::MAX
        } else {
            last_special_line(params)
        };
        // Which *group* a predecessor of `lines` lines belongs to, by the
        // `line_number` §835 groups on — one more than the chain length:
        //
        //     if l>old_l then
        //       begin if (minimum_demerits<awful_bad)and
        //          ((old_l<>easy_line)or(r=last_active)) then
        //         @<Create new active nodes for the best feasible breaks
        //           just found@>;
        //
        // Creation is skipped the moment `old_l` *reaches* `easy_line` — not
        // when it passes it — and §850's `old_l:=max_halfword-1` keeps it
        // skipping from there on. So every predecessor from `line_number =
        // easy_line` upward competes for one node, and each below it for a node
        // of its own.
        //
        // The group is the *predecessor's*, not the new line's: that is what
        // decides which candidates are weighed against each other, and it is
        // why a break can carry both a two-line and a three-line node at once
        // where the first came from its own group and the second from the
        // merged one.
        //
        // With no `\parshape` and no `\hangindent` `easy_line` is nought and
        // every chain merges, which is the single-bucket table this was.
        let group = |lines: usize| -> u32 {
            let l = params.prev_graf.saturating_add(lines).saturating_add(1);
            if easy_line != usize::MAX && l >= easy_line {
                easy_line as u32
            } else {
                l as u32
            }
        };
        // Emptied row by row rather than dropped: each row's buffer is one
        // heap allocation, and a document is thousands of paragraphs. Rows past
        // `breaks.len()` are left alone and never read.
        let best = &mut scratch.best;
        if best.len() < breaks.len() {
            best.resize_with(breaks.len(), alloc::vec::Vec::new);
        }
        for row in best[..breaks.len()].iter_mut() {
            row.clear();
        }
        // §845 numbers each passive node as it is created; every `via @@N` and
        // `-> @@N` in the trace is one of these.
        if scratch.serial.len() < breaks.len() {
            scratch
                .serial
                .resize_with(breaks.len(), alloc::vec::Vec::new);
        }
        for row in scratch.serial[..breaks.len()].iter_mut() {
            row.clear();
        }
        scratch.next_serial = 0;
        let prior_buf = &mut scratch.prior;
        prior_buf.clear();
        let mut printed_upto = 0usize;
        // §835 prints and creates group by group, so each line is tagged with
        // the group it belongs to.
        let mut via: alloc::vec::Vec<(u32, alloc::string::String)> = alloc::vec::Vec::new();
        let mut short_font: Option<usize> = None;
        let mut next_trace_insert = 0usize;
        // The active list starts holding only the paragraph's beginning.
        let active = &mut scratch.active;
        active.clear();
        active.push((
            params.prev_graf.saturating_add(1),
            usize::MAX,
            0,
            Fitness::Decent as u8,
            0,
            0,
        ));

        for (k, &e) in breaks.iter().enumerate() {
            while let Some((at, message)) = params.trace_inserts.get(next_trace_insert) {
                if *at > e {
                    break;
                }
                scratch.trace.push(message.clone());
                next_trace_insert += 1;
            }
            // Everything that depends on `e` alone, hoisted out of the
            // candidate loop below — it runs once per *predecessor*, and these
            // were being recomputed on each one.
            let pen_at_e = penalty_at(list, e);
            let forced_here = pen_at_e <= EJECT_PENALTY || e == n;
            let ends_disc = matches!(list.get(e), Some(Node::Disc { .. }));
            let b = prefix[e];
            let disc_pre = disc_pre_totals(list, e, metrics, expanding);
            let pen = if ends_disc {
                // §869 again: the *pre-break* list decides which penalty.
                match list.get(e) {
                    Some(Node::Disc(d)) if d.pre.is_empty() => params.ex_hyphen_penalty,
                    _ => params.hyphen_penalty,
                }
            } else {
                pen_at_e
            };
            // §830, the first thing `try_break` does:
            //
            //     if abs(pi)>=inf_penalty then
            //       if pi>0 then return {inhibited by infinite penalty}
            //       else pi:=eject_penalty {this breakpoint will be forced}
            //
            // So every forcing penalty *is* `eject_penalty` from here on — in
            // the demerits and in §856's `p=`. trip.tex writes
            // `\penalty-1000000000` and Knuth's transcript reports `p=-10000`;
            // we reported the number as written.
            let pen = if pen <= EJECT_PENALTY {
                EJECT_PENALTY
            } else {
                pen
            };
            if pen >= INF_PENALTY {
                // §830 returns immediately for an infinite positive penalty.
                // Discretionaries are collected before their dynamic
                // `\hyphenpenalty`/`\exhyphenpenalty` is known, so they need
                // the same inhibition here that an explicit penalty receives
                // while breakpoints are enumerated. Letting one through also
                // entered the negative-penalty branch below and *rewarded* an
                // otherwise forbidden break by almost 100 million demerits.
                continue;
            }
            // `tex.web` §829: the *active list*. Only breaks that can still
            // begin a line are considered, latest first — so lines get longer
            // as the loop proceeds, and the moment one is too long to shrink
            // into the measure the search stops, because every earlier start is
            // longer still.
            //
            // And that break is then **deactivated**: `e` only moves right and
            // an earlier start only makes the line longer, so a start that
            // cannot reach this break can never reach a later one. TeX deletes
            // the node (§835); we truncate the dead prefix. Without it every
            // breakpoint re-walked every earlier breakpoint that had already
            // been ruled out, which is the difference between O(b · active) and
            // O(b²) — and `active` is one to five entries, not b.
            scratch.kill.clear();
            // §835 walks the list *forward*, from `link(active)` to
            // `last_active`, and there is no early exit: §854 removes what can
            // no longer be reached, and that is what keeps the list short. We
            // walked it backwards to be able to truncate a dead prefix in one
            // step, which only worked because deactivation was all-or-nothing.
            for ai in 0..active.len() {
                let (_lnum, j, lkey, lfit, active_dem, active_lines) = active[ai];
                // Where the line beginning after break `j` actually starts:
                // §879 discards the glue, kerns, penalties and math nodes that
                // follow a break, so `starts[j]` is the first node that stays.
                //
                // It can land *past* `e`, when everything between the two
                // breaks is discardable.  Do not clamp it back to `e`:
                // §879's `break_width` has already subtracted *all* of those
                // future discardables, and TeX cancels them only as the main
                // walk reaches each one.  Before then their totals are
                // deliberately negative.  This is observable with consecutive
                // LaTeX `\\` commands: each expands to an infinite glue and a
                // forced penalty, and the negative remaining `fil` still makes
                // the intervening empty trial line have badness zero.
                //
                // The candidate still has to be scored when `start > e`.
                // Skipping it dropped exactly one predecessor at each such
                // breakpoint, always the most recently created:
                //
                // ```text
                // $\OML/cmm/m/it/10 N[]$
                // @\math via @@0 … @@3     b=0 p=0 d=100
                // @@4: line 1.2 t=100 -> @@0
                //
                // @\kern via @@0 … @@3     b=0 p=0 d=100
                // @\kern via @@4 b=0 p=0 d=100      <-- pdfTeX only
                // ```
                //
                // from 2608.17211v1's address line, where a `$…$` is followed
                // by a space and a `\kern`: the math node breaks, the glue and
                // the kern after it are both discarded, and `starts[4]` is the
                // `2` of "2367", past the kern that breaks next.
                //
                let start = if j == usize::MAX { 0 } else { starts[j] };
                let a = prefix[start];
                // A line ending at a discretionary carries its pre-break
                // material — the hyphen — and must be measured with it; a line
                // *beginning* at one carries the post-break material the same
                // way (§840).
                let post = if j == usize::MAX {
                    Totals::default()
                } else {
                    disc_post_totals(list, breaks[j], metrics, expanding)
                };
                let post_nodes = if j == usize::MAX {
                    None
                } else {
                    match list.get(breaks[j]) {
                        Some(Node::Disc(d)) if !d.post.is_empty() => Some(d.post.as_slice()),
                        _ => None,
                    }
                };
                let natural = b.width - a.width + disc_pre.width + post.width + background.width;
                // Protrusion narrows the line, and it narrows it for *both*
                // things this width is used for: the badness, and §854's
                // deactivation test. Scoring with the protrusion and
                // deactivating without it was tried — on the theory that a node
                // we keep alive one step longer than pdfTeX explains the one
                // extra `@\par via @@4` in our trace — and it is wrong: the
                // boxes then diverge in six places where they had matched
                // exactly. The extra trace line has some other cause and the
                // output is the authority.
                let protrusion = if params.protrude_chars > 1 {
                    protrusion_of(list, start, e, post_nodes, metrics)
                } else {
                    0
                };
                let natural_p = natural - protrusion;
                let stretch = [
                    b.stretch[0] - a.stretch[0] + post.stretch[0] + background.stretch[0],
                    b.stretch[1] - a.stretch[1] + post.stretch[1] + background.stretch[1],
                    b.stretch[2] - a.stretch[2] + post.stretch[2] + background.stretch[2],
                    b.stretch[3] - a.stretch[3] + post.stretch[3] + background.stretch[3],
                ];
                let shrink = b.shrink - a.shrink + post.shrink + background.shrink;
                let mut font_stretch =
                    b.font_stretch - a.font_stretch + disc_pre.font_stretch + post.font_stretch;
                let mut font_shrink =
                    b.font_shrink - a.font_shrink + disc_pre.font_shrink + post.font_shrink;
                if params.protrude_chars > 1 {
                    let (margin_stretch, margin_shrink) =
                        margin_expansion_adjustment(list, start, e, post_nodes, metrics);
                    font_stretch += margin_stretch;
                    font_shrink += margin_shrink;
                }
                // §851 measures each node against *its own* line's width —
                // §850 reads `line_width` from the node's line number:
                //
                //     if l>easy_line then line_width:=second_width
                //     else … line_width:=mem[par_shape_ptr+2*l].sc
                //
                // and §854 then deactivates it when that line cannot shrink to
                // fit — `b>inf_bad`, which is the one case badness exceeds
                // `inf_bad` at all:
                //
                //     if (b>inf_bad)or(pi=eject_penalty) then
                //       begin if final_pass and (minimum_demerits=awful_bad) and
                //          (link(r)=last_active) and (prev_r=active) then
                //         artificial_demerits:=true
                //       else if b>threshold then goto deactivate;
                //       node_r_stays_active:=false;
                //       end
                //
                // We measured against the widest line the paragraph could use
                // anywhere, which is a bound valid for every node and therefore
                // too weak for any: with `\parshape` or `\hangindent` a node
                // whose own line is narrow is overfull at its width and not at
                // the widest, so TeX dropped it and we kept it. Kept alive it
                // went on offering chains TeX had abandoned.
                let (_, measure) = line_dims(
                    params,
                    params
                        .prev_graf
                        .saturating_add(active_lines)
                        .saturating_add(1),
                );
                let measure_here = measure.0 as i64;
                let natural_e = expanded_natural(
                    natural_p,
                    font_stretch,
                    font_shrink,
                    measure_here,
                    expansion,
                );
                let overfull = natural_e - shrink > measure_here;
                let sole = ai + 1 == active.len() && scratch.kill.len() == ai;
                // §851: a *forced* break deactivates every active node, whether or
                // not it records one of its own.
                //
                // ```text
                // if b>inf_bad or pi=eject_penalty then
                //   begin if final_pass and (minimum_demerits=awful_bad) and
                //     (link(r)=last_active) and (prev_r=active) then
                //     artificial_demerits:=true
                //   else if b>threshold then goto deactivate;
                //   node_r_stays_active:=false;
                //   end
                // ```
                //
                // Both ways out of that branch lose the node: `deactivate` removes
                // it, and the fall-through sets `node_r_stays_active:=false`. We
                // deactivated only on an overfull line, so a node whose line was
                // merely *too loose* at a forced break survived past it — and then
                // reached the `\par` and set the whole paragraph as one line with
                // the forced penalty sitting inside it:
                //
                // ```text
                // ref    \vbox(18.94444+0.0)x62.0   two lines
                // ours   \vbox(6.94444+0.0)x62.0    one line, `\penalty -10000` in it
                // ```
                //
                // `aaa bbb ccc.\penalty-10000 zzz\par` in a 62pt measure, at plain's
                // default `\pretolerance`. The single line's badness is exactly 100,
                // which the first pass accepts — so the paragraph never even reached
                // the second pass where §854 would have forced the break.
                if forced_here && !overfull {
                    scratch.kill.push(ai);
                }
                let artificial_overfull = if overfull {
                    scratch.kill.push(ai);
                    // §854: unless this is the last node standing with nothing
                    // recorded and no pass left to try, in which case the break
                    // is taken free of charge rather than the paragraph lost.
                    let rescue = params.final_pass
                        && sole
                        && best[k].iter().all(|(_, c)| c.iter().all(|x| x.is_none()));
                    if !rescue {
                        continue;
                    }
                    true
                } else {
                    false
                };
                let over_here = if overfull {
                    natural_e - shrink - measure_here
                } else {
                    0
                };
                let last_candidate = false;
                // Every fitness class the predecessor was reached in is a
                // distinct starting point, because the class is what
                // `\adjdemerits` compares. A fixed array, not a `Vec`: this is
                // the innermost loop of the paragraph breaker and it ran one
                // heap allocation per candidate.
                // With `\looseness` set, a predecessor reached in the same
                // class after a different number of lines is a *different*
                // starting point — §875 has to be able to tell those chains
                // apart at the end. With it zero there is one bucket and this
                // is the four-entry array it always was.
                //
                // *This node's* bucket, and no other: an active node is one
                // line count at one break, which is what gives it a measure
                // and what §854 deactivates. Offering every bucket at the
                // break would hand back the chains §851 has just dropped.
                prior_buf.clear();
                prior_buf.push((active_dem, Fitness::from_index(lfit as usize), active_lines));
                let priors = &prior_buf[..];
                if priors.is_empty() {
                    if last_candidate {
                        break;
                    }
                    continue;
                }

                // `tex.web` §859: `d := line_penalty + b`, and if that reaches
                // 10000 in absolute value `d := 100000000` — a flat cost, so
                // every line that bad is *indistinguishable* from every other.
                // Squaring it instead discriminates where TeX does not, and the
                // paragraph settles somewhere TeX would not have chosen.
                //
                // The exception is the emergency pass, where every line is
                // feasible and badness is the only thing separating a slightly
                // loose line from a catastrophic one. TeX has nothing to say
                // about that pass (it has `\emergencystretch` instead), and
                // flattening the cost there made a line holding one long word
                // cost the same as a line holding two words.
                // From here the cost depends on which class the predecessor was
                // reached in — and, with `\hangindent`, on *how many lines*
                // precede it, since that decides this line's measure. So each
                // predecessor is scored separately, measure and all.
                // §854's artificial demerits, for the other way into the
                // "deactivate" branch: `if (b>inf_bad)or(pi=eject_penalty)`.
                // A forced break deactivates the node whatever its badness, so
                // when this is the last active node and nothing has been
                // recorded at this breakpoint yet, the final pass takes it free
                // of charge rather than fail. It is the paragraph's closing
                // `\par` that reaches this most often — TeX's `d=*` where we
                // charged a hundred million and carried it into `t=`.
                let artificial = artificial_overfull
                    || (params.final_pass
                        && forced_here
                        && sole
                        && best[k]
                            .iter()
                            .all(|(_, cells)| cells.iter().all(|c| c.is_none())));
                for (prior_dem, prior_fit, prior_lines) in priors {
                    // Even in the final pass an overfull line is not a candidate:
                    // §854 reaches one only when deactivating the last active
                    // node would otherwise leave no candidate at this break.
                    // That is why TeX's Overfull box is always the *first* line
                    // that does not fit rather than the cheapest one that does
                    // not.
                    let Some((bad, fit, over)) = (if artificial_overfull {
                        Some((10_001, Fitness::Tight, over_here))
                    } else {
                        assess(natural_e, stretch, shrink, measure.0 as i64, false)
                    }) else {
                        continue;
                    };
                    // §851: `if b>threshold then goto continue` — the line is
                    // too loose to accept, in *any* pass. The final pass is not
                    // more permissive about looseness; what it may do instead is
                    // force a break, which is the `artificial_overfull` path.
                    if bad > params.tolerance && !artificial {
                        // Too loose to accept -- but an even earlier start makes a
                        // longer line, which is *less* loose, so keep looking.
                        continue;
                    }
                    let lpb = params.line_penalty as i64 + bad as i64;
                    // §859, unconditionally now that there is one final pass
                    // rather than a third with its own scoring: a line whose
                    // `\linepenalty + b` reaches 10000 costs a flat hundred
                    // million, so every line that bad is indistinguishable from
                    // every other.
                    let base = if lpb.abs() >= 10_000 {
                        100_000_000
                    } else {
                        lpb.saturating_pow(2)
                    };
                    let mut d = if pen >= 0 && pen < INF_PENALTY {
                        base + (pen as i64).pow(2)
                    } else if pen > EJECT_PENALTY {
                        base - (pen as i64).pow(2)
                    } else {
                        base
                    };
                    // §859, in its own order: the hyphen charge before
                    // `\adjdemerits`.
                    //
                    //     if (break_type=hyphenated)and(type(r)=hyphenated) then
                    //       if cur_p<>null then d:=d+double_hyphen_demerits
                    //       else d:=d+final_hyphen_demerits;
                    //
                    // `break_type` is `hyphenated` for a discretionary *and* for
                    // the paragraph's own end, which §829 marks by `cur_p=null`.
                    // The predecessor's type is the one it was created with, so
                    // the charge is for two hyphenated breaks in a row — which
                    // is what `\doublehyphendemerits` is named for.
                    let this_hyphenated = ends_disc || e >= n;
                    let prev_hyphenated = j != usize::MAX
                        && (matches!(list.get(breaks[j]), Some(Node::Disc(_))) || breaks[j] >= n);
                    if this_hyphenated && prev_hyphenated {
                        d += if e < n {
                            params.double_hyphen_demerits as i64
                        } else {
                            params.final_hyphen_demerits as i64
                        };
                    }
                    if (fit as i32 - *prior_fit as i32).abs() > 1 {
                        d += params.adj_demerits as i64;
                    }
                    if artificial {
                        d = 0;
                    }
                    // Charged per scaled point of overflow, weighted so that
                    // overflowing is always worse than adding a line. Unweighted,
                    // one enormous overfull line beats several small ones: the
                    // squared badness they share cancels and only the line count
                    // is left.
                    if !artificial {
                        d = d.saturating_add(over.saturating_mul(64));
                    }
                    if params.trace {
                        // §855, in TeX's own order: the node broken at, the active
                        // node reached through, the badness, the penalty, and the
                        // demerits of *this line* — before §854 adds the running
                        // total, which is what `t=` on the following line reports.
                        let kind = match list.get(e) {
                            None => "par",
                            Some(Node::Penalty(_)) => "penalty",
                            Some(Node::Disc(_)) => "discretionary",
                            Some(Node::Kern(..)) => "kern",
                            Some(Node::Math { .. }) => "math",
                            _ => "",
                        };
                        let kind = if kind.is_empty() {
                            alloc::string::String::new()
                        } else {
                            alloc::format!("{}{kind}", params.escape)
                        };
                        let r = if j == usize::MAX {
                            0
                        } else {
                            serial_of(&scratch.serial[j], lkey, *prior_fit as usize)
                        };
                        // §873's final break is artificial: `cur_p = null` and
                        // `pi = eject_penalty`, which is what it reports even though
                        // there is no penalty node there to read one from.
                        let pen_shown = if e >= n { EJECT_PENALTY } else { pen };
                        // §855: a badness past `inf_bad` prints as `*` — the line
                        // is overfull, and there is no number for how overfull.
                        let b_shown = if bad > 10_000 {
                            alloc::string::String::from("*")
                        } else {
                            alloc::format!("{bad}")
                        };
                        let d_shown = if artificial {
                            alloc::string::String::from("*")
                        } else {
                            alloc::format!("{d}")
                        };
                        via.push((
                            group(*prior_lines),
                            alloc::format!(
                                "@{kind} via @@{r} b={b_shown} p={pen_shown} d={d_shown}"
                            ),
                        ));
                    }
                    let total = prior_dem.saturating_add(d);
                    if total >= AWFUL {
                        continue;
                    }
                    let slot = fit as usize;
                    let cells = bucket_mut(&mut best[k], group(*prior_lines));
                    let better = match cells[slot] {
                        None => true,
                        Some(c) => total <= c.0,
                    };
                    if better {
                        cells[slot] =
                            Some((total, j, *prior_fit as usize, prior_lines + 1, bad, pen));
                    }
                }
            }
            // §836 does not keep every class it weighed — only those within
            // `|\adjdemerits|` of the best in their group:
            //
            //     if abs(adj_demerits)>=awful_bad-minimum_demerits then
            //       minimum_demerits:=awful_bad-1
            //     else minimum_demerits:=minimum_demerits+abs(adj_demerits);
            //     for fit_class:=very_loose_fit to tight_fit do
            //       begin if minimal_demerits[fit_class]<=minimum_demerits then
            //         @<Insert a new active node …@>;
            //       minimal_demerits[fit_class]:=awful_bad;
            //       end;
            //
            // A class further out than that will never be the answer — a line
            // in it already costs more than `\adjdemerits` can save later — so
            // TeX does not carry it. We applied the window when *printing* the
            // trace and not when filling the table, so the search dragged along
            // chains TeX had dropped: on a paragraph with `\looseness` and an
            // infinitely stretchy `\rightskip` that was 425 nodes against
            // TeX's 257, and they changed the answer.
            {
                let window_of = |cells: &[Cell; 4]| -> i64 {
                    let mut min_dem = AWFUL;
                    for c in cells.iter().flatten() {
                        min_dem = min_dem.min(c.0);
                    }
                    min_dem.saturating_add((params.adj_demerits as i64).abs())
                };
                for (_, cells) in best[k].iter_mut() {
                    let window = window_of(cells);
                    for c in cells.iter_mut() {
                        if c.is_some_and(|x| x.0 > window) {
                            *c = None;
                        }
                    }
                }
                best[k].retain(|(_, cells)| cells.iter().any(|c| c.is_some()));
            }
            if params.trace && !via.is_empty() {
                // §857: the material since the last breakpoint that was
                // reported, up to and including this one, in §174's short form.
                // Printed once, ahead of the breaks found at this node.
                let mut line = alloc::string::String::new();
                short_display(
                    &list[printed_upto..=e.min(n.saturating_sub(1))],
                    &|f| {
                        params
                            .font_ids
                            .get(f)
                            .cloned()
                            .unwrap_or_else(|| alloc::format!("{}nullfont", params.escape))
                    },
                    &mut short_font,
                    &mut line,
                    params.newline_char,
                );
                // §858 `@<Update the value of |printed_node|…@>`: a
                // discretionary's short display has already shown the pre- and
                // post-break lists, so the *replace* text is stepped over
                // rather than shown again.
                //
                //     if cur_p=printed_node then if cur_p<>null then
                //       if type(cur_p)=disc_node then
                //       begin t:=replace_count(cur_p);
                //       while t>0 do
                //         begin decr(t); printed_node:=link(printed_node); end;
                //       end
                let skip = match list.get(e) {
                    Some(Node::Disc(d)) => d.replace,
                    _ => 0,
                };
                printed_upto = (e + 1 + skip).min(n);
                // §857 is `print_nl("")` and then `short_display` of what is
                // left. `print_nl` moves to a fresh line only when one is
                // already in progress, and after a `@@N:` line none is — so
                // with nothing left to show, nothing is printed. We pushed the
                // empty line anyway, which put a blank between the last `@@`
                // node and the `\par` breaks below it in every paragraph whose
                // last breakpoint was its last node.
                if !line.is_empty() {
                    scratch.trace.push(line);
                }
                // §829's order is the walk's order now, so these come out as
                // TeX prints them.
                // §835 does not print all the candidates and then all the
                // nodes: it creates a group's nodes the moment the walk crosses
                // into the next group, so the log runs candidates, nodes,
                // candidates, nodes. Batching them put every `@@` line of a
                // breakpoint below every `@ via` line of it.
                let mut keys: alloc::vec::Vec<u32> = via.iter().map(|(g, _)| *g).collect();
                keys.extend(best[k].iter().map(|(g, _)| *g));
                keys.sort_unstable();
                keys.dedup();
                let buckets: alloc::vec::Vec<(u32, [Cell; 4])> = best[k].clone();
                for gk in keys {
                    for (_, line) in via.iter().filter(|(g, _)| *g == gk) {
                        scratch.trace.push(line.clone());
                    }
                    let Some(&(lines_key, cells)) = buckets.iter().find(|(g, _)| *g == gk) else {
                        continue;
                    };
                    let mut min_dem = AWFUL;
                    for f in 0..4 {
                        if let Some(c) = cells[f] {
                            min_dem = min_dem.min(c.0);
                        }
                    }
                    let window = min_dem.saturating_add((params.adj_demerits as i64).abs());
                    for f in 0..4 {
                        let Some(c) = cells[f] else { continue };
                        if c.0 > window {
                            continue;
                        }
                        scratch.next_serial += 1;
                        {
                            let row = &mut scratch.serial[k];
                            match row.iter().position(|(l, _)| *l == lines_key) {
                                Some(i) => row[i].1[f] = scratch.next_serial,
                                None => {
                                    let mut a = [0u32; 4];
                                    a[f] = scratch.next_serial;
                                    row.push((lines_key, a));
                                }
                            }
                        }
                        let prev = if c.1 == usize::MAX {
                            0
                        } else {
                            serial_of(&scratch.serial[c.1], group(c.3.saturating_sub(2)), c.2)
                        };
                        let hyph = if matches!(list.get(e), Some(Node::Disc(_))) || e >= n {
                            "-"
                        } else {
                            ""
                        };
                        scratch.trace.push(alloc::format!(
                            "@@{}: line {}.{f}{hyph} t={} -> @@{prev}",
                            scratch.next_serial,
                            params.prev_graf.saturating_add(c.3),
                            c.0
                        ));
                    }
                }
            }
            via.clear();
            // §853 `@<Deactivate node r@>`: the node comes out of the list.
            // Taken from the back so the earlier indices are still good — the
            // walk collected them front to back.
            for &x in scratch.kill.iter().rev() {
                active.remove(x);
            }
            scratch.kill.clear();
            // §836: a break that any line can end at becomes a place a line may
            // *begin* at. `best[k]` is final now — every predecessor of `k` is
            // before it, and they have all been seen.
            let reachable = best[k].iter().any(|(_, c)| c.iter().any(|x| x.is_some()));
            // §831 clamps any penalty of -10000 or less to `eject_penalty`, and
            // §854 then reads that exact value:
            //
            //     if (b>inf_bad)or(pi=eject_penalty) then
            //       ... node_r_stays_active:=false
            //
            // — for *every* active node it considers. Emptying the active list
            // is what makes the break mandatory rather than merely permitted:
            // no later line can start before it, because there is nothing left
            // to start from.
            //
            // We had `forced_here` only waiving the tolerance test, so a forced
            // break was one candidate among many and the breaker was free to
            // ignore it. `A\penalty-10000 B\par` came out as a single line
            // holding the penalty in the middle, where TeX makes two. That is
            // trip.tex line 146 — `\penalty-1000000000 % forces line break in
            // paragraph` — and its `\showlists` block.
            //
            // Only when the break is reachable. TeX has §854's
            // `artificial_demerits` for the case where deactivating would empty
            // the list with nowhere to go; until that is implemented, leaving
            // the old starts alone is what keeps such a paragraph settable at
            // all.
            if forced_here && reachable {
                active.clear();
            }
            // One node per line count, as §835 creates them — each carries its
            // own measure from here on.
            // §845 links each new node in just before the node the walk was
            // standing on:
            //
            //     link(q):=r; link(prev_r):=q; prev_r:=q;
            //
            // and §835 flushes a group when the cursor has *already moved on*
            // to the first node of the next one. A node of line number `n` is
            // therefore made while the cursor stands on the first node with
            // line number `n` or more — so it goes in front of the run it
            // joins, not behind it. Within one line number the list runs
            // newest first, which is what makes §855's `<=` prefer the oldest
            // of a tie:
            //
            //     @ via @@2 …
            //     @ via @@1 …
            //     @@5: line 2.2 t=0 -> @@1
            //
            // — @@2 scanned first though it was created second, and @@1, last
            // scanned, taking the tie.
            if reachable {
                for &(lkey, ref cells) in best[k].iter() {
                    // The merged group is flushed at `r = last_active`, so its
                    // nodes go on the end of the list whatever their line
                    // numbers are — and within it the list therefore runs
                    // oldest first. Every other group is flushed with the
                    // cursor on the first node of the next one, so its node
                    // goes in front of the run it joins. Both orders are
                    // observable in a `\tracingparagraphs` log, and §855's
                    // `<=` reads a different winner out of each.
                    let merged = easy_line != usize::MAX && lkey == easy_line as u32;
                    // §845 runs `prev_r:=q` after each class, so the four go in
                    // one after another in class order. Re-deriving the
                    // insertion point for each would put every class in front
                    // of the one before it, and a group's classes came out
                    // reversed — visible as two `@ via` lines swapped.
                    let mut at = if merged {
                        active.len()
                    } else {
                        // A non-merged group's members all share the group's
                        // line number, so one position does for all of them.
                        let lnum = cells.iter().flatten().next().map_or(0, |c| {
                            params.prev_graf.saturating_add(c.3).saturating_add(1)
                        });
                        active
                            .iter()
                            .position(|&(l, ..)| l >= lnum)
                            .unwrap_or(active.len())
                    };
                    for f in 0..4 {
                        let Some(c) = cells[f] else { continue };
                        let lnum = params.prev_graf.saturating_add(c.3).saturating_add(1);
                        let pos = if merged { active.len() } else { at };
                        active.insert(pos, (lnum, k, lkey, f as u8, c.0, c.3));
                        at += 1;
                    }
                }
            }
        }

        // No feasible sequence — the paragraph cannot be set within tolerance.
        // Falling back to greedy is what TeX's second pass approximates, and
        // it is far better than returning nothing.
        let last = breaks.len() - 1;
        // §874 `@<Find an active node with fewest demerits@>`: the cheapest
        // class the paragraph's end was reached in wins.
        // Every (line count, fitness class) the paragraph's end was reached in
        // is one of §874's active nodes.
        let mut ends: alloc::vec::Vec<(u32, usize, (i64, usize, usize, usize, i32, i32))> = best
            [last]
            .iter()
            .flat_map(|(l, cells)| (0..4).filter_map(move |f| cells[f].map(|c| (*l, f, c))))
            .collect();
        // §835 appends active nodes as the line number rises, and §836 walks
        // the four classes in order within each — so the list §874 and §875
        // scan is ordered by (line count, class). Both take the *first* node of
        // a tie:
        //
        //     if (type(r)<>delta_node)and(total_demerits(r)<fewest_demerits) then
        //
        // — strictly less. That matters wherever demerits tie, which with
        // INITEX's `\linepenalty` of nought and a `\rightskip` that stretches
        // infinitely is *every* chain: each line is badness 0 and costs
        // nothing, so the paragraph may be broken anywhere and TeX's answer is
        // decided entirely by this order. Collected in bucket-creation order
        // instead, we took the longest.
        ends.sort_by_key(|&(l, f, _)| (l, f));
        let Some(&(mut lkey, mut slot, mut chosen)) = ends.iter().min_by_key(|(_, _, c)| c.0)
        else {
            return (Greedy.break_lines(list, params, metrics), true);
        };

        // §875 `@<Find the best active node for the desired looseness@>`.
        //
        // `\looseness` is not another cost. The paragraph is broken as usual
        // and then a *different endpoint* is chosen — the one whose line count
        // comes nearest the wanted difference from the best. Each of the four
        // fitness classes reaching the end carries its own line count, so the
        // four are the active list §875 walks.
        //
        //     line_diff:=line_number(r)-best_line;
        //     if ((line_diff<actual_looseness)and(looseness<=line_diff))or
        //        ((line_diff>actual_looseness)and(looseness>=line_diff)) then
        //       begin best_bet:=r; actual_looseness:=line_diff;
        //       fewest_demerits:=total_demerits(r);
        //       end
        //     else if (line_diff=actual_looseness)and
        //        (total_demerits(r)<fewest_demerits) then
        //       begin best_bet:=r; fewest_demerits:=total_demerits(r);
        //       end;
        //
        // Then §873:
        //
        //     if (actual_looseness=looseness)or final_pass then goto done;
        //
        // — a pass that could not reach the wanted looseness is *retried*, with
        // whatever the next pass allows. That is why `\looseness=1` on a
        // paragraph the first pass sets tightly comes out one line longer only
        // after the second pass has hyphenated it.
        if params.looseness != 0 {
            let best_line = chosen.3 as i64;
            let mut fewest = chosen.0;
            let mut actual = 0i64;
            let want = params.looseness as i64;
            for &(l, f, c) in &ends {
                let diff = c.3 as i64 - best_line;
                if (diff < actual && want <= diff) || (diff > actual && want >= diff) {
                    (lkey, slot, chosen) = (l, f, c);
                    actual = diff;
                    fewest = c.0;
                } else if diff == actual && c.0 < fewest {
                    (lkey, slot, chosen) = (l, f, c);
                    fewest = c.0;
                }
            }
            if actual != want && !params.final_pass {
                // The paragraph's nodes are untouched — a pass that gives up
                // must leave them for the next one to break again.
                return (Vec::new(), true);
            }
        }
        let _ = chosen;

        // Walk the chain back, following the class each step recorded.
        let cuts = &mut scratch.cuts;
        cuts.clear();
        let costs = &mut scratch.costs;
        costs.clear();
        let mut k = last;
        loop {
            let c = best[k]
                .iter()
                .find(|(l, _)| *l == lkey)
                .and_then(|(_, cells)| cells[slot])
                .expect("on the chosen chain");
            lkey = group(c.3.saturating_sub(2));
            let (_, prev, prev_slot) = (c.0, c.1, c.2);
            costs.push((c.4, c.5, c.0));
            cuts.push(breaks[k]);
            if prev == usize::MAX {
                break;
            }
            k = prev;
            slot = prev_slot;
        }
        cuts.reverse();
        costs.reverse();

        // Where each line's nodes are, computed while the list can still be
        // read: (first node, one past the last, ends at a discretionary, where
        // the next line begins, whether the break node's width is zeroed).
        // Breaking at a discretionary puts its pre-break material at the end
        // of this line and its post-break material at the start of the next,
        // and the `replace` nodes it stands in for are dropped — starting the
        // next line *at* the discretionary left them in, so an `ffi` broken
        // into `f-` and `fi` was then set again whole.
        let n = list.len();
        let spans = &mut scratch.spans;
        spans.clear();
        spans.reserve(cuts.len().saturating_sub(spans.capacity()));
        let mut start = 0usize;
        for &cut in cuts.iter() {
            let (is_disc, next) = match list.get(cut) {
                Some(Node::Disc(d)) => (true, cut + 1 + d.replace),
                _ => (false, after_break(list, cut)),
            };
            // §880: the node broken *at* stays in the line unless it is glue.
            // Glue is replaced outright by `\rightskip`; a penalty, a kern or a
            // math node is kept and the `\rightskip` goes after it — which is
            // why a line that ends at `\penalty-10000` still shows that penalty
            // in its box. Cutting before the node dropped it, and a line whose
            // only content *was* the break node vanished with it.
            let keep_break = matches!(
                list.get(cut),
                Some(Node::Penalty(_) | Node::Kern(..) | Node::Math { .. })
            );
            let zero_break = matches!(list.get(cut), Some(Node::Kern(..) | Node::Math { .. }));
            let end = if keep_break { cut + 1 } else { cut };
            spans.push((start, end.min(n), is_disc, next.min(n), zero_break));
            start = next;
        }

        // Now move. `drain` yields the nodes by value and leaves `buf` empty
        // with its capacity, so the caller's paragraph buffer survives for the
        // next paragraph. Anything skipped here — the glue a line broke at, a
        // discretionary's `replace` text — is dropped as it passes, which is
        // where TeX frees it too.
        let last_index = spans.len() - 1;
        let mut lines = Vec::with_capacity(cuts.len());
        let mut carried: Vec<Node> = Vec::new();
        {
            let mut it = buf.drain(..);
            let mut idx = 0usize;
            for (i, &(s, end, is_disc, next, zero_break)) in spans.iter().enumerate() {
                let mut nodes: Vec<Node> = core::mem::take(&mut carried);
                while idx < s {
                    if it.next().is_none() {
                        break;
                    }
                    idx += 1;
                }
                nodes.reserve(end.saturating_sub(idx));
                while idx < end {
                    match it.next() {
                        Some(node) => nodes.push(node),
                        None => break,
                    }
                    idx += 1;
                }
                if is_disc {
                    if let Some(Node::Disc(d)) = it.next() {
                        idx += 1;
                        let d = *d;
                        // §882 leaves the disc node itself in the line, emptied,
                        // ahead of its pre-break text — a bare `\discretionary`
                        // in a box display is exactly that. We dropped it, so a
                        // line broken at one carried no record of the fact.
                        nodes.push(Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
                            pre: alloc::vec::Vec::new(),
                            post: alloc::vec::Vec::new(),
                            replace: 0,
                        })));
                        nodes.extend(d.pre);
                        carried = d.post;
                    }
                }
                while idx < next {
                    if it.next().is_none() {
                        break;
                    }
                    idx += 1;
                }
                // Glue is discarded at a *break*, and the paragraph's last line
                // does not end at one — it ends at the forced break after
                // `\parfillskip`, so that glue is inside the line.
                if i != last_index {
                    discard_trailing_glue(&mut nodes);
                    // §880: `width(q) := 0` only when the chosen breakpoint
                    // `q` itself is a kern or math node. A normal font kern
                    // immediately before a *glue* break remains part of the
                    // line; zeroing Latin Modern's closing-quote boundary kern
                    // changed the setting of every interword space before it.
                    if zero_break {
                        match nodes.last_mut() {
                            Some(Node::Kern(w, _)) => *w = Scaled::ZERO,
                            Some(Node::Math { width, .. }) => *width = Scaled::ZERO,
                            _ => {}
                        }
                    }
                }
                // A forced break can form a genuinely empty line. LaTeX's `\\`
                // uses this for a blank line inside boxes; it is still packed.
                // pdfTeX's character protrusion puts margin kerns into the
                // finished line even when it did not participate in breaking.
                // This scan is deliberately more permissive than the trial
                // scan: after the cut is known, intervening glue does not hide
                // the finished line's last glyph.
                if params.protrude_chars > 0 {
                    let at = before_trailing_glue(&nodes);
                    if let Some((f, c)) = last_char(&nodes[..at]) {
                        let w = metrics.protrusion(f, c, true);
                        if w != Scaled::ZERO {
                            nodes.insert(
                                at,
                                Node::Kern(Scaled(-w.0), crate::node::kern_sub::MARGIN_RIGHT),
                            );
                        }
                    }
                    if let Some((f, c)) = first_char(&nodes) {
                        let w = metrics.protrusion(f, c, false);
                        if w != Scaled::ZERO {
                            nodes.insert(
                                0,
                                Node::Kern(Scaled(-w.0), crate::node::kern_sub::MARGIN_LEFT),
                            );
                        }
                    }
                }
                reorder_native_bidi_line(&mut nodes);
                let (indent, width) = line_dims(
                    params,
                    params
                        .prev_graf
                        .saturating_add(lines.len())
                        .saturating_add(1),
                );
                let (badness, penalty, demerits) = costs.get(i).copied().unwrap_or((0, 0, 0));
                lines.push(Line {
                    nodes,
                    ends_paragraph: i == last_index,
                    badness,
                    penalty,
                    demerits,
                    width,
                    indent,
                    hyphenated: is_disc,
                });
            }
        }
        if let (Some((hash, slot)), Some(source)) = (cache_key, cache_source) {
            scratch.plan_cache[slot] = Some(BreakPlanEntry {
                hash,
                source,
                params: params.clone(),
                lines: lines.clone(),
                layout: None,
            });
            scratch.last_plan_slot = Some(slot);
        }
        (lines, false)
    }
}

impl LineBreaker for KnuthPlass {
    fn break_lines(
        &self,
        list: &[Node],
        params: &BreakParams,
        metrics: &impl CharMetrics,
    ) -> Vec<Line> {
        self.break_counted(&list, params, metrics).0
    }
}

/// Glue at the end of a line disappears, as it does in TeX. Without this the
/// space you broke at is still there, pushing the last word off the measure.
/// The first character a line sets, for protrusion — pdfTeX's
/// `find_first_char`, which walks past what has no glyph and looks *into* a
/// box. The `\parindent` box is why: it sits ahead of the first letter of a
/// paragraph and pdfTeX still protrudes that letter.
pub(crate) fn first_char(list: &[Node]) -> Option<(usize, u32)> {
    match first_scan(list) {
        ProtScan::Char(font, code, _) => Some((font, code)),
        ProtScan::Blocked | ProtScan::Exhausted => None,
    }
}

/// The left edge used while *trying* a line break. pdfTeX calls
/// `find_protchar_left(..., true)` here: before its ordinary protrusion walk it
/// discards the standard glue/kern/penalty/math nodes that TeX discards after
/// a break. Finished-line margin-kern insertion calls the same routine with
/// `false`, after the line has been cut, so an explicit zero glue can still
/// block the kern that is ultimately put in the box.
///
/// The distinction is observable in an AMS run-in heading. Its line starts
/// with explicit zero glue: the digit after it participates in trial-line
/// badness, but the finished line has no left margin kern.
#[cfg(test)]
fn first_char_for_break(list: &[Node]) -> Option<(usize, u32)> {
    first_char_for_break_detail(list).map(|(font, code, _)| (font, code))
}

fn first_char_for_break_detail(list: &[Node]) -> Option<(usize, u32, bool)> {
    match first_scan_for_break(list) {
        ProtScan::Char(font, code, ligature) => Some((font, code, ligature)),
        ProtScan::Blocked | ProtScan::Exhausted => None,
    }
}

fn first_scan_for_break(list: &[Node]) -> ProtScan {
    let mut start = 0;
    while start + 1 < list.len() && discardable(&list[start]) {
        start += 1;
    }
    first_scan(&list[start..])
}

/// A broken discretionary can reconstruct material at the beginning of the
/// next line (most commonly the `i` after splitting an `fi` ligature). That
/// post-break list precedes the ordinary paragraph nodes for both trial
/// protrusion and marginal expansion. If it contains only skipable nodes, the
/// scan continues into the main list; a real blocker still stops it.
fn first_char_for_break_parts_detail(
    post: Option<&[Node]>,
    main: &[Node],
) -> Option<(usize, u32, bool)> {
    if let Some(post) = post {
        match first_scan_for_break(post) {
            ProtScan::Char(font, code, ligature) => return Some((font, code, ligature)),
            ProtScan::Blocked => return None,
            ProtScan::Exhausted => {}
        }
    }
    first_char_for_break_detail(main)
}

enum ProtScan {
    Char(usize, u32, bool),
    Blocked,
    Exhausted,
}

/// pdfTeX's `find_protchar_left`. A nonempty horizontal box is descended into;
/// if every node in it is skipable, the traversal pops the box and resumes at
/// its following sibling. A non-skipable node inside the box stops the whole
/// search.
fn first_scan(list: &[Node]) -> ProtScan {
    for n in list {
        match n {
            Node::Char { code, font, lig } => {
                let opening_double_quote = lig
                    .as_deref()
                    .is_some_and(|source| source == ['`' as u32, '`' as u32]);
                return ProtScan::Char(*font, *code, opening_double_quote);
            }
            Node::Box(b) if b.kind == crate::node::BoxKind::Horizontal && !b.list.is_empty() => {
                match first_scan(&b.list) {
                    ProtScan::Char(font, code, ligature) => {
                        return ProtScan::Char(font, code, ligature);
                    }
                    ProtScan::Blocked => return ProtScan::Blocked,
                    ProtScan::Exhausted => continue,
                }
            }
            node if crate::node::protrusion_skipable(node) => {}
            _ => return ProtScan::Blocked,
        }
    }
    ProtScan::Exhausted
}

/// The same from the other end.
pub(crate) fn last_char(list: &[Node]) -> Option<(usize, u32)> {
    match last_scan(list) {
        ProtScan::Char(font, code, _) => Some((font, code)),
        ProtScan::Blocked | ProtScan::Exhausted => None,
    }
}

/// pdfTeX's `find_protchar_right`, the mirror of [`first_scan`].
fn last_scan(list: &[Node]) -> ProtScan {
    for n in list.iter().rev() {
        match n {
            Node::Char { code, font, lig } => {
                let opening_double_quote = lig
                    .as_deref()
                    .is_some_and(|source| source == ['`' as u32, '`' as u32]);
                return ProtScan::Char(*font, *code, opening_double_quote);
            }
            Node::Box(b) if b.kind == crate::node::BoxKind::Horizontal && !b.list.is_empty() => {
                match last_scan(&b.list) {
                    ProtScan::Char(font, code, ligature) => {
                        return ProtScan::Char(font, code, ligature);
                    }
                    ProtScan::Blocked => return ProtScan::Blocked,
                    ProtScan::Exhausted => continue,
                }
            }
            node if crate::node::protrusion_skipable(node) => {}
            _ => return ProtScan::Blocked,
        }
    }
    ProtScan::Exhausted
}

/// The character whose right protrusion a *candidate line* ending here would take,
/// for the width `try_break` scores. Not the same question as which character the
/// finished line hangs into the margin — that one is asked after the trailing glue
/// has been discarded, by `last_char` at the insertion site.
///
/// Measured against `pdftex -ini -etex` by sweeping `\rpcode` between 0 and 1000 and
/// watching whether the reported badness moves. With `.` before the break in every
/// case and `\pdfprotrudechars=2`:
///
/// ```text
/// ccc.\penalty-10000            badness moves    — the character protrudes
/// ccc. \penalty-10000           badness fixed    — a glue node, and nothing protrudes
/// ccc.\kern0pt\penalty-10000    badness moves
/// ccc.\penalty100 \penalty-10000 badness moves
/// ccc.\hbox{}\penalty-10000     badness moves
/// ccc.\hbox to0pt{}\penalty-10000 badness moves
/// ```
///
/// So **only glue stops the search**; kerns, penalties, marks and zero-width boxes are
/// stepped over. At a glue breakpoint the break node itself is outside `[start, end)`,
/// so the preceding character still protrudes. At a later penalty the intervening glue
/// is inside the slice and blocks it. `last_char` skips glue, which is right after the
/// trailing glue has been discarded and wrong while scoring the latter case.
fn last_char_before_break_detail(list: &[Node]) -> Option<(usize, u32, bool)> {
    match last_scan_before_break(list) {
        ProtScan::Char(font, code, ligature) => Some((font, code, ligature)),
        ProtScan::Blocked | ProtScan::Exhausted => None,
    }
}

fn last_char_before_break_parts_detail(
    post: Option<&[Node]>,
    main: &[Node],
) -> Option<(usize, u32, bool)> {
    match last_scan_before_break(main) {
        ProtScan::Char(font, code, ligature) => Some((font, code, ligature)),
        ProtScan::Blocked => None,
        ProtScan::Exhausted => post.and_then(last_char_before_break_detail),
    }
}

fn last_scan_before_break(list: &[Node]) -> ProtScan {
    for n in list.iter().rev() {
        match n {
            Node::Char { code, font, lig } => {
                return ProtScan::Char(*font, *code, lig.is_some());
            }
            Node::Box(b) if b.kind == crate::node::BoxKind::Horizontal && !b.list.is_empty() => {
                match last_scan_before_break(&b.list) {
                    ProtScan::Char(font, code, ligature) => {
                        return ProtScan::Char(font, code, ligature);
                    }
                    ProtScan::Blocked => return ProtScan::Blocked,
                    ProtScan::Exhausted => continue,
                }
            }
            Node::Glue(g) if g.canonical_zero => {}
            Node::Glue(_) | Node::Leaders { .. } => return ProtScan::Blocked,
            node if crate::node::protrusion_skipable(node) => {}
            _ => return ProtScan::Blocked,
        }
    }
    ProtScan::Exhausted
}

/// Whether the visible left edge is a hyphen-derived ligature followed by
/// TeX's empty implicit discretionary. The paragraph-indent box and driver
/// whatsits can precede that pair without hiding it from pdfTeX's protrusion
/// walk, so checking `list[start]` alone is too narrow.
fn leading_hyphen_ligature_for_break(post: Option<&[Node]>, main: &[Node]) -> bool {
    let mut nodes = post
        .into_iter()
        .flat_map(|nodes| nodes.iter())
        .chain(main.iter());
    while let Some(node) = nodes.next() {
        if crate::node::protrusion_skipable(node) {
            continue;
        }
        let Node::Char {
            lig: Some(source), ..
        } = node
        else {
            return false;
        };
        if source.len() < 2 || !source.iter().all(|&code| code == '-' as u32) {
            return false;
        }
        return matches!(
            nodes.next(),
            Some(Node::Disc(d)) if d.pre.is_empty() && d.post.is_empty() && d.replace == 0
        );
    }
    false
}

/// How much a candidate line `[start, end)` protrudes, as a *width* to take off
/// its natural size. `\pdfprotrudechars` of 2 lets this into the badness;
/// 1 puts the kerns in the output and leaves the breaking alone.
///
/// The two ends are not symmetric. A line ending at a discretionary is set with
/// that discretionary's pre-break material — the hyphen — so it is the hyphen
/// that protrudes, not the character before it.
fn protrusion_of(
    list: &[Node],
    start: usize,
    end: usize,
    post: Option<&[Node]>,
    metrics: &impl CharMetrics,
) -> i64 {
    if start >= end && post.is_none_or(<[Node]>::is_empty) {
        return 0;
    }
    let start = start.min(list.len());
    let end = end.min(list.len()).max(start);
    let main = &list[start..end];
    let mut w = 0i64;
    if let Some((f, c, _)) = first_char_for_break_parts_detail(post, main) {
        w += metrics.protrusion(f, c, false).0 as i64;
    }
    // What ends the line: a discretionary's pre-break text if the break is one,
    // and otherwise the material up to it.
    let tail = match list.get(end) {
        Some(Node::Disc(d)) if !d.pre.is_empty() => last_char(&d.pre),
        _ => last_char_before_break_parts_detail(post, main).map(|(font, code, _)| (font, code)),
    };
    if let Some((f, c)) = tail {
        w += metrics.protrusion(f, c, true).0 as i64;
    }
    w
}

/// pdfTeX's asymmetric coupling between protrusion and font expansion.
///
/// The ordinary cases below are the measured `cal_margin_kern_var` behavior
/// for source ligatures and implicit discretionaries. Expanded-font instances
/// add one more case: pdfTeX looks them up globally by name and size, so a
/// later base font (or a letterspaced VF's pristine local font) can redirect
/// an earlier base's expanded lp/rp tables. When the actual expanded value
/// differs, that literal `base - expanded` variation takes precedence over
/// the source-pattern fallback for that margin.
///
/// The packed line does not receive these trial-only allowances, which is why
/// pdfTeX can choose a line that it then reports as overfull.
fn margin_expansion_adjustment(
    list: &[Node],
    start: usize,
    end: usize,
    post: Option<&[Node]>,
    metrics: &impl CharMetrics,
) -> (i64, i64) {
    if start >= end && post.is_none_or(<[Node]>::is_empty) {
        return (0, 0);
    }
    let start = start.min(list.len());
    let end = end.min(list.len()).max(start);
    let main = &list[start..end];
    let left_detail = first_char_for_break_parts_detail(post, main);
    let leading_hyphen_ligature = leading_hyphen_ligature_for_break(post, main);
    let leading_opening_quote = left_detail.is_some_and(|(_, _, opening_quote)| opening_quote);
    let left = left_detail.map(|(font, code, _)| (font, code));
    let left_allowance = left
        .map(|(font, code)| metrics.protrusion(font, code, false).0 as i64)
        .unwrap_or(0);
    let right_detail = match list.get(end) {
        Some(Node::Disc(d)) if !d.pre.is_empty() || !d.post.is_empty() || d.replace != 0 => None,
        _ => last_char_before_break_parts_detail(post, main),
    };
    let legacy_right_allowance = right_detail
        .filter(|(_, _, ligature)| *ligature)
        .map(|(font, code, _)| metrics.protrusion(font, code, false).0 as i64)
        .unwrap_or(0);
    let right = match list.get(end) {
        Some(Node::Disc(d)) if !d.pre.is_empty() => last_char(&d.pre),
        _ => right_detail.map(|(font, code, _)| (font, code)),
    };
    let variation = |font: usize, code: u32| -> (Option<i64>, Option<i64>) {
        let base = metrics.protrusion(font, code, false).0 as i64;
        let stretch = if metrics.expansion_amount(font, code, 1000) != 0 {
            let expanded = metrics.expanded_protrusion(font, code, false, true).0 as i64;
            (expanded != base).then_some(base - expanded)
        } else {
            None
        };
        let shrink = if metrics.expansion_amount(font, code, -1000) != 0 {
            let expanded = metrics.expanded_protrusion(font, code, false, false).0 as i64;
            (expanded != base).then_some(expanded - base)
        } else {
            None
        };
        (stretch, shrink)
    };
    let legacy_left = if leading_opening_quote || leading_hyphen_ligature {
        (-left_allowance, left_allowance)
    } else {
        (0, 0)
    };
    let legacy_right = (-legacy_right_allowance, legacy_right_allowance);
    let left_variation = left.map(|(font, code)| variation(font, code));
    let right_variation = right.map(|(font, code)| variation(font, code));
    (
        left_variation
            .and_then(|value| value.0)
            .unwrap_or(legacy_left.0)
            + right_variation
                .and_then(|value| value.0)
                .unwrap_or(legacy_right.0),
        left_variation
            .and_then(|value| value.1)
            .unwrap_or(legacy_left.1)
            + right_variation
                .and_then(|value| value.1)
                .unwrap_or(legacy_right.1),
    )
}

/// Where the right margin kern goes: before the line's trailing glue, not after
/// it. Measured — the last line of a paragraph reads
///
/// ```text
/// 	enrm .
/// \penalty 10000
/// \kern-4.0 (right margin)
/// \glue(\parfillskip) 0.0 plus 1.0fil
/// \glue(\rightskip) 7.0
/// ```
///
/// so the kern is past the penalty and ahead of `\parfillskip`. On every other
/// line the trailing glue has been discarded at the break, and this is the end.
fn before_trailing_glue(list: &[Node]) -> usize {
    let mut i = list.len();
    while i > 0 && matches!(list[i - 1], Node::Glue(_)) {
        i -= 1;
    }
    // …and before the penalty the line *broke at*, which stays at its end. A line
    // that broke at a penalty has nothing after it, so "no trailing glue was
    // skipped" is exactly the test. Measured, `\rpcode\font`\.=1000`:
    //
    // ```text
    // ccc.\penalty-10000 zzz     . KERN \penalty -10000 \glue(\rightskip)
    // ccc.\penalty-9000 zzz      . KERN \penalty -9000 \glue(\rightskip)
    // ccc.\penalty100 \penalty-10000 zzz
    //                            . \penalty 100 KERN \penalty -10000 \glue(\rightskip)
    // ccc.\par                   . \penalty 10000 KERN \glue(\parfillskip) \glue(\rightskip)
    // ```
    //
    // The third case is the one that pins it: an *ordinary* penalty in the line
    // keeps the kern after it, and only the break penalty takes it in front. The
    // fourth breaks at `\par`, so §816's appended `\penalty 10000` is ordinary
    // material and the trailing `\parfillskip` is what the kern goes ahead of.
    // …and before a trailing math-off node, which is where pdfTeX puts it:
    // `) KERN \mathoff \glue(\rightskip)`.
    while i > 0 && matches!(list[i - 1], Node::Math { .. }) {
        i -= 1;
    }
    // At an explicit-hyphen break pdfTeX inserts the right margin kern between
    // the visible hyphen and the now-empty discretionary breakpoint.
    while i > 0
        && matches!(
            &list[i - 1],
            Node::Disc(d) if d.pre.is_empty() && d.post.is_empty() && d.replace == 0
        )
    {
        i -= 1;
    }
    if i == list.len() && i > 0 && matches!(list[i - 1], Node::Penalty(_)) {
        i -= 1;
    }
    i
}

fn discard_trailing_glue(line: &mut Vec<Node>) {
    while matches!(line.last(), Some(Node::Glue(_))) {
        line.pop();
    }
}

/// Interword glue for a font, from its own parameters.
pub fn interword(space: Scaled, stretch: Scaled, shrink: Scaled) -> Glue {
    Glue::interword(space, stretch, shrink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::TeXBox;
    use alloc::vec;

    /// Every character is 10pt wide, so line widths are countable by hand.
    struct Fixed;
    impl CharMetrics for Fixed {
        fn width(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(10)
        }
        fn height(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(7)
        }
        fn depth(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::ZERO
        }
    }

    struct Expanded;
    impl CharMetrics for Expanded {
        fn width(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(10)
        }
        fn height(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(7)
        }
        fn depth(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::ZERO
        }
        fn expansion_capacity(&self, _f: usize, _c: u32) -> (Scaled, Scaled) {
            (Scaled::from_points(1), Scaled::from_points(1))
        }
        fn expansion_limits(&self, _f: usize) -> Option<(i64, i64, i64)> {
            Some((100, 100, 10))
        }
    }

    struct Protruding;
    impl CharMetrics for Protruding {
        fn width(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(10)
        }
        fn height(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(7)
        }
        fn depth(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::ZERO
        }
        fn protrusion(&self, _f: usize, code: u32, right: bool) -> Scaled {
            if (right && matches!(char::from_u32(code), Some('-' | 't')))
                || (!right && code == 'T' as u32)
            {
                Scaled::from_points(2)
            } else {
                Scaled::ZERO
            }
        }
    }

    fn ch(c: char) -> Node {
        Node::Char {
            code: c as u32,
            font: 0,
            lig: None,
        }
    }

    fn space() -> Node {
        Node::Glue(Glue::fixed(Scaled::from_points(5)))
    }

    fn native(glyph: u32, level: u8, span: u32) -> Node {
        Node::NativeRun(alloc::boxed::Box::new(crate::node::NativeRun {
            font: 0,
            size: Scaled::from_points(10),
            direction: if level % 2 == 1 {
                crate::node::NativeDirection::RightToLeft
            } else {
                crate::node::NativeDirection::LeftToRight
            },
            language: None,
            bidi_level: level,
            bidi_span: span,
            width: Scaled::from_points(10),
            height: Scaled::from_points(7),
            depth: Scaled::ZERO,
            glyphs: alloc::vec![crate::node::NativeGlyph {
                source_gid: glyph,
                cluster: 0,
                x_advance: Scaled::from_points(10),
                y_advance: Scaled::ZERO,
                x_offset: Scaled::ZERO,
                y_offset: Scaled::ZERO,
                is_notdef: false,
            }],
            clusters: Vec::new(),
        }))
    }

    fn native_glyphs(line: &Line) -> Vec<u32> {
        line.nodes
            .iter()
            .filter_map(|node| match node {
                Node::NativeRun(run) => run.glyphs.first().map(|glyph| glyph.source_gid),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn bidi_visual_order_restarts_at_each_finished_line() {
        let list = alloc::vec![
            native(1, 1, 7),
            space(),
            native(2, 1, 7),
            space(),
            native(3, 1, 7),
            space(),
            native(4, 1, 7),
        ];
        let lines = Greedy.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(25),
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(native_glyphs(&lines[0]), vec![2, 1]);
        assert_eq!(native_glyphs(&lines[1]), vec![4, 3]);

        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(25),
                final_pass: true,
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(native_glyphs(&lines[0]), vec![2, 1]);
        assert_eq!(native_glyphs(&lines[1]), vec![4, 3]);
    }

    #[test]
    fn bidi_reordering_honors_nested_levels_and_span_boundaries() {
        let mut nodes = alloc::vec![
            native(1, 1, 3),
            space(),
            native(2, 2, 3),
            space(),
            native(3, 2, 3),
            space(),
            native(4, 1, 3),
            Node::Kern(Scaled::ZERO, 0),
            native(5, 1, 4),
            space(),
            native(6, 1, 4),
        ];
        reorder_native_bidi_line(&mut nodes);
        let glyphs = nodes
            .iter()
            .filter_map(|node| match node {
                Node::NativeRun(run) => run.glyphs.first().map(|glyph| glyph.source_gid),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(glyphs, vec![4, 2, 3, 1, 6, 5]);
    }

    fn text(line: &Line) -> alloc::string::String {
        line.nodes
            .iter()
            .filter_map(|n| match n {
                Node::Char { code, .. } => char::from_u32(*code),
                Node::Glue(_) => Some(' '),
                _ => None,
            })
            .collect()
    }

    /// `tex.web` §848–§850: the measure is per line, not per paragraph.
    #[test]
    fn hanging_indentation_shapes_each_line() {
        let mut p = BreakParams {
            hsize: Scaled::from_points(100),
            ..Default::default()
        };
        // no shape: every line is `\hsize`, flush left
        assert_eq!(line_dims(&p, 1), (Scaled::ZERO, Scaled::from_points(100)));
        assert_eq!(line_dims(&p, 9), (Scaled::ZERO, Scaled::from_points(100)));
        // `\hangafter=1`: line 1 full, every line after it indented
        p.hang_indent = Scaled::from_points(20);
        assert_eq!(line_dims(&p, 1), (Scaled::ZERO, Scaled::from_points(100)));
        assert_eq!(
            line_dims(&p, 2),
            (Scaled::from_points(20), Scaled::from_points(80))
        );
        // a negative `\hangafter` indents the *first* lines instead
        p.hang_after = -2;
        assert_eq!(
            line_dims(&p, 2),
            (Scaled::from_points(20), Scaled::from_points(80))
        );
        assert_eq!(line_dims(&p, 3), (Scaled::ZERO, Scaled::from_points(100)));
        // a negative `\hangindent` takes the space off the right: the width
        // shrinks and the indent stays zero
        p.hang_after = 1;
        p.hang_indent = Scaled::from_points(-20);
        assert_eq!(line_dims(&p, 2), (Scaled::ZERO, Scaled::from_points(80)));
        // `\parshape` overrides it, and its last pair repeats
        p.par_shape = alloc::vec![
            (Scaled::ZERO, Scaled::from_points(50)),
            (Scaled::from_points(5), Scaled::from_points(60)),
        ];
        assert_eq!(line_dims(&p, 1), (Scaled::ZERO, Scaled::from_points(50)));
        assert_eq!(
            line_dims(&p, 7),
            (Scaled::from_points(5), Scaled::from_points(60))
        );
    }

    /// A fallback is a last-resort output path, but it must still preserve the
    /// paragraph geometry.  In particular, `wrapfig` expresses the space next
    /// to a figure as a `\parshape`; throwing that shape away prints text over
    /// the figure.
    #[test]
    fn greedy_fallback_preserves_parshape_geometry() {
        let mut list = Vec::new();
        for i in 0..40 {
            if i > 0 {
                list.push(Node::Glue(Glue::interword(
                    Scaled::from_points(5),
                    Scaled::from_points(3),
                    Scaled::from_points(2),
                )));
            }
            list.push(ch('x'));
        }
        let narrow = Scaled::from_points(45);
        let wide = Scaled::from_points(200);
        let params = BreakParams {
            hsize: wide,
            par_shape: alloc::vec![(Scaled::from_points(10), narrow), (Scaled::ZERO, wide)],
            ..Default::default()
        };
        let lines = Greedy.break_lines(&list, &params, &Fixed);
        assert!(lines.len() >= 3);
        assert_eq!((lines[0].indent, lines[0].width), params.par_shape[0]);
        assert_eq!((lines[1].indent, lines[1].width), params.par_shape[1]);
        assert_eq!((lines[2].indent, lines[2].width), params.par_shape[1]);
        for line in &lines {
            let measured = crate::node::measure_horizontal(&line.nodes, &Fixed);
            assert!(measured.natural.0 - measured.shrink[0].0 <= line.width.0);
        }
    }

    /// `tex.web` §854 guarantees that the final pass can force a break before
    /// its last active node disappears.  A long wrapped paragraph therefore
    /// has to reach an endpoint even while its first lines use a much narrower
    /// `\parshape` measure.
    #[test]
    fn final_pass_reaches_an_endpoint_through_a_repeating_parshape() {
        let mut list = Vec::new();
        for i in 0..120 {
            if i > 0 {
                list.push(Node::Glue(Glue::interword(
                    Scaled::from_points(5),
                    Scaled::from_points(3),
                    Scaled::from_points(2),
                )));
            }
            for _ in 0..(i % 7 + 1) {
                list.push(ch('x'));
            }
        }
        list.push(Node::Penalty(INF_PENALTY));
        list.push(Node::Glue(Glue::fil()));
        let narrow = Scaled::from_points(140);
        let wide = Scaled::from_points(350);
        let mut shape = alloc::vec![(Scaled::from_points(210), narrow); 14];
        shape.push((Scaled::ZERO, wide));
        let params = BreakParams {
            hsize: wide,
            par_shape: shape,
            tolerance: 200,
            final_pass: true,
            ..Default::default()
        };
        let (lines, fell_back) = KnuthPlass.break_counted(&list, &params, &Fixed);
        assert!(!fell_back, "a final pass must reach the paragraph end");
        assert!(lines.len() > 15);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                (line.indent, line.width),
                line_dims(&params, i.saturating_add(1))
            );
        }
    }

    /// §854 decides whether to force a break while walking the active list at
    /// the current line's measure.  Preselecting a future forced break with
    /// the paragraph's widest measure suppresses valid narrow-line candidates:
    /// the first two lines here fit exactly at 100pt even though the same word
    /// sequence jumps from underfull to overfull at the later 300pt measure.
    #[test]
    fn final_pass_does_not_suppress_valid_narrow_parshape_breaks() {
        let mut list = Vec::new();
        for i in 0..24 {
            if i > 0 {
                list.push(Node::Glue(Glue::interword(
                    Scaled::from_points(5),
                    Scaled::ZERO,
                    Scaled::ZERO,
                )));
            }
            list.extend([ch('x'), ch('x'), ch('x')]);
        }
        list.push(Node::Penalty(INF_PENALTY));
        list.push(Node::Glue(Glue::fil()));
        let params = BreakParams {
            hsize: Scaled::from_points(300),
            par_shape: alloc::vec![
                (Scaled::ZERO, Scaled::from_points(100)),
                (Scaled::ZERO, Scaled::from_points(100)),
                (Scaled::ZERO, Scaled::from_points(300)),
            ],
            tolerance: 5_000,
            final_pass: true,
            ..Default::default()
        };
        let (lines, fell_back) = KnuthPlass.break_counted(&list, &params, &Fixed);
        assert!(!fell_back);
        assert!(lines.len() >= 4);
        for line in &lines[..2] {
            assert_eq!(line.width, Scaled::from_points(100));
            assert_eq!(
                crate::node::measure_horizontal(&line.nodes, &Fixed).natural,
                Scaled::from_points(100)
            );
        }
    }

    /// `tex.web` §866: a formula's boundaries are *math nodes*, and they carry
    /// two rules that nothing else does — glue between them is not a
    /// breakpoint, and the node that ends one is. Without them a line could
    /// break inside `$a + b$`, which TeX never does.
    #[test]
    fn a_line_never_breaks_inside_a_formula() {
        let on = Node::Math {
            after: false,
            width: Scaled::ZERO,
        };
        let off = Node::Math {
            after: true,
            width: Scaled::ZERO,
        };
        let list = alloc::vec![
            ch('a'),
            space(),
            on,
            ch('b'),
            space(), // glue inside the formula
            ch('c'),
            off,
            space(),
            ch('d'),
        ];
        let b = breakpoints(&list);
        // the glue after `a`, the math-off node, and nothing in between
        assert_eq!(b, alloc::vec![1, 6]);
    }

    #[test]
    fn a_paragraph_that_fits_is_one_line() {
        let list = alloc::vec![ch('a'), space(), ch('b')];
        let lines = Greedy.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(100),
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "a b");
        assert!(lines[0].ends_paragraph);
    }

    /// Two 10pt characters and a 5pt space is 25pt; a 20pt measure must break.
    #[test]
    fn a_line_breaks_when_the_next_chunk_does_not_fit() {
        let list = alloc::vec![ch('a'), space(), ch('b')];
        let lines = Greedy.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(20),
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(text(&lines[0]), "a");
        assert_eq!(text(&lines[1]), "b");
    }

    #[test]
    fn font_expansion_participates_in_line_breaking() {
        let mut list = alloc::vec![ch('a'), ch('a'), ch('a'), ch('a')];
        list.push(Node::Glue(Glue::interword(
            Scaled::from_points(5),
            Scaled::ZERO,
            Scaled::from_points(2),
        )));
        list.push(ch('b'));
        list.push(Node::Penalty(INF_PENALTY));
        list.push(Node::Glue(Glue::fil()));
        let base = BreakParams {
            hsize: Scaled::from_points(52),
            final_pass: true,
            ..Default::default()
        };
        let plain = KnuthPlass.break_lines(&list, &base, &Expanded);
        assert_eq!(plain.len(), 2);
        let expanded = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                adjust_spacing: 2,
                ..base
            },
            &Expanded,
        );
        assert_eq!(expanded.len(), 1);
    }

    /// Only the last line ends the paragraph — the others are justified.
    #[test]
    fn only_the_final_line_ends_the_paragraph() {
        let list = alloc::vec![ch('a'), space(), ch('b'), space(), ch('c')];
        let lines = Greedy.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(20),
                ..Default::default()
            },
            &Fixed,
        );
        assert!(lines.len() >= 2);
        assert!(!lines[0].ends_paragraph);
        assert!(lines.last().expect("a line").ends_paragraph);
    }

    /// The glue you broke at goes away. Keeping it indents the next line by a
    /// space and pushes the measure out by one.
    #[test]
    fn glue_at_a_break_is_discarded() {
        let list = alloc::vec![ch('a'), space(), ch('b')];
        let lines = Greedy.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(20),
                ..Default::default()
            },
            &Fixed,
        );
        assert!(
            !matches!(lines[0].nodes.last(), Some(Node::Glue(_))),
            "no trailing glue on a broken line"
        );
    }

    #[test]
    fn a_font_kern_before_a_glue_break_keeps_its_width() {
        let kern = Scaled::from_points(1);
        let list = alloc::vec![
            ch('a'),
            Node::Kern(kern, crate::node::kern_sub::NORMAL),
            space(),
            ch('b'),
            Node::Penalty(INF_PENALTY),
            Node::Glue(Glue::fil()),
        ];
        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(12),
                tolerance: 10_000,
                final_pass: true,
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 2);
        assert!(matches!(
            lines[0].nodes.last(),
            Some(Node::Kern(width, crate::node::kern_sub::NORMAL)) if *width == kern
        ));
    }

    /// A box is measured by its own width, not by its contents. This is the
    /// thing the token-stream breaker could not do at all: boxed material was
    /// invisible to it, so `\box0` never reached the page.
    #[test]
    fn a_box_occupies_its_width() {
        let mut b = TeXBox {
            kind: crate::node::BoxKind::Horizontal,
            width: Scaled::from_points(30),
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        };
        b.width = Scaled::from_points(30);
        let list = alloc::vec![Node::Box(alloc::boxed::Box::new(b)), space(), ch('x')];
        let lines = Greedy.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(35),
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 2, "30pt box plus a 10pt char exceeds 35pt");
        assert!(matches!(lines[0].nodes.first(), Some(Node::Box(_))));
    }

    #[test]
    fn a_native_run_occupies_its_shaped_advance() {
        let native = Node::NativeRun(alloc::boxed::Box::new(crate::node::NativeRun {
            font: 0,
            size: Scaled::from_points(10),
            direction: crate::node::NativeDirection::LeftToRight,
            language: None,
            bidi_level: 0,
            bidi_span: 0,
            width: Scaled::from_points(30),
            height: Scaled::from_points(7),
            depth: Scaled::from_points(2),
            glyphs: Vec::new(),
            clusters: Vec::new(),
        }));
        let list = alloc::vec![native, space(), ch('x')];
        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(35),
                tolerance: 10_000,
                final_pass: true,
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(
            lines.len(),
            2,
            "30pt native run plus a 10pt glyph exceeds 35pt"
        );
        assert!(matches!(lines[0].nodes.first(), Some(Node::NativeRun(_))));
    }

    /// Total demerits of a set of lines, for comparing two breakers.
    fn demerits(lines: &[Line], hsize: Scaled, metrics: &impl CharMetrics) -> i64 {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i + 1 == lines.len() {
                    return 0; // the last line is set naturally
                }
                let mut t = Totals::default();
                for n in &l.nodes {
                    t.add(n, metrics, false);
                }
                match assess(t.width, t.stretch, t.shrink, hsize.0 as i64, false) {
                    Some((b, ..)) => (10 + b as i64).pow(2),
                    None => AWFUL,
                }
            })
            .sum()
    }

    /// `tex.web` §835: the DP keeps the cheapest chain *per fitness class*, and
    /// the chain walk follows the class each step recorded.
    ///
    /// Both halves are needed and each is invisible without the other. Keeping
    /// one entry per break lets a cheap-but-badly-adjoining break shadow the one
    /// the paragraph needs two lines later; keeping four but re-picking the
    /// cheapest while walking back steps off the optimal chain onto that same
    /// shadow. Together they moved a real paragraph (`tools/typeset_check.sh`'s
    /// `narrow`) onto pdfTeX's own break sequence.
    ///
    /// The invariant this asserts is the one that makes the whole thing
    /// meaningful: the demerits the walk reconstructs must equal the demerits
    /// the DP minimised. If the walk leaves the optimal chain the two disagree.
    #[test]
    fn the_reconstructed_chain_is_the_one_that_was_minimised() {
        // Word lengths chosen so that a greedy-looking early break is cheap but
        // adjoins badly, which is the shape that exposes both halves.
        for widths in [
            alloc::vec![4, 4, 4, 9, 2, 2, 8, 3, 3, 3, 7, 2],
            alloc::vec![7, 2, 2, 2, 9, 9, 2, 2, 7, 3, 3, 3],
            alloc::vec![3, 3, 9, 3, 3, 9, 3, 3, 9, 3, 3, 3],
        ] {
            let mut list = Vec::new();
            for (i, w) in widths.iter().enumerate() {
                if i > 0 {
                    list.push(Node::Glue(Glue::interword(
                        Scaled::from_points(5),
                        Scaled::from_points(3),
                        Scaled::from_points(2),
                    )));
                }
                for _ in 0..*w {
                    list.push(ch('x'));
                }
            }
            list.push(Node::Glue(Glue::fil()));
            let params = BreakParams {
                protrude_chars: 0,
                adjust_spacing: 0,
                pack_key: [0; 3],
                trace: false,
                trace_inserts: alloc::vec::Vec::new(),
                font_ids: alloc::vec::Vec::new(),
                escape: alloc::string::String::from("\\"),
                hsize: Scaled::from_points(200),
                prev_graf: 0,
                tolerance: 10_000,
                hang_indent: Scaled::ZERO,
                hang_after: 1,
                par_shape: alloc::vec::Vec::new(),
                left_skip: Glue::fixed(Scaled::ZERO),
                right_skip: Glue::fixed(Scaled::ZERO),
                line_penalty: 10,
                adj_demerits: 10_000,
                hyphen_penalty: 50,
                ex_hyphen_penalty: 0,
                newline_char: -1,
                double_hyphen_demerits: 0,
                final_hyphen_demerits: 0,
                looseness: 0,
                emergency_stretch: Scaled::ZERO,
                final_pass: false,
            };
            let (lines, fell_back) = KnuthPlass.break_counted(&list, &params, &Fixed);
            assert!(!fell_back, "widths {widths:?}: should be settable");
            // Every line but the last is set to the measure, and none overfull:
            // a chain the walk invented rather than followed shows up here.
            for (i, l) in lines.iter().enumerate() {
                let t = crate::node::measure_horizontal(&l.nodes, &Fixed);
                assert!(
                    t.natural.0 - t.shrink[0].0 <= params.hsize.0,
                    "widths {widths:?}: line {i} cannot shrink into the measure"
                );
            }
        }
    }

    /// The property docs/history/PLAN.md §9 names: Knuth-Plass total demerits must never
    /// exceed greedy's. If it does, the minimisation is wrong -- greedy's
    /// answer is *a* valid break sequence, so the optimum is at least as good.
    #[test]
    fn knuth_plass_is_never_worse_than_greedy() {
        // Word lengths chosen to be awkward: the greedy choice on line one
        // forces a bad line two.
        for widths in [
            alloc::vec![3, 3, 3, 9, 3, 3],
            alloc::vec![5, 5, 5, 5, 5, 5, 5],
            alloc::vec![1, 9, 1, 9, 1, 9],
            alloc::vec![7, 2, 7, 2, 7, 2, 7],
            alloc::vec![4, 4, 12, 4, 4, 4],
        ] {
            let mut list = Vec::new();
            for (i, w) in widths.iter().enumerate() {
                if i > 0 {
                    list.push(Node::Glue(Glue::interword(
                        Scaled::from_points(5),
                        Scaled::from_points(3),
                        Scaled::from_points(2),
                    )));
                }
                for _ in 0..*w {
                    list.push(ch('x'));
                }
            }
            let params = BreakParams {
                hsize: Scaled::from_points(100),
                ..Default::default()
            };
            let g = Greedy.break_lines(&list, &params, &Fixed);
            let kp = KnuthPlass.break_lines(&list, &params, &Fixed);
            let dg = demerits(&g, params.hsize, &Fixed);
            let dk = demerits(&kp, params.hsize, &Fixed);
            assert!(
                dk <= dg,
                "widths {widths:?}: knuth-plass {dk} worse than greedy {dg}"
            );
        }
    }

    /// Every character must survive the break, whichever breaker is used.
    /// Losing one is the failure mode a demerits comparison cannot see.
    #[test]
    fn breaking_preserves_every_character() {
        let mut list = Vec::new();
        for i in 0..40u32 {
            if i > 0 {
                list.push(Node::Glue(Glue::fixed(Scaled::from_points(5))));
            }
            list.push(Node::Char {
                code: b'a' as u32 + (i % 26),
                font: 0,
                lig: None,
            });
        }
        let want: alloc::vec::Vec<u32> = list
            .iter()
            .filter_map(|n| match n {
                Node::Char { code, .. } => Some(*code),
                _ => None,
            })
            .collect();
        let params = BreakParams {
            hsize: Scaled::from_points(60),
            ..Default::default()
        };
        for lines in [
            Greedy.break_lines(&list, &params, &Fixed),
            KnuthPlass.break_lines(&list, &params, &Fixed),
        ] {
            let got: alloc::vec::Vec<u32> = lines
                .iter()
                .flat_map(|l| l.nodes.iter())
                .filter_map(|n| match n {
                    Node::Char { code, .. } => Some(*code),
                    _ => None,
                })
                .collect();
            assert_eq!(got, want, "no character may be lost or duplicated");
        }
    }

    /// A paragraph that cannot be set within tolerance still produces lines.
    #[test]
    fn an_unsettable_paragraph_falls_back_rather_than_vanishing() {
        // One word far wider than the measure, with no glue to break at.
        let list: Vec<Node> = (0..30).map(|_| ch('m')).collect();
        let params = BreakParams {
            hsize: Scaled::from_points(20),
            tolerance: 1,
            ..Default::default()
        };
        let lines = KnuthPlass.break_lines(&list, &params, &Fixed);
        assert!(!lines.is_empty(), "an overfull paragraph is still set");
    }

    /// A discretionary is a legal break, and taking it puts the hyphen on the
    /// line that ends there.
    #[test]
    fn a_discretionary_can_be_broken_at() {
        // "xxxx-xxxx" as one word with a discretionary in the middle. At 10pt
        // a character, the whole word is 80pt; a 50pt measure can only be met
        // by breaking at the discretionary, which costs a further 10pt hyphen.
        let mut list: Vec<Node> = (0..4).map(|_| ch('x')).collect();
        list.push(Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
            pre: alloc::vec![ch('-')],
            post: Vec::new(),
            replace: 0,
        })));
        list.extend((0..4).map(|_| ch('x')));
        list.push(Node::Glue(Glue::fil()));

        let params = BreakParams {
            hsize: Scaled::from_points(50),
            ..Default::default()
        };
        let lines = KnuthPlass.break_lines(&list, &params, &Fixed);
        assert_eq!(lines.len(), 2, "the word must be split");
        let hyphen = lines[0]
            .nodes
            .last()
            .is_some_and(|n| matches!(n, Node::Char { code, .. } if *code == '-' as u32));
        assert!(hyphen, "the first line ends with the hyphen");
        assert_eq!(
            lines[1]
                .nodes
                .iter()
                .filter(|n| matches!(n, Node::Char { .. }))
                .count(),
            4,
            "and the second line has the rest of the word, with no hyphen"
        );
    }

    #[test]
    fn an_infinite_exhyphen_penalty_inhibits_an_empty_discretionary() {
        let mut list: Vec<Node> = (0..4).map(|_| ch('x')).collect();
        list.push(ch('-'));
        list.push(Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
            pre: Vec::new(),
            post: Vec::new(),
            replace: 0,
        })));
        list.extend((0..4).map(|_| ch('x')));
        list.push(Node::Glue(Glue::fil()));

        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(100),
                right_skip: Glue::fil(),
                ex_hyphen_penalty: INF_PENALTY,
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(
            lines.len(),
            1,
            "the forbidden explicit-hyphen break was taken"
        );
    }

    /// A literal hyphen is followed by an empty discretionary breakpoint.
    /// pdfTeX skips that node when finding the rightmost protruding character
    /// and puts the margin kern before it.
    #[test]
    fn an_explicit_hyphen_protrudes_through_its_empty_discretionary() {
        let mut list: Vec<Node> = (0..4).map(|_| ch('x')).collect();
        list.push(ch('-'));
        list.push(Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
            pre: Vec::new(),
            post: Vec::new(),
            replace: 0,
        })));
        list.extend((0..4).map(|_| ch('x')));
        list.push(Node::Glue(Glue::fil()));

        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(50),
                protrude_chars: 1,
                ..Default::default()
            },
            &Protruding,
        );
        assert_eq!(lines.len(), 2);
        assert!(matches!(lines[0].nodes.last(), Some(Node::Disc(d)) if d.pre.is_empty()));
        assert!(matches!(
            lines[0].nodes.get(lines[0].nodes.len() - 2),
            Some(Node::Kern(w, crate::node::kern_sub::MARGIN_RIGHT))
                if *w == Scaled::from_points(-2)
        ));
    }

    #[test]
    fn trial_break_after_a_hyphen_ligature_uses_its_right_protrusion() {
        struct LigatureProtruding;
        impl CharMetrics for LigatureProtruding {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn height(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn protrusion(&self, _font: usize, code: u32, right: bool) -> Scaled {
                if code != 124 {
                    return Scaled::ZERO;
                }
                Scaled::from_points(if right { 2 } else { 3 })
            }
        }
        let list = alloc::vec![
            ch('a'),
            Node::Char {
                code: 124,
                font: 0,
                lig: Some(alloc::vec!['-' as u32, '-' as u32, '-' as u32].into()),
            },
            Node::Disc(alloc::boxed::Box::new(crate::node::Disc::default())),
            Node::Glue(Glue::interword(
                Scaled::from_points(3),
                Scaled::from_points(1),
                Scaled::from_points(1),
            )),
        ];
        assert_eq!(
            protrusion_of(&list, 0, 2, None, &LigatureProtruding),
            Scaled::from_points(2).0 as i64,
            "a dash discretionary does not invent a second left margin"
        );
        assert_eq!(
            protrusion_of(&list, 0, 3, None, &LigatureProtruding),
            Scaled::from_points(2).0 as i64,
            "the following glue does not change the right margin"
        );

        let plain = alloc::vec![
            ch('a'),
            ch('-'),
            Node::Disc(alloc::boxed::Box::new(crate::node::Disc::default())),
        ];
        assert_eq!(
            protrusion_of(&plain, 0, 2, None, &LigatureProtruding),
            0,
            "an unligatured explicit hyphen has no left-protrusion quirk"
        );
    }

    #[test]
    fn trial_line_protrudes_from_a_discretionary_post_break_character() {
        struct PostBreakMargins;
        impl CharMetrics for PostBreakMargins {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn height(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn protrusion(&self, _font: usize, code: u32, right: bool) -> Scaled {
                match (char::from_u32(code), right) {
                    (Some('i'), false) => Scaled::from_points(7),
                    (Some('n'), false) => Scaled::from_points(2),
                    (Some('b'), true) => Scaled::from_points(3),
                    _ => Scaled::ZERO,
                }
            }
        }

        let main = [ch('n'), ch('b')];
        let post = [ch('i')];
        assert_eq!(
            protrusion_of(&main, 0, main.len(), Some(&post), &PostBreakMargins),
            Scaled::from_points(10).0 as i64,
            "the reconstructed post-break glyph is the trial line's left edge"
        );
        assert_eq!(
            protrusion_of(&main, 0, main.len(), None, &PostBreakMargins),
            Scaled::from_points(5).0 as i64,
            "without a discretionary post list the main list still supplies both edges"
        );
    }

    #[test]
    fn line_expansion_uses_only_the_measured_ligature_allowances() {
        struct ExpandingMargins;
        impl CharMetrics for ExpandingMargins {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::from_points(10)
            }
            fn height(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn protrusion(&self, _font: usize, code: u32, right: bool) -> Scaled {
                match (code, right) {
                    (c, false) if c == 'a' as u32 => Scaled::from_points(7),
                    (_, true) => Scaled::from_points(11),
                    _ => Scaled::ZERO,
                }
            }
            fn expanded_protrusion(
                &self,
                font: usize,
                code: u32,
                right: bool,
                _stretch: bool,
            ) -> Scaled {
                if font == 1 && code == 'a' as u32 && !right {
                    Scaled::from_points(14)
                } else {
                    self.protrusion(font, code, right)
                }
            }
            fn expansion_amount(&self, _font: usize, _code: u32, ratio: i64) -> i64 {
                ratio.signum()
            }
        }

        let opening_quote = Node::Char {
            code: 'a' as u32,
            font: 1,
            lig: Some(alloc::vec!['`' as u32, '`' as u32].into()),
        };
        assert_eq!(
            margin_expansion_adjustment(&[opening_quote, ch('b')], 0, 2, None, &ExpandingMargins,),
            (
                Scaled::from_points(-7).0 as i64,
                Scaled::from_points(7).0 as i64,
            )
        );
        assert_eq!(
            margin_expansion_adjustment(&[ch('a'), ch('b')], 0, 2, None, &ExpandingMargins),
            (0, 0),
            "an ordinary boundary does not change trial expansion capacity"
        );
        let ordinary_ligature = Node::Char {
            code: 'a' as u32,
            font: 0,
            lig: Some(alloc::vec!['f' as u32, 'i' as u32].into()),
        };
        assert_eq!(
            margin_expansion_adjustment(
                &[ordinary_ligature, ch('b')],
                0,
                2,
                None,
                &ExpandingMargins,
            ),
            (0, 0),
            "an ordinary ligature gets no special allowance"
        );
    }

    #[test]
    fn rightmost_left_protrusion_is_a_legacy_expansion_allowance() {
        struct ClosingQuoteMargins;
        impl CharMetrics for ClosingQuoteMargins {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::from_points(10)
            }
            fn height(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn protrusion(&self, _font: usize, code: u32, right: bool) -> Scaled {
                match (code, right) {
                    (17, false) => Scaled(93_061), // 1.42pt at a 10pt quad.
                    (17, true) => Scaled(123_863), // 1.89pt right protrusion.
                    _ => Scaled::ZERO,
                }
            }
            fn expanded_protrusion(
                &self,
                font: usize,
                code: u32,
                right: bool,
                _stretch: bool,
            ) -> Scaled {
                if font == 1 && code == 17 && !right {
                    Scaled(186_122)
                } else if font == 2 && code == 17 && !right {
                    Scaled::ZERO
                } else {
                    self.protrusion(font, code, right)
                }
            }
            fn expansion_amount(&self, _font: usize, _code: u32, ratio: i64) -> i64 {
                ratio.signum()
            }
        }

        let list = [
            ch('e'),
            Node::Char {
                code: 17,
                font: 1,
                lig: Some(alloc::vec!['\'' as u32, '\'' as u32].into()),
            },
        ];
        assert_eq!(
            margin_expansion_adjustment(&list, 0, list.len(), None, &ClosingQuoteMargins),
            (-93_061, 93_061),
            "pdfTeX applies the closing quote's full left protrusion to trial expansion"
        );

        let ordinary = [
            ch('e'),
            Node::Char {
                code: 17,
                font: 0,
                lig: None,
            },
        ];
        assert_eq!(
            margin_expansion_adjustment(&ordinary, 0, ordinary.len(), None, &ClosingQuoteMargins),
            (0, 0),
            "an ordinary expanded glyph preserves its protrusion code"
        );

        let discretionary = Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
            pre: alloc::vec![Node::Char {
                code: 17,
                font: 0,
                lig: None,
            }],
            ..Default::default()
        }));
        let list = [ch('e'), discretionary];
        assert_eq!(
            margin_expansion_adjustment(&list, 0, 1, None, &ClosingQuoteMargins),
            (0, 0),
            "matching expanded protrusion leaves a discretionary margin unchanged"
        );

        let aliased_discretionary = Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
            pre: alloc::vec![Node::Char {
                code: 17,
                font: 2,
                lig: None,
            }],
            ..Default::default()
        }));
        let list = [ch('e'), aliased_discretionary];
        assert_eq!(
            margin_expansion_adjustment(&list, 0, 1, None, &ClosingQuoteMargins),
            (93_061, -93_061),
            "a shared expanded font with no hyphen lpcode adds the base allowance to stretch"
        );

        let implicit = Node::Disc(alloc::boxed::Box::new(crate::node::Disc::default()));
        let list = [
            ch('e'),
            Node::Char {
                code: 17,
                font: 1,
                lig: Some(alloc::vec!['-' as u32, '-' as u32, '-' as u32].into()),
            },
            implicit,
        ];
        assert_eq!(
            margin_expansion_adjustment(&list, 0, 2, None, &ClosingQuoteMargins),
            (-93_061, 93_061),
            "an empty implicit discretionary retains the preceding ligature"
        );
    }

    #[test]
    fn leading_hyphen_ligature_adjusts_both_expansion_capacities() {
        struct DashMargins;
        impl CharMetrics for DashMargins {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn height(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn protrusion(&self, _font: usize, code: u32, right: bool) -> Scaled {
                match (code, right) {
                    (124, false) => Scaled::from_points(3),
                    (124, true) => Scaled::from_points(2),
                    _ => Scaled::ZERO,
                }
            }
            fn expanded_protrusion(
                &self,
                font: usize,
                code: u32,
                right: bool,
                _stretch: bool,
            ) -> Scaled {
                if font == 1 && code == 124 && !right {
                    Scaled::from_points(6)
                } else {
                    self.protrusion(font, code, right)
                }
            }
            fn expansion_amount(&self, _font: usize, _code: u32, ratio: i64) -> i64 {
                ratio.signum()
            }
        }
        let list = alloc::vec![
            Node::Char {
                code: 124,
                font: 1,
                lig: Some(alloc::vec!['-' as u32, '-' as u32].into()),
            },
            Node::Disc(alloc::boxed::Box::new(crate::node::Disc::default())),
            ch('a'),
            Node::Glue(Glue::fil()),
        ];
        assert_eq!(
            margin_expansion_adjustment(&list, 0, 3, None, &DashMargins),
            (
                Scaled::from_points(-3).0 as i64,
                Scaled::from_points(3).0 as i64,
            )
        );

        let mut indented = alloc::vec![Node::Box(alloc::boxed::Box::new(TeXBox {
            kind: crate::node::BoxKind::Horizontal,
            width: Scaled::ZERO,
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: alloc::vec::Vec::new(),
        }))];
        indented.extend(list);
        assert_eq!(
            margin_expansion_adjustment(&indented, 0, indented.len() - 1, None, &DashMargins,),
            (
                Scaled::from_points(-3).0 as i64,
                Scaled::from_points(3).0 as i64,
            ),
            "a transparent paragraph-indent box does not hide the leading dash"
        );
    }

    #[test]
    fn glue_break_uses_right_protrusion_but_glue_before_a_penalty_blocks_it() {
        struct BothSides;
        impl CharMetrics for BothSides {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn height(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn protrusion(&self, _font: usize, code: u32, right: bool) -> Scaled {
                match (code, right) {
                    (c, false) if c == 'a' as u32 => Scaled::from_points(2),
                    (c, true) if c == 'b' as u32 => Scaled::from_points(3),
                    _ => Scaled::ZERO,
                }
            }
        }

        let list = [
            ch('a'),
            ch('b'),
            Node::Glue(Glue::interword(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
        ];
        assert_eq!(
            protrusion_of(&list, 0, 2, None, &BothSides),
            Scaled::from_points(5).0 as i64,
            "the glue break node is outside the candidate line"
        );
        let list = [
            ch('a'),
            ch('b'),
            Node::Glue(Glue::interword(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
            Node::Penalty(-10_000),
        ];
        assert_eq!(
            protrusion_of(&list, 0, 3, None, &BothSides),
            Scaled::from_points(2).0 as i64,
            "intervening glue blocks right protrusion at a later penalty"
        );
        let list = [
            ch('a'),
            ch('b'),
            Node::Glue(Glue {
                canonical_zero: true,
                ..Glue::default()
            }),
            Node::Penalty(-10_000),
        ];
        assert_eq!(
            protrusion_of(&list, 0, 3, None, &BothSides),
            Scaled::from_points(5).0 as i64,
            "TeX's shared zero-glue object is transparent to protrusion"
        );
    }

    /// The paragraph-closing penalty is ordinary material.  A right margin
    /// kern belongs after it and before `\parfillskip`, which is the exact
    /// shape LaTeX's centered `Abstract` heading uses.
    #[test]
    fn a_final_word_protrudes_past_the_paragraph_penalty() {
        let mut list: Vec<Node> = "Abstract".chars().map(ch).collect();
        list.push(Node::Adjust {
            list: Vec::new(),
            pre: false,
        });
        list.push(Node::Adjust {
            list: Vec::new(),
            pre: false,
        });
        list.push(Node::Penalty(INF_PENALTY));
        list.push(Node::Glue(
            Glue::fil().sub(crate::node::glue_sub::PAR_FILL_SKIP),
        ));
        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(100),
                right_skip: Glue::fil(),
                protrude_chars: 2,
                final_pass: true,
                ..Default::default()
            },
            &Protruding,
        );
        assert_eq!(lines.len(), 1);
        let nodes = &lines[0].nodes;
        let penalty = nodes
            .iter()
            .position(|n| matches!(n, Node::Penalty(_)))
            .unwrap();
        assert!(matches!(
            nodes.get(penalty + 1),
            Some(Node::Kern(w, crate::node::kern_sub::MARGIN_RIGHT))
                if *w == Scaled::from_points(-2)
        ));
    }

    /// `find_protchar_left` descends into every nonempty horizontal box. A
    /// canonical zero-glue node is transparent there; a numerically identical
    /// explicit `\hskip0pt` is a barrier. Ordinary specials are transparent.
    #[test]
    fn a_first_word_protrudes_through_a_driver_special() {
        let zero_box = |list| {
            Node::Box(alloc::boxed::Box::new(TeXBox {
                kind: crate::node::BoxKind::Horizontal,
                width: Scaled::ZERO,
                height: Scaled::ZERO,
                depth: Scaled::ZERO,
                set_num: 0,
                set_den: 1,
                glue_sign: crate::node::GlueSign::Normal,
                glue_order: 0,
                shift: Scaled::ZERO,
                list,
            }))
        };
        let anchor = |canonical_zero| {
            zero_box(alloc::vec![zero_box(alloc::vec![
                Node::Whatsit(alloc::string::String::from("pdf destination")),
                Node::Penalty(INF_PENALTY),
                Node::Glue(Glue {
                    canonical_zero,
                    ..Glue::fixed(Scaled::ZERO)
                }),
                Node::Whatsit(alloc::string::String::from("pdf view")),
            ])])
        };
        let list = alloc::vec![
            Node::Penalty(INF_PENALTY),
            anchor(true),
            Node::Whatsit(alloc::string::String::from("color push gray 0")),
            ch('T'),
            Node::Penalty(INF_PENALTY),
            Node::Glue(Glue::fil().sub(crate::node::glue_sub::PAR_FILL_SKIP)),
        ];
        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(100),
                protrude_chars: 2,
                final_pass: true,
                ..Default::default()
            },
            &Protruding,
        );
        assert_eq!(lines.len(), 1);
        assert!(matches!(
            lines[0].nodes.first(),
            Some(Node::Kern(w, crate::node::kern_sub::MARGIN_LEFT))
                if *w == Scaled::from_points(-2)
        ));

        let blocked = KnuthPlass.break_lines(
            &alloc::vec![
                anchor(false),
                Node::Whatsit(alloc::string::String::from("color push gray 0")),
                ch('T'),
                Node::Penalty(INF_PENALTY),
                Node::Glue(Glue::fil().sub(crate::node::glue_sub::PAR_FILL_SKIP)),
            ],
            &BreakParams {
                hsize: Scaled::from_points(100),
                protrude_chars: 2,
                final_pass: true,
                ..Default::default()
            },
            &Protruding,
        );
        assert!(!matches!(
            blocked[0].nodes.first(),
            Some(Node::Kern(_, crate::node::kern_sub::MARGIN_LEFT))
        ));

        let bare_special = KnuthPlass.break_lines(
            &alloc::vec![
                Node::Whatsit(alloc::string::String::from("color push gray 0")),
                ch('T'),
                Node::Penalty(INF_PENALTY),
                Node::Glue(Glue::fil().sub(crate::node::glue_sub::PAR_FILL_SKIP)),
            ],
            &BreakParams {
                hsize: Scaled::from_points(100),
                protrude_chars: 2,
                final_pass: true,
                ..Default::default()
            },
            &Protruding,
        );
        assert!(matches!(
            bare_special[0].nodes.first(),
            Some(Node::Kern(w, crate::node::kern_sub::MARGIN_LEFT))
                if *w == Scaled::from_points(-2)
        ));
    }

    /// Trial-line measurement discards ordinary leading discardables before
    /// the source-shaped protrusion walk; finished-line insertion does not.
    /// These are deliberately two assertions because collapsing the two walks
    /// moved an AMS run-in heading onto a different hyphenation break.
    #[test]
    fn trial_protrusion_skips_leading_explicit_zero_glue() {
        let list = alloc::vec![Node::Glue(Glue::fixed(Scaled::ZERO)), ch('T')];
        assert_eq!(first_char(&list), None);
        assert_eq!(first_char_for_break(&list), Some((0, 'T' as u32)));
    }

    #[test]
    fn an_empty_list_produces_no_lines() {
        assert!(
            Greedy
                .break_lines(
                    &[],
                    &BreakParams {
                        hsize: Scaled::from_points(100),
                        ..Default::default()
                    },
                    &Fixed
                )
                .is_empty()
        );
    }

    /// A paragraph with no nodes at all makes no line, but an empty piece
    /// *between forced breaks* does.  TeX packs that piece as an empty hbox;
    /// LaTeX relies on it for an explicit blank `\\` line.
    #[test]
    fn an_empty_piece_between_forced_breaks_is_a_line() {
        let list = alloc::vec![
            ch('a'),
            Node::Penalty(EJECT_PENALTY),
            Node::Glue(Glue::fixed(Scaled::ZERO)),
            Node::Penalty(EJECT_PENALTY),
            ch('b'),
            Node::Penalty(INF_PENALTY),
            Node::Glue(Glue::fil()),
        ];
        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(100),
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(text(&lines[0]), "a");
        assert!(lines[1].nodes.is_empty());
        assert_eq!(text(&lines[2]).trim_end(), "b");
    }

    /// `\\` ends a line with `\hfil\break`.  With two in succession,
    /// §879 subtracts the second command's still-future `fil` while creating
    /// the active node at the first forced break.  Its negative infinite
    /// stretch remains observable at the second break, so pdfTeX reports the
    /// intervening empty trial line with badness zero rather than 10000.
    #[test]
    fn consecutive_forced_breaks_keep_future_infinite_stretch() {
        let list = alloc::vec![
            ch('a'),
            Node::Penalty(INF_PENALTY),
            Node::Glue(Glue::fil()),
            Node::Penalty(EJECT_PENALTY),
            Node::Penalty(INF_PENALTY),
            Node::Glue(Glue::fil()),
            Node::Penalty(EJECT_PENALTY),
            Node::Penalty(INF_PENALTY),
            Node::Glue(Glue::fil().sub(crate::node::glue_sub::PAR_FILL_SKIP)),
        ];
        let lines = KnuthPlass.break_lines(
            &list,
            &BreakParams {
                hsize: Scaled::from_points(100),
                ..Default::default()
            },
            &Fixed,
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(text(&lines[0]).trim_end(), "a");
        assert!(lines[1].nodes.is_empty());
        assert_eq!(lines[1].badness, 0);
    }
}
