//! Boxes, glue and the node list.
//!
//! docs/history/PLAN.md decision 2: these are real Rust types, not Knuth's flat `mem[]` word
//! array. The flat array is the single most tempting thing to port faithfully
//! and the most expensive to reverse, so it is settled here.
//!
//! Glue is the substance of TeX's typesetting: every space carries a natural
//! width plus how far it may stretch and shrink, and setting a box means
//! distributing the difference across its glue.

use crate::scaled::Scaled;
use alloc::string::String;
use alloc::vec::Vec;

/// Marker stored at either end of a ligature's provenance when the font's
/// implicit boundary character participated. TeX prints it as `|` in a full
/// node display and omits it from §175's short display.
pub const LIG_BOUNDARY: u32 = 256;

/// Stretch and shrink components, by infinity order.
///
/// `fil` beats any finite amount, `fill` beats `fil`, and `filll` beats both —
/// which is how `\hfil` centres things and `\hfill` pushes them flush.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Glue {
    pub width: Scaled,
    pub stretch: Scaled,
    pub stretch_order: u8,
    pub shrink: Scaled,
    pub shrink_order: u8,
    /// `tex.web`'s `subtype` of a glue *node*: which parameter the glue was
    /// taken from, so a diagnostic can say `\\glue(\\baselineskip) 3.0` instead
    /// of `\\glue 3.0` (§189). Zero means "no parameter" — the glue a font
    /// supplies between words, or one written out as `\\vskip 3pt`.
    ///
    /// It is *provenance*, not value, so it is deliberately outside `PartialEq`
    /// below: `\\ifdim` and every other comparison must see two 3pt glues as
    /// equal however each was obtained.
    #[serde(default)]
    pub subtype: u8,
    /// Whether this node points at TeX's shared `zero_glue` specification.
    /// Pointer identity is semantically visible to pdfTeX's protrusion walk:
    /// canonical zero glue is transparent, while an explicit `\hskip0pt` is
    /// not. The numeric components alone cannot distinguish the two.
    #[serde(default)]
    pub canonical_zero: bool,
}

/// `tex.web`'s kern subtypes (§155).
pub mod kern_sub {
    /// A kern the font asked for, or one math spacing inserted. Not a
    /// breakpoint, and printed with no space after `\kern`.
    pub const NORMAL: u8 = 0;
    /// A `\kern`, a `\/`, or an `\mkern` once it has left `mu` behind (§717).
    /// §866 lets a line break here when glue follows.
    pub const EXPLICIT: u8 = 1;
    /// The kern `\accent` puts around its character (§1125), which §191 marks
    /// `(for accent)` so a display says why it is there.
    pub const ACC: u8 = 2;
    /// pdfTeX's character protrusion: the negative kern that lets the first or
    /// last glyph of a line hang into the margin. `\showbox` marks them
    /// `(left margin)` and `(right margin)`, and — unlike an explicit kern —
    /// prints no space after `\kern`.
    pub const MARGIN_LEFT: u8 = 3;
    pub const MARGIN_RIGHT: u8 = 4;
    /// The change in advance left by substituting an expanded pdfTeX font.
    /// aiTeX keeps the base font in its DVI and represents that advance as a
    /// kern, so PDF placement remains faithful without inventing DVI fonts.
    pub const EXPANSION: u8 = 5;
    /// pdfTeX's automatic character kern from `\knaccode`/`\knbccode`.
    pub const AUTO: u8 = 6;
    /// `\mkern`, still measured in `mu`. §717 cannot convert it until the
    /// *style* is known, and the style is not known until `mlist_to_hlist`
    /// walks the list — so the node carries its `mu` value until then, exactly
    /// as `mu_glue` does for `\mskip`. §191 prints one as `\mkern3.0mu`.
    pub const MU: u8 = 99;
}

/// §189's names for [`Glue::subtype`]: `glue_par` code + 1, so zero stays free
/// for "not from a parameter".
pub mod glue_sub {
    pub const LINE_SKIP: u8 = 1;
    pub const BASELINE_SKIP: u8 = 2;
    pub const PAR_SKIP: u8 = 3;
    pub const ABOVE_DISPLAY_SKIP: u8 = 4;
    pub const BELOW_DISPLAY_SKIP: u8 = 5;
    pub const ABOVE_DISPLAY_SHORT_SKIP: u8 = 6;
    pub const BELOW_DISPLAY_SHORT_SKIP: u8 = 7;
    pub const LEFT_SKIP: u8 = 8;
    pub const RIGHT_SKIP: u8 = 9;
    pub const TOP_SKIP: u8 = 10;
    pub const SPLIT_TOP_SKIP: u8 = 11;
    pub const TAB_SKIP: u8 = 12;
    pub const SPACE_SKIP: u8 = 13;
    pub const XSPACE_SKIP: u8 = 14;
    pub const PAR_FILL_SKIP: u8 = 15;
    pub const THIN_MU_SKIP: u8 = 16;
    pub const MED_MU_SKIP: u8 = 17;
    pub const THICK_MU_SKIP: u8 = 18;
    /// `\nonscript` (§1171), which is a glue node that removes what follows.
    pub const COND_MATH: u8 = 98;
    /// `\mskip` (§1171): glue measured in `mu`.
    pub const MU: u8 = 99;

    /// The control sequence §189 prints for a subtype, without its backslash.
    pub fn name(sub: u8) -> Option<&'static str> {
        Some(match sub {
            LINE_SKIP => "lineskip",
            BASELINE_SKIP => "baselineskip",
            PAR_SKIP => "parskip",
            ABOVE_DISPLAY_SKIP => "abovedisplayskip",
            BELOW_DISPLAY_SKIP => "belowdisplayskip",
            ABOVE_DISPLAY_SHORT_SKIP => "abovedisplayshortskip",
            BELOW_DISPLAY_SHORT_SKIP => "belowdisplayshortskip",
            LEFT_SKIP => "leftskip",
            RIGHT_SKIP => "rightskip",
            TOP_SKIP => "topskip",
            SPLIT_TOP_SKIP => "splittopskip",
            TAB_SKIP => "tabskip",
            SPACE_SKIP => "spaceskip",
            XSPACE_SKIP => "xspaceskip",
            PAR_FILL_SKIP => "parfillskip",
            THIN_MU_SKIP => "thinmuskip",
            MED_MU_SKIP => "medmuskip",
            THICK_MU_SKIP => "thickmuskip",
            COND_MATH => "nonscript",
            MU => "mskip",
            _ => return None,
        })
    }
}

// Provenance is not value. Two glues that measure the same are the same glue
// whatever parameter each came from, so `subtype` sits outside these impls;
// deriving them would let a display-only field change what `\ifdim` says.
impl PartialEq for Glue {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.stretch == other.stretch
            && self.stretch_order == other.stretch_order
            && self.shrink == other.shrink
            && self.shrink_order == other.shrink_order
    }
}

impl Eq for Glue {}

impl Glue {
    /// The same glue, recorded as having come from a parameter (§189).
    pub fn sub(mut self, subtype: u8) -> Self {
        self.subtype = subtype;
        if self.width == Scaled::ZERO && self.stretch == Scaled::ZERO && self.shrink == Scaled::ZERO
        {
            // `new_param_glue` uses the parameter's glue-spec pointer. TeX's
            // assignment path applies `trap_zero_glue`, so a zero-valued glue
            // parameter is the shared canonical specification.
            self.canonical_zero = true;
        }
        self
    }
}

impl Glue {
    pub fn fixed(width: Scaled) -> Self {
        Self {
            width,
            ..Default::default()
        }
    }

    /// Interword glue as a font specifies it.
    pub fn interword(space: Scaled, stretch: Scaled, shrink: Scaled) -> Self {
        Self {
            width: space,
            stretch,
            stretch_order: 0,
            shrink,
            shrink_order: 0,
            subtype: 0,
            canonical_zero: false,
        }
    }

    /// `\hfil` — infinitely stretchable at order 1.
    pub fn fil() -> Self {
        Self {
            stretch: Scaled::from_points(1),
            stretch_order: 1,
            ..Default::default()
        }
    }
}

/// One item in a horizontal or vertical list.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Node {
    /// A character in the current font.
    Char {
        code: u32,
        font: usize,
        /// `tex.web` §143/§144: when the font's ligature program replaces a run
        /// of characters, TeX keeps the run as well as the result — a
        /// *ligature node*. §193 displays it as `\rip C (ligature B-)`: the
        /// character actually set, then what it came from.
        ///
        /// A field on `Char` rather than a variant of its own, deliberately.
        /// Every one of the forty-odd places that matches `Node::Char` would
        /// have had to learn about a new variant, and the ones that did not
        /// would have dropped the glyph silently through a `_` arm. As a field
        /// the compiler names each site instead.
        ///
        /// `None` for an ordinary character, which is nearly all of them; the
        /// boxed slice keeps `Char` inside the enum's existing size.
        lig: Option<alloc::boxed::Box<[u32]>>,
    },
    /// One OpenType run shaped by the host-side Unicode service.
    ///
    /// The engine owns this deliberately small interchange form: no font
    /// parser, shaper, Unicode database, or filesystem type crosses into the
    /// `no_std` interpreter.  Its advance and vertical metrics participate in
    /// the ordinary TeX paragraph and box algorithms; shipout later converts
    /// the glyphs and logical clusters to `aitex-page` events.
    // Boxed because native runs are rare and structurally large. Keeping the
    // payload inline would enlarge every classic `Node::Char` in every job.
    NativeRun(alloc::boxed::Box<NativeRun>),
    /// A blank rectangle. [`Scaled::RUNNING`] in a dimension means take it
    /// from the enclosing box.
    Rule {
        width: Scaled,
        height: Scaled,
        depth: Scaled,
    },
    Glue(Glue),
    /// Fixed space that does not stretch.
    /// Fixed space, with `tex.web`'s kern *subtype* alongside it.
    ///
    /// The subtype is not decoration. §866 makes a kern a legal breakpoint only
    /// when it is `explicit` — a `\kern` the document wrote, or `\/`, or a
    /// converted `\mkern` — so the kerns a font supplies between characters do
    /// not offer the line breaker a break however much glue follows them. §191
    /// also prints the two differently: `\kern 3.0` with a space for an
    /// explicit one, `\kern0.50172` without for a font's.
    Kern(Scaled, u8),
    /// A legal breakpoint, with the cost of breaking there. `<= -10000` forces
    /// a break; `>= 10000` forbids one.
    Penalty(i32),
    /// The boundary of an inline formula — `tex.web` §1196's math node. Its
    /// width is `\mathsurround`, and it does two things beyond that: a break is
    /// legal *after* one when glue follows, and glue *between* one and the next
    /// is not a breakpoint at all. That second rule is why TeX never breaks a
    /// line inside `$a + b$`.
    Math {
        /// True for the node that *ends* a formula (TeX's `after` subtype).
        after: bool,
        width: Scaled,
    },
    Box(alloc::boxed::Box<TeXBox>),
    /// §1377's language whatsit. It has zero dimensions and is invisible in
    /// the DVI, but changes the language and fragment minima used for words
    /// that follow it in the paragraph.
    Language {
        language: u8,
        left: u8,
        right: u8,
    },
    /// `\special{...}` — passed through to the driver untouched.
    Whatsit(alloc::string::String),
    /// A deferred `\write` (`tex.web` §1352): the *index* of a token list the
    /// engine is holding until the page carrying this node is shipped, which is
    /// when TeX expands it and writes the result. `\protected@write` depends on
    /// that timing — it deliberately leaves `\thepage` unexpanded so the page
    /// number comes out right — and so every `\pageref` in LaTeX does.
    ///
    /// The node also has to exist for its own sake: it is not discardable, so the
    /// page builder and `\vsplit` break differently around it, which is what
    /// `\clearpage`'s `\write\m@ne{}` is for.
    WriteOut(u32),
    /// A discretionary break: `\-`, `\discretionary{}{}{}`, or one the
    /// hyphenator inserted.
    ///
    /// If the line breaks here, `pre` ends the line and `post` begins the next,
    /// and `replace` following nodes are dropped. If it does not break, `pre`
    /// and `post` vanish and the `replace` nodes stay. A hyphen is the simple
    /// case: `pre` is the hyphen character, `post` is empty, `replace` is zero.
    Disc(alloc::boxed::Box<Disc>),
    /// `\insert<n>{...}` — material set aside during page building.
    ///
    /// It rides along in the vertical list until a page fires, at which point
    /// it is pulled out into `\box<n>` for `\output` to typeset. A footnote is
    /// the reason it exists: the note's text is written in the middle of a
    /// paragraph but has to appear at the foot of whichever page that
    /// paragraph's line lands on, which is not known until the page breaks.
    ///
    /// The page builder charges it to `\pagegoal` rather than `\pagetotal` —
    /// it takes no room where it stands — and may split it or hold it over;
    /// see [`crate::page::PageBuilder::contribute_insertion`].
    ///
    /// The four values `tex.web` §1100 freezes into the node when the insertion
    /// ends, all read *inside* the insertion's group: `height` is the natural
    /// height **plus depth** of the vpacked material (one number, as TeX stores
    /// it), `depth` is `\splitmaxdepth`, `split_top` is `\splittopskip` and
    /// `float_cost` is `\floatingpenalty`. The page builder needs every one of
    /// them to decide whether the insertion fits, where to split it if it does
    /// not, and what a held-over one costs (§1008–§1010).
    Ins(alloc::boxed::Box<Ins>),
    /// `\leaders`/`\cleaders`/`\xleaders` — glue whose space is filled with
    /// copies of a box (`tex.web` §149). It measures exactly as its glue does;
    /// the box only matters when the list is shipped (§626–§627), which is why
    /// leaders can be a glue node in TeX and a variant of one here.
    ///
    /// `kind` is TeX's subtype: 0 `\leaders` (copies on a grid fixed to the
    /// enclosing box, so adjacent lines of dots line up), 1 `\cleaders` (copies
    /// centred in the space), 2 `\xleaders` (copies spread evenly).
    Leaders {
        kind: u8,
        glue: Glue,
        boxed: alloc::boxed::Box<TeXBox>,
    },
    /// `\vadjust{...}` — vertical material to be placed after the line the
    /// command was written on, or before it for e-TeX's `\vadjust pre`. It rides
    /// out of the horizontal
    /// list exactly as an insertion does, but where an insertion goes to a box
    /// for `\output` to place, an adjustment is spliced straight into the
    /// vertical list — which is how LaTeX puts a penalty or a rule between two
    /// particular lines of a paragraph.
    Adjust {
        list: Vec<Node>,
        pre: bool,
    },
    /// `\mark{...}` or e-TeX's `\marks<n>{...}`. The text is stored fully
    /// expanded at the point the mark ran (`tex.web` §1095: scanned the same
    /// way an `\edef` body is), and is **not** re-expanded when a later
    /// `\topmarks`/`\firstmarks`/`\botmarks` query reads it back. Classic
    /// `\mark` is class zero.
    Mark {
        class: u16,
        tokens: alloc::vec::Vec<crate::token::Token>,
    },
}

/// pdfTeX's `cp_skipable`: nodes that do not hide a protruding character at
/// either margin.
///
/// The original uses pointer identity for its one glue exception: only the
/// canonical `zero_glue` specification is transparent, while an explicit
/// `\hskip0pt` is a barrier. [`Glue::canonical_zero`] preserves that otherwise
/// invisible distinction. Nonzero `\leftskip`/`\rightskip` is handled
/// separately by the margin-kern readers, exactly as in pdfTeX.
pub fn protrusion_skipable(node: &Node) -> bool {
    match node {
        Node::Ins(_)
        | Node::Mark { .. }
        | Node::Adjust { .. }
        | Node::Penalty(_)
        | Node::Language { .. }
        | Node::Whatsit(_)
        | Node::WriteOut(_) => true,
        Node::Disc(d) => d.pre.is_empty() && d.post.is_empty() && d.replace == 0,
        Node::Math { width, .. } => *width == Scaled::ZERO,
        Node::Kern(width, subtype) => {
            *width == Scaled::ZERO || matches!(*subtype, kern_sub::NORMAL | kern_sub::AUTO)
        }
        Node::Box(b) => {
            b.kind == BoxKind::Horizontal
                && b.width == Scaled::ZERO
                && b.height == Scaled::ZERO
                && b.depth == Scaled::ZERO
                && b.list.is_empty()
        }
        Node::Glue(g) => g.canonical_zero,
        Node::Char { .. } | Node::NativeRun(_) | Node::Rule { .. } | Node::Leaders { .. } => false,
    }
}

/// A discretionary's three lists — behind a `Box` in [`Node`], because holding
/// them inline made *every* node in every list 64 bytes wide when a character
/// needs eight. Discretionaries are rare; characters are not.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Disc {
    pub pre: Vec<Node>,
    pub post: Vec<Node>,
    pub replace: usize,
}

/// An insertion's material and the four values §1100 freezes into it. Behind a
/// `Box` for the same reason as [`Disc`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ins {
    pub number: u16,
    pub list: Vec<Node>,
    pub height: crate::scaled::Scaled,
    pub depth: crate::scaled::Scaled,
    pub split_top: Glue,
    pub float_cost: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BoxKind {
    Horizontal,
    Vertical,
}

/// Writing direction of a host-shaped run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NativeDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// One logical Unicode cluster retained independently of visual glyph order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeCluster {
    pub byte_start: u32,
    pub byte_end: u32,
    pub text: String,
}

/// One glyph in a host-shaped run, measured in TeX scaled points.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeGlyph {
    pub source_gid: u32,
    pub cluster: u32,
    pub x_advance: Scaled,
    pub y_advance: Scaled,
    pub x_offset: Scaled,
    pub y_offset: Scaled,
    pub is_notdef: bool,
}

/// A shaped inline object. The font ID is document-local and is resolved by
/// the host when the native PDF resources are prepared.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct NativeRun {
    pub font: u32,
    pub size: Scaled,
    pub direction: NativeDirection,
    /// BCP 47 language used for shaping and optional tagged output.
    pub language: Option<String>,
    /// UAX #9 resolved embedding level. Shaping keeps logical run order; the
    /// line breaker uses this level to reorder each finished line separately.
    pub bidi_level: u8,
    /// Document-local identity of the Unicode span that supplied this run.
    /// Adjacent TeX/native material must never be swept into its bidi reorder.
    pub bidi_span: u32,
    pub width: Scaled,
    pub height: Scaled,
    pub depth: Scaled,
    pub glyphs: Vec<NativeGlyph>,
    pub clusters: Vec<NativeCluster>,
}

impl NativeRun {
    /// Construct a shaped run with neutral optional metadata.
    ///
    /// Language and bidi identity are builders so future optional fields do
    /// not force downstream hosts to use a public struct literal.
    pub fn new(
        font: u32,
        size: Scaled,
        direction: NativeDirection,
        width: Scaled,
        height: Scaled,
        depth: Scaled,
        glyphs: Vec<NativeGlyph>,
        clusters: Vec<NativeCluster>,
    ) -> Self {
        Self {
            font,
            size,
            direction,
            language: None,
            bidi_level: 0,
            bidi_span: 0,
            width,
            height,
            depth,
            glyphs,
            clusters,
        }
    }

    pub fn with_language(mut self, language: Option<String>) -> Self {
        self.language = language;
        self
    }

    pub fn with_bidi(mut self, level: u8, span: u32) -> Self {
        self.bidi_level = level;
        self.bidi_span = span;
        self
    }
}

/// A box: a list of nodes plus the dimensions it was set to.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TeXBox {
    pub kind: BoxKind,
    pub width: Scaled,
    pub height: Scaled,
    pub depth: Scaled,
    /// How much each unit of glue was stretched or shrunk to reach `width`,
    /// as an exact rational rather than a float.
    ///
    /// `tex.web` uses a `glue_ratio` (a real). We use `set_num / set_den` in
    /// scaled points because floating point would put rounding differences into
    /// glyph positions, and byte-exact DVI comparison against `pdftex` is our
    /// only test oracle (docs/history/PLAN.md §4). Integer arithmetic also keeps this crate
    /// `no_std`, where `f64::round` does not exist.
    pub set_num: i64,
    pub set_den: i64,
    pub glue_sign: GlueSign,
    pub glue_order: u8,
    pub shift: Scaled,
    pub list: Vec<Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlueSign {
    Normal,
    Stretching,
    Shrinking,
}

/// Totals gathered while measuring a list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Totals {
    pub natural: Scaled,
    pub height: Scaled,
    pub depth: Scaled,
    /// Stretch available at each infinity order, 0 (finite) through 3.
    pub stretch: [Scaled; 4],
    pub shrink: [Scaled; 4],
    /// Maximum finite glyph-width variation from `\pdffontexpand`.
    pub font_stretch: Scaled,
    pub font_shrink: Scaled,
}

/// Measure a horizontal list without setting it.
pub fn measure_horizontal(list: &[Node], metrics: &impl CharMetrics) -> Totals {
    measure_horizontal_with_expansion::<true>(list, metrics)
}

/// Measure a horizontal list when font-expansion capacity is not consumed.
/// Packing itself uses only the natural/glue and vertical dimensions; avoiding
/// the expansion-table lookup matters for every ordinary, non-microtype line.
pub fn measure_horizontal_basic(list: &[Node], metrics: &impl CharMetrics) -> Totals {
    measure_horizontal_with_expansion::<false>(list, metrics)
}

fn measure_horizontal_with_expansion<const EXPANSION: bool>(
    list: &[Node],
    metrics: &impl CharMetrics,
) -> Totals {
    let mut t = Totals::default();
    for n in list {
        match n {
            Node::Char { code, font, .. } => {
                let (width, height, depth) = metrics.dimensions(*font, *code);
                t.natural = t.natural + width;
                t.height = t.height.max(height);
                t.depth = t.depth.max(depth);
                if EXPANSION {
                    let (stretch, shrink) = metrics.expansion_capacity(*font, *code);
                    t.font_stretch = t.font_stretch + stretch;
                    t.font_shrink = t.font_shrink + shrink;
                }
            }
            Node::NativeRun(run) => {
                t.natural = t.natural + run.width;
                t.height = t.height.max(run.height);
                t.depth = t.depth.max(run.depth);
            }
            Node::Rule {
                width,
                height,
                depth,
            } => {
                // A running dimension contributes nothing here; it is resolved
                // against the enclosing box when the rule is shipped out.
                if *width != Scaled::RUNNING {
                    t.natural = t.natural + *width;
                }
                t.height = t.height.max(*height);
                t.depth = t.depth.max(*depth);
            }
            // Leaders take their *width* from the glue, as ordinary glue does —
            // but §656 also makes the box they repeat count towards the height
            // and depth of the list, the mirror of §668's width rule in `vpack`:
            //
            //     if subtype(p)>=a_leaders then
            //       begin g:=leader_ptr(p);
            //       if height(g)>h then h:=height(g);
            //       if depth(g)>d then d:=depth(g);
            //       end;
            //
            // The repeated box is set inside this list, so the list has to be
            // tall enough to hold one. `\leaders\hrule height 5pt\hfill` in an
            // `\hbox` is 5pt tall in TeX and was 0pt here — and that is how
            // `longtable` draws every rule:
            //
            //     \multispan\LT@cols \unskip
            //       \leaders\hrule\@height\arrayrulewidth\hfill\cr
            //
            // so a `longtable`'s `\hline`s and its booktabs rules added no
            // height at all. 2608.15339v1's first chunk came out 1.30005pt
            // short — exactly `\toprule` (0.8pt) plus `\midrule` (0.5pt) — and
            // the table then fitted on a page pdfTeX had to break.
            Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                t.natural = t.natural + g.width;
                t.stretch[g.stretch_order as usize] =
                    t.stretch[g.stretch_order as usize] + g.stretch;
                t.shrink[g.shrink_order as usize] = t.shrink[g.shrink_order as usize] + g.shrink;
                if let Node::Leaders { boxed, .. } = n {
                    t.height = t.height.max(boxed.height);
                    t.depth = t.depth.max(boxed.depth);
                }
            }
            Node::Kern(k, _) => t.natural = t.natural + *k,
            // A math node is a kern of `\mathsurround` as far as measuring goes.
            Node::Math { width, .. } => t.natural = t.natural + *width,
            Node::Box(b) => {
                t.natural = t.natural + b.width;
                t.height = t.height.max(b.height - b.shift);
                t.depth = t.depth.max(b.depth + b.shift);
            }
            // An unbroken discretionary contributes nothing; the material it
            // would insert only exists if the line breaks there. A mark is
            // pure metadata for the page builder and takes no space, exactly
            // like a penalty.
            // An insertion takes no space where it stands: it is going
            // somewhere else.
            Node::Disc { .. }
            | Node::Penalty(_)
            | Node::Language { .. }
            | Node::Whatsit(_)
            | Node::WriteOut(_)
            | Node::Mark { .. }
            | Node::Ins { .. }
            | Node::Adjust { .. } => {}
        }
    }
    t
}

/// Where character dimensions come from.
///
/// docs/history/PLAN.md decision 5: font access is behind a trait so TFM-only can ship now
/// and OpenType arrive later without touching the engine.
pub trait CharMetrics {
    fn width(&self, font: usize, code: u32) -> Scaled;
    fn height(&self, font: usize, code: u32) -> Scaled;
    fn depth(&self, font: usize, code: u32) -> Scaled;
    /// The three dimensions horizontal packing needs together. Metrics
    /// implementations with an indexed character record can override this to
    /// perform one lookup instead of three.
    fn dimensions(&self, font: usize, code: u32) -> (Scaled, Scaled, Scaled) {
        (
            self.width(font, code),
            self.height(font, code),
            self.depth(font, code),
        )
    }
    /// The italic correction. Only math needs it, and only for the nucleus of
    /// an atom that carries a script, so a metrics source that has none can
    /// answer zero.
    fn italic(&self, _font: usize, _code: u32) -> Scaled {
        Scaled::ZERO
    }
    /// Does this font have interword space? A *text* font does; a math font
    /// does not. `tex.web` §749 uses exactly this test to decide whether a
    /// character's italic correction applies in a formula — `\rm x` inside math
    /// takes its letters from a text font and gets no correction.
    fn has_space(&self, _font: usize) -> bool {
        false
    }
    /// Is there a glyph at this position at all? A delimiter search walks
    /// charlists that run off the end of a font.
    fn char_exists(&self, _font: usize, _code: u32) -> bool {
        false
    }
    /// The TFM tag: 2 means this glyph has a bigger variant (a charlist), 3
    /// that it has an extensible recipe. `tex.web` §544.
    fn char_tag(&self, _font: usize, _code: u32) -> u8 {
        0
    }
    /// The tag's operand: the next variant, or the recipe's index.
    fn char_remainder(&self, _font: usize, _code: u32) -> u32 {
        0
    }
    /// One extensible recipe: top, middle, bottom and the repeated piece.
    fn exten(&self, _font: usize, _index: u32) -> Option<(u32, u32, u32, u32)> {
        None
    }
    /// The kern between this character and the font's `\skewchar` — how far a
    /// math accent over it is shifted (`tex.web` §741).
    fn skew(&self, _font: usize, _code: u32) -> Scaled {
        Scaled::ZERO
    }
    /// `\fontdimen5`, used from the accent's own font by `tex.web` §738.
    /// This is distinct from the family-2 x-height that bounds scripts.
    fn x_height(&self, _font: usize) -> Scaled {
        Scaled::ZERO
    }
    /// The font's kern for this pair, if it has one.
    fn kern(&self, _font: usize, _left: u32, _right: u32) -> Option<Scaled> {
        None
    }
    /// How far this character may hang into a margin: pdfTeX's `\lpcode` or
    /// `\rpcode` as a length. The base is the font's *quad*, not the
    /// character's width — measured, see `TfmFont::protrusion`.
    fn protrusion(&self, _font: usize, _code: u32, _right: bool) -> Scaled {
        Scaled::ZERO
    }
    /// Protrusion on the expanded limit font selected from this base font.
    /// pdfTeX's expanded instances are global and may currently borrow their
    /// lp/rp tables from another base font with the same TFM name and size.
    /// Metrics without that aliasing use the base protrusion unchanged.
    fn expanded_protrusion(&self, font: usize, code: u32, right: bool, _stretch: bool) -> Scaled {
        self.protrusion(font, code, right)
    }
    /// Maximum glyph-width variation at this character's `\efcode`, as
    /// `(stretch, shrink)`. Zero means that this font is not expandable.
    fn expansion_capacity(&self, _font: usize, _code: u32) -> (Scaled, Scaled) {
        (Scaled::ZERO, Scaled::ZERO)
    }
    /// `(stretch limit, shrink limit, step)` from `\pdffontexpand`.
    fn expansion_limits(&self, _font: usize) -> Option<(i64, i64, i64)> {
        None
    }
    /// Actual glyph-width change at a line expansion ratio in -1000..1000.
    fn expansion_delta(&self, _font: usize, _code: u32, _ratio: i64) -> Scaled {
        Scaled::ZERO
    }
    /// Discrete horizontal font expansion selected for this character, in
    /// thousandths. `\efcode` participates before the font's expansion step is
    /// applied, so characters on one line can select neighboring instances.
    /// A PDF renderer scales the font program by this amount.
    fn expansion_amount(&self, _font: usize, _code: u32, _ratio: i64) -> i64 {
        0
    }
    /// Expansion capacity of an automatic kern between two characters.
    ///
    /// `base` is the width of the kern node that TeX actually built. It need
    /// not be the kern currently specified for `left` followed by `right`: a
    /// later ligature can change the neighboring glyph while leaving the old
    /// kern node in place.
    fn expansion_kern_capacity(
        &self,
        _font: usize,
        _left: u32,
        _right: u32,
        _base: Scaled,
    ) -> (Scaled, Scaled) {
        (Scaled::ZERO, Scaled::ZERO)
    }
    /// Width change of that automatic kern at the selected line ratio.
    fn expansion_kern_delta(
        &self,
        _font: usize,
        _left: u32,
        _right: u32,
        _base: Scaled,
        _ratio: i64,
    ) -> Scaled {
        Scaled::ZERO
    }
    /// The font's ligature instruction for this pair: replacement character
    /// and `op_byte`. Math needs the operator because `tex.web` §752's
    /// `make_ord` supports every retain/delete/advance form. That is how
    /// ```` `` ```` becomes an opening quotation mark inside `$…$`.
    fn ligature(&self, _font: usize, _left: u32, _right: u32) -> Option<(u32, u8)> {
        None
    }
}

/// Package a horizontal list into a box of the given width.
///
/// `to` of `None` means natural width. Otherwise the difference is taken up by
/// the glue, recording the ratio so the DVI writer can place things exactly as
/// TeX would.
/// `tex.web` §660/§664: the badness of the box most recently packed, which is
/// what `\badness` reads. It is engine state in TeX (`last_badness`), set by
/// every `hpack` and `vpack`, and read immediately afterwards.
///
/// A cell rather than a parameter because packing happens far from the engine
/// struct — `hpack` is a free function and takes only what it measures.
/// Reading `\badness` as a plain settable integer that was always 0 is why
/// LaTeX's mark mechanism reported "Infinite shrinkage found in 'page'" on
/// every page of every document: it packs a box, checks `\badness > 0`, and
/// took the error branch every time.
pub static LAST_BADNESS: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

/// What the last `hpack`/`vpack` had to do to reach its target, in the raw
/// terms §660 and §675 judge: how far off the natural size was, how much glue
/// of the highest order was available to cover it, and whether that order was
/// finite.
///
/// Deliberately *not* a verdict. Whether this is an `Overfull` or a `Loose` or
/// nothing at all depends on `\hbadness`, `\hfuzz`, `\vbadness` and `\vfuzz`,
/// which live in the engine's parameter table and have no business here — the
/// same reason `LAST_BADNESS` is a cell rather than a return value. The caller
/// packs, then asks.
#[derive(Debug, Clone, Copy, Default)]
pub struct PackMeasure {
    /// Target minus natural. Positive means the glue had to stretch.
    pub diff: i32,
    /// Glue available at the highest order present, in that order's units.
    pub avail: i32,
    /// The highest order with any glue: 0 is finite, 1..3 are fil/fill/filll.
    pub order: u8,
    /// §660 and §675 both say nothing about an empty list, however badly it
    /// misses its target — `if list_ptr(r)<>null`.
    pub empty: bool,
    /// Which of §660 and §675 applies, and so whether the message says `\hbox`
    /// and "too wide" or `\vbox` and "too high".
    pub vertical: bool,
}

/// The measurement from the most recent pack, for the caller to judge.
///
/// One slot, not a queue: TeX reports a box as it packs it, and every caller
/// here reads this immediately after the call that filled it. A queue would
/// invite reporting a box against the wrong line.
///
/// Three atomics rather than one struct behind a lock, because this crate has
/// no dependencies (`tools/check_zero_deps.py` enforces it) and `spin` would be
/// one. The flags word carries `order` in its low two bits and one bit each for
/// `empty` and `vertical`.
static PACK_DIFF: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);
static PACK_AVAIL: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);
static PACK_FLAGS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Record what the pack had to do, for §660/§675 to judge afterwards.
fn note_pack(diff: Scaled, avail: Scaled, order: u8, empty: bool, vertical: bool) {
    use core::sync::atomic::Ordering::Relaxed;
    PACK_DIFF.store(diff.0, Relaxed);
    PACK_AVAIL.store(avail.0, Relaxed);
    PACK_FLAGS.store(
        (order as u32 & 3) | ((empty as u32) << 2) | ((vertical as u32) << 3),
        Relaxed,
    );
}

/// §660 and §675's verdict on a packed box, or `None` if it is unremarkable.
///
/// The four outcomes and their thresholds, from §660 (stretching), §664
/// (shrinking) and §675 (the vertical twins of both):
///
/// ```text
/// stretching, finite order:  b = badness(diff, avail)
///                            b > badness_param -> Underfull if b > 100 else Loose
/// shrinking within capacity: finite order: b = badness(-diff, avail)
///                                           b > badness_param -> Tight
/// shrinking past capacity:   over = -diff - avail
///                            over > fuzz or badness_param < 100 -> Overfull by `over`
///                            otherwise -> no report
/// ```
///
/// An empty list is never reported however badly it misses — `if
/// list_ptr(r)<>null` guards both — and neither is a box whose highest glue
/// order is infinite, since infinite glue absorbs anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackVerdict {
    /// Shrank further than its glue allowed, by this many scaled points.
    Overfull(i32),
    /// Stretched to badness above the threshold, and above 100.
    Underfull(i32),
    /// Stretched to badness above the threshold, but at most 100.
    Loose(i32),
    /// Shrank to badness above the threshold.
    Tight(i32),
}

impl PackMeasure {
    pub fn verdict(self, badness_param: i32, fuzz: i32) -> Option<PackVerdict> {
        if self.empty {
            return None;
        }
        if self.diff >= 0 {
            if self.order != 0 {
                return None;
            }
            let b = crate::page::PageBuilder::badness(Scaled(self.diff), Scaled(self.avail));
            if b > badness_param {
                return Some(if b > 100 {
                    PackVerdict::Underfull(b)
                } else {
                    PackVerdict::Loose(b)
                });
            }
            return None;
        }
        // §664: an *infinite* shrink absorbs anything, so `\hss` is never
        // overfull — which is what `\llap` is built on.
        if self.order != 0 {
            return None;
        }
        let over = -self.diff - self.avail;
        if over > 0 {
            return if over > fuzz || badness_param < 100 {
                Some(PackVerdict::Overfull(over))
            } else {
                None
            };
        }
        let b = crate::page::PageBuilder::badness(Scaled(-self.diff), Scaled(self.avail));
        if b > badness_param {
            return Some(PackVerdict::Tight(b));
        }
        None
    }
}

/// What the last pack had to do. See [`PackMeasure`].
pub fn last_pack() -> PackMeasure {
    use core::sync::atomic::Ordering::Relaxed;
    let f = PACK_FLAGS.load(Relaxed);
    PackMeasure {
        diff: PACK_DIFF.load(Relaxed),
        avail: PACK_AVAIL.load(Relaxed),
        order: (f & 3) as u8,
        empty: f & 4 != 0,
        vertical: f & 8 != 0,
    }
}

/// Restore the observable state produced by a previous packing operation.
///
/// Reusing a byte-for-byte equivalent packed line is only transparent if the
/// following `\badness` query and the caller's pack verdict see the same state
/// as they would after `hpack`.  The repeated-paragraph layout cache uses this
/// immediately before contributing each cached line.
pub fn restore_last_pack(measure: PackMeasure, badness: i32) {
    use core::sync::atomic::Ordering::Relaxed;
    PACK_DIFF.store(measure.diff, Relaxed);
    PACK_AVAIL.store(measure.avail, Relaxed);
    PACK_FLAGS.store(
        (measure.order as u32 & 3)
            | ((measure.empty as u32) << 2)
            | ((measure.vertical as u32) << 3),
        Relaxed,
    );
    LAST_BADNESS.store(badness, Relaxed);
}

/// The badness recorded by the most recent horizontal or vertical pack.
pub fn last_badness() -> i32 {
    LAST_BADNESS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Badness of a list that must change by `diff` when `avail` is available at
/// the relevant order — `tex.web` §108's `badness(t,s)`, via the page builder's
/// implementation so the two cannot drift.
fn note_badness(diff: Scaled, avail: Scaled, order: u8, shrinking: bool) {
    let b = if diff.0 == 0 {
        0
    } else if order > 0 {
        // Infinite glue absorbs anything: badness zero.
        0
    } else if shrinking && diff.0.unsigned_abs() > avail.0.unsigned_abs() {
        // §664: shrinking past what is available is "overfull", reported as the
        // maximum badness rather than a number.
        1_000_000
    } else {
        crate::page::PageBuilder::badness(Scaled(diff.0.abs()), avail)
    };
    LAST_BADNESS.store(b, core::sync::atomic::Ordering::Relaxed);
}

pub fn hpack(list: Vec<Node>, to: Option<Scaled>, metrics: &impl CharMetrics) -> TeXBox {
    let _p = crate::profile::Phase::enter(crate::profile::P_PACK);
    let t = measure_horizontal_basic(&list, metrics);
    hpack_measured(list, to, t)
}

fn hpack_measured(list: Vec<Node>, to: Option<Scaled>, t: Totals) -> TeXBox {
    let width = to.unwrap_or(t.natural);
    let diff = width - t.natural;
    if diff.0 >= 0 {
        let o = highest_order(&t.stretch);
        note_badness(diff, t.stretch[o as usize], o, false);
        note_pack(diff, t.stretch[o as usize], o, list.is_empty(), false);
    } else {
        let o = highest_order(&t.shrink);
        note_badness(diff, t.shrink[o as usize], o, true);
        note_pack(diff, t.shrink[o as usize], o, list.is_empty(), false);
    }

    let (sign, order, num, den) = if diff.0 > 0 {
        // Stretch at the highest order that has any, as TeX does: finite glue
        // is ignored entirely once infinite glue is present.
        let order = highest_order(&t.stretch);
        let avail = t.stretch[order as usize];
        if avail.0 == 0 {
            (GlueSign::Normal, 0, 0, 1)
        } else {
            (GlueSign::Stretching, order, diff.0 as i64, avail.0 as i64)
        }
    } else if diff.0 < 0 {
        let order = highest_order(&t.shrink);
        let avail = t.shrink[order as usize];
        if avail.0 == 0 {
            (GlueSign::Normal, 0, 0, 1)
        } else {
            // TeX caps the shrink ratio at 1 — but only for *finite* shrink
            // (`tex.web` §664's `and (o = normal)`). Infinite shrink is exactly
            // what `\hss` is for: `\llap{X}` is an `\hbox to 0pt` whose `\hss`
            // must shrink by the whole width of `X`, which is far more than the
            // 1 unit of `fil` it nominally offers. Capping it there moved every
            // `\llap` and `\rlap` — plain's `\textindent`, so every footnote
            // marker — to within a point of the reference point.
            (
                GlueSign::Shrinking,
                order,
                shrink_ratio(-diff.0, avail.0, order),
                avail.0 as i64,
            )
        }
    } else {
        (GlueSign::Normal, 0, 0, 1)
    };

    TeXBox {
        kind: BoxKind::Horizontal,
        width,
        height: t.height,
        depth: t.depth,
        set_num: num,
        set_den: den,
        glue_sign: sign,
        glue_order: order,
        shift: Scaled::ZERO,
        list,
    }
}

/// `hpack` that can leave `tex.web` §663's overfull rule behind.
///
/// A box that has to shrink further than its glue allows is *overfull*, and TeX
/// marks one on the page: it appends a rule `\overfullrule` wide at the very end
/// of the list — the black bar in the margin of a line that would not fit.
/// plain.tex sets `\overfullrule=5pt`, so the bar appears in any plain document
/// that overflows a line; LaTeX sets it to zero, which is why LaTeX documents
/// show none.
///
/// The rule is appended *after* packing and the box's dimensions are not
/// recomputed — §663 appends to the list and falls straight through to
/// `common_ending` — so it sticks out past the box's declared width. That is the
/// whole point of it.
///
/// Three conditions, from §664 and §663: the shrink must be *finite* (an `\hss`
/// shrinks without limit and is never overfull, which is what `\llap` is built
/// on), the list must be non-empty, and the shortfall must exceed `\hfuzz`.
pub fn hpack_over(
    list: Vec<Node>,
    to: Option<Scaled>,
    metrics: &impl CharMetrics,
    hfuzz: Scaled,
    overfull_rule: Scaled,
) -> TeXBox {
    let measured = match to {
        Some(w) if overfull_rule.0 > 0 && !list.is_empty() => {
            let t = measure_horizontal_basic(&list, metrics);
            let diff = w - t.natural;
            let overfull = diff.0 < 0
                && highest_order(&t.shrink) == 0
                && t.shrink[0].0 < -diff.0
                && (-diff.0 as i64 - t.shrink[0].0 as i64) > hfuzz.0 as i64;
            Some((t, overfull))
        }
        _ => None,
    };
    let (mut b, overfull) = match measured {
        Some((t, overfull)) => {
            let _p = crate::profile::Phase::enter(crate::profile::P_PACK);
            (hpack_measured(list, to, t), overfull)
        }
        None => (hpack(list, to, metrics), false),
    };
    if overfull {
        b.list.push(Node::Rule {
            width: overfull_rule,
            height: Scaled::RUNNING,
            depth: Scaled::RUNNING,
        });
    }
    b
}

fn highest_order(amounts: &[Scaled; 4]) -> u8 {
    for o in (1..4).rev() {
        if amounts[o].0 != 0 {
            return o as u8;
        }
    }
    0
}

/// Stack boxes vertically. Baseline skip is not applied at E1; that arrives
/// with the page builder.
pub fn vpack(list: Vec<Node>, to: Option<Scaled>, max_depth: Scaled) -> TeXBox {
    let _p = crate::profile::Phase::enter(crate::profile::P_PACK);
    let m = measure_vertical(&list, max_depth);
    let (height, width, depth, stretch, shrink) = (m.height, m.width, m.depth, m.stretch, m.shrink);
    vpack_measured(list, to, height, width, depth, stretch, shrink)
}

/// What a vertical list measures, without consuming it.
///
/// Split out of [`vpack`] because the caller often needs the natural height
/// *before* deciding what to pack to — and the only way to ask used to be to
/// `vpack` a **clone** of the list and read the answer off the box. That deep
/// copy, and dropping it again, was 8% of a typesetting run: every `\vbox` was
/// packed twice.
#[derive(Debug, Clone, Copy, Default)]
pub struct VTotals {
    pub height: Scaled,
    pub width: Scaled,
    pub depth: Scaled,
    pub stretch: [Scaled; 4],
    pub shrink: [Scaled; 4],
}

pub fn measure_vertical(list: &[Node], max_depth: Scaled) -> VTotals {
    let mut height = Scaled::ZERO;
    let mut width = Scaled::ZERO;
    let mut depth = Scaled::ZERO;
    let mut stretch = [Scaled::ZERO; 4];
    let mut shrink = [Scaled::ZERO; 4];
    // `tex.web` §668, verbatim in its bookkeeping: `depth` is the depth *so far*,
    // carried forward from the last box or rule and **reset to zero by any glue or
    // kern that follows it**. Only a box or rule at the very end of the list gives
    // the vbox a depth; a trailing `\vskip` folds that depth back into the height.
    // Accumulating `height + depth` per box and subtracting the last depth at the
    // end is the same thing *only* when nothing follows the last box — and LaTeX's
    // `\@makecol` ends its page box with `\@textbottom`'s `\vskip`, so ours came
    // out 2.85pt shorter than it should and the glue above stretched to make up
    // the difference, carrying the footnote down with it.
    for n in list {
        match n {
            Node::Box(b) => {
                height = height + depth + b.height;
                depth = b.depth;
                // §668: a box shifted right by `\moveright` widens the list it is
                // in by the shift as well as by its own width.
                width = width.max(b.width + b.shift);
            }
            // A rule stacks exactly like a box: §668 handles the two in the same
            // branch. Skipping it makes `\vbox{\hrule height2pt}` measure zero.
            Node::Rule {
                width: w,
                height: h,
                depth: d,
            } => {
                height = height + depth + *h;
                depth = *d;
                if *w != Scaled::RUNNING {
                    width = width.max(*w);
                }
            }
            Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                height = height + depth + g.width;
                depth = Scaled::ZERO;
                stretch[g.stretch_order as usize] = stretch[g.stretch_order as usize] + g.stretch;
                shrink[g.shrink_order as usize] = shrink[g.shrink_order as usize] + g.shrink;
                // §668: a glue node that is a *leader* also widens the box —
                //
                //     if subtype(p)>=a_leaders then
                //       begin g:=leader_ptr(p);
                //       if width(g)>w then w:=width(g); end
                //
                // The leader box is repeated across the glue, so the box has to
                // be wide enough to hold one of them. Ordinary glue contributes
                // no width, which is why this hangs off the same arm rather
                // than being folded into it.
                if let Node::Leaders { boxed, .. } = n {
                    width = width.max(boxed.width);
                }
            }
            Node::Kern(k, _) => {
                height = height + depth + *k;
                depth = Scaled::ZERO;
            }
            _ => {}
        }
    }
    // ...but only up to `\boxmaxdepth`; the excess is folded back into the
    // height (`tex.web` 668). INITEX sets it to zero, so a plain `\vbox` has
    // no depth at all and every descender counts as height.
    if depth > max_depth {
        height = height + (depth - max_depth);
        depth = max_depth;
    }
    VTotals {
        height,
        width,
        depth,
        stretch,
        shrink,
    }
}

#[allow(clippy::too_many_arguments)]
fn vpack_measured(
    list: Vec<Node>,
    to: Option<Scaled>,
    height: Scaled,
    width: Scaled,
    depth: Scaled,
    stretch: [Scaled; 4],
    shrink: [Scaled; 4],
) -> TeXBox {
    // A vertical box is *set* exactly as a horizontal one is (`tex.web` §668
    // and §658 are the same code with the axes swapped): the difference between
    // the natural height and the target is taken up by the glue, and the ratio
    // is recorded so the shipper can place things where the packing decided.
    // Leaving it unset made every `\vbox to` put its slack at the bottom — so a
    // footnote, which plain.tex places after the page's `\vfill`, sat directly
    // under the text instead of at the foot of the page.
    let target = to.unwrap_or(height);
    let diff = target - height;
    if diff.0 >= 0 {
        let o = highest_order(&stretch);
        note_badness(diff, stretch[o as usize], o, false);
        note_pack(diff, stretch[o as usize], o, list.is_empty(), true);
    } else {
        let o = highest_order(&shrink);
        note_badness(diff, shrink[o as usize], o, true);
        note_pack(diff, shrink[o as usize], o, list.is_empty(), true);
    }
    let (sign, order, num, den) = if diff.0 > 0 {
        let order = highest_order(&stretch);
        let avail = stretch[order as usize];
        if avail.0 == 0 {
            (GlueSign::Normal, 0, 0, 1)
        } else {
            (GlueSign::Stretching, order, diff.0 as i64, avail.0 as i64)
        }
    } else if diff.0 < 0 {
        let order = highest_order(&shrink);
        let avail = shrink[order as usize];
        if avail.0 == 0 {
            (GlueSign::Normal, 0, 0, 1)
        } else {
            (
                GlueSign::Shrinking,
                order,
                shrink_ratio(-diff.0, avail.0, order),
                avail.0 as i64,
            )
        }
    } else {
        (GlueSign::Normal, 0, 0, 1)
    };
    TeXBox {
        kind: BoxKind::Vertical,
        width,
        height: target,
        depth,
        set_num: num,
        set_den: den,
        glue_sign: sign,
        glue_order: order,
        shift: Scaled::ZERO,
        list,
    }
}

/// The width glue actually takes in a set box.
///
/// Rounds half away from zero using integer arithmetic only, so the result is
/// identical on every platform — a prerequisite for byte-exact DVI comparison.
/// TeX's *cumulative* glue setting (`tex.web` §625).
///
/// TeX does not round each glue independently. It keeps a running total of the
/// The numerator of a shrink ratio. Finite shrink stops at the amount actually
/// available (the box is then overfull); infinite shrink does not.
fn shrink_ratio(want: i32, avail: i32, order: u8) -> i64 {
    let want = want as i64;
    if order == 0 {
        want.min(avail as i64)
    } else {
        want
    }
}

/// stretch (or shrink) seen so far in the box, rounds `glue_set × that total`,
/// and gives each glue the *difference* from the previous rounded total. The
/// effect is that rounding error never accumulates: the last glue absorbs it, so
/// the set widths sum to exactly the box's width.
///
/// Rounding each glue on its own — which is what [`set_glue_width`] does, and
/// what this engine did — gives every identical glue an identical width, and the
/// sum drifts from the box width by up to one scaled point per glue. Against
/// pdfTeX the divergence shows up as a single space one sp wider than ours and
/// every glyph after it shifted, which is invisible on paper and fatal to a
/// byte-comparison.
///
/// One setter per box: TeX saves and restores the accumulator around each
/// recursion into a sub-box.
#[derive(Debug, Default, Clone, Copy)]
pub struct GlueSetter {
    /// Running total of stretch (or shrink) units at this box's order.
    cur_glue: i64,
    /// `round(glue_set × cur_glue)` — the last rounded total.
    cur_g: i64,
}

impl GlueSetter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The width this glue takes in a box packed with `sign`/`order`/`num`/`den`.
    pub fn width(&mut self, g: &Glue, sign: GlueSign, order: u8, num: i64, den: i64) -> Scaled {
        let before = self.cur_g;
        let relevant = match sign {
            GlueSign::Stretching => g.stretch_order == order,
            GlueSign::Shrinking => g.shrink_order == order,
            GlueSign::Normal => false,
        };
        if relevant && den != 0 {
            self.cur_glue += match sign {
                GlueSign::Shrinking => -(g.shrink.0 as i64),
                _ => g.stretch.0 as i64,
            };
            // `tex.web` §625 computes `round(glue_set × cur_glue)` where
            // `glue_set` is a *real*. Doing it as the exact rational `num ×
            // cur_glue / den` is more accurate — and therefore wrong: when the
            // product lands exactly on .5 the two round opposite ways, and the
            // glyphs after that glue are all one scaled point out from pdfTeX's.
            // The box keeps the exact rational (it is what gets serialized);
            // only the DVI-writing arithmetic follows TeX into floating point,
            // which is the only place TeX's float is observable.
            let gs = num as f64 / den as f64;
            let mut t = gs * self.cur_glue as f64;
            // `vet_glue`: TeX clamps the product to exactly ±1,000,000,000
            // before rounding. This is deliberately below `max_dimen`; TRIP's
            // max-height page makes the distinction visible in its DVI.
            const LIMIT: f64 = 1_000_000_000.0;
            t = t.clamp(-LIMIT, LIMIT);
            // Round half away from zero, as Pascal's `round` does. `as i64`
            // truncates toward zero, so the ±0.5 does the work — and `f64::round`
            // is not available in `core`.
            self.cur_g = if t >= 0.0 {
                (t + 0.5) as i64
            } else {
                (t - 0.5) as i64
            };
        }
        Scaled(g.width.0 + (self.cur_g - before) as i32)
    }
}

/// The width one glue takes, rounded on its own.
///
/// Correct for a box with a single glue, and for measuring; for *writing a box
/// out* use [`GlueSetter`], which is what TeX does.
pub fn set_glue_width(g: &Glue, sign: GlueSign, order: u8, num: i64, den: i64) -> Scaled {
    fn scale(amount: Scaled, num: i64, den: i64) -> i32 {
        if den == 0 {
            return 0;
        }
        let n = amount.0 as i64 * num;
        let half = den / 2;
        let rounded = if n >= 0 {
            (n + half) / den
        } else {
            (n - half) / den
        };
        rounded as i32
    }
    match sign {
        GlueSign::Normal => g.width,
        GlueSign::Stretching if g.stretch_order == order => {
            Scaled(g.width.0 + scale(g.stretch, num, den))
        }
        GlueSign::Shrinking if g.shrink_order == order => {
            Scaled(g.width.0 - scale(g.shrink, num, den))
        }
        _ => g.width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// `tex.web` §625: glue widths come from a running total, not from rounding
    /// each glue on its own. With a glue set ratio of exactly 1/2 and one unit of
    /// stretch per glue, the rounded running total goes 1, 1, 2, 2 — so the glues
    /// take 1, 0, 1, 0 extra scaled points and the four together take exactly the
    /// 2sp the box was stretched by. Rounding each glue independently gives
    /// 1, 1, 1, 1 and overshoots by 2sp, which is how a line ends up a couple of
    /// scaled points wide and every glyph after the first space sits off pdfTeX's.
    #[test]
    fn glue_is_set_from_a_running_total() {
        let g = Glue {
            width: Scaled(100),
            stretch: Scaled(1),
            ..Default::default()
        };
        let mut setter = GlueSetter::new();
        let widths: Vec<i32> = (0..4)
            .map(|_| setter.width(&g, GlueSign::Stretching, 0, 1, 2).0)
            .collect();
        assert_eq!(widths, vec![101, 100, 101, 100]);
        assert_eq!(
            widths.iter().sum::<i32>(),
            402,
            "sums to width + total stretch"
        );
        // Independent rounding is what this replaced, and it disagrees.
        assert_eq!(set_glue_width(&g, GlueSign::Stretching, 0, 1, 2).0, 101);
    }

    /// Shrinking runs the same accumulator with a negative total.
    #[test]
    fn shrinking_uses_the_same_running_total() {
        let g = Glue {
            width: Scaled(100),
            shrink: Scaled(1),
            ..Default::default()
        };
        let mut setter = GlueSetter::new();
        let widths: Vec<i32> = (0..4)
            .map(|_| setter.width(&g, GlueSign::Shrinking, 0, 1, 2).0)
            .collect();
        assert_eq!(widths, vec![99, 100, 99, 100]);
    }

    #[test]
    fn glue_movement_is_vetted_at_texs_billion_sp_limit() {
        let g = Glue {
            stretch: Scaled(600_000_000),
            ..Default::default()
        };
        let mut setter = GlueSetter::new();
        assert_eq!(
            setter.width(&g, GlueSign::Stretching, 0, 2, 1),
            Scaled(1_000_000_000)
        );
    }

    /// Every character 10pt wide, 7pt tall, 2pt deep.
    struct Fixed;
    impl CharMetrics for Fixed {
        fn width(&self, _: usize, _: u32) -> Scaled {
            Scaled::from_points(10)
        }
        fn height(&self, _: usize, _: u32) -> Scaled {
            Scaled::from_points(7)
        }
        fn depth(&self, _: usize, _: u32) -> Scaled {
            Scaled::from_points(2)
        }
    }

    fn chars(n: usize) -> Vec<Node> {
        (0..n)
            .map(|i| Node::Char {
                code: b'a' as u32 + i as u32,
                font: 0,
                lig: None,
            })
            .collect()
    }

    #[test]
    fn natural_width_is_the_sum_of_the_parts() {
        let b = hpack(chars(3), None, &Fixed);
        assert_eq!(b.width, Scaled::from_points(30));
        assert_eq!(b.height, Scaled::from_points(7));
        assert_eq!(b.depth, Scaled::from_points(2));
        assert_eq!(b.glue_sign, GlueSign::Normal);
    }

    #[test]
    fn glue_stretches_to_fill_a_wider_box() {
        let list = vec![
            Node::Char {
                code: b'a' as u32,
                font: 0,
                lig: None,
            },
            Node::Glue(Glue::interword(
                Scaled::from_points(5),
                Scaled::from_points(5),
                Scaled::from_points(1),
            )),
            Node::Char {
                code: b'b' as u32,
                font: 0,
                lig: None,
            },
        ];
        // natural 25pt; ask for 30pt, so the 5pt of stretch is fully used.
        let b = hpack(list, Some(Scaled::from_points(30)), &Fixed);
        assert_eq!(b.glue_sign, GlueSign::Stretching);
        assert_eq!(b.set_num, b.set_den, "ratio is exactly 1");

        let g = Glue::interword(
            Scaled::from_points(5),
            Scaled::from_points(5),
            Scaled::from_points(1),
        );
        assert_eq!(
            set_glue_width(&g, b.glue_sign, b.glue_order, b.set_num, b.set_den),
            Scaled::from_points(10)
        );
    }

    #[test]
    fn infinite_glue_absorbs_everything_and_finite_glue_is_ignored() {
        // The rule that makes \hfil centre things: once any infinite stretch is
        // present, finite stretch takes none of the slack.
        let list = vec![
            Node::Glue(Glue::fixed(Scaled::from_points(1))),
            Node::Char {
                code: b'x' as u32,
                font: 0,
                lig: None,
            },
            Node::Glue(Glue::fil()),
        ];
        let b = hpack(list, Some(Scaled::from_points(100)), &Fixed);
        assert_eq!(b.glue_sign, GlueSign::Stretching);
        assert_eq!(b.glue_order, 1, "must stretch at the fil order");

        let finite = Glue::fixed(Scaled::from_points(1));
        assert_eq!(
            set_glue_width(&finite, b.glue_sign, b.glue_order, b.set_num, b.set_den),
            Scaled::from_points(1),
            "finite glue keeps its natural width when fil is present"
        );
    }

    #[test]
    fn shrinking_never_exceeds_the_available_shrink() {
        // An overfull box is overfull; TeX does not shrink glue past its limit.
        let list = vec![
            Node::Char {
                code: b'a' as u32,
                font: 0,
                lig: None,
            },
            Node::Glue(Glue::interword(
                Scaled::from_points(5),
                Scaled::ZERO,
                Scaled::from_points(2),
            )),
            Node::Char {
                code: b'b' as u32,
                font: 0,
                lig: None,
            },
        ];
        // natural 25pt, asked for 10pt: 15pt too much, only 2pt of shrink.
        let b = hpack(list, Some(Scaled::from_points(10)), &Fixed);
        assert_eq!(b.glue_sign, GlueSign::Shrinking);
        assert!(b.set_num <= b.set_den, "shrink ratio must be capped at 1");
    }

    /// `tex.web` §664 caps the shrink ratio at 1 only when the shrink is finite
    /// (`and (o = normal)`). `\hss` exists to shrink without limit: `\llap{X}`
    /// is an `\hbox to 0pt` whose `\hss` must absorb the whole width of `X`.
    #[test]
    fn infinite_shrink_is_not_capped() {
        let list = vec![
            Node::Glue(Glue {
                width: Scaled::ZERO,
                stretch: Scaled::from_points(1),
                stretch_order: 1,
                shrink: Scaled::from_points(1),
                shrink_order: 1,
                subtype: 0,
                canonical_zero: false,
            }),
            Node::Char {
                code: b'a' as u32,
                font: 0,
                lig: None,
            },
        ];
        // natural 10pt (the `Fixed` metric), asked for 0pt: the fil glue takes
        // all of it, a ratio of 10 — not the 1 a finite shrink would stop at.
        let b = hpack(list, Some(Scaled::ZERO), &Fixed);
        assert_eq!(b.glue_sign, GlueSign::Shrinking);
        assert_eq!(b.glue_order, 1);
        assert!(
            b.set_num > b.set_den,
            "infinite shrink must be allowed past ratio 1, got {}/{}",
            b.set_num,
            b.set_den
        );
        assert_eq!(b.width, Scaled::ZERO);
    }

    #[test]
    fn a_box_with_no_glue_cannot_be_stretched() {
        let b = hpack(chars(2), Some(Scaled::from_points(100)), &Fixed);
        assert_eq!(b.width, Scaled::from_points(100), "width is still honoured");
        assert_eq!(b.glue_sign, GlueSign::Normal, "but nothing stretched");
    }

    #[test]
    fn kerns_count_toward_width_but_do_not_stretch() {
        let list = vec![
            Node::Char {
                code: b'a' as u32,
                font: 0,
                lig: None,
            },
            Node::Kern(Scaled::from_points(3), kern_sub::NORMAL),
        ];
        assert_eq!(hpack(list, None, &Fixed).width, Scaled::from_points(13));
    }

    #[test]
    fn penalties_and_whatsits_occupy_no_space() {
        let list = vec![
            Node::Penalty(-10000),
            Node::Char {
                code: b'a' as u32,
                font: 0,
                lig: None,
            },
            Node::Whatsit("ps: something".into()),
        ];
        assert_eq!(hpack(list, None, &Fixed).width, Scaled::from_points(10));
    }

    #[test]
    fn vpack_sums_heights_and_keeps_the_last_depth() {
        let line = hpack(chars(1), None, &Fixed); // 7pt high, 2pt deep
        let v = vpack(
            vec![
                Node::Box(alloc::boxed::Box::new(line.clone())),
                Node::Box(alloc::boxed::Box::new(line)),
            ],
            None,
            Scaled::MAX,
        );
        // two lines of 9pt total, less the final depth which becomes the box's
        assert_eq!(v.height, Scaled::from_points(16));
        assert_eq!(v.depth, Scaled::from_points(2));
    }

    #[test]
    fn an_empty_box_is_well_defined() {
        let b = hpack(vec![], None, &Fixed);
        assert_eq!(b.width, Scaled::ZERO);
        assert_eq!(b.height, Scaled::ZERO);
        assert!(b.list.is_empty());
    }
}
