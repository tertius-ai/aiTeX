//! The typesetting half of the fast engine.
//!
//! [`crate::fast::Fast`] is a mouth and an eqtb: it resolves what a token stream
//! *says*. This is where what it says becomes material — characters and glue
//! become nodes, paragraphs become lines, lines become a page.
//!
//! Nothing here is new machinery. `node`, `tfm`, `linebreak`, `page`, `shipout`
//! and `dvi` were written for the layered engine and depend only on `node` and
//! `scaled` — not on `Eqtb`, `Gullet` or `Stomach`. That independence is what
//! makes this file a *join* rather than a second implementation: it is the
//! adapter that turns `Fast`'s state into the arguments those modules already
//! take.
//!
//! What is deliberately not here yet: `\output` (the page builder is driven in
//! its simplest mode, one `\vsize` worth of material per page), inserts,
//! alignment and math. Each of those has working code in `stomach.rs` waiting
//! for the same treatment.

use crate::linebreak::{BreakParams, KnuthPlass, LineBreaker};
use crate::node::{BoxKind, CharMetrics, Glue, Node, TeXBox, hpack, vpack};
use crate::page::{Decision, PageBuilder};
use crate::scaled::Scaled;
use crate::tfm::{LigKern, TfmFont};
use alloc::string::String;
use alloc::vec::Vec;

/// `prev_depth` before anything has been contributed. `tex.web` §212 uses
/// -1000pt and suppresses the interline glue above it, which is what keeps a page
/// from starting with a baselineskip gap.
pub const IGNORE_DEPTH: Scaled = Scaled(-65536000);

/// Which list material is being appended to. TeX has six modes; a format load
/// plus a plain paragraph needs two, and adding the rest is a matter of the
/// primitives that enter them existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Vertical,
    Horizontal,
    Math,
}

/// A font table that answers [`CharMetrics`] — what `hpack` and the line
/// breaker measure with. Borrowed for the duration of one call so the font
/// table is never borrowed while a list is being extended.
struct FontTable<'a>(&'a [TfmFont]);

/// Translate the common TFM program (kerns and op-zero ligatures) straight to
/// final nodes. The general shaper has to retain a positional work graph for
/// TeX's operand-retaining operators; ordinary text fonts have none, and
/// constructing that graph once per word was pure allocation traffic.
fn shape_simple_nodes(
    tfm: &TfmFont,
    chars: &[u32],
    left_boundary: bool,
    right_boundary: bool,
    font: usize,
    out: &mut Vec<Node>,
) {
    let Some((&first, rest)) = chars.split_first() else {
        return;
    };
    let raw = |code| Node::Char {
        code,
        font,
        lig: None,
    };
    match left_boundary
        .then(|| tfm.left_boundary_lig_kern(first))
        .flatten()
    {
        Some(LigKern::Kern(width)) => {
            out.push(Node::Kern(width, crate::node::kern_sub::NORMAL));
            out.push(raw(first));
        }
        Some(LigKern::Ligature { code, op: 0 }) => out.push(Node::Char {
            code,
            font,
            lig: Some(alloc::vec![crate::node::LIG_BOUNDARY, first].into_boxed_slice()),
        }),
        _ => out.push(raw(first)),
    }

    for &right in rest {
        let command = match out.last() {
            Some(Node::Char { code, font: f, .. }) if *f == font => tfm.lig_kern(*code, right),
            _ => None,
        };
        match command {
            Some(LigKern::Kern(width)) => {
                out.push(Node::Kern(width, crate::node::kern_sub::NORMAL));
                out.push(raw(right));
            }
            Some(LigKern::Ligature { code, op: 0 }) => {
                let Some(Node::Char {
                    code: left, lig, ..
                }) = out.pop()
                else {
                    unreachable!("a simple ligature has a glyph on its left")
                };
                let mut source = lig.map_or_else(|| alloc::vec![left], Vec::from);
                source.push(right);
                out.push(Node::Char {
                    code,
                    font,
                    lig: Some(source.into_boxed_slice()),
                });
            }
            _ => out.push(raw(right)),
        }
    }

    if right_boundary {
        let command = match out.last() {
            Some(Node::Char { code, font: f, .. }) if *f == font => tfm
                .boundary_char()
                .and_then(|boundary| tfm.lig_kern(*code, boundary)),
            _ => None,
        };
        match command {
            Some(LigKern::Kern(width)) => {
                out.push(Node::Kern(width, crate::node::kern_sub::NORMAL));
            }
            Some(LigKern::Ligature { code, op: 0 }) => {
                let Some(Node::Char {
                    code: left, lig, ..
                }) = out.pop()
                else {
                    unreachable!("a boundary ligature has a glyph on its left")
                };
                let mut source = lig.map_or_else(|| alloc::vec![left], Vec::from);
                source.push(crate::node::LIG_BOUNDARY);
                out.push(Node::Char {
                    code,
                    font,
                    lig: Some(source.into_boxed_slice()),
                });
            }
            _ => {}
        }
    }
}

/// The same table, public so the alignment driver can measure entries.
pub struct FontTableRef<'a>(pub &'a [TfmFont]);

impl CharMetrics for FontTableRef<'_> {
    fn width(&self, font: usize, code: u32) -> Scaled {
        FontTable(self.0).width(font, code)
    }
    fn height(&self, font: usize, code: u32) -> Scaled {
        FontTable(self.0).height(font, code)
    }
    fn depth(&self, font: usize, code: u32) -> Scaled {
        FontTable(self.0).depth(font, code)
    }
    fn dimensions(&self, font: usize, code: u32) -> (Scaled, Scaled, Scaled) {
        FontTable(self.0).dimensions(font, code)
    }
    fn italic(&self, font: usize, code: u32) -> Scaled {
        FontTable(self.0).italic(font, code)
    }
    fn has_space(&self, font: usize) -> bool {
        FontTable(self.0).has_space(font)
    }
    fn char_exists(&self, font: usize, code: u32) -> bool {
        FontTable(self.0).char_exists(font, code)
    }
    fn char_tag(&self, font: usize, code: u32) -> u8 {
        FontTable(self.0).char_tag(font, code)
    }
    fn char_remainder(&self, font: usize, code: u32) -> u32 {
        FontTable(self.0).char_remainder(font, code)
    }
    fn exten(&self, font: usize, index: u32) -> Option<(u32, u32, u32, u32)> {
        FontTable(self.0).exten(font, index)
    }
    fn skew(&self, font: usize, code: u32) -> Scaled {
        FontTable(self.0).skew(font, code)
    }
    fn x_height(&self, font: usize) -> Scaled {
        FontTable(self.0).x_height(font)
    }
    fn kern(&self, font: usize, left: u32, right: u32) -> Option<Scaled> {
        FontTable(self.0).kern(font, left, right)
    }
    fn ligature(&self, font: usize, left: u32, right: u32) -> Option<(u32, u8)> {
        FontTable(self.0).ligature(font, left, right)
    }
    fn protrusion(&self, font: usize, code: u32, right: bool) -> Scaled {
        FontTable(self.0).protrusion(font, code, right)
    }
    fn expanded_protrusion(&self, font: usize, code: u32, right: bool, stretch: bool) -> Scaled {
        FontTable(self.0).expanded_protrusion(font, code, right, stretch)
    }
    fn expansion_capacity(&self, font: usize, code: u32) -> (Scaled, Scaled) {
        FontTable(self.0).expansion_capacity(font, code)
    }
    fn expansion_limits(&self, font: usize) -> Option<(i64, i64, i64)> {
        FontTable(self.0).expansion_limits(font)
    }
    fn expansion_delta(&self, font: usize, code: u32, ratio: i64) -> Scaled {
        FontTable(self.0).expansion_delta(font, code, ratio)
    }
    fn expansion_amount(&self, font: usize, code: u32, ratio: i64) -> i64 {
        FontTable(self.0).expansion_amount(font, code, ratio)
    }
    fn expansion_kern_capacity(
        &self,
        font: usize,
        left: u32,
        right: u32,
        base: Scaled,
    ) -> (Scaled, Scaled) {
        FontTable(self.0).expansion_kern_capacity(font, left, right, base)
    }
    fn expansion_kern_delta(
        &self,
        font: usize,
        left: u32,
        right: u32,
        base: Scaled,
        ratio: i64,
    ) -> Scaled {
        FontTable(self.0).expansion_kern_delta(font, left, right, base, ratio)
    }
}

impl CharMetrics for FontTable<'_> {
    #[inline(always)]
    fn width(&self, font: usize, code: u32) -> Scaled {
        self.0.get(font).map_or(Scaled::ZERO, |f| f.width(code))
    }
    fn height(&self, font: usize, code: u32) -> Scaled {
        self.0.get(font).map_or(Scaled::ZERO, |f| f.height(code))
    }
    fn depth(&self, font: usize, code: u32) -> Scaled {
        self.0.get(font).map_or(Scaled::ZERO, |f| f.depth(code))
    }
    #[inline(always)]
    fn dimensions(&self, font: usize, code: u32) -> (Scaled, Scaled, Scaled) {
        self.0
            .get(font)
            .map_or((Scaled::ZERO, Scaled::ZERO, Scaled::ZERO), |f| {
                f.dimensions(code)
            })
    }
    fn italic(&self, font: usize, code: u32) -> Scaled {
        self.0.get(font).map_or(Scaled::ZERO, |f| f.italic(code))
    }
    fn has_space(&self, font: usize) -> bool {
        self.0.get(font).is_some_and(|f| f.space().0 != 0)
    }
    fn char_exists(&self, font: usize, code: u32) -> bool {
        self.0.get(font).is_some_and(|f| f.has_char(code))
    }
    fn char_tag(&self, font: usize, code: u32) -> u8 {
        self.0.get(font).map_or(0, |f| f.char_tag(code))
    }
    fn char_remainder(&self, font: usize, code: u32) -> u32 {
        self.0.get(font).map_or(0, |f| f.char_remainder(code))
    }
    fn exten(&self, font: usize, index: u32) -> Option<(u32, u32, u32, u32)> {
        self.0.get(font).and_then(|f| f.exten(index))
    }
    fn kern(&self, font: usize, left: u32, right: u32) -> Option<Scaled> {
        match self.0.get(font).and_then(|f| f.lig_kern(left, right)) {
            Some(crate::tfm::LigKern::Kern(k)) => Some(k),
            _ => None,
        }
    }
    fn ligature(&self, font: usize, left: u32, right: u32) -> Option<(u32, u8)> {
        match self.0.get(font).and_then(|f| f.lig_kern(left, right)) {
            Some(crate::tfm::LigKern::Ligature { code, op }) => Some((code, op)),
            _ => None,
        }
    }
    fn protrusion(&self, font: usize, code: u32, right: bool) -> Scaled {
        self.0
            .get(font)
            .map_or(Scaled::ZERO, |f| f.protrusion(code, right))
    }
    fn expanded_protrusion(&self, font: usize, code: u32, right: bool, stretch: bool) -> Scaled {
        let owner = self
            .0
            .get(font)
            .and_then(|f| f.expanded_protrusion_owner(stretch))
            .unwrap_or(font);
        self.0
            .get(owner)
            .map_or(Scaled::ZERO, |f| f.protrusion(code, right))
    }
    fn expansion_capacity(&self, font: usize, code: u32) -> (Scaled, Scaled) {
        self.0
            .get(font)
            .map_or((Scaled::ZERO, Scaled::ZERO), |f| f.expansion_capacity(code))
    }
    fn expansion_limits(&self, font: usize) -> Option<(i64, i64, i64)> {
        self.0.get(font).and_then(TfmFont::expansion_limits)
    }
    fn expansion_delta(&self, font: usize, code: u32, ratio: i64) -> Scaled {
        self.0
            .get(font)
            .map_or(Scaled::ZERO, |f| f.expansion_delta(code, ratio))
    }
    fn expansion_amount(&self, font: usize, code: u32, ratio: i64) -> i64 {
        self.0
            .get(font)
            .map_or(0, |f| f.expansion_amount(code, ratio))
    }
    fn expansion_kern_capacity(
        &self,
        font: usize,
        left: u32,
        right: u32,
        base: Scaled,
    ) -> (Scaled, Scaled) {
        self.0.get(font).map_or((Scaled::ZERO, Scaled::ZERO), |f| {
            f.expansion_kern_capacity(left, right, base)
        })
    }
    fn expansion_kern_delta(
        &self,
        font: usize,
        left: u32,
        right: u32,
        base: Scaled,
        ratio: i64,
    ) -> Scaled {
        self.0.get(font).map_or(Scaled::ZERO, |f| {
            f.expansion_kern_delta(left, right, base, ratio)
        })
    }
    fn skew(&self, font: usize, code: u32) -> Scaled {
        let Some(f) = self.0.get(font) else {
            return Scaled::ZERO;
        };
        let Some(sk) = f.skew_char() else {
            return Scaled::ZERO;
        };
        match f.lig_kern(code, sk) {
            Some(crate::tfm::LigKern::Kern(k)) => k,
            _ => Scaled::ZERO,
        }
    }
    fn x_height(&self, font: usize) -> Scaled {
        self.0.get(font).map_or(Scaled::ZERO, |f| f.param(5))
    }
}

/// `tex.web` §1146's horizontal extent of a packed paragraph line.
///
/// Only glue that is actually moving under the box's glue set makes the
/// position unknowable. Finite interword glue on a last line whose slack was
/// taken by `\parfillskip` remains at its natural width.
fn packed_line_extent(b: &TeXBox, metrics: &impl CharMetrics) -> Scaled {
    let mut v = b.shift;
    let mut w = None;
    const UNKNOWN: Scaled = Scaled::MAX;
    for n in &b.list {
        match n {
            Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                let moving = match b.glue_sign {
                    crate::node::GlueSign::Stretching => {
                        b.glue_order == g.stretch_order && g.stretch != Scaled::ZERO
                    }
                    crate::node::GlueSign::Shrinking => {
                        b.glue_order == g.shrink_order && g.shrink != Scaled::ZERO
                    }
                    crate::node::GlueSign::Normal => false,
                };
                if moving {
                    v = UNKNOWN;
                } else if v != UNKNOWN {
                    v = v + g.width;
                }
                // §1146 treats leaders as visible material: after applying
                // their glue movement it assigns `w := v`, just as it does
                // after a character, rule, or box. A stretching `\hrulefill`
                // therefore makes the preceding line's reach `max_dimen` and
                // forces the following display to use the long skips. Treating
                // leaders as anonymous glue left `w` unset and selected the
                // short skips instead.
                if matches!(n, Node::Leaders { .. }) {
                    w = Some(v);
                }
            }
            Node::Char { .. } | Node::NativeRun(_) | Node::Box(_) | Node::Rule { .. } => {
                if v == UNKNOWN {
                    return UNKNOWN;
                }
                v = v + crate::node::measure_horizontal_basic(core::slice::from_ref(n), metrics)
                    .natural;
                w = Some(v);
            }
            Node::Kern(k, _) => {
                if v != UNKNOWN {
                    v = v + *k;
                }
            }
            _ => {}
        }
    }
    w.unwrap_or(Scaled(-crate::scaled::MAX_DIMEN))
}

/// The parameters a paragraph is broken with, read out of the engine's
/// registers by the caller. Passed rather than reached for, so this stays a
/// function of its inputs (the same reason `BreakParams` itself is).
#[derive(Debug, Clone)]
pub struct ParParams {
    pub hsize: Scaled,
    /// pdfTeX's `\pdfadjustspacing`: positive applies expansion while packing;
    /// values greater than one also use it while choosing line breaks.
    pub adjust_spacing: i32,
    /// pdfTeX's `\pdfprotrudechars` — see `BreakParams::protrude_chars`.
    pub protrude_chars: i32,
    /// `\hangindent`, `\hangafter` and `\parshape` — the paragraph shape.
    /// plain's `\item` and `\hang` set the first two, so every list depends on
    /// them, and TeX clears all three when the paragraph ends (`tex.web` §1070).
    pub hang_indent: Scaled,
    pub hang_after: i32,
    /// `\looseness` — §875: the paragraph is wanted this many lines longer
    /// (or shorter) than the fewest-demerits break would make it.
    pub looseness: i32,
    pub par_shape: alloc::vec::Vec<(Scaled, Scaled)>,
    /// `\leftskip`/`\rightskip` — glue on every line of the paragraph.
    pub left_skip: Glue,
    pub right_skip: Glue,
    pub parindent: Scaled,
    pub pretolerance: i32,
    pub tolerance: i32,
    pub line_penalty: i32,
    pub adj_demerits: i32,
    /// `tex.web` §890's penalties *between* the lines of a finished paragraph.
    /// They are what stops a page break from leaving a first line stranded at the
    /// foot of a page (`\clubpenalty`) or a last line at the head of the next
    /// (`\widowpenalty`), and LaTeX sets both to 150. Without them the page
    /// builder took every break at face value and filled each page one line
    /// fuller than TeX does.
    pub interline_penalty: i32,
    pub club_penalty: i32,
    pub widow_penalty: i32,
    /// e-TeX penalty arrays. Empty selects the singular value above; otherwise
    /// the last entry repeats for indices beyond the declared length.
    pub interline_penalties: alloc::vec::Vec<i32>,
    pub club_penalties: alloc::vec::Vec<i32>,
    pub widow_penalties: alloc::vec::Vec<i32>,
    pub broken_penalty: i32,
    pub baselineskip: Scaled,
    /// The *stretch and shrink* of `\baselineskip` and `\lineskip`. §679 builds
    /// the interline glue with `new_skip_param`, which copies the whole glue
    /// spec and then overrides only its width — so a `\baselineskip` of
    /// `10pt plus 41pt` puts 41pt of stretch between every pair of lines.
    /// Building rigid glue from the width alone loses it, and with it every
    /// page's stretch: trip.tex sets exactly that and our page totals came out
    /// `plus 1.0fil` where TeX's are `plus 41.0 plus 1.0fil`.
    pub baselineskip_glue: crate::node::Glue,
    pub lineskip_glue: crate::node::Glue,
    /// `\lineskip` — used instead of the computed gap when the boxes would come
    /// closer together than `\lineskiplimit`. Without it a tall box on the next
    /// line silently overlaps the one above.
    pub lineskip: Scaled,
    pub lineskiplimit: Scaled,
    /// `\parskip` as *glue*, not a width. Its stretch is what a page is filled
    /// out with — LaTeX's is `0pt plus 1pt`, so a page of prose has one unit of
    /// give per paragraph and nothing else. Keeping only the width left every
    /// page rigid, and `\vbox to \vsize` then had nothing to stretch.
    pub parskip: Glue,
    /// `\vsize` and `\topskip` — what the page builder aims for, and the glue it
    /// puts above the first box on every page.
    pub vsize: Scaled,
    pub topskip: Glue,
    pub maxdepth: Scaled,
    /// `\parfillskip`, read rather than assumed. plain.tex sets `0pt plus 1fil`,
    /// which is what sets a paragraph's last line at its natural width; INITEX
    /// leaves it at 0pt, and then TeX justifies the last line too. Hardcoding
    /// the `fil` made every paragraph break differently from pdfTeX under
    /// INITEX — the whole break pattern shifts, not just the last line.
    pub parfillskip: Glue,
    pub hyphen_penalty: i32,
    /// §869's penalty for a discretionary with an empty pre-break list.
    pub ex_hyphen_penalty: i32,
    /// `\newlinechar`, for §59's rule in the trace's short display.
    pub newline_char: i64,
    /// §1034's `cur_lang` at the paragraph's start, which §1091 freezes into
    /// `prev_graf` — the patterns a paragraph is hyphenated by.
    pub language: u8,
    /// §859's charge for two hyphenated lines in a row.
    pub double_hyphen_demerits: i32,
    pub final_hyphen_demerits: i32,
    pub left_hyphen_min: usize,
    pub right_hyphen_min: usize,
    /// `\hfuzz` and `\overfullrule` — how much a box may overflow before TeX
    /// calls it overfull, and how wide a rule it leaves in the margin when it
    /// does (`tex.web` §663).
    pub hfuzz: Scaled,
    /// `\hbadness` — how bad a line may be before §660 reports it. Carried
    /// beside `hfuzz` because the two are judged together and the judging
    /// happens where the lines are packed, far from the parameter table.
    pub hbadness: i32,
    pub overfull_rule: Scaled,
    /// `\tracingparagraphs` (`tex.web` §846): narrate the line-break search.
    pub tracing_paragraphs: i32,
    /// `\tracinglostchars` controls §581's warning when paragraph
    /// hyphenation asks `new_character` for a hyphen the word's font does not
    /// contain. The miss happens inside the second line-breaking pass, so the
    /// parameter has to travel with the rest of that pass's state.
    pub tracing_lost_chars: bool,
    /// §896: `\uchyph`. The `\lccode` table is borrowed by `end_paragraph`
    /// only if the first line-breaking pass fails.
    /// A word beginning with a capital — one whose `\lccode` is not itself — is
    /// hyphenated only when `\uchyph` is positive, and the patterns are matched
    /// against the `\lccode` image rather than Unicode's own lowercase.
    pub uchyph: bool,
    /// `\emergencystretch` (§873). Not applied — that would be a third pass
    /// with `background[2]` widened — but its *sign* decides whether there is a
    /// third pass at all, and so whether `@emergencypass` is ever printed.
    pub emergency_stretch: Scaled,
    /// Font identifiers for §174's short display, filled only when tracing.
    pub font_ids: alloc::vec::Vec<alloc::string::String>,
    /// Printed `\escapechar` for paragraph-trace control-sequence names.
    pub escape: alloc::string::String,
}

impl Default for ParParams {
    fn default() -> Self {
        Self {
            protrude_chars: 0,
            adjust_spacing: 0,
            hsize: Scaled::from_points(469),
            hang_indent: Scaled::ZERO,
            hang_after: 1,
            looseness: 0,
            par_shape: alloc::vec::Vec::new(),
            left_skip: Glue::fixed(Scaled::ZERO),
            right_skip: Glue::fixed(Scaled::ZERO),
            parindent: Scaled::ZERO,
            pretolerance: 100,
            tolerance: 200,
            line_penalty: 10,
            adj_demerits: 10_000,
            interline_penalty: 0,
            club_penalty: 0,
            widow_penalty: 0,
            interline_penalties: alloc::vec::Vec::new(),
            club_penalties: alloc::vec::Vec::new(),
            widow_penalties: alloc::vec::Vec::new(),
            broken_penalty: 0,
            baselineskip: Scaled::from_points(12),
            baselineskip_glue: crate::node::Glue::fixed(Scaled::from_points(12)),
            lineskip_glue: crate::node::Glue::fixed(Scaled::ZERO),
            lineskip: Scaled::ZERO,
            lineskiplimit: Scaled::ZERO,
            parskip: Glue::fixed(Scaled::ZERO),
            vsize: Scaled::from_points(648),
            topskip: Glue::default(),
            maxdepth: Scaled::ZERO,
            parfillskip: Glue::default(),
            hyphen_penalty: 50,
            ex_hyphen_penalty: 0,
            newline_char: -1,
            language: 0,
            double_hyphen_demerits: 0,
            final_hyphen_demerits: 0,
            left_hyphen_min: 2,
            right_hyphen_min: 3,
            hfuzz: Scaled::ZERO,
            hbadness: 0,
            overfull_rule: Scaled::ZERO,
            tracing_paragraphs: 0,
            tracing_lost_chars: false,
            uchyph: true,
            emergency_stretch: Scaled::ZERO,
            font_ids: alloc::vec::Vec::new(),
            escape: alloc::string::String::from("\\"),
        }
    }
}

/// A box whose group is still open: `\hbox to 100pt{` seen, `}` not yet.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenBox {
    pub kind: BoxKind,
    /// The input line this box was opened on — `tex.web`'s `mode_line`, which
    /// §218 prints in `### restricted horizontal mode entered at line 433`.
    pub entered_at: i64,
    /// `\vtop` rather than `\vbox`: `tex.web` §1087 hands the height of the
    /// *first* box or rule to the box's height and puts everything else into
    /// its depth, so a `\vtop` aligns on its first baseline. LaTeX's
    /// `\marginpar` is a `\vtop`, and a note set as a `\vbox` sits a whole
    /// line too high.
    pub vtop: bool,
    /// `to <dimen>` — pack to exactly this width.
    pub to: Option<Scaled>,
    /// `spread <dimen>` — pack to natural width plus this. Resolved at close,
    /// because the natural width is not known until the list is complete.
    pub spread: Option<Scaled>,
    pub list: Vec<Node>,
    /// A paragraph in progress *inside* this box, if it is vertical. Every
    /// vertical list can hold paragraphs, not just the page: `\vbox{text}` and
    /// the body of an `\insert` (which is what a footnote is) both set their
    /// text into lines and contribute those lines to the box.
    pub hlist: Vec<Node>,
    pub in_paragraph: bool,
    /// This open list is §768's span level for the current alignment entry.
    /// The alignment and row levels around it have no separate node owner in
    /// this implementation, but §218 still has to display all three.
    #[serde(default)]
    pub alignment_cell: bool,
    /// The mode to go back to when this box closes.
    pub saved_mode: Mode,
    /// §216's `mode_line` for the level this box interrupted.
    ///
    /// `tex.web` keeps `mode_line` in the nest entry, so `push_nest` and
    /// `pop_nest` save and restore it: a paragraph begun inside an alignment
    /// cell or a box has a `mode_line` of its own, and the paragraph outside
    /// gets its back when the level pops. Ours was one field for the whole
    /// typesetter, so the innermost paragraph's line stayed — and §663's
    /// warning named it as the outer paragraph's start. trip.tex's `\valign`
    /// at line 171 is exactly that: TeX reports `at lines 171--187` and we
    /// reported `185--187`, 185 being a line inside the alignment.
    pub(crate) saved_par_line: i64,
    /// The enclosing list's `\spacefactor`. `tex.web` §1083's `push_nest` saves
    /// it and §1084's `pop_nest` puts it back: a box — or an `\insert`, which is
    /// what a footnote is — must not leak its own space factor to the paragraph
    /// it interrupted. A footnote ending in `.` left the reference paragraph
    /// with `\spacefactor` 3000.
    pub(crate) saved_sf: i64,
    /// The enclosing horizontal list's §1034 `clang`; a restricted list gets
    /// its own value and cannot leak an explicit `\setlanguage` outward.
    pub(crate) saved_language: u8,
    /// The enclosing paragraph's §1091 initial language state. A nested box
    /// may start and finish paragraphs of its own, but popping that nest must
    /// restore the values frozen for the paragraph outside it.
    pub(crate) saved_paragraph_language: u8,
    pub(crate) saved_paragraph_left_min: usize,
    pub(crate) saved_paragraph_right_min: usize,
    /// `prev_depth` for *this* box's vertical list. Every vertical list has its
    /// own: `tex.web` §679's interline glue is not a property of the page.
    pub prev_depth: Scaled,
    /// `tex.web` §212's `pg_field`, for a *vertical* box. `push_nest` sets it
    /// to zero and `pop_nest` throws it away, so a paragraph broken inside a
    /// `\vbox` leaves the enclosing list's `\prevgraf` alone.
    pub prev_graf: i64,
    /// `\raise`/`\lower`/`\moveleft`/`\moveright` — how far this box is shifted
    /// when it lands. Captured when the box opens, like `dest`, because a box
    /// nested inside it closes first and must not take it.
    pub shift: Scaled,
    /// Where this box goes when it closes, captured when it *opened*.
    ///
    /// It has to be captured here, not read when a box finishes: in
    /// `\setbox5=\vbox{\hbox{A}\hbox{B}}` the inner `\hbox{A}` finishes first,
    /// and a pending destination read at close time would land in box 5 while
    /// the `\vbox` went nowhere — or, for `\shipout\vbox{\box255…}`, would send
    /// `\box255` to the DVI and leave the `\vbox` on the page.
    pub dest: Option<BoxDest>,
}

/// Where a finished box goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BoxDest {
    /// `\setbox<n>=`. `global` is captured when `\setbox` is *read*, not when
    /// the box closes: `tex.web` §1077 keeps the box context on the save stack,
    /// so an assignment inside the box cannot steal the `\global` prefix.
    Register(u16, bool),
    /// `\shipout`
    Ship,
    /// `\leaders`/`\cleaders`/`\xleaders`: the box is not appended anywhere —
    /// it waits for the glue that will repeat it (`tex.web` §1078). Bound when
    /// the box *opens*, as TeX's `scan_box` does, so a box built *inside* the
    /// leader box is an ordinary box.
    ///
    /// Which of the three commands it was rides here for the same reason. A
    /// leader box may itself contain leaders — trip.tex's does, three of them —
    /// and a single pending slot let the inner commands overwrite the outer's
    /// flavour long before the outer glue arrived, so a `\cleaders` came out as
    /// `\leaders` and its copies were laid out on the wrong grid.
    Leader(u8),
}

/// `tex.web` §100: TeX's own halving, which rounds an odd value away from zero
/// rather than truncating. One scaled point of difference in a display's
/// centring is visible in a diff of every glyph after it.
fn half(x: i32) -> i32 {
    if x % 2 != 0 { (x + 1) / 2 } else { x / 2 }
}

/// What a display formula's placement depends on — `tex.web` §1199–§1205.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayParams {
    pub display_width: Scaled,
    pub display_indent: Scaled,
    pub above: Glue,
    pub below: Glue,
    pub above_short: Glue,
    pub below_short: Glue,
    pub pre_penalty: i32,
    pub post_penalty: i32,
    /// `\predisplaysize` — how far the line before the display reached. §1203
    /// picks the short skips only when the display starts beyond it.
    pub pre_display_size: Scaled,
}

/// The numbers a formula's spacing depends on, read out of the registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct MathParams {
    /// Raw math-glue values, still measured in `mu`. Their point values depend
    /// on the style in which each spacing node occurs.
    pub thin: Glue,
    pub med: Glue,
    pub thick: Glue,
    /// `\fontdimen6` of family 2 at each of the three sizes — the quad §703
    /// divides by 18 to get one `mu`:
    ///
    /// ```text
    /// if cur_style<script_style then cur_size:=text_size
    /// else cur_size:=script_size*((cur_style-text_style) div 2);
    /// cur_mu:=x_over_n(math_quad(cur_size),18);
    /// ```
    ///
    /// Per *size*, because that is the whole of the rule: a `\mkern` inside a
    /// superscript measures against the script font's quad.
    pub quad: [Scaled; 3],
    pub axis: [Scaled; 3],
    pub sup: [[Scaled; 3]; 3],
    pub sub: [[Scaled; 2]; 3],
    pub sup_drop: [Scaled; 3],
    pub sub_drop: [Scaled; 3],
    pub x_height: [Scaled; 3],
    pub script_space: Scaled,
    /// `\fontdimen8` of family 3 at each size — the thickness of every rule a
    /// formula draws.
    pub default_rule_thickness: [Scaled; 3],
    /// `\fontdimen` 8/9/10 and 11/12 of family 2 — where a fraction's
    /// numerator and denominator sit.
    pub num: [[Scaled; 3]; 3],
    pub denom: [[Scaled; 2]; 3],
    pub null_delimiter_space: Scaled,
    pub delim: [[Scaled; 2]; 3],
    pub delimiter_factor: i32,
    pub delimiter_shortfall: Scaled,
    pub big_op_spacing: [[Scaled; 5]; 3],
    pub bin_op_penalty: i32,
    pub rel_penalty: i32,
    /// `\mathsurround` — the width of the math nodes either side of an inline
    /// formula.
    pub math_surround: Scaled,
}

#[derive(Clone)]
struct LigCacheEntry {
    hash: u64,
    font: usize,
    left_boundary: bool,
    right_boundary: bool,
    no_ligatures: bool,
    chars: Vec<u32>,
    nodes: Vec<Node>,
}

const LIG_CACHE_SLOTS: usize = 512;

/// The typesetting state: fonts, the current mode, and the lists being built.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Typeset {
    /// Indexed by font id. Slot 0 is `\nullfont`, which is why ids from `\font`
    /// start at 1 — see `Fast::do_font`.
    pub fonts: Vec<TfmFont>,
    /// Parallel to `fonts`: the TFM name, which the DVI postamble needs.
    pub font_names: Vec<String>,
    pub mode: Mode,
    /// The paragraph being built.
    pub hlist: Vec<Node>,
    /// The main vertical list: finished lines and whatever else was contributed.
    pub vlist: Vec<Node>,
    /// Finished pages.
    pub pages: Vec<TeXBox>,
    /// Pages handed straight to the host and not kept (see `Fast::set_page_sink`).
    pub pages_shipped: usize,
    /// The line breaker's tables, kept between paragraphs. They are working
    /// memory rather than TeX state; serializing the last paragraph's retained
    /// capacities into a preloaded format only bloats and slows every load.
    #[serde(skip)]
    pub break_scratch: crate::linebreak::BreakScratch,
    /// §660's verdict on each line of the paragraph just broken, for the lines
    /// that earned one, with a copy of the line so it can be shown. Only warned
    /// lines are cloned; an ordinary paragraph adds nothing here. Drained by
    /// the caller, which is the only place with somewhere to report to.
    /// Never dumped: it is live for the few statements between packing a
    /// paragraph's lines and reporting them, and a format has no paragraph in
    /// progress.
    #[serde(skip)]
    pub line_warnings: alloc::vec::Vec<(crate::node::PackMeasure, TeXBox)>,
    /// Boxes whose groups are still open, innermost last.
    pub open: Vec<OpenBox>,
    /// Set when a `\halign` finished inside the current display and put its
    /// rows on the vertical list itself; the closing `$$` then contributes no
    /// box of its own. See [`Typeset::display_body_done`].
    display_body_done: bool,
    /// `\box`/`\setbox` registers.
    pub boxes: alloc::collections::BTreeMap<u16, TeXBox>,
    /// `\spacefactor` — 1000 everywhere a paragraph or box starts.
    pub space_factor: i64,
    /// §1034's `clang`, the language in force at the current point of the
    /// horizontal list. Language whatsits update it; opening a box saves it.
    pub language: u8,
    /// §1091 freezes the paragraph's initial language and hyphen minima in the
    /// horizontal nest entry. Later parameter changes are represented by
    /// language whatsits and must not rewrite these starting values.
    pub paragraph_language: u8,
    pub paragraph_left_min: usize,
    pub paragraph_right_min: usize,
    /// The raw same-font input behind the character run currently being
    /// shaped when the run ends; this gives the TFM cursor its TeX semantics
    /// without making the input layer look ahead.
    lig_run_font: Option<usize>,
    lig_run_start: usize,
    lig_run_chars: Vec<u32>,
    /// Reusable output buffer for run shaping. It is transient working memory,
    /// not format state.
    #[serde(skip)]
    lig_run_nodes: Vec<Node>,
    /// Direct-mapped cache of completed TFM word runs. Admission happens only
    /// on a run's second sighting, so unique words pay no node clone. This is
    /// transient acceleration, never format state.
    #[serde(skip)]
    lig_cache: Vec<Option<LigCacheEntry>>,
    #[serde(skip)]
    lig_cache_seen: Vec<u64>,
    lig_run_left_boundary: bool,
    cancel_left_boundary: bool,
    /// §1035's `ins_disc`: a discretionary is owed after the ligature run now
    /// being built, because the last raw character consumed into it was the
    /// font's `\hyphenchar`.
    ///
    /// It cannot be appended when that character arrives. §1035 decides
    /// `ins_disc` inside `wrapup`, which runs when the run *ends*, and appends
    /// the node only after `pack_lig` has collapsed the run:
    ///
    /// ```text
    /// @d wrapup(#)==if cur_l<non_char then
    ///   begin if link(cur_q)>null then
    ///     if character(tail)=qi(hyphen_char[main_f]) then ins_disc:=true;
    ///   if ligature_present then pack_lig(#);
    ///   if ins_disc then
    ///     begin ins_disc:=false;
    ///     if mode>0 then tail_append(new_disc);
    ///     end;
    ///   end
    /// ```
    ///
    /// Appending it eagerly put a `Disc` between the hyphens of `---`, and
    /// `append_char` only ligatures against a `Char` that is *last* in the
    /// list — so the em-dash and en-dash ligatures could never form in a
    /// paragraph. They formed in an `\hbox`, which made it look like the
    /// ligature machinery was at fault; in fact §1035's `mode>0` means a
    /// restricted horizontal list gets no discretionary at all, so there was
    /// nothing there to break the run.
    pub pending_disc: bool,
    /// `\spaceskip` and `\xspaceskip` when they are not zero glue. `None` means
    /// the font's own space is used, which is the usual case.
    pub space_skip: Option<Glue>,
    pub x_space_skip: Option<Glue>,
    /// pdfTeX's three microtype list-building switches. The per-character
    /// values live with their font; these decide whether to apply them.
    #[serde(default)]
    micro_adjust_interword: bool,
    #[serde(default)]
    micro_prepend_kern: bool,
    #[serde(default)]
    micro_append_kern: bool,
    /// `math_fonts[size][family]` — the engine font index for each math family
    /// at text, script and scriptscript size. Set by `\textfont` and friends.
    pub math_fonts: [[Option<usize>; 16]; 3],
    /// §722's pending complaints, drained by the engine after each formula.
    math_font_issues: alloc::vec::Vec<crate::math::MathFontIssue>,
    /// The formula being built, innermost last: `{...}` inside math and each
    /// script is its own list.
    pub mlist: Vec<Vec<crate::math::Noad>>,
    /// `tex.web` §1181's `incompleat_noad`, one per open math list: what
    /// `\over` set aside as the numerator, and the thickness it asked for.
    /// Everything after it becomes the denominator when the list closes.
    /// Read by §219 too: `\showlists` names the fraction whose denominator the
    /// list being built is.
    pub incompleat: Vec<Option<(Vec<crate::math::Noad>, crate::math::Kind)>>,
    /// §216's `mode_line`, one per open math nest level. A display and each
    /// sub-formula can begin on different input lines, and §218 reports every
    /// one of them independently when `\showlists` walks the nest.
    #[serde(default)]
    pub math_entered_at: Vec<i64>,
    /// The sign of each math nest level's mode: only the outer level of a
    /// display is positive `mmode`; inline formulas and every sub-formula are
    /// negative. Style changes in an inner level do not change this sign.
    #[serde(default)]
    pub math_inner: Vec<bool>,
    pub math_style: crate::math::Style,
    /// The mode to return to when the formula closes — a *stack*, because a
    /// formula can be nested inside another: an `$…$` in an alignment template
    /// inside a `\vbox` inside a display. One slot meant the inner formula's
    /// close restored the outer one's return mode, and the display stayed in
    /// the paragraph.
    saved_mode: Vec<Mode>,
    /// The *style* to return to. A formula nested inside another — an `$…$` in
    /// an alignment template inside a `\vbox` inside a display — must put the
    /// outer style back, or the outer `$$` no longer knows it is a display and
    /// the display never ends.
    saved_style: Vec<crate::math::Style>,
    /// Whether each open formula is set directly into a paragraph, which is
    /// what decides `mlist_penalties`.
    math_in_paragraph: Vec<bool>,
    /// Hyphenation patterns, from `\patterns{…}`.
    pub hyphenator: crate::hyphenate::Hyphenator,
    /// The page under construction. Driven in its simplest mode for now: with no
    /// `\output` routine TeX ships `\box255` itself (`tex.web` §1023), and that
    /// is what happens here. The builder's reentrancy — `\output` running
    /// mid-build and handing material back with `\unvbox255` — is modelled in
    /// `page.rs` and waits only for `\output` to be runnable from here.
    pub page: PageBuilder,
    /// e-TeX's `split_disc`: discardable nodes pruned from the remainder of
    /// the most recent `\vsplit`, when `\savingvdiscards` was positive.
    pub split_discards: Vec<Node>,
    /// `tex.web`'s `prev_depth`: the depth of the last box contributed, which is
    /// what the interline glue is measured from. A separate variable rather than
    /// a look back at the vertical list, because the page builder drains that
    /// list as it goes.
    prev_depth: Scaled,
    /// `\prevgraf` of the *main* vertical list — the bottom of §212's nest,
    /// which no `pop_nest` ever reaches.
    prev_graf: i64,
    /// §1025's nest level for `\output`, which this engine does not open a box
    /// for. The mode is still `-vmode` while the routine runs, and `\ifinner`
    /// and §211's `print_mode` both ask.
    pub output_active: bool,
    /// Set once anything has been contributed, so a leading `\parskip` and the
    /// baselineskip above the first line are suppressed the way TeX's
    /// `prev_depth = ignore_depth` does.
    started: bool,
    /// How far the last line of the last paragraph reached to the right of its
    /// reference point — `tex.web` §1146's `w`, which becomes `\predisplaysize`
    /// and decides whether a display gets the short skips or the long ones.
    /// `None` means there was no paragraph, i.e. TeX's `-max_dimen`.
    pub last_line_extent: Option<Scaled>,
    /// `(badness, penalty, running demerits)` for each line of the paragraph
    /// just broken.
    pub last_par_costs: Vec<(i32, i32, i64)>,
    /// `tex.web`'s `prev_graf`: how many lines the current paragraph has set.
    pub lines_so_far: usize,
    /// The input line to stamp on the *next* box opened, set by the engine
    /// before it opens one. `tex.web` keeps `line` globally and reads it in
    /// `push_nest`; we have no such global here.
    pub entered_at: i64,
    /// §1091's `mode_line` for the *paragraph's* nest level: `new_graf` calls
    /// `push_nest`, and §216 stamps the line the paragraph began on.
    ///
    /// It has to be its own field. §663's `in paragraph at lines N--M` reads
    /// it, and `entered_at` above is a staging slot for the next box — so an
    /// `\hbox` opened anywhere inside the paragraph overwrote it, and the
    /// warning named the box's line as the paragraph's start.
    pub par_line: i64,
}

/// Which of §1105's three the command is: `\unskip`, `\unkern`, `\unpenalty`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RemoveKind {
    Glue,
    Kern,
    Penalty,
}

/// Substitute pdfTeX's expanded-font advances before a finished paragraph
/// line is packed. The DVI keeps the base font and carries each width change as
/// a kern; consequently both the subsequent glue setting and the PDF glyph
/// positions see the same advances as the expanded TFM.
fn apply_font_expansion(
    list: &mut Vec<Node>,
    target: Scaled,
    adjust_spacing: i32,
    metrics: &impl CharMetrics,
) {
    if adjust_spacing <= 0 || list.is_empty() {
        return;
    }
    let totals = crate::node::measure_horizontal(list, metrics);
    let diff = target.0 as i64 - totals.natural.0 as i64;
    let mut kern_stretch = 0i64;
    let mut kern_shrink = 0i64;
    for (index, node) in list.iter().enumerate() {
        let Node::Kern(base, crate::node::kern_sub::NORMAL) = node else {
            continue;
        };
        if let (
            Some(Node::Char {
                code: left,
                font: left_font,
                ..
            }),
            Some(Node::Char {
                code: right,
                font: right_font,
                ..
            }),
        ) = (
            index.checked_sub(1).and_then(|i| list.get(i)),
            list.get(index + 1),
        ) {
            if left_font == right_font {
                let (stretch, shrink) =
                    metrics.expansion_kern_capacity(*left_font, *left, *right, *base);
                kern_stretch += stretch.0 as i64;
                kern_shrink += shrink.0 as i64;
            }
        }
    }
    let (capacity, infinite) = if diff > 0 {
        (
            totals.font_stretch.0 as i64 + kern_stretch,
            totals.stretch[1..].iter().any(|v| *v != Scaled::ZERO),
        )
    } else if diff < 0 {
        (
            totals.font_shrink.0 as i64 + kern_shrink,
            totals.shrink[1..].iter().any(|v| *v != Scaled::ZERO),
        )
    } else {
        return;
    };
    // `hpack(..., cal_expand_ratio)` expands only when finite-order glue is
    // being set. `fil` and higher orders take the line's slack themselves.
    if capacity <= 0 || infinite {
        return;
    }
    let numerator = diff.saturating_mul(1000);
    let ratio = if numerator >= 0 {
        (numerator + capacity / 2) / capacity
    } else {
        -((-numerator + capacity / 2) / capacity)
    }
    .clamp(-1000, 1000);
    if ratio == 0 {
        return;
    }

    let original = core::mem::take(list);
    list.reserve(original.len().saturating_mul(2));
    let mut current_amount = 0;
    let mut previous_char = None::<(usize, u32)>;
    let mut nodes = original.into_iter().peekable();
    while let Some(mut node) = nodes.next() {
        let (delta, amount) = match &node {
            Node::Char { code, font, .. } => (
                metrics.expansion_delta(*font, *code, ratio),
                metrics.expansion_amount(*font, *code, ratio),
            ),
            _ => (Scaled::ZERO, current_amount),
        };
        if amount != current_amount {
            list.push(Node::Whatsit(alloc::format!("aitex:fontexpand {amount}")));
            current_amount = amount;
        }
        if let (
            Node::Kern(width, crate::node::kern_sub::NORMAL),
            Some((font, left)),
            Some(Node::Char {
                font: right_font,
                code: right,
                ..
            }),
        ) = (&mut node, previous_char, nodes.peek())
        {
            if font == *right_font {
                let base = *width;
                *width = base + metrics.expansion_kern_delta(font, left, *right, base, ratio);
            }
        }
        previous_char = match &node {
            Node::Char { font, code, .. } => Some((*font, *code)),
            Node::Kern(_, crate::node::kern_sub::NORMAL) => previous_char,
            _ => None,
        };
        list.push(node);
        if delta != Scaled::ZERO {
            list.push(Node::Kern(delta, crate::node::kern_sub::EXPANSION));
        }
    }
    if current_amount != 0 {
        list.push(Node::Whatsit(alloc::string::String::from(
            "aitex:fontexpand 0",
        )));
    }
}

impl Typeset {
    pub fn new() -> Self {
        // Slot 0 exists so `\nullfont` is a real font rather than an absent one:
        // a character set in it measures zero rather than panicking a lookup.
        Self {
            fonts: alloc::vec![TfmFont::empty()],
            font_names: alloc::vec![String::from("nullfont")],
            mode: Mode::Vertical,
            hlist: Vec::new(),
            vlist: Vec::new(),
            pages: Vec::new(),
            pages_shipped: 0,
            break_scratch: crate::linebreak::BreakScratch::default(),
            line_warnings: alloc::vec::Vec::new(),
            open: Vec::new(),
            display_body_done: false,
            boxes: alloc::collections::BTreeMap::new(),
            space_factor: 1000,
            language: 0,
            paragraph_language: 0,
            paragraph_left_min: 1,
            paragraph_right_min: 1,
            lig_run_font: None,
            lig_run_start: 0,
            lig_run_chars: Vec::new(),
            lig_run_nodes: Vec::new(),
            lig_cache: Vec::new(),
            lig_cache_seen: Vec::new(),
            lig_run_left_boundary: true,
            cancel_left_boundary: false,
            pending_disc: false,
            space_skip: None,
            x_space_skip: None,
            micro_adjust_interword: false,
            micro_prepend_kern: false,
            micro_append_kern: false,
            math_fonts: [[None; 16]; 3],
            math_font_issues: alloc::vec::Vec::new(),
            mlist: Vec::new(),
            incompleat: Vec::new(),
            math_entered_at: Vec::new(),
            math_inner: Vec::new(),
            math_style: crate::math::Style::Text,
            saved_mode: Vec::new(),
            saved_style: Vec::new(),
            math_in_paragraph: Vec::new(),
            hyphenator: crate::hyphenate::Hyphenator::new(),
            page: PageBuilder::new(Scaled::from_points(648)),
            split_discards: Vec::new(),
            prev_depth: IGNORE_DEPTH,
            prev_graf: 0,
            output_active: false,
            started: false,
            last_line_extent: None,
            last_par_costs: Vec::new(),
            lines_so_far: 0,
            entered_at: 0,
            par_line: 0,
        }
    }

    /// The list material is currently being appended to: the innermost open box,
    /// or the paragraph, or the main vertical list.
    /// `tex.web` §1145's `head = tail`: the paragraph a `$$` interrupts has
    /// nothing in it yet, so there is no last line to measure and
    /// `\predisplaysize` is `-max_dimen`.
    pub fn paragraph_so_far_is_empty(&mut self) -> bool {
        self.current_list().is_empty()
    }

    fn current_list(&mut self) -> &mut Vec<Node> {
        if let Some(b) = self.open.last_mut() {
            if b.in_paragraph {
                &mut b.hlist
            } else {
                &mut b.list
            }
        } else if self.mode == Mode::Horizontal {
            &mut self.hlist
        } else {
            &mut self.vlist
        }
    }

    fn current_list_read(&self) -> &[Node] {
        if let Some(b) = self.open.last() {
            if b.in_paragraph { &b.hlist } else { &b.list }
        } else if self.mode == Mode::Horizontal {
            &self.hlist
        } else {
            &self.vlist
        }
    }

    /// Update the list-building microtype switches. An assignment between two
    /// characters ends the current optimized character run in pdfTeX too, so
    /// finish it before changing how adjacent pairs are interpreted.
    pub fn set_microtype_flags(&mut self, adjust: bool, prepend: bool, append: bool) {
        if (adjust, prepend, append)
            == (
                self.micro_adjust_interword,
                self.micro_prepend_kern,
                self.micro_append_kern,
            )
        {
            return;
        }
        self.finish_char_run(true);
        self.flush_pending_disc();
        self.micro_adjust_interword = adjust;
        self.micro_prepend_kern = prepend;
        self.micro_append_kern = append;
    }

    fn adjust_microtype_space(&self, glue: &mut Glue) {
        if !self.micro_adjust_interword {
            return;
        }
        let list = self.current_list_read();
        let preceding = match list.last() {
            Some(Node::Char { code, font, .. }) => Some((*font, *code)),
            Some(Node::Kern(_, crate::node::kern_sub::AUTO)) => list
                .get(list.len().saturating_sub(2))
                .and_then(|n| match n {
                    Node::Char { code, font, .. } => Some((*font, *code)),
                    _ => None,
                }),
            _ => None,
        };
        let Some((font, code)) = preceding else {
            return;
        };
        let Some(tfm) = self.fonts.get(font) else {
            return;
        };
        let (width, stretch, shrink) = tfm.micro_spacing(code);
        glue.width = glue.width + width;
        glue.stretch = glue.stretch + stretch;
        glue.shrink = glue.shrink + shrink;
    }

    #[inline]
    fn lig_cache_hash(
        font: usize,
        chars: &[u32],
        left_boundary: bool,
        right_boundary: bool,
        no_ligatures: bool,
    ) -> u64 {
        let mut h = (0xcbf2_9ce4_8422_2325u64 ^ font as u64).wrapping_mul(0x0000_0100_0000_01b3);
        for &c in chars {
            h = (h ^ u64::from(c)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= u64::from(left_boundary)
            | (u64::from(right_boundary) << 1)
            | (u64::from(no_ligatures) << 2);
        // Zero is the empty marker in `lig_cache_seen`.
        if h == 0 { 1 } else { h }
    }

    /// Is horizontal material about to start a paragraph here? True in a
    /// vertical list — the page's or a vertical box's — with none in progress.
    pub fn needs_paragraph_here(&self) -> bool {
        // Never in math mode. §1090 starts a paragraph from *vertical* mode
        // only, and a formula is neither vertical nor a place a paragraph can
        // begin. The distinction is invisible on the page — where `self.mode`
        // is `Math` for the whole formula — and decisive inside a `\vbox`,
        // where this looked at the box instead: opening a display sets the
        // box's `in_paragraph` false (the paragraph so far has just been
        // broken), so the first character of the *formula* asked for a new
        // paragraph, `\everypar` fired inside the display, and the display's
        // own penalties, skips and box were appended to that paragraph's
        // horizontal list — which §1206 then clears. Every display inside a
        // vertical box vanished, contents and all.
        if self.mode == Mode::Math {
            return false;
        }
        match self.open.last() {
            Some(b) => b.kind == BoxKind::Vertical && !b.in_paragraph,
            None => self.mode == Mode::Vertical,
        }
    }

    /// Append a node wherever material is going.
    pub fn append(&mut self, n: Node) {
        let _p = crate::profile::Phase::enter(crate::profile::P_APPEND);
        self.finish_char_run(true);
        // Anything that is not another character ends the ligature run, which
        // is §1035's `wrapup`. The owed discretionary goes down first, so it
        // lands directly after the ligature and before whatever ends the run.
        self.flush_pending_disc();
        // `tex.web` §729: glue, a kern or a penalty inside a formula belongs to
        // the *math list*, not to whatever list encloses it. Putting a `\qquad`
        // in the enclosing horizontal list moved it out of the formula.
        if self.mode == Mode::Math {
            self.push_noad(crate::math::Noad {
                kind: crate::math::Kind::Raw,
                nucleus: crate::math::Field::List(alloc::vec![n]),
                ..Default::default()
            });
            return;
        }
        self.current_list().push(n);
    }

    fn rebuild_char_run(&mut self, right_boundary: bool, final_run: bool) {
        let Some(font) = self.lig_run_font else {
            return;
        };
        let tfm = match self.fonts.get(font) {
            Some(f) => f,
            None => return,
        };
        let first = self.lig_run_chars.first().copied();
        let last = self.lig_run_chars.last().copied();
        let micro_kerns = self.micro_append_kern || self.micro_prepend_kern;
        let effective_right_boundary = final_run && right_boundary;
        // Single glyphs are cheaper to shape than to look up.  From two glyphs
        // onward, second-sighting admission makes common prose words worth
        // retaining without allowing one-off document text to fill the cache.
        let cache_key = (!micro_kerns && (2..=64).contains(&self.lig_run_chars.len())).then(|| {
            let no_ligatures = tfm.ligatures_disabled();
            let hash = Self::lig_cache_hash(
                font,
                &self.lig_run_chars,
                self.lig_run_left_boundary,
                effective_right_boundary,
                no_ligatures,
            );
            (hash, no_ligatures, hash as usize & (LIG_CACHE_SLOTS - 1))
        });
        if let Some((hash, no_ligatures, slot)) = cache_key {
            if let Some(entry) = self.lig_cache.get(slot).and_then(Option::as_ref)
                && entry.hash == hash
                && entry.font == font
                && entry.left_boundary == self.lig_run_left_boundary
                && entry.right_boundary == effective_right_boundary
                && entry.no_ligatures == no_ligatures
                && entry.chars == self.lig_run_chars
            {
                let mut nodes = core::mem::take(&mut self.lig_run_nodes);
                nodes.clone_from(&entry.nodes);
                let start = self.lig_run_start;
                {
                    let list = self.current_list();
                    list.truncate(start.min(list.len()));
                    list.append(&mut nodes);
                }
                self.lig_run_nodes = nodes;
                return;
            }
        }
        let leading = if micro_kerns {
            tfm.micro_kern(None, first, self.micro_append_kern, self.micro_prepend_kern)
        } else {
            Scaled::ZERO
        };
        let trailing = if final_run && micro_kerns {
            tfm.micro_kern(last, None, self.micro_append_kern, self.micro_prepend_kern)
        } else {
            Scaled::ZERO
        };
        let mut nodes = core::mem::take(&mut self.lig_run_nodes);
        nodes.clear();
        nodes.reserve(self.lig_run_chars.len().saturating_mul(2));
        if leading != Scaled::ZERO {
            nodes.push(Node::Kern(leading, crate::node::kern_sub::AUTO));
        }

        // An automatic kern is considered before the TFM ligature/kern
        // command for the same pair. A nonzero one therefore ends the current
        // shaping segment; no ligature or ordinary font kern crosses it.
        let mut segment_start = 0usize;
        for i in 0..self.lig_run_chars.len() {
            let between = if micro_kerns && i + 1 < self.lig_run_chars.len() {
                tfm.micro_kern(
                    Some(self.lig_run_chars[i]),
                    Some(self.lig_run_chars[i + 1]),
                    self.micro_append_kern,
                    self.micro_prepend_kern,
                )
            } else {
                Scaled::ZERO
            };
            let segment_ends = between != Scaled::ZERO || i + 1 == self.lig_run_chars.len();
            if !segment_ends {
                continue;
            }
            let end = i + 1;
            let segment = &self.lig_run_chars[segment_start..end];
            let left_boundary =
                segment_start == 0 && self.lig_run_left_boundary && leading == Scaled::ZERO;
            let right_boundary = final_run
                && end == self.lig_run_chars.len()
                && right_boundary
                && trailing == Scaled::ZERO;
            if tfm.has_simple_ligature_program() {
                shape_simple_nodes(
                    tfm,
                    segment,
                    left_boundary,
                    right_boundary,
                    font,
                    &mut nodes,
                );
                if between != Scaled::ZERO {
                    nodes.push(Node::Kern(between, crate::node::kern_sub::AUTO));
                }
                segment_start = end;
                continue;
            }
            if !tfm.shape_text_needed(segment, left_boundary, right_boundary) {
                nodes.extend(segment.iter().copied().map(|code| Node::Char {
                    code,
                    font,
                    lig: None,
                }));
                if between != Scaled::ZERO {
                    nodes.push(Node::Kern(between, crate::node::kern_sub::AUTO));
                }
                segment_start = end;
                continue;
            }
            let shaped = tfm.shape_text(segment, left_boundary, right_boundary);
            for item in shaped {
                match item {
                    crate::tfm::ShapedItem::Glyph {
                        code,
                        source,
                        ligature,
                        left_hit,
                        right_hit,
                        ..
                    } => {
                        let lig = if ligature {
                            let mut original = Vec::with_capacity(
                                source.len() + usize::from(left_hit) + usize::from(right_hit),
                            );
                            if left_hit {
                                original.push(crate::node::LIG_BOUNDARY);
                            }
                            for local in source {
                                if let Some(&raw) =
                                    self.lig_run_chars.get(segment_start.saturating_add(local))
                                {
                                    original.push(raw);
                                }
                            }
                            if right_hit {
                                original.push(crate::node::LIG_BOUNDARY);
                            }
                            Some(original.into_boxed_slice())
                        } else {
                            None
                        };
                        nodes.push(Node::Char { code, font, lig });
                    }
                    crate::tfm::ShapedItem::Kern { width, .. } => {
                        nodes.push(Node::Kern(width, crate::node::kern_sub::NORMAL));
                    }
                }
            }
            if between != Scaled::ZERO {
                nodes.push(Node::Kern(between, crate::node::kern_sub::AUTO));
            }
            segment_start = end;
        }
        if trailing != Scaled::ZERO {
            nodes.push(Node::Kern(trailing, crate::node::kern_sub::AUTO));
        }
        // Admit only after the same key has missed once before. A unique word
        // writes one hash; the second occurrence pays the clone, and every
        // later occurrence copies the already-shaped nodes without consulting
        // the TFM program.
        let mut cache_entry = None;
        if let Some((hash, no_ligatures, slot)) = cache_key {
            if self.lig_cache_seen.len() != LIG_CACHE_SLOTS {
                self.lig_cache_seen.resize(LIG_CACHE_SLOTS, 0);
                self.lig_cache.resize_with(LIG_CACHE_SLOTS, || None);
            }
            if self.lig_cache_seen[slot] == hash {
                cache_entry = Some((
                    slot,
                    LigCacheEntry {
                        hash,
                        font,
                        left_boundary: self.lig_run_left_boundary,
                        right_boundary: effective_right_boundary,
                        no_ligatures,
                        chars: self.lig_run_chars.clone(),
                        nodes: nodes.clone(),
                    },
                ));
            } else {
                self.lig_cache_seen[slot] = hash;
            }
        }
        let start = self.lig_run_start;
        {
            let list = self.current_list();
            list.truncate(start.min(list.len()));
            list.append(&mut nodes);
        }
        self.lig_run_nodes = nodes;
        if let Some((slot, entry)) = cache_entry {
            self.lig_cache[slot] = Some(entry);
        }
    }

    /// Finish the raw character run, optionally letting the font's implicit
    /// right boundary participate. `\noboundary` is the sole false caller.
    pub fn finish_char_run(&mut self, right_boundary: bool) {
        if self.lig_run_font.is_none() {
            return;
        }
        self.rebuild_char_run(right_boundary, true);
        self.lig_run_font = None;
        self.lig_run_chars.clear();
    }

    pub fn cancel_next_left_boundary(&mut self) {
        self.cancel_left_boundary = true;
    }

    pub fn clear_cancelled_boundary(&mut self) {
        self.cancel_left_boundary = false;
    }

    /// Append §1377's zero-width language whatsit and make its language the
    /// current `clang` for subsequent characters.
    pub fn append_language(&mut self, language: u8, left: u8, right: u8) {
        self.append(Node::Language {
            language,
            left,
            right,
        });
        self.language = language;
    }

    /// Initialize §1091's language state for a newly entered or resumed
    /// unrestricted paragraph.
    pub fn start_paragraph_language(&mut self, language: u8, left: usize, right: usize) {
        self.language = language;
        self.paragraph_language = language;
        self.paragraph_left_min = left;
        self.paragraph_right_min = right;
    }

    /// §1035's `wrapup`, less the packing: put down the discretionary owed to
    /// the ligature run that has just ended.
    ///
    /// The `mode>0` test is re-checked here rather than trusted from when the
    /// character arrived, because the run can end by entering somewhere the
    /// node does not belong — `a---$x$` ends it at the `$`, and the list that
    /// is current by then is the formula's.
    pub fn flush_pending_disc(&mut self) {
        self.finish_char_run(true);
        if !core::mem::take(&mut self.pending_disc) {
            return;
        }
        if self.mode_test() != (Mode::Horizontal, false) {
            return;
        }
        // Pushed straight at the list: going through `append` would re-enter
        // this and §729 would divert it into a math list.
        self.current_list()
            .push(Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
                pre: Vec::new(),
                post: Vec::new(),
                replace: 0,
            })));
    }

    /// Append to the *enclosing list* even in math mode, bypassing §729's
    /// "glue in a formula belongs to the math list".
    ///
    /// The one caller is a `\halign` in display math, whose rows §774 puts on
    /// the enclosing vertical list rather than in the formula — so §1206's
    /// penalties and display skips belong there beside them, not in an mlist
    /// that the alignment has already bypassed.
    /// The current display's body has already been contributed, by a `\halign`
    /// that finished inside it (`tex.web` §812). The closing `$$` must not then
    /// contribute an empty box on top of it: an empty box is still a box, so it
    /// takes interline glue, and `align` came out with an extra
    /// `\baselineskip` of space under it.
    pub fn mark_display_body_done(&mut self) {
        self.display_body_done = true;
    }

    pub fn append_vertical(&mut self, n: Node) {
        self.current_list().push(n);
    }

    /// Where the next node appended will land. §812 places its penalties and
    /// skips as the rows go by — so the interline glue is computed against the
    /// right neighbours — and comes back for their *values* once the
    /// assignments after the alignment have been obeyed.
    /// §1105's `(mode=vmode)and(tail=head)`: the *main* vertical list has
    /// nothing on it, so `\unskip` and its fellows have nothing to remove and
    /// TeX refuses to go after what the page builder has already taken.
    pub fn main_vertical_is_empty(&mut self) -> bool {
        self.open.is_empty() && self.vertical_len() == 0
    }

    pub fn vertical_len(&mut self) -> usize {
        self.current_list().len()
    }

    /// Replace a node the caller put there itself, found by the index
    /// `vertical_len` gave it.
    pub fn patch_vertical(&mut self, at: usize, n: Node) {
        if let Some(slot) = self.current_list().get_mut(at) {
            *slot = n;
        }
    }

    /// `\prevdepth` of the list material is going to.
    pub fn prev_depth(&self) -> Scaled {
        match self.open.last() {
            Some(ob) => ob.prev_depth,
            None => self.prev_depth,
        }
    }

    pub fn set_prev_depth(&mut self, d: Scaled) {
        match self.open.last_mut() {
            Some(ob) => ob.prev_depth = d,
            None => self.prev_depth = d,
        }
    }

    /// `\prevgraf`, which lives on the innermost **vertical** level.
    ///
    /// §1244 reads and writes it the same way, and the walk is the whole point:
    ///
    /// ```text
    /// nest[nest_ptr]:=cur_list; p:=nest_ptr;
    /// while abs(nest[p].mode_field)<>vmode do decr(p);
    /// cur_val:=nest[p].pg_field;
    /// ```
    ///
    /// `pg_field` means something else in math mode (§212 calls it a "context
    /// code"), and horizontal mode has no paragraphs of its own to count, so
    /// `\prevgraf` inside an `\hbox` is the enclosing vertical list's — to read
    /// *and* to set.
    fn prev_graf_level(&self) -> Option<usize> {
        self.open
            .iter()
            .rposition(|b| b.kind == crate::node::BoxKind::Vertical)
    }

    pub fn prev_graf(&self) -> i64 {
        match self.prev_graf_level() {
            Some(i) => self.open[i].prev_graf,
            None => self.prev_graf,
        }
    }

    /// §218 shows every nest level, and the outermost is the main vertical
    /// list's own `pg_field` — not whatever `\vbox` happens to be open above it.
    pub fn prev_graf_main(&self) -> i64 {
        self.prev_graf
    }

    pub fn set_prev_graf(&mut self, v: i64) {
        match self.prev_graf_level() {
            Some(i) => self.open[i].prev_graf = v,
            None => self.prev_graf = v,
        }
    }

    /// What `\ifvmode`, `\ifhmode`, `\ifmmode` and `\ifinner` test — `tex.web`
    /// §487, which reads `abs(mode)` and `mode < 0`. The mode is not one
    /// variable here: a box on the stack sets the mode for as long as it is
    /// open, and a vertical box with a paragraph running in it is in horizontal
    /// mode. Everything inside a box is *internal*; only the page's own vertical
    /// list and the paragraph being built directly on it are not.
    pub fn mode_test(&self) -> (Mode, bool) {
        if self.mode == Mode::Math {
            // `tex.web` §1153: a `{` in math mode is a *subformula*, and §1136's
            // `push_math` gives it `mode := -mmode` — inner — whatever style it
            // will eventually be set in:
            //
            //     procedure push_math(@!c:group_code);
            //     begin push_nest; mode:=-mmode; incompleat_noad:=null;
            //     new_save_level(c);
            //     end;
            //
            // So `$$B{C}D$$` reports `{math mode: the letter C}` and then
            // `{display math mode: the letter D}`. Only the outermost level of
            // a *display* is `mmode` positive; `$…$` and every braced group are
            // not. The mode sign is therefore stored per nest level; style
            // remains a typesetting choice, not a mode identity.
            let inner =
                self.math_inner.last().copied().unwrap_or(
                    self.math_style != crate::math::Style::Display || self.mlist.len() > 1,
                );
            return (Mode::Math, inner);
        }
        match self.open.last() {
            // A paragraph is never *inner*, even inside a `\vbox`: `tex.web`
            // §1091's `new_graf` sets `mode := hmode`, positive, whatever list
            // it interrupts.
            Some(b) if b.in_paragraph => (Mode::Horizontal, false),
            Some(b) => (
                match b.kind {
                    BoxKind::Horizontal => Mode::Horizontal,
                    BoxKind::Vertical => Mode::Vertical,
                },
                true,
            ),
            // §1025 pushes a nest level for `\output` and sets `mode := -vmode`,
            // so the routine runs in *internal* vertical mode however it looks
            // from here: we run it on the outer list with `prev_depth` and
            // `prev_graf` saved rather than opening a box for it. `\ifinner` is
            // true in an output routine, and §211's `print_mode` — which §367's
            // trace consults to decide whether to name the mode at all — says
            // "internal vertical mode".
            //
            //     begin output_active:=true; incr(dead_cycles);
            //     push_nest; mode:=-vmode; prev_depth:=ignore_depth; …
            //
            // `-vmode`, flatly, and *not* whatever mode the page builder
            // interrupted. Returning `self.mode` here leaked that mode in,
            // because a `push_nest` we do not do is the only thing that would
            // have replaced it. When a display fired the output routine
            // mid-paragraph, `self.mode` was still `Horizontal`, so §1094's
            // `head_for_vmode` saw hmode, put the `\unvbox` back and inserted a
            // `\par` — which LaTeX's `\par` discards in inner mode — and read
            // the same `\unvbox` again, for ever. A float followed by a display
            // hung outright; 2608.17233v1 shipped 1 page of 8.
            None if self.output_active => (Mode::Vertical, true),
            None => (self.mode, false),
        }
    }

    /// `$` — enter math mode (`tex.web` §1145). Returns nothing; the caller has
    /// already decided whether this is display math.
    pub fn begin_math(&mut self, display: bool, entered_at: i64) {
        // §1035's `wrapup` runs when the *character run* ends, and a `$` ends
        // one. In TeX it runs in `main_control` before the math command is
        // dispatched, so the discretionary owed to a trailing explicit hyphen
        // goes down while the mode is still `hmode` and lands before `\mathon`:
        //
        //     l e n g t h - \discretionary \mathon a \mathoff \glue …
        //
        // We only flushed from `append`, which entering math does not go
        // through, so the discretionary waited and was put down by the next
        // *character* — after the formula and after the space glue:
        //
        //     l e n g t h - \mathon a \mathoff \glue \discretionary …
        //
        // A breakpoint one atom and one space to the right of where TeX has
        // it. The line ending there is longer, so its badness is lower: 66
        // against TeX's 106, which is the difference between completing a
        // first pass within `\pretolerance` and falling through to the second.
        // 2608.18024v1 had one paragraph of 976 take the first pass where
        // pdfTeX takes the second, so a word pdfTeX hyphenates came out whole.
        self.flush_pending_disc();
        let (m, inner) = self.mode_test();
        self.math_in_paragraph.push(m == Mode::Horizontal && !inner);
        self.saved_mode.push(self.mode);
        self.saved_style.push(self.math_style);
        self.mode = Mode::Math;
        self.math_style = if display {
            crate::math::Style::Display
        } else {
            crate::math::Style::Text
        };
        self.mlist.push(Vec::new());
        self.incompleat.push(None);
        self.math_entered_at.push(entered_at);
        self.math_inner.push(!display);
    }

    /// A `{` inside a formula opens a sub-formula, whose result becomes an
    /// ordinary atom (`tex.web` §1153). Not the same thing as a box.
    pub fn begin_math_group(&mut self, entered_at: i64) {
        self.mlist.push(Vec::new());
        self.incompleat.push(None);
        self.math_entered_at.push(entered_at);
        self.math_inner.push(true);
    }

    /// Expose the nest level underneath an outer formula while §1197 expands
    /// the token that must supply a display's second `$`.
    ///
    /// TeX calls `fin_mlist` first, and that routine pops the math nest before
    /// `get_x_token`. Consequently `\ifvmode`, `\ifhmode` and `\ifinner`
    /// inside a closing macro describe the enclosing list, not the display.
    /// aiTeX converts and installs the display in one later operation, so this
    /// narrow preview supplies the same observable mode without moving any
    /// nodes before the closing token has been validated.
    pub fn preview_mode_after_math(&mut self) -> (Mode, crate::math::Style) {
        let current = (self.mode, self.math_style);
        self.mode = self.saved_mode.last().copied().unwrap_or(Mode::Vertical);
        self.math_style = self
            .saved_style
            .last()
            .copied()
            .unwrap_or(crate::math::Style::Text);
        current
    }

    /// End [`Self::preview_mode_after_math`] after §1197 has read the closing
    /// token. The real pop still belongs to `end_display_math`.
    pub fn restore_mode_preview(&mut self, preview: (Mode, crate::math::Style)) {
        (self.mode, self.math_style) = preview;
    }

    /// `$` again, or the `}` of a sub-formula: convert the innermost list.
    ///
    /// Whether the result becomes a noad (a sub-formula) or joins the
    /// horizontal list (the whole formula) is the caller's decision, because
    /// only it knows which brace closed.
    pub fn end_math_list(&mut self, m: &MathParams, style: crate::math::Style) -> Vec<Node> {
        self.end_math_list_pen(m, style, false)
    }

    pub fn end_math_list_pen(
        &mut self,
        m: &MathParams,
        style: crate::math::Style,
        penalties: bool,
    ) -> Vec<Node> {
        let _p = crate::profile::Phase::enter(crate::profile::P_MATH);
        // `tex.web` §1184: a fraction left incompleat when the list ends takes
        // everything after `\over` as its denominator, and the whole list
        // becomes that one fraction noad.
        let list = self.take_math_list();
        let metrics = FontTable(&self.fonts);
        let mf = self.math_fonts;
        let font_issues = core::cell::RefCell::new(alloc::vec::Vec::new());
        let ctx = crate::math::MathContext {
            thin: m.thin,
            med: m.med,
            thick: m.thick,
            fonts: &|size, fam| mf[size][fam],
            font_issues: &font_issues,
            quad: m.quad,
            axis: m.axis,
            sup: m.sup,
            sub: m.sub,
            sup_drop: m.sup_drop,
            sub_drop: m.sub_drop,
            x_height: m.x_height,
            script_space: m.script_space,
            default_rule_thickness: m.default_rule_thickness,
            num: m.num,
            denom: m.denom,
            null_delimiter_space: m.null_delimiter_space,
            delim: m.delim,
            delimiter_factor: m.delimiter_factor,
            delimiter_shortfall: m.delimiter_shortfall,
            big_op_spacing: m.big_op_spacing,
            bin_op_penalty: m.bin_op_penalty,
            rel_penalty: m.rel_penalty,
        };
        let out = crate::math::mlist_to_hlist_pen(list, style, &ctx, &metrics, penalties);
        drop(ctx);
        self.math_font_issues.extend(font_issues.into_inner());
        out
    }

    /// §722's complaints from the last `mlist_to_hlist`, for the caller to
    /// report: this layer has no error stream.
    pub fn take_math_font_issues(&mut self) -> alloc::vec::Vec<crate::math::MathFontIssue> {
        core::mem::take(&mut self.math_font_issues)
    }

    /// §718's `flush_math`: the current math list and its incompleat fraction
    /// are thrown away.
    ///
    /// ```text
    /// procedure flush_math;
    /// begin flush_node_list(link(head)); flush_node_list(incompleat_noad);
    /// link(head):=null; tail:=head; incompleat_noad:=null;
    /// end;
    /// ```
    ///
    /// The nest level is *not* popped — §1194 calls this and then carries on
    /// through `fin_mlist` and the whole of "Finish displayed math" exactly as
    /// if the formula had simply been empty, which is why a display whose fonts
    /// are missing still contributes its penalties and its display skips.
    /// §1130's test for "nothing but the alignment itself is between `$$`":
    /// `(tail<>head)or(incompleat_noad<>null)` — the current math list has
    /// something in it, or a fraction is waiting for its denominator.
    pub fn math_has_content(&self) -> bool {
        self.mlist.last().is_some_and(|l| !l.is_empty())
            || self.incompleat.last().is_some_and(|i| i.is_some())
    }

    /// §812's `pop_nest` for a display whose body was an alignment: the mode
    /// and the style the formula was entered from come back, and nothing is
    /// appended — the rows are already on the enclosing vertical list.
    pub fn pop_math_nest(&mut self) {
        self.math_in_paragraph.pop();
        self.mlist.pop();
        self.incompleat.pop();
        self.math_entered_at.pop();
        self.math_inner.pop();
        self.mode = self.saved_mode.pop().unwrap_or(Mode::Vertical);
        if let Some(st) = self.saved_style.pop() {
            self.math_style = st;
        }
    }

    /// Take back the noad just pushed — §1193 ignores a `\right` that had no
    /// `\left`, and its delimiter noad has already been made.
    /// §1183's `incompleat_noad<>null`: a fraction is already waiting for its
    /// denominator in this list.
    pub fn has_incompleat(&self) -> bool {
        self.incompleat.last().is_some_and(|i| i.is_some())
    }

    pub fn pop_noad(&mut self) {
        if let Some(l) = self.mlist.last_mut() {
            l.pop();
        }
    }

    pub fn flush_math(&mut self) {
        if let Some(l) = self.mlist.last_mut() {
            l.clear();
        }
        if let Some(i) = self.incompleat.last_mut() {
            *i = None;
        }
    }

    /// Close the whole formula and put its nodes in the horizontal list.
    pub fn end_math(&mut self, m: &MathParams) {
        let style = self.math_style;
        // `tex.web` §1196: `mlist_penalties := (mode > 0)` — a formula set into
        // a real paragraph carries the break penalties; one in a box does not.
        let in_paragraph = self.math_in_paragraph.pop().unwrap_or(false);
        let nodes = self.end_math_list_pen(m, style, in_paragraph);
        self.mode = self.saved_mode.pop().unwrap_or(Mode::Vertical);
        if let Some(s) = self.saved_style.pop() {
            self.math_style = s;
        }
        if self.mode == Mode::Vertical {
            self.mode = Mode::Horizontal;
        }
        // §1196: an inline formula is bracketed by *math nodes* of
        // `\mathsurround`. Their width is usually zero, but they are what tells
        // the line breaker where a formula begins and ends — and TeX never
        // breaks a line inside one.
        let surround = m.math_surround;
        let l = self.current_list();
        l.push(Node::Math {
            after: false,
            width: surround,
        });
        l.extend(nodes);
        l.push(Node::Math {
            after: true,
            width: surround,
        });
        // `tex.web` §1196: a formula always leaves the space factor at 1000.
        // Without it `Nested: $x$ and` kept the colon's 2000 across the
        // formula, and the space after it got the font's extra space.
        self.space_factor = 1000;
    }

    /// Close a *display* formula — `tex.web` §1199–§1205.
    ///
    /// The formula is packed at its natural width and centred in
    /// `\displaywidth`, with `\abovedisplayskip`/`\belowdisplayskip` around it
    /// (or the short pair, when the last line of the paragraph ended far enough
    /// to the left to leave room). Then the paragraph resumes.
    #[allow(clippy::too_many_arguments)]
    /// `tex.web` §1199-§1205: the display is packed, positioned, and — if the
    /// formula ended with `\eqno` or `\leqno` — set beside its equation
    /// number, which arrives here already packed as `a`.
    pub fn end_display_math(
        &mut self,
        m: &MathParams,
        d: &DisplayParams,
        p: &ParParams,
        a: Option<TeXBox>,
        l: bool,
    ) -> Option<(crate::node::PackMeasure, TeXBox)> {
        self.math_in_paragraph.pop();
        let list = self.end_math_list(m, crate::math::Style::Display);
        // §1200 packs a display with `adjust_tail` enabled. §650 therefore
        // removes insertions, marks and adjustments from the hlist before it
        // is measured; §1205 appends that migrating material after the
        // equation (and its number, if any) but before the post-display
        // penalty. Keeping these nodes in the display box hides insertions
        // from the page builder and strands `\vadjust` penalties inside an
        // unbreakable hbox.
        let mut migrating = Vec::new();
        let mut kept = Vec::with_capacity(list.len());
        for n in list {
            match n {
                Node::Ins(_) | Node::Mark { .. } | Node::Adjust { .. } => migrating.push(n),
                other => kept.push(other),
            }
        }
        let list = kept;
        let mut b = {
            let metrics = FontTable(&self.fonts);
            crate::node::hpack(list.clone(), None, &metrics)
        };
        self.mode = self.saved_mode.pop().unwrap_or(Mode::Vertical);
        if let Some(st) = self.saved_style.pop() {
            self.math_style = st;
        }
        // §812: a `\halign` that finished inside this display has already put
        // its rows on the vertical list, with the display's penalties and skips
        // around them. The closing `$$` contributes nothing further — not the
        // (empty) box, which would still take interline glue, and not a second
        // pair of skips, which arrive *after* the rows and read as space below.
        if core::mem::take(&mut self.display_body_done) {
            return None;
        }
        let mut warning = None;
        let z = d.display_width;
        let s = d.display_indent;
        let mut w = b.width;
        // §1200: the number needs its own width plus a quad of clearance.
        let quad = self.math_param(6);
        let mut e = a.as_ref().map_or(Scaled::ZERO, |ab| ab.width);
        let q = if a.is_some() { e + quad } else { Scaled::ZERO };
        // §1201: squeeze the equation if the number will not fit beside it —
        // and if even that is not enough, the number goes on a line of its own,
        // which is what `e := 0` means from here on.
        if w + q > z {
            let sh = total_shrink(&list);
            if e != Scaled::ZERO
                && (w - sh[0] + q <= z
                    || sh[1] != Scaled::ZERO
                    || sh[2] != Scaled::ZERO
                    || sh[3] != Scaled::ZERO)
            {
                let packed = {
                    let metrics = FontTable(&self.fonts);
                    crate::node::hpack_over(
                        list.clone(),
                        Some(z - q),
                        &metrics,
                        p.hfuzz,
                        p.overfull_rule,
                    )
                };
                warning = Some((crate::node::last_pack(), packed.clone()));
                b = packed;
            } else {
                e = Scaled::ZERO;
                if w > z {
                    let metrics = FontTable(&self.fonts);
                    let packed = crate::node::hpack_over(
                        list.clone(),
                        Some(z),
                        &metrics,
                        p.hfuzz,
                        p.overfull_rule,
                    );
                    warning = Some((crate::node::last_pack(), packed.clone()));
                    b = packed;
                }
            }
            w = b.width;
        }
        // §1202: centre it — but not so close to the number that they touch.
        // `tex.web` §100's `half`, which rounds an odd value *up*.
        let mut disp = Scaled(half(z.0 - w.0));
        if e > Scaled::ZERO && disp < e + e {
            disp = Scaled(half(z.0 - w.0 - e.0));
            if matches!(list.first(), Some(Node::Glue(_))) {
                disp = Scaled::ZERO;
            }
        }
        // §1203: the *long* skips are the "not enough clearance" case — the
        // previous line reached at least as far as the display's left edge, so
        // they would collide. Only a line that stopped short of it earns the
        // short skips.
        self.append(Node::Penalty(d.pre_penalty));
        let (g1, mut g2) = if disp + s <= d.pre_display_size || l {
            (
                d.above.sub(crate::node::glue_sub::ABOVE_DISPLAY_SKIP),
                Some(d.below.sub(crate::node::glue_sub::BELOW_DISPLAY_SKIP)),
            )
        } else {
            (
                d.above_short
                    .sub(crate::node::glue_sub::ABOVE_DISPLAY_SHORT_SKIP),
                Some(
                    d.below_short
                        .sub(crate::node::glue_sub::BELOW_DISPLAY_SHORT_SKIP),
                ),
            )
        };
        let mut a = a;
        if l && e == Scaled::ZERO {
            // A `\leqno` too wide to share the line goes above it.
            if let Some(mut ab) = a.take() {
                ab.shift = s;
                self.contribute(ab, p);
            }
            self.append(Node::Penalty(crate::page::INF_PENALTY));
        } else {
            self.append(Node::Glue(g1));
        }
        // §1204: the number rides in the same box as the display, a kern apart.
        if e != Scaled::ZERO {
            if let Some(ab) = a.take() {
                let r = Node::Kern(z - w - e - disp, crate::node::kern_sub::NORMAL);
                let merged = if l {
                    disp = Scaled::ZERO;
                    alloc::vec![
                        Node::Box(alloc::boxed::Box::new(ab)),
                        r,
                        Node::Box(alloc::boxed::Box::new(b))
                    ]
                } else {
                    alloc::vec![
                        Node::Box(alloc::boxed::Box::new(b)),
                        r,
                        Node::Box(alloc::boxed::Box::new(ab))
                    ]
                };
                b = {
                    let metrics = FontTable(&self.fonts);
                    crate::node::hpack(merged, None, &metrics)
                };
            }
        }
        b.shift = s + disp;
        self.contribute(b, p);
        // §1205: an `\eqno` that would not fit beside the display goes under it,
        // hard against the right margin, and the glue below is dropped.
        if e == Scaled::ZERO && !l {
            if let Some(mut ab) = a.take() {
                self.append(Node::Penalty(crate::page::INF_PENALTY));
                let aw = ab.width;
                ab.shift = s + z - aw;
                self.contribute(ab, p);
                g2 = None;
            }
        }
        for n in migrating {
            match n {
                Node::Adjust { list, .. } => self.current_list().extend(list),
                other => self.current_list().push(other),
            }
        }
        self.append(Node::Penalty(d.post_penalty));
        if let Some(g) = g2 {
            self.append(Node::Glue(g));
        }
        warning
    }

    /// `tex.web` §1206 `resume_after_display`: the paragraph goes on, in
    /// horizontal mode, with no indentation and no `\parskip`.
    pub fn resume_after_display(&mut self, line: i64) {
        // §1200 opens the resumed paragraph's nest level itself:
        //
        //     push_nest; mode:=hmode; space_factor:=1000; set_cur_lang; …
        //
        // and §216's `push_nest` stamps `mode_line := line`. So the paragraph
        // that carries on after a display is "entered at" the line of the
        // closing `$$`, not the line the interrupted paragraph began on — §218
        // prints that in `### horizontal mode entered at line N`, and we were
        // still reporting the original start.
        self.par_line = line;
        // §1200: `unsave; prev_graf := prev_graf + 3`.
        //
        // A display counts as three lines of the paragraph it interrupts —
        // §1145 leaves the count alone going in and §1200 adds the three coming
        // out — so `\prevgraf` on the far side of a display is three more than
        // it was, and `\hangafter`, `\parshape` and `\looseness` all measure
        // from it. A hanging paragraph with a display in it hangs the wrong
        // lines without this, and we never added them at all.
        let pg = self.prev_graf();
        self.set_prev_graf(pg + 3);
        self.space_factor = 1000;
        match self.open.last_mut() {
            Some(b) => {
                b.in_paragraph = true;
                b.hlist.clear();
            }
            None => self.mode = Mode::Horizontal,
        }
    }

    /// Close an `\overline`/`\underline` group: the list becomes the nucleus of
    /// a noad that draws a rule over or under it.
    pub fn end_math_group_decorated(
        &mut self,
        m: &MathParams,
        kind: crate::math::Kind,
        class: crate::math::Class,
    ) {
        let _ = m;
        let list = self.take_math_list();
        self.push_noad(crate::math::Noad {
            class,
            nucleus: crate::math::Field::group(list),
            kind,
            ..Default::default()
        });
    }

    /// What the list being built ends with, for `\lastpenalty` and friends.
    pub fn last_node(&mut self) -> Option<Node> {
        // §1034's character loop has already put its current glyph on TeX's
        // list when an expandable test asks about the tail.  aiTeX keeps the
        // raw run buffered until its boundary is known, so expose the same
        // provisional (no right-boundary) nodes before inspecting the list.
        // Do not finish the run: a character produced later by the expansion
        // can still ligature or kern with this one.  BibLaTeX's punctuation
        // machinery, for example, tests `\lastskip` between an author's
        // initial and the following period.
        if self.lig_run_font.is_some() {
            self.rebuild_char_run(false, false);
        }
        // A math list is mixed nodes and noads in TeX. This representation
        // wraps the former in a `Kind::Raw` noad, so recover its tail node for
        // §424 before `current_list` falls through to the enclosing h/v-list.
        if self.mode == Mode::Math {
            return self
                .mlist
                .last()
                .and_then(|l| l.last())
                .filter(|n| n.kind == crate::math::Kind::Raw)
                .and_then(|n| match &n.nucleus {
                    crate::math::Field::List(l) => l.last().cloned(),
                    _ => None,
                });
        }
        if let Some(n) = self.current_list().last().cloned() {
            return Some(n);
        }
        if self.open.is_empty() && self.mode == Mode::Vertical {
            // The main vertical list is `vlist` *and* whatever the page builder
            // has staged but not yet examined: TeX has one list here, and its
            // `tail` covers both. Skipping the staged part answered from the
            // page builder's record instead — a glue moved long ago — so
            // `\addpenalty` saw a non-zero `\lastskip` where TeX sees none and
            // emitted a spurious negative skip, which is a legal page
            // breakpoint. A page could then break inside a `tabular`.
            if let Some(n) = self.page.contributions.last() {
                return Some(n.clone());
            }
            // §424: with everything already moved to the page, the answer comes
            // from what the page builder recorded as it moved it (§991).
            return self.page.last_moved();
        }
        None
    }

    /// §424's node-type code for [`Fastset::last_node`], which can name kinds
    /// that carry no value (a box, a whatsit) and so cannot be answered with a
    /// node.
    pub fn last_node_type(&mut self) -> i32 {
        if self.lig_run_font.is_some() {
            self.rebuild_char_run(false, false);
        }
        if self.mode == Mode::Math {
            return self
                .mlist
                .last()
                .and_then(|l| l.last())
                .filter(|n| n.kind == crate::math::Kind::Raw)
                .and_then(|n| match &n.nucleus {
                    crate::math::Field::List(l) => l.last(),
                    _ => None,
                })
                .map_or(-1, crate::page::node_type_code);
        }
        if let Some(n) = self.current_list().last() {
            return crate::page::node_type_code(n);
        }
        if self.open.is_empty() && self.mode == Mode::Vertical {
            if let Some(n) = self.page.contributions.last() {
                return crate::page::node_type_code(n);
            }
            return self.page.last_node_type;
        }
        -1
    }

    /// `\lastbox` — the last box on the current list, removed (`tex.web`
    /// §1080). `None` if the list does not end in one.
    pub fn take_last_box(&mut self) -> Option<TeXBox> {
        let l = self.current_list();
        match l.last() {
            Some(Node::Box(_)) => match l.pop() {
                Some(Node::Box(mut b)) => {
                    // §1080 removes the box from the list and immediately
                    // clears `shift_amount(cur_box)`: the displacement belonged
                    // to its old position, not to the box value returned by
                    // `\lastbox`. Keeping it made a later `\showbox` and any
                    // reinserted copy retain a stale `\moveleft`/`\raise`.
                    b.shift = Scaled::ZERO;
                    Some(*b)
                }
                _ => Option::None,
            },
            _ => Option::None,
        }
    }

    /// `\vsplit<n> to<dimen>` — `tex.web` §977.
    ///
    /// The register is broken at the best place for that height; what is left
    /// goes back into it, pruned at the top so the first baseline sits
    /// `\splittopskip` below wherever it lands. The register is voided if
    /// nothing is left.
    /// Does box `n`'s vertical list carry glue that shrinks infinitely? §977
    /// asks this before splitting, §1004's equivalent is asked of each glue as
    /// it reaches the page. Only the top level: `vert_break` never looks inside
    /// a box, so glue in a nested one is not glue this split can use.
    pub fn list_has_infinite_shrink(&self, n: u16) -> bool {
        self.boxes.get(&n).is_some_and(|b| {
            b.list.iter().any(|node| match node {
                Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                    g.shrink_order != 0 && g.shrink != Scaled::ZERO
                }
                _ => false,
            })
        })
    }

    /// A loaded font's design size in scaled points, for §1260's reuse test —
    /// which compares the size a font was *resolved* to, not the one asked for.
    /// Zero if the font was never loaded or its metrics could not be read, in
    /// which case there is nothing useful to compare.
    /// §1258's `font_dsize`: the size the `.tfm` was *designed* at, whatever it
    /// is being used at. `font_design_size` above answers the resolved size and
    /// is what §1260's reuse test wants; this is what §298's `at` clause wants.
    pub fn font_dsize(&self, id: usize) -> i64 {
        self.fonts.get(id).map_or(0, |f| f.design_size.0 as i64)
    }

    pub fn font_design_size(&self, id: usize) -> i64 {
        self.fonts
            .get(id)
            .map(|f| {
                if f.at_size.0 != 0 {
                    f.at_size.0 as i64
                } else {
                    f.design_size.0 as i64
                }
            })
            .unwrap_or(0)
    }

    /// The kind of box a register holds, for §977's check before it splits.
    pub fn box_kind(&self, n: u16) -> Option<BoxKind> {
        self.boxes.get(&n).map(|b| b.kind)
    }

    pub fn vsplit(
        &mut self,
        n: u16,
        h: Scaled,
        split_max_depth: Scaled,
        split_top_skip: Glue,
        save_discards: bool,
    ) -> Option<TeXBox> {
        // e-TeX clears `split_disc` at the start of every `\vsplit`, including
        // a split of a void register or an hbox that it subsequently rejects.
        self.split_discards.clear();
        // §977 checks the kind *before* it takes anything: "I can't split such
        // a box, so I'll leave it alone" — the register keeps its contents. We
        // removed first and returned nothing, which voided it.
        if self.boxes.get(&n)?.kind != BoxKind::Vertical {
            return Option::None;
        }
        let v = self.boxes.remove(&n)?;
        let (at, _, trace) =
            crate::page::vert_break_full_traced(&v.list, h, split_max_depth, self.page.trace);
        self.page
            .log
            .extend(trace.into_iter().map(crate::page::PageLog::Text));
        let mut list = v.list;
        let rest = list.split_off(at.min(list.len()));
        // §979: every `\mark` in the part being taken off sets the split
        // marks — the first one `\splitfirstmark`, the last `\splitbotmark` —
        // and §977 clears both at the start whether or not any are found. We
        // recorded neither, so a document that splits a box and asks what was
        // in it got nothing back.
        self.page.split_first_marks.clear();
        self.page.split_bot_marks.clear();
        for node in &list {
            if let Node::Mark { class, tokens } = node {
                self.page
                    .split_first_marks
                    .entry(*class)
                    .or_insert_with(|| tokens.clone());
                self.page.split_bot_marks.insert(*class, tokens.clone());
            }
        }
        let (rest, discards) =
            crate::page::prune_page_top_saved(rest, split_top_skip, save_discards);
        self.split_discards = discards;
        if !rest.is_empty() {
            self.boxes
                .insert(n, vpack(rest, Option::None, split_max_depth));
        }
        Some(vpack(list, Some(h), split_max_depth))
    }

    /// A box built in math mode: an ordinary atom whose nucleus is that box.
    pub fn append_box_math(&mut self, b: TeXBox, vcenter: bool) {
        self.push_noad(crate::math::Noad {
            class: crate::math::Class::Ord,
            nucleus: crate::math::Field::List(alloc::vec![Node::Box(alloc::boxed::Box::new(b))]),
            kind: if vcenter {
                crate::math::Kind::VCenter
            } else {
                crate::math::Kind::Normal
            },
            ..Default::default()
        });
    }

    /// Close a `\left…\right` group: the list, delimiters and all, becomes one
    /// Inner atom (`tex.web` §1191).
    pub fn end_math_group_inner(&mut self, m: &MathParams) {
        let _ = m;
        let mut list = self.take_math_list();
        // §1184 `fin_mlist(p)` has a special splice when `p` is the `right_noad`
        // closing a list with an incomplete fraction. The `left_noad` was in
        // the numerator when `\over` captured the list, while the `right_noad`
        // was just appended to the denominator. Both belong around the
        // completed fraction, not inside its fields:
        //
        //     left_noad -> fraction_noad -> right_noad
        //
        // Leaving them in the fields put the left delimiter in the numerator
        // and the right delimiter in the denominator. A non-null right fence
        // then widened and deepened the fraction itself.
        let moved = if let [fraction] = list.as_mut_slice()
            && matches!(fraction.kind, crate::math::Kind::Fraction(..))
            && let crate::math::Field::Sub(numerator) = &mut fraction.nucleus
            && let crate::math::Field::Sub(denominator) = &mut fraction.denominator
            && matches!(
                numerator.first().map(|n| n.kind),
                Some(crate::math::Kind::LeftDelim(_))
            )
            && matches!(
                denominator.last().map(|n| n.kind),
                Some(crate::math::Kind::RightDelim(_))
            ) {
            let left = numerator.remove(0);
            let right = denominator.pop().expect("checked above");
            let fraction = core::mem::take(fraction);
            Some((left, fraction, right))
        } else {
            None
        };
        if let Some((left, fraction, right)) = moved {
            list = alloc::vec![left, fraction, right];
        }
        self.push_noad(crate::math::Noad {
            class: crate::math::Class::Inner,
            nucleus: crate::math::Field::Sub(list),
            ..Default::default()
        });
    }

    /// Close a sub-formula and attach it as an ordinary atom.
    pub fn end_math_group(&mut self, m: &MathParams) {
        // The list is *not* converted here. `tex.web` keeps a sub-formula as
        // `sub_mlist` and converts it when the enclosing construction knows what
        // style it belongs in: the numerator of a fraction is one size smaller
        // than the fraction, and a sub-formula that had already been set at the
        // outer size came out a size too big — visible the moment a fraction is
        // nested in a fraction.
        let mut list = self.take_math_list();
        let _ = m;
        // §1197 removes one more apparently harmless pair of braces. When a
        // group that is the nucleus of an ordinary atom contains exactly one
        // accent noad, TeX replaces that outer atom with the accent itself:
        //
        //     else if type(p)=accent_noad then if saved(0)=nucleus(tail) then
        //       if type(tail)=ord_noad then replace the tail of the list by p
        //
        // This is observable as soon as a script follows the group. Keeping
        // the extra Ord made `{\hat A}_b^c` attach the scripts outside the
        // accent noad; TeX attaches them to the accent, and §738 then swaps
        // them into the box beneath the zero-width accent character.
        let replacement =
            if list.len() == 1 && matches!(list[0].kind, crate::math::Kind::Accent(..)) {
                list.pop().expect("one accent noad")
            } else {
                crate::math::Noad {
                    class: crate::math::Class::Ord,
                    nucleus: crate::math::Field::group(list),
                    ..Default::default()
                }
            };
        // The literal `{` path appended this ordinary noad before pushing the
        // sub-formula's nest level. Fill that placeholder on close.
        if let Some(tail) = self.mlist.last_mut().and_then(|l| l.last_mut()) {
            *tail = replacement;
        } else {
            self.push_noad(replacement);
        }
    }

    /// Pop the innermost math list, completing a fraction left open in it.
    /// Pack a finished list at its natural width, with the current fonts.
    pub fn hpack_natural(&self, list: Vec<Node>) -> TeXBox {
        let metrics = FontTable(&self.fonts);
        crate::node::hpack(list, None, &metrics)
    }

    pub fn take_math_list_pub(&mut self) -> Vec<crate::math::Noad> {
        self.take_math_list()
    }

    fn take_math_list(&mut self) -> Vec<crate::math::Noad> {
        let list = self.mlist.pop().unwrap_or_default();
        self.math_entered_at.pop();
        self.math_inner.pop();
        match self.incompleat.pop() {
            Some(Some((num, thickness))) => alloc::vec![crate::math::Noad {
                class: crate::math::Class::Inner,
                nucleus: crate::math::Field::Sub(num),
                denominator: crate::math::Field::Sub(list),
                kind: thickness,
                ..Default::default()
            }],
            _ => list,
        }
    }

    /// `\over`, `\atop`, `\above` — `tex.web` §1181. Everything in the current
    /// list so far becomes the numerator; the list starts again empty.
    pub fn start_fraction(&mut self, thickness: crate::math::Kind) {
        // `\over` outside a formula is an error, not a crash: TeX inserts the
        // missing `$` and carries on.
        let Some(l) = self.mlist.last_mut() else {
            return;
        };
        let num = core::mem::take(l);
        if let Some(slot) = self.incompleat.last_mut() {
            // TeX complains about `a\over b\over c` and keeps the first; so do we.
            if slot.is_none() {
                *slot = Some((num, thickness));
                return;
            }
        }
        // Not in math, or a second fraction: put the list back.
        if let Some(l) = self.mlist.last_mut() {
            *l = num;
        }
    }

    /// `\overline{…}`, `\underline{…}` — one noad whose nucleus is the group
    /// that follows. The group is read as a sub-formula, so this is called with
    /// the finished list.
    pub fn push_decorated(&mut self, kind: crate::math::Kind, nucleus: Vec<crate::math::Noad>) {
        self.push_noad(crate::math::Noad {
            class: crate::math::Class::Ord,
            nucleus: crate::math::Field::Sub(nucleus),
            kind,
            ..Default::default()
        });
    }

    /// `\overline x` — the decorated noad's nucleus is that one character.
    pub fn push_decorated_char(
        &mut self,
        kind: crate::math::Kind,
        class: crate::math::Class,
        family: usize,
        code: u32,
    ) {
        self.push_noad(crate::math::Noad {
            class,
            nucleus: crate::math::Field::Char { family, code },
            kind,
            ..Default::default()
        });
    }

    /// `\limits` and friends apply to the operator just read (`tex.web` §1156).
    /// §1159 `math_limit_switch`. Returns false when there is no operator for
    /// the switch to apply to, which is §1159's error:
    ///
    /// ```text
    /// begin if head<>tail then if type(tail)=op_noad then
    ///   begin subtype(tail):=cur_chr; return;
    ///   end;
    /// print_err("Limit controls must follow a math operator");
    /// ```
    ///
    /// An *operator*, not merely the last thing in the list: `A\limits` is an
    /// error, and we set the flag on whatever noad was last.
    pub fn set_limits(&mut self, l: crate::math::Limits) -> bool {
        match self.mlist.last_mut().and_then(|l| l.last_mut()) {
            Some(n) if n.class == crate::math::Class::Op => {
                n.limits = l;
                true
            }
            _ => false,
        }
    }

    pub fn push_noad(&mut self, n: crate::math::Noad) {
        if let Some(l) = self.mlist.last_mut() {
            l.push(n);
        }
    }

    /// A character in math mode becomes an atom: its `\mathcode` says which
    /// class it belongs to and which family it is drawn from.
    pub fn push_atom(&mut self, class: crate::math::Class, family: usize, code: u32) {
        self.push_noad(crate::math::Noad {
            class,
            nucleus: crate::math::Field::Char { family, code },
            subscript: crate::math::Field::Empty,
            superscript: crate::math::Field::Empty,
            ..Default::default()
        });
    }

    /// `^` or `_` — attach a field to the atom just built, or to an empty one.
    /// Attach a script to the atom at the end of the list, inserting an empty
    /// one to carry it when the atom there cannot take it (`tex.web` §1176):
    ///
    /// ```text
    /// @d scripts_allowed(#)==(type(#)>=ord_noad)and(type(#)<left_noad)
    ///
    /// begin t:=empty; p:=null;
    /// if tail<>head then if scripts_allowed(tail) then
    ///   begin p:=supscr(tail)+cur_cmd-sup_mark; t:=math_type(p);
    ///   end;
    /// if (p=null)or(t<>empty) then @<Insert a dummy noad…@>;
    /// ```
    ///
    /// `left_noad` and `right_noad` are *outside* the allowed range, so a
    /// script written after `\left|` or `\right)` belongs to a fresh empty
    /// atom, not to the delimiter. `\left|_{\theta=\theta_0}\right.` is the
    /// everyday way to write an evaluation bar, and we were hanging the
    /// subscript on the delimiter noad — where §762 builds the delimiter box
    /// and reads nothing else, so the subscript vanished from the page
    /// entirely. 2608.16468v1 lost `θ=θ₀` out of the middle of a theorem.
    ///
    /// The same insertion happens when the slot is already taken, which is what
    /// makes `x^1^2` mean `x^1{}^2`; the caller reports §1177's error.
    /// Returns whether that second case applied.
    pub fn attach_script(&mut self, sup: bool, field: crate::math::Field) {
        if let Some(l) = self.mlist.last_mut() {
            if let Some(noad) = l.last_mut() {
                if sup {
                    noad.superscript = field;
                } else {
                    noad.subscript = field;
                }
            }
        }
    }

    /// §1176's half that runs *before* `scan_math`: decide whether the atom at
    /// the end of the list can carry this script, and put an empty one there if
    /// it cannot. Returns whether the slot was already taken, which is §1177's
    /// "Double superscript".
    ///
    /// Split from [`Typeset::attach_script`] because TeX reports that error
    /// before reading the field — `scan_math(p)` is the line after — so the
    /// error context shows `x^1^` and not `x^1^2`.
    pub fn prepare_script(&mut self, sup: bool) -> bool {
        let Some(l) = self.mlist.last_mut() else {
            return false;
        };
        let allowed = match l.last() {
            None => false,
            // Not atoms at all: a style switch, and §729's glue/kern/penalty.
            Some(n) => !matches!(
                n.kind,
                crate::math::Kind::Style(_)
                    | crate::math::Kind::Choice
                    | crate::math::Kind::Raw
                    | crate::math::Kind::LeftDelim(_)
                    | crate::math::Kind::RightDelim(_)
                    | crate::math::Kind::MiddleDelim(_)
            ),
        };
        let taken = allowed
            && l.last()
                .is_some_and(|n| !if sup { &n.superscript } else { &n.subscript }.is_empty());
        if !allowed || taken {
            l.push(crate::math::Noad {
                class: crate::math::Class::Ord,
                nucleus: crate::math::Field::Empty,
                subscript: crate::math::Field::Empty,
                superscript: crate::math::Field::Empty,
                ..Default::default()
            });
        }
        taken
    }

    /// `\fontdimen<n>` of the symbol font (family 2 at text size) — where TeX
    /// keeps the numbers that position everything in a formula.
    /// `\fontdimen n` of family `fam` at `size` — the numbers §700 calls
    /// `mathsy` (family 2) and `mathex` (family 3).
    pub fn math_fam_param(&self, size: usize, fam: usize, n: usize) -> Scaled {
        self.math_fonts[size][fam]
            .and_then(|f| self.fonts.get(f))
            .map_or(Scaled::ZERO, |f| f.param(n))
    }

    pub fn math_param_at(&self, size: usize, n: usize) -> Scaled {
        self.math_fonts[size][2]
            .and_then(|f| self.fonts.get(f))
            .map_or(Scaled::ZERO, |f| f.param(n))
    }

    pub fn math_param(&self, n: usize) -> Scaled {
        self.math_fonts[0][2]
            .and_then(|f| self.fonts.get(f))
            .map_or(Scaled::ZERO, |f| f.param(n))
    }

    /// `\/` — the italic correction of the last character (`tex.web` §1113).
    /// Without it a roman letter following an italic word sits against its
    /// overhang, and every measurement after it is a fraction of a point short.
    pub fn append_italic_correction(&mut self) {
        let last = match self.current_list().last() {
            Some(Node::Char { code, font, .. }) => Some((*code, *font)),
            _ => None,
        };
        if let Some((code, font)) = last {
            let k = self
                .fonts
                .get(font)
                .map_or(Scaled::ZERO, |f| f.italic(code));
            // §1113 appends the kern whatever the correction comes to:
            //
            //     tail_append(new_kern(char_italic(f)(char_info(f)(character(p)))));
            //     subtype(tail):=explicit;
            //
            // A zero one is not nothing. The subtype is `explicit`, and §866
            // makes an explicit kern a legal breakpoint when glue follows — so
            // dropping it costs the line breaker a place to break, not just a
            // node. LaTeX ends `\textbf{…}` with an italic correction, and in
            // an upright bold font that correction is zero, so every
            // `\textbf{word} word` in a paragraph lost a breakpoint TeX had.
            // The paragraph trace names it: `@\kern via @@5` where we printed
            // `@ via @@5`, every number identical.
            self.current_list()
                .push(Node::Kern(k, crate::node::kern_sub::EXPLICIT));
        }
    }

    /// `\unskip`/`\unkern`/`\unpenalty`: drop a trailing discardable node.
    /// §1105 `delete_last`, for one of the three kinds.
    ///
    /// ```text
    /// if not is_char_node(tail) then
    ///   if type(tail)=cur_chr then …remove it…
    /// ```
    ///
    /// The type must *match*: `\unskip` removes glue and nothing else,
    /// `\unkern` a kern, `\unpenalty` a penalty. Removing whichever
    /// discardable happened to be last made `\unskip` eat a kern and
    /// `\unpenalty` eat glue, which is how a document loses an italic
    /// correction it never asked to lose.
    pub fn remove_last_discardable(&mut self, kind: RemoveKind) {
        // Math has its own current list. Raw glue, kern, and penalty nodes are
        // represented as one-node `Raw` noads here; reaching through to the
        // enclosing hbox instead makes `\unskip` inside `$...$` delete glue
        // that precedes the formula. The `array` package's column template is
        // exactly `\hfil\hskip1sp$#\unskip$\hfil`: deleting that 1sp changes
        // every matrix column width and eventually moves glyphs by a scaled
        // point.
        if self.mode == Mode::Math {
            let matches_kind = self
                .mlist
                .last()
                .and_then(|list| list.last())
                .is_some_and(|noad| {
                    if noad.kind != crate::math::Kind::Raw {
                        return false;
                    }
                    let crate::math::Field::List(nodes) = &noad.nucleus else {
                        return false;
                    };
                    matches!(
                        (kind, nodes.as_slice()),
                        (RemoveKind::Glue, [Node::Glue(_) | Node::Leaders { .. }])
                            | (RemoveKind::Kern, [Node::Kern(_, _)])
                            | (RemoveKind::Penalty, [Node::Penalty(_)])
                    )
                });
            if matches_kind {
                if let Some(list) = self.mlist.last_mut() {
                    list.pop();
                }
            }
            return;
        }
        let l = self.current_list();
        let matches_kind = match (kind, l.last()) {
            (RemoveKind::Glue, Some(Node::Glue(_) | Node::Leaders { .. })) => true,
            (RemoveKind::Kern, Some(Node::Kern(_, _))) => true,
            (RemoveKind::Penalty, Some(Node::Penalty(_))) => true,
            _ => false,
        };
        if !matches_kind {
            return;
        }
        // §1105 `delete_last` refuses when the node it would remove is part of a
        // discretionary's *replace* text:
        //
        //     if not is_char_node(q) then if type(q)=disc_node then
        //       begin for m:=1 to replace_count(q) do p:=link(p);
        //       if p=tail then return;
        //       end;
        //
        // §145 stores that text as `replace_count` nodes following the disc
        // node, so removing one without decrementing the count leaves the list
        // claiming more than it has. That is not a cosmetic inconsistency:
        // §882 skips `1 + replace` nodes when it breaks at the discretionary,
        // so a stale count swallows whatever follows. In trip.tex the
        // `\unkern` after `\discretionary{…}{…}{\?/A\kern2pt}` ate the
        // `\vbox{\hrule width 6pt}` on the next line.
        // §1105 walks from the head; we walk *back* from the tail, which is the
        // same answer for far less work. If the tail is inside a replace region
        // then every node between the disc and the tail is replace text, so the
        // first disc found going back is the only candidate — and it can only
        // reach the tail if its count is at least the distance. TeX's
        // `replace_count` is a quarterword, so nothing more than 255 nodes back
        // can cover the tail, and the scan stops there.
        //
        // Walking forward instead is what TeX does and is O(n) per `\unskip`.
        // That is affordable in TeX and was not here: LaTeX calls `\unskip`
        // constantly, and the forward walk cost three packages their load
        // (biblatex, cite and todonotes all timed out) before this was
        // rewritten.
        // Only a *kern* can be reached this way. §1121 allows a discretionary
        // list to hold boxes, rules, kerns, characters and ligatures and
        // nothing else — glue and a penalty are "Improper discretionary list"
        // and are removed as the list is built, so in TeX they are never
        // replace nodes and `\unskip`/`\unpenalty` after a discretionary
        // delete from the enclosing list as usual.
        //
        // We do not enforce §1121 yet, so our replace text *can* hold glue or a
        // penalty and the count is one larger than TeX's. Refusing on those
        // would then differ from TeX in the other direction — it did, before
        // this guard was narrowed. Restricting the refusal to kerns matches
        // TeX's behaviour exactly for every list TeX would accept, and leaves
        // the lists it would have rejected behaving as they did.
        if !matches!(l.last(), Some(Node::Kern(_, _))) {
            l.pop();
            return;
        }
        let last = l.len().saturating_sub(1);
        let floor = last.saturating_sub(255);
        let mut j = last;
        while j > floor {
            j -= 1;
            if let Node::Disc(d) = &l[j] {
                if j + d.replace >= last {
                    return;
                }
                break;
            }
        }
        l.pop();
    }

    /// Is a paragraph about to start? True in vertical mode outside any box, so
    /// the caller knows to supply `\parindent`/`\parskip` before the first
    /// character goes in.
    pub fn needs_paragraph_start(&self) -> bool {
        self.mode == Mode::Vertical && self.open.is_empty()
    }

    /// Begin a paragraph: `\parskip` above it, then the indent box.
    ///
    /// `tex.web` §1091: every paragraph's horizontal list starts with an empty
    /// hbox `\parindent` wide (`\noindent` is exactly the same thing without
    /// the box). It is a real node, so the first line is one indent narrower and
    /// every break in the paragraph shifts if it is missing.
    pub fn start_paragraph(&mut self, parindent: Scaled, parskip: Glue, indent: bool, line: i64) {
        if self.needs_paragraph_here() {
            // §216's `mode_line := line`, for the level `new_graf` is about to
            // push. The line is passed in rather than read from `entered_at`,
            // which is a staging slot for the *next box*: an `\hbox` opened
            // later in the paragraph puts its own line there, and §663's
            // warning then named the box's line as the paragraph's start.
            self.par_line = line;
        }
        if !self.needs_paragraph_here() {
            // `tex.web` §1092 `indent_in_hmode`: a paragraph is already running,
            // so `\indent` just appends the box (and resets the space factor)
            // and `\noindent` does nothing whatever. plain's `\textindent` —
            // every footnote marker — reaches this path.
            if indent {
                match self.mode_test().0 {
                    Mode::Horizontal => {
                        self.space_factor = 1000;
                        let b = Self::indent_box(parindent);
                        self.current_list().push(b);
                    }
                    // §1092 `indent_in_hmode`: in a formula the null box is
                    // the sub-box nucleus of a new ordinary noad. It is still
                    // `\parindent` wide, but it does not start a paragraph or
                    // alter the surrounding horizontal mode.
                    Mode::Math => {
                        let Node::Box(b) = Self::indent_box(parindent) else {
                            unreachable!()
                        };
                        self.append_box_math(*b, false);
                    }
                    Mode::Vertical => {}
                }
            }
            return;
        }
        self.space_factor = 1000;
        if let Some(b) = self.open.last_mut() {
            // §1091's condition is `(mode = vmode) or (head <> tail)`. Inside a
            // box the mode is *internal* vertical, so it is the second half
            // that decides: `\parskip` goes in only when the box already holds
            // something.
            let empty = b.list.is_empty();
            b.in_paragraph = true;
            b.hlist.clear();
            // §1091 appends `new_param_glue(par_skip_code)` — a node, whatever
            // the parameter is worth. Skipping it when `\parskip` is zero left
            // `\vbox{\hrule AA}` a node short of TeX's, and a document that
            // counts nodes or looks at `\lastskip` could tell.
            if !empty {
                b.list
                    .push(Node::Glue(parskip.sub(crate::node::glue_sub::PAR_SKIP)));
            }
        } else {
            // …and in *outer* vertical mode the first half decides, and it is
            // always true: `\parskip` precedes every paragraph, including the
            // first one of the document on an empty list. We suppressed it
            // there, which cost a page its `\parskip` stretch — 42pt of it in
            // trip.tex, where the difference showed up as a page total of
            // `plus 85.0` against TeX's `plus 127.0`.
            self.vlist
                .push(Node::Glue(parskip.sub(crate::node::glue_sub::PAR_SKIP)));
            self.mode = Mode::Horizontal;
        }
        if indent {
            let b = Self::indent_box(parindent);
            self.current_list().push(b);
        }
    }

    /// The empty box `\indent` appends — `tex.web` §1091's `new_null_box` with
    /// `width := par_indent`.
    fn indent_box(parindent: Scaled) -> Node {
        Node::Box(alloc::boxed::Box::new(TeXBox {
            kind: BoxKind::Horizontal,
            width: parindent,
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        }))
    }

    /// `\hbox`/`\vbox`/`\vtop` saw its `{`. An `\hbox` puts us in horizontal
    /// mode and a `\vbox` in vertical, which is what makes a space inside
    /// `\vbox{ }` vanish and one inside `\hbox{ }` real.
    pub fn open_box(
        &mut self,
        kind: BoxKind,
        to: Option<Scaled>,
        spread: Option<Scaled>,
        dest: Option<BoxDest>,
        shift: Scaled,
    ) {
        self.open_box_kind(kind, to, spread, dest, shift, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_box_kind(
        &mut self,
        kind: BoxKind,
        to: Option<Scaled>,
        spread: Option<Scaled>,
        dest: Option<BoxDest>,
        shift: Scaled,
        vtop: bool,
    ) {
        let saved_mode = self.mode;
        let saved_par_line = self.par_line;
        let saved_sf = self.space_factor;
        let saved_language = self.language;
        let saved_paragraph_language = self.paragraph_language;
        let saved_paragraph_left_min = self.paragraph_left_min;
        let saved_paragraph_right_min = self.paragraph_right_min;
        self.space_factor = 1000;
        self.language = 0;
        self.mode = match kind {
            BoxKind::Horizontal => Mode::Horizontal,
            BoxKind::Vertical => Mode::Vertical,
        };
        self.open.push(OpenBox {
            kind,
            entered_at: self.entered_at,
            vtop,
            to,
            spread,
            list: Vec::new(),
            hlist: Vec::new(),
            in_paragraph: false,
            alignment_cell: false,
            saved_mode,
            saved_par_line,
            saved_sf,
            saved_language,
            saved_paragraph_language,
            saved_paragraph_left_min,
            saved_paragraph_right_min,
            prev_depth: IGNORE_DEPTH,
            prev_graf: 0,
            shift,
            dest,
        });
    }

    /// Mark the innermost open list as an alignment span.  It remains an
    /// ordinary h/v list for building and packing; only diagnostic nest
    /// traversal needs to distinguish its role.
    pub fn mark_alignment_cell(&mut self) {
        if let Some(b) = self.open.last_mut() {
            b.alignment_cell = true;
        }
    }

    /// Close the innermost open box and pack it. Returns `None` if no box group
    /// was open.
    pub fn close_box(
        &mut self,
        max_depth: Scaled,
        hfuzz: Scaled,
        overfull_rule: Scaled,
    ) -> Option<(TeXBox, Option<BoxDest>)> {
        let b = self.open.pop()?;
        self.mode = b.saved_mode;
        self.par_line = b.saved_par_line;
        self.space_factor = b.saved_sf;
        self.language = b.saved_language;
        self.paragraph_language = b.saved_paragraph_language;
        self.paragraph_left_min = b.saved_paragraph_left_min;
        self.paragraph_right_min = b.saved_paragraph_right_min;
        let dest = b.dest;
        let metrics = FontTable(&self.fonts);
        // Measure only when the answer is used. `\vbox{…}` with neither `to`
        // nor `spread` — which is nearly every box — needs no natural size at
        // all, and the vertical arm used to get it by `vpack`ing a **clone** of
        // the list and reading the height off the result. That deep copy and
        // the drop that followed it were 8% of a typesetting run.
        let to = match (b.to, b.spread) {
            (Some(t), _) => Some(t),
            (None, Some(s)) => {
                let natural = match b.kind {
                    BoxKind::Horizontal => {
                        crate::node::measure_horizontal_basic(&b.list, &metrics).natural
                    }
                    BoxKind::Vertical => crate::node::measure_vertical(&b.list, max_depth).height,
                };
                Some(natural + s)
            }
            (None, None) => None,
        };
        let mut packed = match b.kind {
            // §663: an `\hbox to` that will not fit leaves a rule behind.
            BoxKind::Horizontal => {
                crate::node::hpack_over(b.list, to, &metrics, hfuzz, overfull_rule)
            }
            BoxKind::Vertical => vpack(b.list, to, max_depth),
        };
        packed.shift = b.shift;
        if b.vtop {
            // §1087: the height is the first box's or rule's height (nought if
            // the list starts with anything else), and the rest becomes depth.
            let h = match packed.list.first() {
                Some(Node::Box(fb)) => fb.height,
                Some(Node::Rule { height, .. }) if *height != Scaled::RUNNING => *height,
                _ => Scaled::ZERO,
            };
            packed.depth = packed.depth - h + packed.height;
            packed.height = h;
        }
        Some((packed, dest))
    }

    /// Close the innermost open box and hand back its *list*, unpacked.
    ///
    /// An alignment entry is not a box yet: its width is not known until every
    /// row has been read, so the list is kept and packed at the end.
    pub fn close_box_raw(&mut self) -> Option<Vec<Node>> {
        let b = self.open.pop()?;
        self.mode = b.saved_mode;
        self.par_line = b.saved_par_line;
        self.space_factor = b.saved_sf;
        self.language = b.saved_language;
        self.paragraph_language = b.saved_paragraph_language;
        self.paragraph_left_min = b.saved_paragraph_left_min;
        self.paragraph_right_min = b.saved_paragraph_right_min;
        Some(b.list)
    }

    /// How many lines the paragraph just ended contributed. `tex.web` §1149
    /// reads `prev_graf` to decide which measure a display takes.
    pub fn lines_so_far(&self) -> usize {
        self.lines_so_far
    }

    /// Is a box group open? `Fast` asks before treating a `}` as an ordinary
    /// group close.
    pub fn in_box(&self) -> bool {
        !self.open.is_empty()
    }

    /// Register a font's metrics under `id`, growing the table if needed.
    pub fn set_font(&mut self, id: usize, name: &str, tfm: TfmFont) {
        if self.fonts.len() <= id {
            self.fonts.resize_with(id + 1, TfmFont::empty);
            self.font_names.resize(id + 1, String::new());
        }
        self.fonts[id] = tfm;
        self.font_names[id] = String::from(name);
        // A format can reuse a font number while loading a different metric
        // file. Cached nodes are keyed by the number for speed, so replacement
        // invalidates the transient table.
        self.lig_cache.clear();
        self.lig_cache_seen.clear();
        self.break_scratch.clear_plan_cache();
    }

    /// Reproduce pdfTeX's global lookup of an expanded limit font.
    ///
    /// `samsung--base+20 at 10.95pt`, for example, is one internal font even
    /// when several independent NFSS shapes use `samsung--base at 10.95pt` as
    /// their base. Configuring the later shape redirects the shared expanded
    /// font's protrusion tables to that shape. Every earlier base that links to
    /// the same expanded limit must therefore observe the new owner.
    pub fn link_expanded_limit_protrusion(
        &mut self,
        owner: usize,
        stretch: bool,
        private: bool,
        private_fonts: &alloc::collections::BTreeSet<u16>,
    ) {
        let Some(owner_font) = self.fonts.get(owner) else {
            return;
        };
        let Some((owner_stretch, owner_shrink, _)) = owner_font.expansion_limits() else {
            return;
        };
        let amount = if stretch { owner_stretch } else { owner_shrink };
        if amount <= 0 {
            return;
        }
        let Some(name) = self.font_names.get(owner).cloned() else {
            return;
        };
        let size = owner_font.at_size;
        if private {
            if let Some(font) = self.fonts.get_mut(owner) {
                font.set_expanded_protrusion_owner(stretch, owner, false);
            }
            return;
        }
        for (id, font) in self.fonts.iter_mut().enumerate() {
            let Some((candidate_stretch, candidate_shrink, _)) = font.expansion_limits() else {
                continue;
            };
            let candidate_amount = if stretch {
                candidate_stretch
            } else {
                candidate_shrink
            };
            if !private_fonts.contains(&(id as u16))
                && candidate_amount == amount
                && font.at_size == size
                && self.font_names.get(id) == Some(&name)
            {
                font.set_expanded_protrusion_owner(stretch, owner, true);
            }
        }
    }

    /// Append a character, applying the font's ligature and kern program.
    ///
    /// `tex.web` §1034. The pair is examined as it is built, so ligatures
    /// cascade: `f` `f` becomes `ff`, and the next `i` pairs with `ff` to give
    /// `ffi`. Without this the widths are wrong and every line break after the
    /// first ligature drifts.
    /// `hyphen` is §1035's `character(tail)=qi(hyphen_char[main_f])` for *this*
    /// character: the caller knows the font's `\hyphenchar` and the mode. It is
    /// recorded rather than acted on — see [`Typeset::pending_disc`].
    pub fn append_char(&mut self, code: u32, font: usize, hyphen: bool) {
        let _p = crate::profile::Phase::enter(crate::profile::P_BUILD);
        // Outside a box, the caller has already run `start_paragraph` (it needs
        // `\parindent`, which lives in the registers). Inside one, an `\hbox` is
        // already horizontal and a `\vbox` gets a paragraph of its own.
        if self.open.is_empty() {
            self.mode = Mode::Horizontal;
        }
        if self.lig_run_font.is_some() && self.lig_run_font != Some(font) {
            self.finish_char_run(true);
            self.flush_pending_disc();
        }
        // A raw hyphen does not necessarily end the TFM run: `--` and `---`
        // must still be allowed to form ligatures. Once the next pair has no
        // ligature, however, §1035 wraps the current glyph before consuming
        // the new character and puts the empty discretionary there. Rebuilding
        // an undivided raw run would instead overwrite `pending_disc` with the
        // new character's value and lose that breakpoint.
        //
        // Split without either implicit boundary. The two pieces are still
        // one input run; this is only the point where TeX has made the left
        // glyph permanent. A kern command belongs after the discretionary and
        // is copied across the split explicitly.
        let split = if self.pending_disc && self.lig_run_font == Some(font) {
            // Ordinary runs are shaped once, when something closes them. A
            // raw hyphen is the one point where §1035 needs the *current*
            // translated tail before the run is complete: `--` may continue
            // into a ligature, while `-a` must make the hyphen permanent and
            // insert its discretionary first. Materialize only for that
            // decision instead of rebuilding the whole word after every
            // character.
            self.rebuild_char_run(false, false);
            let previous = match self.current_list().last() {
                Some(Node::Char { code, font: f, .. }) if *f == font => Some(*code),
                _ => None,
            };
            match previous
                .and_then(|left| self.fonts.get(font).and_then(|f| f.lig_kern(left, code)))
            {
                Some(LigKern::Ligature { .. }) => None,
                Some(LigKern::Kern(width)) => Some(Some(width)),
                None => Some(None),
            }
        } else {
            None
        };
        if let Some(kern) = split {
            self.finish_char_run(false);
            self.flush_pending_disc();
            if let Some(width) = kern {
                self.current_list()
                    .push(Node::Kern(width, crate::node::kern_sub::NORMAL));
            }
            self.cancel_next_left_boundary();
        }
        if self.lig_run_font.is_none() {
            self.lig_run_font = Some(font);
            self.lig_run_start = self.current_list().len();
            self.lig_run_chars.clear();
            self.lig_run_left_boundary = !core::mem::take(&mut self.cancel_left_boundary);
        }
        self.lig_run_chars.push(code);
        // The raw character just consumed is TeX's `character(tail)` for
        // §1035, even when the TFM program immediately folds it into a glyph.
        self.pending_disc = hyphen;
    }

    /// A space in horizontal mode: the current font's interword glue
    /// (`\fontdimen2/3/4`), modified by the space factor.
    ///
    /// `tex.web` §1043: stretch is scaled by `sf/1000` and shrink by `1000/sf`,
    /// and at `sf >= 2000` the font's `\fontdimen7` extra space is added. INITEX
    /// gives every uppercase letter `\sfcode` 999, on the theory that a capital
    /// often ends an abbreviation rather than a sentence — so a paragraph
    /// beginning "A second..." has a *different first space* from one beginning
    /// "The quick...", and every glyph after it moves.
    /// `\ ` — TeX's control space, `tex.web` §1030/§1041. It appends the
    /// *normal* interword space: `\spaceskip` if that is set, otherwise the
    /// font's own, and in neither case modified by the space factor. It does
    /// not change the space factor either.
    pub fn append_normal_space(&mut self, font: usize, space_skip: Option<Glue>) {
        // §1030 dispatches `ex_space` — the control space `\ ` — in *both*
        // horizontal and math mode:
        //
        //     hmode+ex_space,mmode+ex_space:
        //       @<Append a normal inter-word space to the current list@>
        //
        // and §1041's code is the same for the two. Refusing math mode made
        // `\ ` a no-op inside `$…$`, so `$a\ b$` measured exactly `$ab$`.
        // LaTeX's `~` is `\nobreakspace`, which is `\leavevmode\nobreak\ `, so
        // every tie inside math lost its space: `$10^6~ M_{\odot}$` came out
        // 2.5625pt — one `\fontdimen2` — narrower than pdfTeX's, which was
        // enough to move a line break in a real paper.
        //
        // Vertical mode is still refused here; there `\ ` starts a paragraph
        // first, which the caller does.
        if !matches!(self.mode_test().0, Mode::Horizontal | Mode::Math) {
            return;
        }
        self.finish_char_run(true);
        self.flush_pending_disc();
        // §1041: with the space factor at 1000 a non-zero `\spaceskip` is used
        // as-is, so the node records the parameter. The font's own interword
        // glue is not a parameter and stays subtype-less.
        let mut g = match space_skip {
            Some(g) => g.sub(crate::node::glue_sub::SPACE_SKIP),
            None => match self.fonts.get(font) {
                Some(f) => Glue::interword(f.param(2), f.param(3), f.param(4)),
                None => return,
            },
        };
        self.adjust_microtype_space(&mut g);
        // §729 again: glue inside a formula belongs to the *math list*.
        // `append` knows that and `current_list` does not — pushing there put
        // the space outside the formula, before `\mathon`, where it is still a
        // space of the right size in the wrong place. It reads identically in
        // a width measurement and differently in a box listing.
        self.append(Node::Glue(g));
    }

    pub fn append_space(&mut self, font: usize) {
        // The mode that matters is the *innermost list's*: `\vbox{text}` runs a
        // paragraph inside a box whose own mode is vertical, and reading
        // `self.mode` there threw away every interword space in it.
        if self.mode_test().0 != Mode::Horizontal {
            let _p = crate::profile::Phase::enter(crate::profile::P_BUILD);
            return; // a space in vertical mode is discarded
        }
        self.finish_char_run(true);
        self.flush_pending_disc();
        if self.micro_adjust_interword {
            // §1030 routes every ordinary space through `append_normal_space`
            // while this extension is enabled, regardless of `\spacefactor`.
            let mut g = match self.space_skip {
                Some(g) => g.sub(crate::node::glue_sub::SPACE_SKIP),
                None => match self.fonts.get(font) {
                    Some(f) => Glue::interword(f.param(2), f.param(3), f.param(4)),
                    None => return,
                },
            };
            self.adjust_microtype_space(&mut g);
            self.current_list().push(Node::Glue(g));
            return;
        }
        let sf = self.space_factor;
        // `tex.web` §1043 `app_space`: `\xspaceskip` replaces the space outright
        // once the factor reaches 2000, and `\spaceskip` replaces the font's
        // glue at any factor — but is still scaled by it.
        if sf >= 2000 {
            if let Some(g) = self.x_space_skip {
                self.current_list()
                    .push(Node::Glue(g.sub(crate::node::glue_sub::XSPACE_SKIP)));
                return;
            }
        }
        let base = match self.space_skip {
            Some(g) => (g.width, g.stretch, g.shrink),
            None => match self.fonts.get(font) {
                Some(f) => (f.param(2), f.param(3), f.param(4)),
                None => return,
            },
        };
        let (mut w, mut st, mut sh) = base;
        if sf != 1000 {
            if sf >= 2000 {
                // §1045: `width(main_p) := width(main_p) + extra_space(cur_font)`
                // — applied to whatever `main_p` is, `\spaceskip` or the font's
                // own glue alike. We had it only for the font's, on the theory
                // that a `\spaceskip` brings none of its own; the comment said
                // so and the section does not. `\spaceskip=3pt plus 2pt` at
                // factor 2000 is 4.11111pt wide in TeX (3pt plus cmr10's
                // `\fontdimen7`) and was 3pt here.
                if let Some(f) = self.fonts.get(font) {
                    w = w + f.param(7);
                }
            }
            st = crate::scaled::xn_over_d(st, sf as i32, 1000).0;
            sh = crate::scaled::xn_over_d(sh, 1000, sf as i32).0;
        }
        let mut g = Glue::interword(w, st, sh);
        if let Some(s) = self.space_skip {
            g.stretch_order = s.stretch_order;
            g.shrink_order = s.shrink_order;
            // §1041: at factor 1000 the parameter is used *as-is*, so the node
            // records which parameter it was (`new_param_glue`). Once §1045 has
            // modified it the glue is its own (`new_glue`) and is subtype-less,
            // which is how `\showbox` tells a scaled space from an unscaled one.
            if sf == 1000 {
                g.subtype = crate::node::glue_sub::SPACE_SKIP;
            }
        }
        self.current_list().push(Node::Glue(g));
    }

    /// `\accent<n><char>` — `tex.web` §1123–§1125.
    ///
    /// The accent is centred over the character and shifted to sit on its top:
    /// vertically by the difference between the font's x-height and the
    /// character's height, horizontally by half the difference in their widths,
    /// less the slant correction. The two kerns around it undo the accent's own
    /// width so the pair occupies exactly the character's.
    pub fn append_accent(
        &mut self,
        accent: u32,
        ch: Option<u32>,
        accent_font: usize,
        base_font: usize,
        a: crate::scaled::Scaled,
        x: crate::scaled::Scaled,
        accent_slant: i32,
    ) {
        if self.fonts.get(accent_font).is_none() {
            return;
        }
        let mut acc = Node::Char {
            code: accent,
            font: accent_font,
            lig: None,
        };
        if let Some(q) = ch {
            let Some(f) = self.fonts.get(base_font) else {
                return;
            };
            let w = f.width(q);
            let h = f.height(q);
            let base_slant = f.param(1).0 as i64;
            if h != x {
                // Boxing it is what lets it be shifted: a character node has no
                // shift of its own.
                let metrics = FontTable(&self.fonts);
                let mut b = crate::node::hpack(alloc::vec![acc], None, &metrics);
                b.shift = x - h;
                acc = Node::Box(alloc::boxed::Box::new(b));
            }
            // `round((w-a)/2 + h*t - x*s)`, in exact arithmetic over the
            // common denominator, rounding half away from zero as TeX's
            // `round` does. The base and accent slants can belong to different
            // fonts, and the accent values were frozen before assignments.
            let num = (w.0 - a.0) as i64 * 65536 + 2 * h.0 as i64 * base_slant
                - 2 * x.0 as i64 * accent_slant as i64;
            let den = 2 * 65536;
            let delta = crate::scaled::Scaled(if num >= 0 {
                ((num + den / 2) / den) as i32
            } else {
                -((-num + den / 2) / den) as i32
            });
            self.current_list()
                .push(Node::Kern(delta, crate::node::kern_sub::ACC));
            self.current_list().push(acc);
            self.current_list().push(Node::Kern(
                crate::scaled::Scaled(-a.0 - delta.0),
                crate::node::kern_sub::ACC,
            ));
            // §1125 appends `q` directly. It does not enter §1034's ligature
            // loop, so a following character starts a fresh run and cannot
            // form a ligature with the accented base.
            self.current_list().push(Node::Char {
                code: q,
                font: base_font,
                lig: None,
            });
        } else {
            self.current_list().push(acc);
        }
        // §1125: an accented character always leaves the space factor at 1000.
        self.space_factor = 1000;
    }

    /// `tex.web` §1034: what a character does to the space factor. A code of
    /// exactly 1000 resets it; a smaller one sets it; a larger one only takes
    /// effect if the factor was not already below 1000 (so a capital after a
    /// full stop does not undo the sentence spacing).
    pub fn set_space_factor_from(&mut self, sfcode: i64) {
        self.space_factor = if sfcode == 1000 {
            1000
        } else if sfcode < 1000 {
            if sfcode > 0 {
                sfcode
            } else {
                self.space_factor
            }
        } else if self.space_factor < 1000 {
            1000
        } else {
            sfcode
        };
    }

    pub fn space_factor(&self) -> i64 {
        self.space_factor
    }

    pub fn set_space_factor(&mut self, v: i64) {
        self.space_factor = v;
    }

    /// End the current paragraph: break it into lines and contribute them.
    ///
    /// Two passes, as `tex.web` §863 does: `\pretolerance` with no hyphenation
    /// first, because most paragraphs do not need hyphens and they are ugly.
    /// Only if that fails does the second pass hyphenate and try again at
    /// `\tolerance`.
    pub fn end_paragraph(
        &mut self,
        p: &ParParams,
        lccode: &alloc::collections::BTreeMap<char, char>,
        font_hyphen: &alloc::collections::BTreeMap<u16, i64>,
    ) {
        // Which paragraph is ending: the one in the innermost open vertical box,
        // or the page's.
        let in_box = matches!(self.open.last(), Some(b) if b.in_paragraph);
        if !in_box && (self.mode != Mode::Horizontal || !self.open.is_empty()) {
            return;
        }
        // §816 starts the active list at `prev_graf + 1`. Normally a new
        // paragraph has reset this to zero; a paragraph resumed after display
        // math deliberately has not, because the lines before the display and
        // the display's three-line contribution are part of the same count.
        let prev_graf = self.prev_graf().max(0) as usize;
        let mut list = if in_box {
            let b = self.open.last_mut().expect("checked");
            b.in_paragraph = false;
            core::mem::take(&mut b.hlist)
        } else {
            self.mode = Mode::Vertical;
            core::mem::take(&mut self.hlist)
        };
        if list.is_empty() {
            return;
        }
        // §826 checks the two background skips before it starts the first
        // line-breaking pass. They are parameter specs rather than nodes in
        // `list`, so normalize private copies here; `break_in_place` performs
        // the same operation on every glue node it encounters. The amount is
        // retained and only the order becomes finite, exactly as TeX's
        // `finite_shrink` does.
        self.break_scratch.infinite_shrink = false;
        let background_has_infinite_shrink = [p.left_skip, p.right_skip]
            .iter()
            .any(|g| g.shrink_order != 0 && g.shrink != Scaled::ZERO);
        let finite_params = background_has_infinite_shrink.then(|| {
            let mut q = p.clone();
            for g in [&mut q.left_skip, &mut q.right_skip] {
                if g.shrink_order != 0 && g.shrink != Scaled::ZERO {
                    g.shrink_order = 0;
                }
            }
            q
        });
        if background_has_infinite_shrink {
            self.break_scratch.infinite_shrink = true;
        }
        let p = finite_params.as_ref().unwrap_or(p);
        // Remove trailing glue: TeX discards the glue node at a paragraph's end
        // (`tex.web` §816) before adding the finishing penalty and fil. Leader
        // nodes are glue nodes too; retaining a trailing `\leaders...\hskip`
        // makes the last line wider and ships leader material that TeX drops.
        if matches!(list.last(), Some(Node::Glue(_) | Node::Leaders { .. })) {
            list.pop();
        }
        // §816 puts `\penalty10000` between the paragraph and its `\parfillskip`,
        // and it is not decoration: glue is a legal breakpoint, so without the
        // penalty the last line can be broken *before* the `\parfillskip` and
        // the fill left to form a line of its own. With `\parfillskip` at plain's
        // `0pt plus 1fil` that empty line is free — nothing stretches, nothing
        // is overfull — so the breaker took it whenever `\rightskip` also
        // stretched, and every such paragraph came out one line too long.
        list.push(Node::Penalty(crate::page::INF_PENALTY));
        // `\parfillskip`, whatever the document set it to. plain.tex's
        // `0pt plus 1fil` is what sets the last line at natural width; INITEX's
        // 0pt justifies it like any other.
        list.push(Node::Glue(
            p.parfillskip.sub(crate::node::glue_sub::PAR_FILL_SKIP),
        ));

        let base = BreakParams {
            protrude_chars: p.protrude_chars,
            adjust_spacing: p.adjust_spacing,
            pack_key: [p.hfuzz.0, p.overfull_rule.0, p.hbadness],
            emergency_stretch: Scaled::ZERO,
            prev_graf,
            trace: p.tracing_paragraphs > 0,
            font_ids: p.font_ids.clone(),
            escape: p.escape.clone(),
            hsize: p.hsize,
            hang_indent: p.hang_indent,
            hang_after: p.hang_after,
            looseness: p.looseness,
            par_shape: p.par_shape.clone(),
            left_skip: p.left_skip,
            right_skip: p.right_skip,
            tolerance: p.tolerance,
            line_penalty: p.line_penalty,
            adj_demerits: p.adj_demerits,
            hyphen_penalty: p.hyphen_penalty,
            ex_hyphen_penalty: p.ex_hyphen_penalty,
            newline_char: p.newline_char,
            double_hyphen_demerits: p.double_hyphen_demerits,
            final_hyphen_demerits: p.final_hyphen_demerits,
            trace_inserts: alloc::vec::Vec::new(),
            // §873: the second pass is the final one unless `\emergencystretch`
            // buys a third. `base` is what that pass runs with, so the flag
            // belongs on it and not on a pass of our own invention.
            final_pass: false,
        };
        let mut first = base.clone();
        first.tolerance = p.pretolerance;
        // `\pretolerance` < 0 means "do not try the first pass at all"
        // (`tex.web` §863). Skipping it also skips the clone of the whole
        // paragraph that feeding it needs.
        let try_first = p.pretolerance >= 0;
        // §863/§866/§873 head each pass. The header goes in before the pass
        // runs, so the breaks it reports sit under it.
        // §863 announces the *first* pass and only that one:
        //
        //     if pretolerance>=0 then
        //       begin if tracing_paragraphs>0 then
        //         begin begin_diagnostic; print_nl("@firstpass"); end;
        //       …
        //     else begin threshold:=tolerance; second_pass:=true; …
        //       if tracing_paragraphs>0 then begin_diagnostic;
        //       end;
        //
        // With `\pretolerance` negative there is no first pass to announce, so
        // the narration starts straight into the breaks — no header at all.
        // `@secondpass` is §873's, printed when a first pass has *failed*, so a
        // paragraph that never had one cannot produce it. We printed it
        // anyway.
        if p.tracing_paragraphs > 0 && try_first {
            self.break_scratch
                .trace
                .push(alloc::string::String::from("@firstpass"));
        }
        let (lines, fell_back) = if try_first {
            let metrics = FontTable(&self.fonts);
            // In place: on success the paragraph's nodes are *moved* into the
            // lines and `list` is left empty with its capacity. A pass that
            // falls back returns before touching it, so the second pass still
            // has the paragraph to hyphenate.
            KnuthPlass.break_in_place(&mut list, &first, &metrics, &mut self.break_scratch)
        } else {
            (Vec::new(), true)
        };
        let lines = if fell_back {
            // §896 reads `\hyphenchar` of the *word's own font* and gives the
            // word up when it is outside 0..255:
            //
            //     done2: hyf_char:=hyphen_char[hf];
            //     if hyf_char<0 then goto done1;
            //     if hyf_char>255 then goto done1;
            //
            // A hardcoded `-` here inserted a character the font may not have
            // and TeX may not have chosen: INITEX's `\defaulthyphenchar` is
            // nought, so a font loaded without one hyphenates with character 0.
            let hyphen_char =
                |f: usize| -> i64 { font_hyphen.get(&(f as u16)).copied().unwrap_or(-1) };
            let lc = |c: u32| -> u32 {
                match char::from_u32(c).and_then(|ch| lccode.get(&ch)) {
                    Some(l) => *l as u32,
                    None => 0,
                }
            };
            let (mut hyphenated, _, missing_hyphens) = crate::hyphenate::hyphenate_list(
                &self.hyphenator,
                p.language,
                &list,
                p.left_hyphen_min,
                p.right_hyphen_min,
                &hyphen_char,
                &self.fonts,
                p.uchyph,
                &lc,
            );
            let metrics = FontTable(&self.fonts);
            if p.tracing_paragraphs > 0 && try_first {
                self.break_scratch
                    .trace
                    .push(alloc::string::String::from("@secondpass"));
            }
            // §873: `final_pass := (emergency_stretch <= 0)` when the second
            // pass begins. With the parameter at zero — INITEX's value, and
            // trip's — the second pass *is* the final one: it may force a
            // break rather than give up, and there is no third pass at all. We
            // ran the second pass as if it could fail and then a third that
            // scored differently, which is two departures at once: an
            // `@emergencypass` TeX does not print, and a paragraph settled by
            // rules §859 does not have.
            let mut second = base.clone();
            second.final_pass = p.emergency_stretch <= Scaled::ZERO;
            if p.tracing_lost_chars {
                second.trace_inserts = missing_hyphens
                    .into_iter()
                    .map(|(at, font, code)| {
                        let name = self
                            .font_names
                            .get(font)
                            .map_or("nullfont", alloc::string::String::as_str);
                        (at, crate::fast::missing_character_message(code, name))
                    })
                    .collect();
            }
            let (l2, f2) = KnuthPlass.break_in_place(
                &mut hyphenated,
                &second,
                &metrics,
                &mut self.break_scratch,
            );
            if f2 {
                // Only reachable with `\emergencystretch` positive, which is
                // what buys the third pass — and what the pass *does* with it
                // is §873's `background[2] := background[2] + emergency_stretch`.
                if p.tracing_paragraphs > 0 {
                    self.break_scratch
                        .trace
                        .push(alloc::string::String::from("@emergencypass"));
                }
                let mut emergency = base.clone();
                emergency.final_pass = true;
                emergency.emergency_stretch = p.emergency_stretch;
                KnuthPlass
                    .break_in_place(
                        &mut hyphenated,
                        &emergency,
                        &metrics,
                        &mut self.break_scratch,
                    )
                    .0
            } else {
                l2
            }
        } else {
            lines
        };

        // The paragraph's buffer goes back where it came from, emptied but with
        // its capacity: the next paragraph fills the same allocation instead of
        // growing a new one from nothing. A `Vec` doubling to two thousand
        // 64-byte nodes copies about twice its final size on the way, and that
        // was half a gigabyte over a thousand-page document.
        list.clear();
        if in_box {
            if let Some(b) = self.open.last_mut() {
                if b.hlist.capacity() < list.capacity() {
                    b.hlist = list;
                }
            }
        } else if self.hlist.capacity() < list.capacity() {
            self.hlist = list;
        }

        self.last_par_costs.clear();
        self.last_par_costs
            .extend(lines.iter().map(|l| (l.badness, l.penalty, l.demerits)));
        // §660's verdicts, gathered while the lines are packed and handed to the
        // caller afterwards. A local rather than a field so the closure below
        // does not need `&mut self` while `metrics` holds `&self.fonts`.
        let mut warns: alloc::vec::Vec<(crate::node::PackMeasure, TeXBox)> = alloc::vec::Vec::new();
        let n_lines = lines.len();
        let layout_key = [p.hfuzz.0, p.overfull_rule.0, p.hbadness];
        let mut cached_layout = self.break_scratch.take_plan_layout(layout_key);
        if cached_layout
            .as_ref()
            .is_some_and(|boxes| boxes.len() != n_lines)
        {
            cached_layout = None;
        }
        let reused_layout = cached_layout.is_some();
        let mut cached_boxes = cached_layout.unwrap_or_default().into_iter();
        let mut layout_to_remember = (!reused_layout).then(Vec::new);
        self.lines_so_far += n_lines;
        self.last_line_extent = None;
        for (i, line) in lines.into_iter().enumerate() {
            let ended_at_disc = line.hyphenated;
            let (b, migrating) = if let Some(cached) = cached_boxes.next() {
                crate::node::restore_last_pack(cached.measure, cached.badness);
                (cached.boxed, Vec::new())
            } else {
                // Partition in place: the line already owns its nodes, and
                // copying them into a second `Vec` to drop the migrating few
                // meant every node in the document was copied and dropped one
                // extra time.
                let mut kept = line.nodes;
                let mut migrating = Vec::new();
                let mut w = 0usize;
                for r in 0..kept.len() {
                    if matches!(
                        kept[r],
                        Node::Ins { .. } | Node::Mark { .. } | Node::Adjust { .. }
                    ) {
                        migrating.push(kept[r].clone());
                    } else {
                        kept.swap(w, r);
                        w += 1;
                    }
                }
                kept.truncate(w);
                // `tex.web` §881/§887: `\rightskip` closes every line and
                // `\leftskip` opens it. They are what the line was measured
                // with, so they have to be in it when it is packed.
                kept.push(Node::Glue(
                    p.right_skip.sub(crate::node::glue_sub::RIGHT_SKIP),
                ));
                if p.left_skip != Glue::fixed(Scaled::ZERO) {
                    kept.insert(
                        0,
                        Node::Glue(p.left_skip.sub(crate::node::glue_sub::LEFT_SKIP)),
                    );
                }
                // Keep the immutable font-table borrow inside this block. Once
                // the line is packed it can be contributed immediately.
                let (b, measure, badness) = {
                    let metrics = FontTable(&self.fonts);
                    apply_font_expansion(&mut kept, line.width, p.adjust_spacing, &metrics);
                    let mut b = crate::node::hpack_over(
                        kept,
                        Some(line.width),
                        &metrics,
                        p.hfuzz,
                        p.overfull_rule,
                    );
                    let m = crate::node::last_pack();
                    let badness = crate::node::last_badness();
                    if m.verdict(p.hbadness, p.hfuzz.0).is_some() {
                        warns.push((m, b.clone()));
                    }
                    b.shift = line.indent;
                    (b, m, badness)
                };
                if let Some(layout) = &mut layout_to_remember {
                    layout.push(crate::linebreak::CachedLayoutLine {
                        boxed: b.clone(),
                        measure,
                        badness,
                    });
                }
                (b, migrating)
            };
            // `tex.web` §1146: how far the last line's characters reach right
            // of its reference point. Compute it before contributing the box so
            // this paragraph never needs to retain all its packed lines.
            if i + 1 == n_lines {
                self.last_line_extent = Some(packed_line_extent(&b, &FontTable(&self.fonts)));
            }
            // e-TeX's `\vadjust pre` material precedes the line it migrated
            // from; ordinary adjustments and insertions follow it.
            let mut after = Vec::new();
            for n in migrating {
                match n {
                    Node::Adjust { list, pre: true } => self.current_list().extend(list),
                    other => after.push(other),
                }
            }
            self.contribute(b, p);
            // `tex.web` §655: an insertion inside a line does not belong to the
            // line. `hpack` lifts it out, and it lands in the vertical list just
            // after the line it was written on — which is how a footnote reaches
            // the page its reference is on.
            for n in after {
                match n {
                    // §655: an adjustment's *contents* go into the vertical
                    // list, not the node — `\vadjust{\penalty-100}` has to
                    // leave a penalty the page builder can break at, and a node
                    // wrapping one is not that.
                    Node::Adjust { list, .. } => self.current_list().extend(list),
                    other => self.current_list().push(other),
                }
            }
            // §890: a penalty after every line but the last. `\clubpenalty` rides
            // on the first line (breaking after it would strand it), `\widowpenalty`
            // on the second-to-last (breaking there would strand the last), and
            // `\brokenpenalty` on any line that ended at a hyphen.
            if i + 1 < n_lines {
                let indexed = |values: &[i32], one_based: usize, fallback: i32| {
                    values
                        .get(
                            one_based
                                .saturating_sub(1)
                                .min(values.len().saturating_sub(1)),
                        )
                        .copied()
                        .unwrap_or(fallback)
                };
                let mut q = indexed(&p.interline_penalties, i + 1, p.interline_penalty);
                q += indexed(
                    &p.club_penalties,
                    i + 1,
                    if i == 0 { p.club_penalty } else { 0 },
                );
                q += indexed(
                    &p.widow_penalties,
                    n_lines - i - 1,
                    if i + 2 == n_lines { p.widow_penalty } else { 0 },
                );
                if ended_at_disc {
                    q += p.broken_penalty;
                }
                if q != 0 {
                    self.current_list().push(Node::Penalty(q));
                }
            }
        }
        if !reused_layout && warns.is_empty() {
            if let Some(layout) = layout_to_remember {
                self.break_scratch.remember_plan_layout(layout_key, layout);
            }
        }
        self.line_warnings = warns;
    }

    /// Add a box to the vertical list, with the interline glue that belongs
    /// above it.
    ///
    /// `tex.web` §679: the glue is `\baselineskip` *minus* the previous box's
    /// depth and this one's height, so consecutive baselines sit exactly
    /// `\baselineskip` apart whatever the boxes' extents. Nothing is inserted
    /// above the first box on a page — that is what `prev_depth = ignore_depth`
    /// means, and getting it wrong starts every page with a blank line.
    pub fn contribute(&mut self, b: TeXBox, p: &ParParams) {
        if self.open.is_empty() {
            self.started = true;
        }
        self.append_box(b, p);
    }

    /// Append a box wherever material is going, with the interline glue that
    /// belongs above it if that list is vertical.
    ///
    /// Every vertical list gets this, not only the page: `\vbox{\hbox{A}\hbox{B}}`
    /// separates its two lines by `\baselineskip` exactly as the page does, and
    /// an `\output` routine that wraps `\box255` with a folio line is the case
    /// where it shows.
    pub fn append_box(&mut self, b: TeXBox, p: &ParParams) {
        let vertical = self.appending_vertically();
        if !vertical {
            // §1076: a box appended in horizontal mode resets the space factor.
            //
            //     else begin if abs(mode)=hmode then space_factor:=1000
            //
            // A box is not a character, so nothing about the letter before it
            // should still be governing the space after it. hyperref sets every
            // citation label in an `\hbox`, and `\cite` after a `?` or a `.`
            // therefore kept a space factor of 3000 across the box: the space
            // following `\citet{…}` came out as a sentence space, 1.21665pt too
            // wide with three times the stretch, and the paragraph broke
            // elsewhere.
            self.space_factor = 1000;
            self.current_list()
                .push(Node::Box(alloc::boxed::Box::new(b)));
            return;
        }
        self.append_box_vertical(b, p);
    }

    /// Is material about to join a *vertical* list?
    ///
    /// A box appended while a paragraph is running inside a vertical box
    /// belongs to the paragraph, not to the box's vertical list.
    pub fn appending_vertically(&self) -> bool {
        match self.open.last() {
            Some(ob) => ob.kind == BoxKind::Vertical && !ob.in_paragraph,
            None => self.mode == Mode::Vertical,
        }
    }

    /// `tex.web` §1083's `adjusted_hbox_group`: an `\hbox` written in vertical
    /// mode and appended straight to that list does *not* keep its insertions,
    /// marks and `\vadjust` material.
    ///
    /// §651 transfers all three out of the list `hpack` is measuring, and §1076
    /// drops them on the vertical list immediately after the box. It is the
    /// same migration §888 does for the lines of a paragraph, and it matters
    /// for the same reasons: an `\insert` has to reach the page builder to be
    /// placed, a `\mark` has to reach it to be seen by `\botmark`, and a
    /// `\vadjust{\penalty-100}` has to leave a penalty the page can break at.
    /// Sealed inside the box, none of them exist as far as the page is
    /// concerned.
    ///
    /// Only for a directly-appended `\hbox`: `\setbox0\hbox{…}` keeps its
    /// material (§1083 tests `box_context < box_flag`), and a `\vbox` never
    /// migrates because `vpackage` has no adjustment list.
    pub fn append_box_adjusted(&mut self, mut b: TeXBox, p: &ParParams) {
        let mut migrating: Vec<Node> = Vec::new();
        let mut kept: Vec<Node> = Vec::with_capacity(b.list.len());
        for n in core::mem::take(&mut b.list) {
            match n {
                Node::Ins(_) | Node::Mark { .. } | Node::Adjust { .. } => migrating.push(n),
                other => kept.push(other),
            }
        }
        b.list = kept;
        let mut after = Vec::new();
        for n in migrating {
            match n {
                Node::Adjust { list, pre: true } => self.current_list().extend(list),
                other => after.push(other),
            }
        }
        self.append_box(b, p);
        for n in after {
            match n {
                // §655: an adjustment's *contents* go on the list, not the node.
                Node::Adjust { list, .. } => self.current_list().extend(list),
                other => self.current_list().push(other),
            }
        }
    }

    /// Append a box *as if* in vertical mode — with `tex.web` §679's interline
    /// glue — whatever the current mode says.
    ///
    /// The caller that needs this is a `\halign` in display math. §774 puts one
    /// into internal vertical mode for the duration and takes its `prev_depth`
    /// from the enclosing vertical list, precisely so its rows are spaced by
    /// `\baselineskip` like any other lines. Appending them in math mode
    /// instead stacks them flush, which is how amsmath's `align` came out with
    /// its rows 12pt apart — the height of its own `\strut@` — where TeX sets
    /// them 15.
    pub fn append_box_vertical(&mut self, b: TeXBox, p: &ParParams) {
        let pd = match self.open.last() {
            Some(ob) => ob.prev_depth,
            None => self.prev_depth,
        };
        if pd > IGNORE_DEPTH {
            let gap = p.baselineskip - pd - b.height;
            // `tex.web` §679: if that would put the baselines closer than
            // `\lineskiplimit`, use `\lineskip` instead. Otherwise a tall line
            // silently overlaps the one above it.
            //
            // Either way the *whole* glue parameter is copied — `new_skip_param`
            // keeps its stretch and shrink and overrides only the width.
            let glue = if gap < p.lineskiplimit {
                p.lineskip_glue.sub(crate::node::glue_sub::LINE_SKIP)
            } else {
                let mut g = p
                    .baselineskip_glue
                    .sub(crate::node::glue_sub::BASELINE_SKIP);
                g.width = gap;
                g
            };
            self.current_list().push(Node::Glue(glue));
        }
        let depth = b.depth;
        match self.open.last_mut() {
            Some(ob) => ob.prev_depth = depth,
            None => self.prev_depth = depth,
        }
        self.current_list()
            .push(Node::Box(alloc::boxed::Box::new(b)));
    }

    /// Hand the vertical list to the page builder and pack any page it completes.
    ///
    /// `tex.web` §1005: whether a page is full is a question only the builder can
    /// answer, and the answer changes with every box contributed — so this runs
    /// after each contribution, not once at the end.
    pub fn build_page(&mut self, p: &ParParams) -> Option<TeXBox> {
        if !self.open.is_empty() {
            return None;
        }
        // `tex.web` §987 `freeze_page_specs`: `\pagegoal` is set to `\vsize`
        // *once*, when the page first receives something. Setting it on every
        // call would undo the room each insertion has claimed from it.
        //
        // §987 freezes when the first *box, rule or insertion* arrives, not on
        // every call, and we freeze one call early: `\end` on an emptied list
        // freezes a page that never exists, and `\tracingpages` prints a
        // `%% goal height=` line for it that TeX does not. Guarding the freeze
        // on the contribution list holding a box, rule or insertion fixes the
        // stray line and *changes page breaking* — one typeset_check case moved
        // and TRIP went from 8 pages to 18 — so the freeze has to move to where
        // the box is actually contributed rather than be predicted from here.
        // §987 `freeze_page_specs` runs when the first box, rule or insertion
        // *arrives*, not on every call. Predicting it from the contribution list
        // is not the same thing, but it is much closer than freezing
        // unconditionally: `\end` on an emptied list froze a page that never
        // exists, and `\tracingpages` printed a `%% goal height=` line for it
        // that TeX does not — twice, in trip.tex's first two pages.
        let arriving = self
            .page
            .contributions
            .iter()
            .chain(self.vlist.iter())
            .any(|n| matches!(n, Node::Box(_) | Node::Rule { .. } | Node::Ins(_)));
        if !self.page.frozen && arriving {
            self.page.page_goal = p.vsize;
            self.page.best_size = p.vsize;
            // §987 freezes `\maxdepth` with the goal.
            self.page.page_max_depth = p.maxdepth;
            if self.page.trace {
                self.page
                    .log
                    .push(crate::page::PageLog::Text(alloc::format!(
                        "%% goal height={}, max depth={}",
                        crate::scaled::print_scaled(p.vsize),
                        crate::scaled::print_scaled(p.maxdepth)
                    )));
            }
        }
        self.page.topskip = p.topskip;
        self.page.contributions.append(&mut self.vlist);
        if let Some(Decision::FirePage) = self.page.build() {
            // `freeze_page_specs` captured this value when the first box,
            // rule, or insertion reached the page. The output routine may run
            // after the surrounding macro has restored `\maxdepth` (a
            // `longtable` page does exactly that), but §1017 packages
            // `\box255` with the frozen `page_max_depth`, not the current
            // parameter value.
            let page_max_depth = self.page.page_max_depth;
            let material = self.page.fire_up();
            if material.is_empty() {
                return None;
            }
            // `prev_depth` is *not* reset here. It belongs to the vertical list
            // being contributed to, which the page builder only drains; firing a
            // page does not start a new list. Resetting it dropped the interline
            // glue below whatever line happened to precede a page break, closing
            // up the two lines around it by a whole baselineskip. `\topskip` is
            // what handles the gap at a page's top, and the builder inserts it.
            // §1017: `\box255` is packed to `best_size` — `\pagegoal` as it
            // stood at the chosen break, which is `\vsize` less the room the
            // page's insertions claimed.
            return Some(vpack(material, Some(self.page.fired_size), page_max_depth));
        }
        None
    }

    /// Everything the builder has not yet examined, plus the page so far —
    /// `\end`'s last page. No next page to defer to, so splitting at the
    /// cheapest break here would discard the remainder.
    /// End of job.
    pub fn reset_after_job(&mut self) {
        self.started = false;
        self.prev_depth = IGNORE_DEPTH;
    }

    /// `tex.web` §1054's test for whether the job is over: nothing on the page,
    /// nothing contributed, nothing waiting. Asked *before* appending `\end`'s
    /// material, because appending it to an already-empty page manufactures one
    /// more page — with a folio on it, since `\output` runs for it like any
    /// other. plain.tex's `\bye` is `\par\vfill\supereject\end`, and the
    /// `\supereject` has already flushed everything by the time `\end` arrives.
    pub fn job_is_over(&self) -> bool {
        self.page.page.is_empty() && self.page.contributions.is_empty() && self.vlist.is_empty()
    }

    pub fn append_end_sentinel(&mut self, p: &ParParams) {
        // `tex.web` §1054: `\end` appends `\hbox to\hsize{}`, `\vfill` and a
        // forced break, then lets the page builder run once more. Three things
        // fall out of that and none of them is decoration:
        //   * the forced penalty fires the last page through the *normal* path,
        //     so it is packed to `\vsize` like every other and `\output` runs
        //     for it;
        //   * the `\vfill` absorbs the slack, so packing to `\vsize` does not
        //     stretch the interline glue and spread the last page's lines;
        //   * the builder breaks *before* this material, so it never appears on
        //     the page — which is why the last line's depth is the last thing
        //     the page emits.
        self.vlist.push(Node::Box(alloc::boxed::Box::new(TeXBox {
            kind: BoxKind::Horizontal,
            width: p.hsize,
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        })));
        self.vlist.push(Node::Glue(Glue {
            stretch: Scaled::from_points(1),
            stretch_order: 2, // \vfill
            ..Default::default()
        }));
        self.vlist.push(Node::Penalty(-1_073_741_824));
    }

    /// Which fonts the finished pages actually use — the DVI need only define
    /// those. plain.tex loads sixteen; defining all of them puts sixteen font
    /// definitions in a file that sets text in one.
    pub fn used_fonts(&self) -> Vec<bool> {
        let mut used = alloc::vec![false; self.fonts.len()];
        for p in &self.pages {
            mark_used(&p.list, &mut used);
        }
        used
    }
}

fn mark_used(list: &[Node], used: &mut [bool]) {
    for n in list {
        match n {
            Node::Char { font, .. } => {
                if let Some(u) = used.get_mut(*font) {
                    *u = true;
                }
            }
            Node::Box(b) => mark_used(&b.list, used),
            Node::Disc(d) => {
                mark_used(&d.pre, used);
                mark_used(&d.post, used);
            }
            _ => {}
        }
    }
}

/// A box's kind, for callers that need to know how to repack it.
pub fn box_kind(b: &TeXBox) -> BoxKind {
    b.kind
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font with no character widths — `TfmFont::empty()` has none, and there
    /// is no way to synthesise them without a real `.tfm`. So these tests
    /// exercise the parts that do not need them: glue, modes, interline spacing
    /// and the shape of the break. Character metrics are covered where they can
    /// be checked properly, against `pdftex`'s own `\wd` of the same text.
    fn spaced() -> TfmFont {
        // Interword glue 20pt, stretching 10pt and shrinking 5pt: numbers whose
        // arithmetic can be done in the head, so a wrong break is obvious.
        let mut f = TfmFont::empty();
        f.set_param(2, Scaled::from_points(20));
        f.set_param(3, Scaled::from_points(10));
        f.set_param(4, Scaled::from_points(5));
        f
    }

    #[test]
    fn globally_reused_expanded_font_takes_the_latest_protrusion_owner() {
        let mut normal = TfmFont::empty();
        normal.set_param(6, Scaled::from_points(10));
        normal.set_protrude_code(45, false, 182);
        normal.set_expansion(20, 20, 1, true);

        let mut substituted_shape = TfmFont::empty();
        substituted_shape.set_param(6, Scaled::from_points(10));
        substituted_shape.set_expansion(20, 20, 1, true);

        let mut ts = Typeset::new();
        ts.set_font(1, "shared", normal);
        let private = alloc::collections::BTreeSet::new();
        ts.link_expanded_limit_protrusion(1, true, false, &private);
        ts.link_expanded_limit_protrusion(1, false, false, &private);
        ts.set_font(2, "shared", substituted_shape);
        ts.link_expanded_limit_protrusion(2, true, false, &private);
        ts.link_expanded_limit_protrusion(2, false, false, &private);

        let metrics = FontTable(&ts.fonts);
        assert_eq!(
            metrics.protrusion(1, 45, false),
            Scaled(119_276),
            "the normal base keeps its own 0.182em hyphen lpcode"
        );
        assert_eq!(
            metrics.expanded_protrusion(1, 45, false, true),
            Scaled::ZERO,
            "its shared +20 instance now borrows the later shape's empty table"
        );
        assert_eq!(
            metrics.expanded_protrusion(1, 45, false, false),
            Scaled::ZERO,
            "the -20 instance is shared independently but has the same latest owner"
        );
    }

    #[test]
    fn script_after_mathchoice_gets_a_separate_atom() {
        use crate::math::{Field, Kind, Noad};

        let mut ts = Typeset::new();
        ts.mlist.push(alloc::vec![Noad {
            kind: Kind::Choice,
            choices: Some(alloc::boxed::Box::new(Default::default())),
            ..Default::default()
        }]);

        assert!(!ts.prepare_script(false));
        ts.attach_script(
            false,
            Field::Char {
                family: 1,
                code: b'i' as u32,
            },
        );

        let list = ts.mlist.last().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].subscript.is_empty());
        assert_eq!(
            list[1].subscript,
            Field::Char {
                family: 1,
                code: b'i' as u32,
            }
        );
    }

    #[test]
    fn interline_glue_is_baselineskip_minus_depth_and_height() {
        let mut ts = Typeset::new();
        let b = |h: i32, d: i32| TeXBox {
            kind: BoxKind::Horizontal,
            width: Scaled::ZERO,
            height: Scaled::from_points(h),
            depth: Scaled::from_points(d),
            set_num: 0,
            set_den: 1,
            shift: Scaled::ZERO,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            list: Vec::new(),
        };
        let p = ParParams {
            baselineskip: Scaled::from_points(12),
            ..Default::default()
        };
        ts.contribute(b(7, 2), &p);
        ts.contribute(b(7, 2), &p);
        // Nothing above the first box; 12 - 2 - 7 = 3pt above the second.
        match &ts.vlist[..] {
            [Node::Box(_), Node::Glue(g), Node::Box(_)] => {
                assert_eq!(g.width, Scaled::from_points(3));
            }
            other => panic!("unexpected vertical list: {other:?}"),
        }
    }

    #[test]
    fn vertical_mode_drops_a_space_and_horizontal_mode_keeps_every_one() {
        let mut ts = Typeset::new();
        ts.set_font(1, "spaced", spaced());
        ts.append_space(1); // vertical mode: discarded (`tex.web` §1045)
        assert!(ts.hlist.is_empty());
        ts.append_char(b'a' as u32, 1, false);
        ts.append_space(1);
        ts.append_space(1);
        ts.append_char(b'b' as u32, 1, false);
        ts.finish_char_run(true);
        // Two spacer *tokens* make two glues. Runs of spaces in a file collapse
        // in the tokenizer (TeX's `skip_blanks` state), not here — and this used
        // to collapse them here as well, which silently ate the space after any
        // construct that appended nothing: `A \setbox0=\hbox{x} B` came out
        // with one space where TeX sets two.
        assert_eq!(ts.hlist.len(), 4);
        assert!(matches!(ts.hlist[1], Node::Glue(_)));
        assert!(matches!(ts.hlist[2], Node::Glue(_)));
    }

    #[test]
    fn last_node_materializes_a_pending_character_without_ending_its_run() {
        let mut ts = Typeset::new();
        ts.set_font(1, "spaced", spaced());
        ts.mode = Mode::Horizontal;
        ts.hlist
            .push(Node::Glue(Glue::fixed(Scaled::from_points(3))));
        ts.append_char(b'P' as u32, 1, false);

        assert!(matches!(
            ts.last_node(),
            Some(Node::Char { code, font: 1, .. }) if code == b'P' as u32
        ));
        assert_eq!(
            ts.lig_run_font,
            Some(1),
            "inspection must keep the run live"
        );

        ts.append_char(b'.' as u32, 1, false);
        ts.finish_char_run(true);
        assert!(matches!(
            ts.hlist.last(),
            Some(Node::Char { code, font: 1, .. }) if *code == b'.' as u32
        ));
    }

    #[test]
    fn unskip_in_math_only_removes_math_glue() {
        let mut ts = Typeset::new();
        ts.mode = Mode::Horizontal;
        ts.hlist.push(Node::Glue(Glue::fixed(Scaled(1))));
        ts.begin_math(false, 1);
        ts.append(Node::Glue(Glue::fixed(Scaled(2))));

        assert_eq!(ts.mlist.last().map(Vec::len), Some(1));
        ts.remove_last_discardable(RemoveKind::Glue);
        assert_eq!(ts.mlist.last().map(Vec::len), Some(0));
        assert_eq!(ts.hlist.len(), 1, "the enclosing 1sp glue must survive");

        ts.append(Node::Kern(Scaled(3), crate::node::kern_sub::EXPLICIT));
        ts.remove_last_discardable(RemoveKind::Glue);
        assert_eq!(ts.mlist.last().map(Vec::len), Some(1));
        ts.remove_last_discardable(RemoveKind::Kern);
        assert_eq!(ts.mlist.last().map(Vec::len), Some(0));
    }

    #[test]
    fn unskip_removes_leaders_because_they_are_glue_nodes() {
        let mut ts = Typeset::new();
        ts.mode = Mode::Horizontal;
        ts.hlist.push(Node::Leaders {
            kind: 0,
            glue: Glue::fixed(Scaled(1)),
            boxed: alloc::boxed::Box::new(TeXBox {
                kind: BoxKind::Horizontal,
                width: Scaled::ZERO,
                height: Scaled::ZERO,
                depth: Scaled::ZERO,
                set_num: 0,
                set_den: 1,
                glue_sign: crate::node::GlueSign::Normal,
                glue_order: 0,
                shift: Scaled::ZERO,
                list: Vec::new(),
            }),
        });

        ts.remove_last_discardable(RemoveKind::Glue);

        assert!(ts.hlist.is_empty());
    }

    /// §1146 records leaders as visible material even when their glue is
    /// stretching. Their exact right edge is then deliberately `max_dimen`,
    /// which prevents a following display from taking the short-skip path.
    #[test]
    fn stretching_leaders_make_the_pre_display_extent_unknown() {
        let leader = TeXBox {
            kind: BoxKind::Horizontal,
            width: Scaled::from_points(1),
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        };
        let line = TeXBox {
            kind: BoxKind::Horizontal,
            width: Scaled::from_points(100),
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 1,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Stretching,
            glue_order: 1,
            shift: Scaled::ZERO,
            list: alloc::vec![Node::Leaders {
                kind: 0,
                glue: Glue {
                    stretch: Scaled::from_points(1),
                    stretch_order: 1,
                    ..Default::default()
                },
                boxed: alloc::boxed::Box::new(leader),
            }],
        };

        let fonts = Vec::new();
        assert_eq!(packed_line_extent(&line, &FontTable(&fonts)), Scaled::MAX);
    }

    #[test]
    fn a_paragraph_becomes_lines() {
        let mut ts = Typeset::new();
        ts.set_font(1, "spaced", spaced());
        // 12 zero-width characters separated by 20pt glue: 220pt of natural
        // width that can shrink to 165pt, against a 50pt measure. It has to
        // break, and no line can hold more than three spaces.
        for i in 0..12 {
            if i > 0 {
                ts.append_space(1);
            }
            ts.append_char(b'x' as u32, 1, false);
        }
        let p = ParParams {
            hsize: Scaled::from_points(50),
            tolerance: 10_000,
            pretolerance: 10_000,
            ..Default::default()
        };
        // `end_paragraph` hands its lines to the page builder, so the finished
        // page is where to look for them.
        ts.end_paragraph(
            &p,
            &alloc::collections::BTreeMap::new(),
            &alloc::collections::BTreeMap::new(),
        );
        ts.append_end_sentinel(&p);
        let page = ts.build_page(&p).expect("a page");
        ts.pages.push(page);
        let boxes: Vec<&TeXBox> = ts.pages[0]
            .list
            .iter()
            .filter_map(|n| match n {
                Node::Box(b) => Some(&**b),
                _ => None,
            })
            .collect();
        assert!(
            boxes.len() >= 3,
            "expected the paragraph to break, got {} line(s)",
            boxes.len()
        );
        // Every line but the last is set exactly to the measure.
        for b in &boxes[..boxes.len() - 1] {
            assert_eq!(b.width, Scaled::from_points(50));
        }
        assert_eq!(ts.mode, Mode::Vertical);
    }

    /// The page builder splits a document that does not fit, and the last page
    /// keeps its natural height rather than being stretched to `\vsize`.
    #[test]
    fn pages_are_split_at_vsize() {
        let mut ts = Typeset::new();
        let p = ParParams {
            baselineskip: Scaled::from_points(10),
            vsize: Scaled::from_points(50),
            ..Default::default()
        };
        let b = || TeXBox {
            kind: BoxKind::Horizontal,
            width: Scaled::from_points(10),
            height: Scaled::from_points(10),
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        };
        // Twelve 10pt boxes against a 50pt page: more than two pages' worth.
        for _ in 0..12 {
            ts.contribute(b(), &p);
            while let Some(page) = ts.build_page(&p) {
                ts.pages.push(page);
            }
        }
        ts.append_end_sentinel(&p);
        while let Some(page) = ts.build_page(&p) {
            ts.pages.push(page);
        }
        assert!(
            ts.pages.len() >= 3,
            "expected several pages, got {}",
            ts.pages.len()
        );
        for page in &ts.pages[..ts.pages.len() - 1] {
            assert_eq!(page.height, Scaled::from_points(50));
        }
        assert!(ts.pages.last().unwrap().height <= Scaled::from_points(50));
    }

    #[test]
    fn fired_page_uses_the_max_depth_frozen_when_the_page_started() {
        let mut ts = Typeset::new();
        let frozen_max_depth = Scaled::ZERO;
        let later_max_depth = Scaled::from_points(6);
        let target = Scaled::from_points(20);
        let depth = Scaled::from_points(2);
        ts.page.frozen = true;
        ts.page.started = true;
        ts.page.page_goal = target;
        ts.page.best_size = target;
        ts.page.page_max_depth = frozen_max_depth;
        ts.page.page.push(Node::Box(alloc::boxed::Box::new(TeXBox {
            kind: BoxKind::Horizontal,
            width: Scaled::from_points(10),
            height: Scaled::from_points(8),
            depth,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        })));
        ts.page
            .contributions
            .push(Node::Penalty(crate::page::EJECT_PENALTY));

        let page = ts
            .build_page(&ParParams {
                vsize: target,
                maxdepth: later_max_depth,
                ..Default::default()
            })
            .expect("forced page");

        assert_eq!(page.depth, frozen_max_depth);
        assert_eq!(
            page.list.last().and_then(|node| match node {
                Node::Box(boxed) => Some(boxed.depth),
                _ => None,
            }),
            Some(depth),
            "the raw last box stays deep; only the fired page clamps it"
        );
    }
}

/// The shrinkability of a horizontal list, by infinity order — `tex.web` §1201
/// asks whether a display can be squeezed to make room for its equation number.
fn total_shrink(list: &[Node]) -> [Scaled; 4] {
    let mut t = [Scaled::ZERO; 4];
    for n in list {
        if let Node::Glue(g) = n {
            let o = (g.shrink_order as usize).min(3);
            t[o] = t[o] + g.shrink;
        }
    }
    t
}
