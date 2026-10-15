//! Math lists and `mlist_to_hlist` — `tex.web` §680–§767.
//!
//! Math is not typeset directly. A formula is first built as a list of
//! *noads*, each carrying what kind of thing it is — an ordinary symbol, a
//! binary operator, a relation — and only then converted to a horizontal list.
//! The conversion is where the spacing happens, and it is the whole reason for
//! the two-stage design: how much space goes between two atoms depends on both
//! of their classes, so no amount of local knowledge while reading `a+b` can
//! decide it. The `+` only becomes a binary operator once we know something
//! precedes it.
//!
//! That last point is worth stating plainly, because it is the subtlety the
//! spacing table alone does not capture: `$-x$` and `$a-x$` set differently.
//! In the first the `-` is a *unary* sign and gets no space; in the second it
//! is binary and gets a medium space on each side. TeX resolves this by
//! rewriting a `Bin` to an `Ord` when it appears where a binary operator
//! cannot (`tex.web` §727), which is done here before any spacing is applied.

use crate::node::{CharMetrics, Glue, Node};
use crate::scaled::Scaled;

/// `tex.web` §157: a penalty this large forbids a break outright.
const INF_PENALTY: i32 = 10_000;
use alloc::vec::Vec;

/// What kind of atom this is. The order matters: it indexes the spacing table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Class {
    /// A letter or ordinary symbol: `x`, `\alpha`.
    #[default]
    Ord = 0,
    /// A large operator: `\sum`, `\int`.
    Op = 1,
    /// A binary operator: `+`, `\times`.
    Bin = 2,
    /// A relation: `=`, `\leq`.
    Rel = 3,
    /// An opening delimiter: `(`, `[`.
    Open = 4,
    /// A closing delimiter: `)`, `]`.
    Close = 5,
    /// Punctuation: `,`, `;`.
    Punct = 6,
    /// A sub-formula treated as a unit — what `{...}` produces.
    Inner = 7,
}

impl Class {
    /// The class encoded in the top nibble of a mathcode.
    ///
    /// Class 7 is not a class at all: it means "variable family" -- take the
    /// family from `\fam` if that is a real family, and otherwise from the
    /// code. The atom itself is ordinary. Reading 7 as `Inner` spaces every
    /// letter and digit in a formula as though it were a sub-formula.
    pub fn from_mathcode(code: i32) -> Class {
        match (code / 0x1000) & 0x7 {
            1 => Class::Op,
            2 => Class::Bin,
            3 => Class::Rel,
            4 => Class::Open,
            5 => Class::Close,
            6 => Class::Punct,
            _ => Class::Ord,
        }
    }

    /// Does this mathcode take its family from `\fam`?
    pub fn is_variable_family(code: i32) -> bool {
        (code / 0x1000) & 0x7 == 7
    }
}

/// One atom of a math list.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Noad {
    pub class: Class,
    /// The atom's own material.
    pub nucleus: Field,
    pub subscript: Field,
    pub superscript: Field,
    /// What kind of atom this is beyond its class — an ordinary symbol, or one
    /// of the constructions that draw a rule of their own.
    pub kind: Kind,
    /// A fraction's denominator; `Empty` for everything else, where the
    /// numerator lives in `nucleus`.
    pub denominator: Field,
    /// `\mathchoice`'s four lists, in display, text, script and scriptscript
    /// order. Only the one matching the style in force is set.
    pub choices: Option<alloc::boxed::Box<[Vec<Noad>; 4]>>,
    /// Whether a large operator's scripts go above and below it — `\limits`,
    /// `\nolimits`, or the default, which is limits in display style only.
    pub limits: Limits,
    /// §752's `math_text_char`: suppress the italic correction when this
    /// character comes from a font with interword space. Usually inferred from
    /// the next noad; some ligature operators leave it set after changing or
    /// deleting that neighbour.
    #[serde(default)]
    pub math_text_char: bool,
}

/// `tex.web` §682's `op_noad` subtypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Limits {
    /// `\displaylimits`: limits in display style, beside it otherwise.
    #[default]
    Display,
    Limits,
    NoLimits,
}

/// Constructions that are not plain atoms — `tex.web` §683's noad types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    #[default]
    Normal,
    /// `\overline{…}` — a rule above the nucleus (`tex.web` §734).
    Over,
    /// `\underline{…}` — a rule below it (§735).
    Under,
    /// `\over`, `\atop`, `\above` — §743–§748. `None` means the default rule
    /// thickness (`\over`); `Some(0)` is `\atop`; anything else is `\above`.
    /// The `…withdelims` forms carry the delimiters that go either side.
    Fraction(Option<i32>, Delim, Delim),
    /// `\vcenter` — a vertical box centred on the axis (§734's `make_vcenter`).
    VCenter,
    /// `\displaystyle` and its three friends — §1171. Not an atom: it switches
    /// the style for the rest of the list it is in and sets nothing.
    Style(Style),
    /// `\radical` — `\sqrt` is `\radical"270370`. The delimiter is the sign,
    /// grown to reach over the nucleus (§737).
    Radical(Delim),
    /// `\mathaccent<15-bit><field>` — the accent's family and character.
    Accent(usize, u32),
    /// `\mathchoice` — four lists, one per style (§1174). Whichever matches
    /// the style in force is spliced in; the rest are dropped.
    Choice,
    /// Glue, a kern or a penalty *inside* a formula — `tex.web` §729 passes
    /// these straight through, and they are transparent for spacing: the atoms
    /// either side of a `\qquad` are still neighbours.
    Raw,
    /// `\left<delim>` and `\right<delim>` — §762's left and right noads. They
    /// are sized from the height and depth of *everything between them*, so
    /// they cannot be built until the rest of the list has been.
    LeftDelim(Delim),
    RightDelim(Delim),
    /// e-TeX's `\middle`. §1191 makes it a `right_noad` with subtype
    /// `middle_noad`, so it is sized by §762 exactly as the pair around it is.
    MiddleDelim(Delim),
}

/// What sits in one of a noad's three positions.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum Field {
    #[default]
    Empty,
    /// A character in a math family.
    Char { family: usize, code: u32 },
    /// Already-typeset material — a `{...}` group, or a box.
    List(Vec<Node>),
    /// A math list not yet converted. A fraction's numerator and denominator
    /// are set in styles that are not known until the fraction is built, so
    /// they have to be kept as noads until then.
    Sub(Vec<Noad>),
}

impl Field {
    pub fn is_empty(&self) -> bool {
        matches!(self, Field::Empty)
    }

    /// `tex.web` §1186: what a closing `}` in math mode leaves behind.
    ///
    /// A group holding exactly one ordinary atom with no scripts and nothing
    /// else about it *is* that atom's nucleus — TeX copies the nucleus field
    /// out and frees the noad. `\mathbin{B}` is therefore a Bin atom whose
    /// nucleus is the character `B`, not one wrapped around a sub-list.
    ///
    /// It is not cosmetic. A sub-list is converted by §720 with `hpack`, so the
    /// collapse decides whether an `\hbox` appears in the middle of the formula
    /// and whether the character's italic correction ends up inside it or
    /// beside it. Both are visible in any `\showbox` of a formula.
    pub fn group(list: Vec<Noad>) -> Field {
        let single = list.len() == 1 && {
            let n = &list[0];
            n.class == Class::Ord
                && n.subscript.is_empty()
                && n.superscript.is_empty()
                && n.kind == Kind::Normal
                && n.denominator.is_empty()
                && n.choices.is_none()
                && n.limits == Limits::Display
        };
        if single {
            let mut list = list;
            core::mem::take(&mut list[0].nucleus)
        } else {
            Field::Sub(list)
        }
    }
}

/// How much space goes between two adjacent classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Spacing {
    None,
    Thin,
    Medium,
    Thick,
    /// Space in text and display styles, but none in script styles — which is
    /// what keeps subscripts from being pulled apart.
    ThinConditional,
    MediumConditional,
    ThickConditional,
}

/// `tex.web` §764's table, transcribed.
///
/// Rows are the left atom's class, columns the right's. The asymmetries are
/// deliberate and load-bearing: a relation gets thick space on both sides, a
/// binary operator medium, and punctuation gets space *after* it but not
/// before — which is why `f(x), g(y)` sets correctly and `f(x) ,g(y)` does not.
/// Two adjacent binary operators is impossible after `\Bin` fixup, so those
/// entries never fire.
const SPACING: [[Spacing; 8]; 8] = {
    use Spacing::*;
    [
        //        Ord    Op     Bin     Rel     Open   Close  Punct  Inner
        /* Ord */
        [
            None,
            Thin,
            MediumConditional,
            ThickConditional,
            None,
            None,
            None,
            ThinConditional,
        ],
        /* Op  */
        [
            Thin,
            Thin,
            None,
            ThickConditional,
            None,
            None,
            None,
            ThinConditional,
        ],
        /* Bin */
        [
            MediumConditional,
            MediumConditional,
            None,
            None,
            MediumConditional,
            None,
            None,
            MediumConditional,
        ],
        /* Rel */
        [
            ThickConditional,
            ThickConditional,
            None,
            None,
            ThickConditional,
            None,
            None,
            ThickConditional,
        ],
        /* Opn */ [None, None, None, None, None, None, None, None],
        /* Cls */
        [
            None,
            Thin,
            MediumConditional,
            ThickConditional,
            None,
            None,
            None,
            ThinConditional,
        ],
        /* Pun */
        [
            ThinConditional,
            ThinConditional,
            None,
            ThinConditional,
            ThinConditional,
            ThinConditional,
            ThinConditional,
            ThinConditional,
        ],
        /* Inr */
        [
            ThinConditional,
            Thin,
            MediumConditional,
            ThickConditional,
            ThinConditional,
            None,
            ThinConditional,
            ThinConditional,
        ],
    ]
};

pub fn spacing(left: Class, right: Class) -> Spacing {
    SPACING[left as usize][right as usize]
}

/// The style a formula is being set in. Only the distinction that affects
/// spacing and font size is modelled here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Style {
    Display,
    Text,
    Script,
    ScriptScript,
    // `tex.web` §688 numbers eight styles, not four: each has a *cramped* form,
    // set in the parts of a formula that sit under something — a denominator, a
    // square root, an `\overline`. The only difference is that a superscript is
    // raised less (§758's `sup3`), which is exactly what keeps it from running
    // into the bar above it.
    DisplayCramped,
    TextCramped,
    ScriptCramped,
    ScriptScriptCramped,
}

impl Style {
    /// Script styles suppress the conditional spaces.
    pub fn is_script(self) -> bool {
        self.size() > 0
    }

    /// Display style, cramped or not — what `tex.web` writes `cur_style <
    /// text_style`.
    pub fn is_display(self) -> bool {
        matches!(self, Style::Display | Style::DisplayCramped)
    }

    pub fn is_cramped(self) -> bool {
        matches!(
            self,
            Style::DisplayCramped
                | Style::TextCramped
                | Style::ScriptCramped
                | Style::ScriptScriptCramped
        )
    }

    /// `tex.web` §702's `cramped_style`: the same size, cramped.
    pub fn cramped(self) -> Style {
        match self {
            Style::Display | Style::DisplayCramped => Style::DisplayCramped,
            Style::Text | Style::TextCramped => Style::TextCramped,
            Style::Script | Style::ScriptCramped => Style::ScriptCramped,
            _ => Style::ScriptScriptCramped,
        }
    }

    /// §702's `sup_style`: one size smaller, keeping the cramping.
    pub fn sup(self) -> Style {
        let smaller = self.one_smaller_script();
        if self.is_cramped() {
            smaller.cramped()
        } else {
            smaller
        }
    }

    /// §702's `sub_style`: one size smaller, always cramped.
    pub fn sub(self) -> Style {
        self.one_smaller_script().cramped()
    }

    /// The size a script takes: script from display or text, scriptscript from
    /// either script style.
    fn one_smaller_script(self) -> Style {
        match self.size() {
            0 => Style::Script,
            _ => Style::ScriptScript,
        }
    }

    /// §702's `num_style`: one step smaller, keeping the cramping.
    pub fn num(self) -> Style {
        let smaller = self.one_smaller();
        if self.is_cramped() {
            smaller.cramped()
        } else {
            smaller
        }
    }

    /// §702's `denom_style`: one step smaller, always cramped.
    pub fn denom(self) -> Style {
        self.one_smaller().cramped()
    }

    fn one_smaller(self) -> Style {
        match self {
            Style::Display | Style::DisplayCramped => Style::Text,
            Style::Text | Style::TextCramped => Style::Script,
            _ => Style::ScriptScript,
        }
    }

    /// Which of `\textfont`, `\scriptfont`, `\scriptscriptfont` to draw from.
    pub fn size(self) -> usize {
        match self {
            Style::Display | Style::Text | Style::DisplayCramped | Style::TextCramped => 0,
            Style::Script | Style::ScriptCramped => 1,
            Style::ScriptScript | Style::ScriptScriptCramped => 2,
        }
    }
}

/// A deferred diagnostic from §722's `fetch`. Math conversion has no error
/// stream of its own, so the engine reports these after the formula is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MathFontIssue {
    UndefinedFamily {
        size: usize,
        family: usize,
        code: u32,
    },
    MissingCharacter {
        font: usize,
        code: u32,
    },
}

/// The three math glue parameters and the fonts to draw families from.
pub struct MathContext<'a> {
    /// Raw `\thinmuskip`, `\medmuskip`, and `\thickmuskip` values in `mu`.
    /// They cannot be converted until the style of each occurrence is known.
    pub thin: Glue,
    pub med: Glue,
    pub thick: Glue,
    /// `fonts[size][family]` — the engine font index, or `None` if unset.
    pub fonts: &'a dyn Fn(usize, usize) -> Option<usize>,
    /// §722 `fetch`'s failures, in the order TeX encounters them.
    pub font_issues: &'a core::cell::RefCell<alloc::vec::Vec<MathFontIssue>>,
    /// `\fontdimen6` of family 2 at each size — §703's `math_quad(cur_size)`,
    /// which divided by 18 is one `mu`.
    pub quad: [Scaled; 3],
    /// Axis height (`\fontdimen22` of family 2) at each of the three sizes —
    /// about which fractions and delimiters are centred. It is *per size*: a
    /// fraction inside a superscript is centred on the script font's axis, not
    /// the text font's, and using one for all three put every nested fraction
    /// a fraction of a point out.
    pub axis: [Scaled; 3],
    /// `\fontdimen` 13/14/15 of family 2 at each of the three sizes —
    /// `sup1`/`sup2`/`sup3`, the minimum a superscript is raised in display,
    /// text and cramped styles. Per *size*, like everything else here: a script
    /// inside a script is measured against the scriptscript font's numbers.
    pub sup: [[Scaled; 3]; 3],
    /// `\fontdimen16`/`17` — `sub1`/`sub2`, the minimum a subscript drops.
    pub sub: [[Scaled; 2]; 3],
    /// `\fontdimen18` — `sup_drop`: how far below the nucleus's top a
    /// superscript may start. `\fontdimen19` is `sub_drop`.
    pub sup_drop: [Scaled; 3],
    pub sub_drop: [Scaled; 3],
    /// `\fontdimen5` of family 2 at each of the three sizes — the x-height,
    /// which bounds how low a script may sit.
    pub x_height: [Scaled; 3],
    /// `\scriptspace` — extra width TeX puts after a script box (`tex.web`
    /// §756), so a following symbol does not sit against the script. plain.tex
    /// sets it to 0.5pt, which is exactly how much every `$x^2$` was too narrow.
    pub script_space: Scaled,
    /// `\fontdimen8` of family 3 at each size — the `default_rule_thickness`
    /// every rule a formula draws is measured in.
    pub default_rule_thickness: [Scaled; 3],
    /// `\fontdimen` 8/9/10 of family 2 — `num1`/`num2`/`num3`, how far a
    /// numerator is raised in display style, in text style with a rule, and in
    /// text style without one.
    pub num: [[Scaled; 3]; 3],
    /// `\fontdimen` 11/12 of family 2 — `denom1`/`denom2`.
    pub denom: [[Scaled; 2]; 3],
    /// `\nulldelimiterspace` — the width of the empty boxes that stand in for
    /// a fraction's delimiters when it has none.
    pub null_delimiter_space: Scaled,
    /// `\fontdimen` 20 and 21 of family 2 — `delim1`/`delim2`, how tall a
    /// fraction's delimiters are made in display and in text style.
    pub delim: [[Scaled; 2]; 3],
    /// `\delimiterfactor` and `\delimitershortfall` — how completely a
    /// `\left`/`\right` delimiter has to cover what it encloses (§762).
    pub delimiter_factor: i32,
    pub delimiter_shortfall: Scaled,
    /// `\fontdimen` 9–13 of family 3 at each size — the five clearances §751
    /// stacks a large operator's limits with.
    pub big_op_spacing: [[Scaled; 5]; 3],
    /// `\binoppenalty` and `\relpenalty` — how much a line break after a
    /// binary operator or a relation costs.
    pub bin_op_penalty: i32,
    pub rel_penalty: i32,
}

/// Rewrite `Bin` atoms that cannot be binary, per `tex.web` §727.
///
/// A binary operator needs something to its left that it could operate on. At
/// the start of a list, or after another operator, a relation, an opening
/// delimiter or punctuation, it is a unary sign instead — and gets no space.
/// This is why `$-x$` and `$a-x$` differ.
pub fn fix_binaries(list: &mut [Noad]) {
    // §726 opens the first pass with `r:=null; r_type:=op_noad`, and `op_noad`
    // is in the demoting set below — which is how a formula that starts with
    // `-` gets an ordinary minus rather than a binary one.
    let mut prev = Class::Op;
    let mut last: Option<usize> = None;
    for i in 0..list.len() {
        // §727 dispatches on `type(q)`, and a style node or a piece of glue is
        // not a noad: it reaches `done_with_node`, which leaves `r` and
        // `r_type` alone. So the `\mkern` in `$x+\mkern3mu-y$` does not come
        // between the two operators as far as this rule can tell, and the
        // minus is still unary.
        if matches!(list[i].kind, Kind::Style(_) | Kind::Raw) {
            continue;
        }
        if list[i].class == Class::Bin
            && matches!(
                prev,
                Class::Bin | Class::Op | Class::Rel | Class::Open | Class::Punct
            )
        {
            list[i].class = Class::Ord;
        }
        // §727's other direction, which the previous version of this function
        // claimed the loop would handle by itself and does not:
        //
        //     rel_noad,close_noad,punct_noad,right_noad: begin
        //       @<Convert a final bin_noad to an ord_noad@>;
        //
        // The noad demoted is the one *before* — `r`, not `q`. `$x+=y$` has no
        // binary `+`, because nothing follows for it to operate on.
        if matches!(list[i].class, Class::Rel | Class::Close | Class::Punct) {
            if let Some(r) = last {
                if list[r].class == Class::Bin {
                    list[r].class = Class::Ord;
                }
            }
        }
        // `\middle` has the spacing identity of a close delimiter on this
        // side, but it begins a fresh left/right segment for what follows.
        // In particular, a minus immediately after it is unary just as one
        // immediately after `\left` is. Leaving `prev=Close` kept that minus
        // binary and inserted a spurious 4mu before its operand.
        prev = if matches!(list[i].kind, Kind::MiddleDelim(_)) {
            Class::Open
        } else {
            list[i].class
        };
        last = Some(i);
    }
    // And §726 runs the same conversion once more when the list ends:
    //
    //     while q<>null do @<Process node-or-noad q…@>;
    //     @<Convert \(a)a final |bin_noad| to an |ord_noad|@>;
    //
    // `$--$` is the smallest case. The second minus is the last noad, so it is
    // ordinary, and there is no `\medmuskip` between the two — where the
    // corpus met it was a displayed formula ending in a binary operator.
    if let Some(r) = last {
        if list[r].class == Class::Bin {
            list[r].class = Class::Ord;
        }
    }
}

/// Convert a math list to a horizontal list.
///
/// `tex.web` §720. Two passes: translate each noad's own material, then walk
/// the result inserting inter-atom glue. They cannot be merged, because the
/// glue between two atoms depends on classes that the first pass may still
/// change.
pub fn mlist_to_hlist(
    list: Vec<Noad>,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> Vec<Node> {
    mlist_to_hlist_pen(list, style, ctx, metrics, false)
}

/// The same, with `tex.web`'s `mlist_penalties`: a formula set directly into a
/// paragraph gets `\binoppenalty` after an eligible binary operator and
/// `\relpenalty` after an eligible relation (§761), which is what discourages
/// a line break there. The final atom gets no penalty because §761's `q<>null`
/// guard requires another math-list node. A sub-formula, or one inside a box,
/// gets none — and `clean_box` turns them off for exactly that reason.
pub fn mlist_to_hlist_pen(
    mut list: Vec<Noad>,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
    penalties: bool,
) -> Vec<Node> {
    // §728 splices the selected `\mathchoice` list into the surrounding mlist
    // before atoms are classified and spaced. Treating the choice itself as
    // an ordinary atom inserts spacing on both sides of it. In particular,
    // amsmath's display-style `\iint` is
    // `\intop\mkern-6mu\mathchoice{\mkern-3mu}{}{}{}\intop`; an Ord choice
    // added a second thin space and made every such centred display 3mu too
    // wide.
    fn select_choices(list: Vec<Noad>, mut style: Style) -> (Vec<Noad>, Style) {
        let mut selected = Vec::with_capacity(list.len());
        for mut noad in list {
            if let Kind::Style(next) = noad.kind {
                style = next;
                selected.push(noad);
                continue;
            }
            if noad.kind == Kind::Choice {
                if let Some(mut choices) = noad.choices.take() {
                    let index = if style.is_display() {
                        0
                    } else {
                        style.size() + 1
                    };
                    let branch = core::mem::take(&mut choices[index]);
                    let (branch, next_style) = select_choices(branch, style);
                    selected.extend(branch);
                    style = next_style;
                }
                continue;
            }
            selected.push(noad);
        }
        (selected, style)
    }

    list = select_choices(list, style).0;
    fix_binaries(&mut list);
    math_ligatures(&mut list, style, ctx, metrics);

    let mut out: Vec<Node> = Vec::new();
    // `tex.web` §730: a style node changes the style for the rest of the list
    // and is not an atom — it sets nothing and takes no space, so the atoms
    // either side of it are still neighbours for spacing.
    // Two passes, as `tex.web` §726 and §760 are: the first translates every
    // atom and records the tallest and deepest, the second puts the glue
    // between them — and builds `\left`/`\right`, which need those maxima.
    // A middle delimiter is asymmetric. e-TeX represents `\middle` by
    // finishing a right noad and immediately starting the next left/right
    // segment: it faces `Close` on its left and `Open` on its right. Keeping
    // only one class made `\middle|\,\lim` look like Close--Op and inserted an
    // extra automatic thin space after the delimiter, in addition to the
    // explicit `\,`.
    let mut pieces: Vec<(Class, Class, Style, Vec<Node>)> = Vec::new();
    let mut cur = style;
    let mut max_h = Scaled::ZERO;
    let mut max_d = Scaled::ZERO;
    // Glue and kerns, with the atom index they sit before.
    // §730 converts `mu_glue` and a `\mkern` while *walking* the list, with
    // `cur_mu` following `cur_style` — so each passed-through node has to
    // remember the style it was found under.
    let mut raw: Vec<(usize, Vec<Node>, Style)> = Vec::new();
    // §730's `\nonscript` is itself retained, but in script and scriptscript
    // styles it removes the very next glue or kern node. Track that adjacency
    // while the original mlist order is still visible: style nodes and atoms
    // are barriers even though neither survives as raw material in this
    // vector, while two adjacent raw noads are not.
    let mut suppress_after_nonscript = false;
    for (idx, noad) in list.iter().enumerate() {
        if let Kind::Style(s) = noad.kind {
            suppress_after_nonscript = false;
            cur = s;
            continue;
        }
        if matches!(
            noad.kind,
            Kind::LeftDelim(_) | Kind::RightDelim(_) | Kind::MiddleDelim(_)
        ) {
            suppress_after_nonscript = false;
            let after_class = if matches!(noad.kind, Kind::MiddleDelim(_)) {
                Class::Open
            } else {
                noad.class
            };
            pieces.push((noad.class, after_class, cur, Vec::new()));
            continue;
        }
        if noad.kind == Kind::Raw {
            if let Field::List(nodes) = &noad.nucleus {
                let mut kept = Vec::with_capacity(nodes.len());
                for node in nodes.iter().cloned() {
                    if suppress_after_nonscript {
                        suppress_after_nonscript = false;
                        if matches!(node, Node::Glue(_) | Node::Kern(..)) {
                            continue;
                        }
                    }
                    let is_nonscript = cur.is_script()
                        && matches!(
                            node,
                            Node::Glue(g) if g.subtype == crate::node::glue_sub::COND_MATH
                        );
                    kept.push(node);
                    suppress_after_nonscript = is_nonscript;
                }
                // `tex.web` §729 includes ordinary rule and box nodes in the
                // height/depth maxima used by §762 to size `\left` and
                // `\right`. They remain raw material for the second pass, but
                // that does not make them dimensionless. In particular,
                // `breqn` implements `\bigg` with a tall `\vrule` between a
                // left delimiter and `\right.`; ignoring the rule selected the
                // small delimiter instead of its extensible variant.
                let extent = crate::node::measure_horizontal_basic(&kept, metrics);
                max_h = max2(max_h, extent.height);
                max_d = max2(max_d, extent.depth);
                raw.push((pieces.len(), kept, cur));
            }
            continue;
        }
        suppress_after_nonscript = false;
        let mut piece = Vec::new();
        translate(
            noad,
            cur,
            ctx,
            metrics,
            is_text_char(&list, idx),
            &mut piece,
        );
        let z = crate::node::hpack(piece.clone(), Option::None, metrics);
        max_h = max2(max_h, z.height);
        max_d = max2(max_d, z.depth);
        pieces.push((noad.class, noad.class, cur, piece));
    }
    let mut delims = list.iter().filter(|n| {
        matches!(
            n.kind,
            Kind::LeftDelim(_) | Kind::RightDelim(_) | Kind::MiddleDelim(_)
        )
    });
    let n_pieces = pieces.len();
    // §761 examines the node *immediately* following an atom before adding an
    // automatic binary or relation penalty. Raw math-list material sits in
    // that position before the spacing glue is inserted. Remember whether the
    // first such node at each atom boundary is a penalty; looking only at the
    // next atom class both inserted a penalty ahead of an explicit `\penalty`
    // and mistook a relation beyond intervening glue for an adjacent one.
    let mut raw_first_is_penalty = alloc::vec![None; n_pieces + 1];
    for (at, nodes, _) in &raw {
        if raw_first_is_penalty[*at].is_none()
            && let Some(node) = nodes.first()
        {
            raw_first_is_penalty[*at] = Some(matches!(node, Node::Penalty(_)));
        }
    }
    let mut prev: Option<Class> = None;
    let mut raw = raw.into_iter().peekable();
    let classes: Vec<Class> = pieces.iter().map(|(c, _, _, _)| *c).collect();
    for (i, (class, after_class, st, piece)) in pieces.into_iter().enumerate() {
        let next_class = classes.get(i + 1).copied();
        while raw.peek().is_some_and(|(at, _, _)| *at == i) {
            let (_, nodes, at_style) = raw.next().expect("peeked");
            out.extend(nodes.into_iter().map(|n| mu_to_pt(n, at_style, ctx)));
        }
        if let Some(left) = prev {
            if let Some(g) = glue_for(spacing(left, class), st, ctx) {
                out.push(Node::Glue(g));
            }
        }
        if piece.is_empty() && matches!(class, Class::Open | Class::Close) {
            if let Some(n) = delims.next() {
                let d = match n.kind {
                    Kind::LeftDelim(d) | Kind::RightDelim(d) | Kind::MiddleDelim(d) => d,
                    _ => Delim::default(),
                };
                // §762 is called with `style`, the style in which this mlist
                // itself is being converted, not `cur_style`.  A style node
                // between `\left` and `\right` changes the atoms that follow
                // it, but both delimiters remain centred on the axis of the
                // enclosing list.  In particular, a `\displaystyle\frac`
                // inside a superscript must not move only the closing
                // delimiter by one point.
                out.push(Node::Box(alloc::boxed::Box::new(make_left_right(
                    d, style, max_h, max_d, ctx, metrics,
                ))));
                prev = Some(after_class);
                continue;
            }
        }
        out.extend(piece);
        // §761: the penalty goes *after* the atom, unless what follows is a
        // penalty already or a relation.
        if penalties {
            let raw_next = raw_first_is_penalty[i + 1];
            let has_next = next_class.is_some() || raw_next.is_some();
            let next_is_penalty = raw_next == Some(true);
            let next_is_relation = raw_next.is_none() && next_class == Some(Class::Rel);
            let pen = match class {
                Class::Bin => ctx.bin_op_penalty,
                Class::Rel => ctx.rel_penalty,
                _ => INF_PENALTY,
            };
            if has_next && pen < INF_PENALTY && !next_is_penalty && !next_is_relation {
                out.push(Node::Penalty(pen));
            }
        }
        prev = Some(after_class);
    }
    // Anything left over sat past the last atom, and is converted the same way.
    for (_, nodes, at_style) in raw {
        out.extend(nodes.into_iter().map(|n| mu_to_pt(n, at_style, ctx)));
    }
    out
}

/// `tex.web` §752 `make_ord`: run a font's ligature/kern program over adjacent
/// math characters of one family.
///
/// ```text
///     A ligature found in a math formula does not create a |ligature_node|,
///     because there is no question of hyphenation afterwards; the ligature
///     will simply be stored in an ordinary |char_node|, after residing in an
///     |ord_noad|.
/// ```
///
/// Operators 1/5 replace the left operand, 2/6 the right, 3/7/11 insert a new
/// noad, and the remaining operators replace both. Operators at most three
/// restart at the same left noad; the others return from that `make_ord` call.
/// A kern instruction inserts an actual kern noad at the point it is found;
/// inferring kerns from the final rewritten glyph sequence puts them between
/// the wrong pairs for the retain/insert operators.
///
/// The corpus reaches this through `$``1\times2\cdots"$`: a displayed formula
/// quoting a product. Without it two grave accents stood where an opening
/// quotation mark belongs, and the line was two characters wide instead of one.
fn math_ligatures(
    list: &mut Vec<Noad>,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) {
    let mut i = 0;
    let mut cur_style = style;
    // A malformed TFM can encode a ligature cycle. TeX checks interrupts in
    // this loop; use a generous deterministic bound in this non-interactive
    // layer so hostile input cannot spin forever.
    let mut fuel = 1_000_000usize;
    while i + 1 < list.len() {
        if fuel == 0 {
            break;
        }
        fuel -= 1;
        if let Kind::Style(s) = list[i].kind {
            cur_style = s;
            i += 1;
            continue;
        }
        // §752 enters only for an ordinary, unscripted `math_char`, followed
        // by a character noad of one of the seven spacing classes in the same
        // family. An op-11 insertion is already `math_text_char` and cannot
        // start another ligature when the outer traversal reaches it.
        if list[i].class != Class::Ord
            || list[i].kind != Kind::Normal
            || list[i].math_text_char
            || !list[i].subscript.is_empty()
            || !list[i].superscript.is_empty()
            || list[i + 1].kind != Kind::Normal
            || list[i + 1].class == Class::Inner
            || list[i + 1].math_text_char
        {
            i += 1;
            continue;
        }
        let (
            Field::Char { family, code: left },
            Field::Char {
                family: right_family,
                code: right,
            },
        ) = (&list[i].nucleus, &list[i + 1].nucleus)
        else {
            i += 1;
            continue;
        };
        if family != right_family {
            i += 1;
            continue;
        }
        let (family, left, right) = (*family, *left, *right);
        // TeX makes this assignment before `fetch`; it survives a kern or a
        // pair with no ligature instruction.
        list[i].math_text_char = true;
        let Some(f) = (ctx.fonts)(cur_style.size(), family) else {
            i += 1;
            continue;
        };
        let Some((lig, op)) = metrics.ligature(f, left, right) else {
            // A kern instruction is material inserted after `q`; it is not a
            // property inferred later from whichever glyphs happen to remain
            // adjacent after the ligature program has rewritten the list.
            if let Some(k) = metrics.kern(f, left, right) {
                list.insert(
                    i + 1,
                    Noad {
                        kind: Kind::Raw,
                        nucleus: Field::List(alloc::vec![Node::Kern(
                            k,
                            crate::node::kern_sub::NORMAL,
                        )]),
                        ..Default::default()
                    },
                );
            }
            i += 1;
            continue;
        };
        match op {
            // Replace the left or right operand, retaining both.
            1 | 5 => list[i].nucleus = Field::Char { family, code: lig },
            2 | 6 => list[i + 1].nucleus = Field::Char { family, code: lig },
            // Insert the ligature between them. Op 11 makes the inserted noad
            // a `math_text_char`; 3 and 7 make an ordinary `math_char`.
            3 | 7 | 11 => {
                list.insert(
                    i + 1,
                    Noad {
                        class: Class::Ord,
                        nucleus: Field::Char { family, code: lig },
                        math_text_char: op == 11,
                        ..Default::default()
                    },
                );
            }
            // Replace both operands. The surviving left noad inherits the
            // right one's scripts, exactly as §752's `othercases` requires.
            _ => {
                let p = list.remove(i + 1);
                list[i].nucleus = Field::Char { family, code: lig };
                list[i].subscript = p.subscript;
                list[i].superscript = p.superscript;
            }
        }
        if op <= 3 {
            list[i].math_text_char = false;
            continue; // `goto restart`
        }
        i += 1;
    }
}

/// `tex.web` §752 `make_ord`: is this noad's nucleus a `math_text_char`?
///
/// The name is about italic corrections and nothing else. §755 skips the
/// correction when the nucleus is a `math_text_char` *and* the font has a
/// nonzero `space` — that is, when the character came from a text font and is
/// sitting in the middle of a word:
///
/// ```text
///     if (math_type(nucleus(q))=math_text_char)and(space(cur_f)<>0) then
///       delta:=0; {no italic correction in mid-word of text font}
/// ```
///
/// "Mid-word" is the whole of it, and §752 is where it is decided:
///
/// ```text
///     if math_type(subscr(q))=empty then if math_type(supscr(q))=empty then
///      if math_type(nucleus(q))=math_char then
///       begin p:=link(q);
///       if p<>null then if (type(p)>=ord_noad)and(type(p)<=punct_noad) then
///         if math_type(nucleus(p))=math_char then
///         if fam(nucleus(p))=fam(nucleus(q)) then
///           begin math_type(nucleus(q)):=math_text_char;
/// ```
///
/// So only a character with *another character of the same family after it*
/// loses its correction. The last letter of `\log` keeps one — cmr's `g`
/// leans right by 0.0136em, and 12pt of it is the 0.1632pt every displayed
/// `\log` in the corpus was missing. Suppressing the correction for every
/// text-font character instead is right for `lo` and wrong for `g`.
///
/// Only `make_ord` performs the conversion, so this asks for an `Ord` atom.
/// A `Punct` or `Rel` nucleus keeps its correction whatever follows it.
fn is_text_char(list: &[Noad], i: usize) -> bool {
    let Some(q) = list.get(i) else {
        return false;
    };
    if q.math_text_char {
        return true;
    }
    if q.class != Class::Ord || q.kind != Kind::Normal {
        return false;
    }
    if !q.subscript.is_empty() || !q.superscript.is_empty() {
        return false;
    }
    let Field::Char { family: fq, .. } = q.nucleus else {
        return false;
    };
    let Some(p) = list.get(i + 1) else {
        return false;
    };
    // `type(p)>=ord_noad` and `<=punct_noad`: the seven atom types, and not
    // `inner_noad`, a fraction, a radical, an accent or a style node.
    if p.kind != Kind::Normal || p.class == Class::Inner {
        return false;
    }
    matches!(p.nucleus, Field::Char { family, .. } if family == fq)
}

/// The font kern between the character nuclei of atoms `i` and `i+1`, when
/// they are adjacent characters of the same font.
/// `tex.web` §730: a `\mskip`'s glue and a `\mkern`'s width are in `mu` until
/// the list is walked, and §717 converts them against `cur_mu`:
///
/// ```text
/// glue_node: begin if subtype(q)=mu_glue then
///     begin x:=glue_ptr(q);
///     y:=math_glue(x,cur_mu); delete_glue_ref(x); glue_ptr(q):=y;
///     subtype(q):=normal;
///     end
/// …
/// kern_node: math_kern(q,cur_mu);
/// ```
///
/// `cur_mu` is §703's `math_quad(cur_size)/18`, and `cur_size` follows the
/// *style* — so the same `\mkern9mu` is one width in a formula and another in
/// a superscript. Converting at scan time instead used the text quad
/// everywhere: amsmath builds every `\xrightarrow` as
/// `$\scriptstyle\mkern5mu{…}\mkern9mu$`, so every extensible arrow in the
/// corpus came out 2.4pt too wide, and at that width one more `\cleaders` copy
/// of the arrow's shaft fitted than TeX had room for.
fn mu_to_pt(n: Node, style: Style, ctx: &MathContext<'_>) -> Node {
    // §716 `mu_mult`: the value splits into whole `mu` and a fraction, and each
    // is multiplied by one `mu` separately. Scaling in one step is off by a few
    // scaled points, which is enough to move every glyph after it.
    let one_mu = crate::scaled::x_over_n(ctx.quad[style.size()], 18).0;
    let scale = |v: Scaled| {
        let (whole, frac) = crate::scaled::x_over_n(v, 65536);
        Scaled(one_mu.0 * whole.0 + crate::scaled::xn_over_d(one_mu, frac.0, 65536).0.0)
    };
    match n {
        Node::Kern(w, crate::node::kern_sub::MU) => {
            Node::Kern(scale(w), crate::node::kern_sub::EXPLICIT)
        }
        Node::Glue(g) if g.subtype == crate::node::glue_sub::MU => Node::Glue(Glue {
            width: scale(g.width),
            // §716 converts a stretch or shrink only when its order is finite:
            // `1fil` of math glue is `1fil` of ordinary glue.
            stretch: if g.stretch_order == 0 {
                scale(g.stretch)
            } else {
                g.stretch
            },
            stretch_order: g.stretch_order,
            shrink: if g.shrink_order == 0 {
                scale(g.shrink)
            } else {
                g.shrink
            },
            shrink_order: g.shrink_order,
            // §716 clears `mu_glue` as soon as the glue leaves `mu` behind.
            subtype: 0,
            canonical_zero: g.canonical_zero,
        }),
        other => other,
    }
}

/// `tex.web` §762 `make_left_right`: how tall a `\left`/`\right` delimiter has
/// to be. It covers `\delimiterfactor` thousandths of the distance from the
/// axis to the furthest edge of what it encloses, and in any case comes within
/// `\delimitershortfall` of covering all of it.
fn make_left_right(
    d: Delim,
    style: Style,
    max_h: Scaled,
    max_d: Scaled,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> crate::node::TeXBox {
    let size = style.size();
    let delta2 = max_d + ctx.axis[size];
    let delta1 = max2(max_h + max_d - delta2, delta2);
    let mut delta = Scaled((delta1.0 / 500) * ctx.delimiter_factor);
    let alt = Scaled(delta1.0 + delta1.0 - ctx.delimiter_shortfall.0);
    if delta < alt {
        delta = alt;
    }
    var_delimiter(d, size, delta, ctx, metrics)
}

fn glue_for(s: Spacing, style: Style, ctx: &MathContext<'_>) -> Option<Glue> {
    use crate::node::glue_sub;
    use Spacing::*;
    // §766 stores which of the three parameters the space came from, so a
    // formula's `\showlists` reads `\glue(\thickmuskip) 5.0 plus 5.0` — the one
    // thing that tells a wrong space apart from a wrong *class*.
    let (g, subtype) = match s {
        None => return Option::None,
        Thin => (ctx.thin, glue_sub::THIN_MU_SKIP),
        Medium => (ctx.med, glue_sub::MED_MU_SKIP),
        Thick => (ctx.thick, glue_sub::THICK_MU_SKIP),
        ThinConditional if style.is_script() => return Option::None,
        MediumConditional if style.is_script() => return Option::None,
        ThickConditional if style.is_script() => return Option::None,
        ThinConditional => (ctx.thin, glue_sub::THIN_MU_SKIP),
        MediumConditional => (ctx.med, glue_sub::MED_MU_SKIP),
        ThickConditional => (ctx.thick, glue_sub::THICK_MU_SKIP),
    };
    // §716's conversion is the same one used for an explicit `\mskip`; retain
    // the parameter subtype afterwards so `\showlists` names its provenance.
    match mu_to_pt(Node::Glue(g.sub(glue_sub::MU)), style, ctx) {
        Node::Glue(g) => Some(g.sub(subtype)),
        _ => unreachable!("math glue conversion returned a non-glue node"),
    }
}

/// `tex.web` §702: the style a numerator is set in — one step smaller, never
/// past `\scriptscriptstyle`. A denominator takes the same size in a *cramped*
/// style; we have no cramped styles, and the difference only moves a
/// superscript inside the denominator.
fn smaller(style: Style) -> Style {
    style.num()
}

/// `tex.web` §706: an hbox holding a field, set in the given style — what every
/// construction measures and stacks. A one-character field keeps its italic
/// correction here, unlike §720's `clean_box`, because nothing downstream
/// removes it again.
fn clean_box(
    f: &Field,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> crate::node::TeXBox {
    let mut inner = Vec::new();
    match f {
        // §720 runs a *character* field through the full translation, as a
        // one-noad list — which is what gives it its italic correction.
        // Emitting the character alone left every unbraced script narrow by
        // exactly that: `$y^z$` was 0.287pt short and `$x^{z}$` was not.
        Field::Char { .. } => {
            let n = Noad {
                nucleus: f.clone(),
                ..Default::default()
            };
            inner = mlist_to_hlist(alloc::vec![n], style, ctx, metrics);
        }
        // A `sub_mlist` is translated here and only then inspected by §720.
        // Going through `emit_field` packed it into an hbox first, hiding the
        // fact that its translated list was exactly one vbox. TeX returns that
        // vbox directly when it is unshifted; the distinction affects the
        // height of a braced accent used as a script.
        Field::Sub(noads) => {
            inner = mlist_to_hlist(noads.clone(), style, ctx, metrics);
        }
        _ => emit_field(f, style, ctx, metrics, &mut inner),
    }
    // §720 does not wrap a box that is already one: a list that is exactly one
    // unshifted box *is* the clean box. Wrapping it anyway costs nothing in
    // position but emits an extra `down` in the DVI for every superscript that
    // holds a fraction, and a comparison of movements is how these are checked.
    // It still passes through the trivial-box cleanup below, so returning a
    // one-character sub-mlist here does not leave its lone italic-correction
    // kern visible.
    let mut b = if inner.len() == 1 {
        if let Node::Box(b) = &inner[0]
            && b.shift == Scaled::ZERO
        {
            *b.clone()
        } else {
            crate::node::hpack(inner, Option::None, metrics)
        }
    } else {
        crate::node::hpack(inner, Option::None, metrics)
    };
    // §720's "simplify a trivial box":
    //
    //     q:=list_ptr(x);
    //     if is_char_node(q) then
    //       begin r:=link(q);
    //       if r<>null then if link(r)=null then
    //         if not is_char_node(r) then if type(r)=kern_node then
    //           begin free_node(r,small_node_size); link(q):=null;
    //           end;
    //       end
    //
    // One character followed by exactly one kern: the kern goes. The order is
    // the whole of it — this runs *after* `hpack`, so `width(x)` already counts
    // the correction and only the node is removed. An earlier attempt dropped
    // the kern before packing and lost the width with it, which put every
    // fraction's denominator a quarter-point off-centre; the note left behind
    // concluded the rule itself was wrong. It is not: `$\scriptstyle y$` is
    // 4.30675pt in pdfTeX — glyph plus correction — *and* its box holds one
    // character.
    let trivial = matches!(b.list.as_slice(), [Node::Char { .. }, Node::Kern(_, _)]);
    if trivial {
        b.list.truncate(1);
    }
    b
}

/// `tex.web` §704: a rule of the given thickness, as wide as the box it lands
/// in (a running width).
fn fraction_rule(t: Scaled) -> Node {
    Node::Rule {
        width: Scaled::RUNNING,
        height: t,
        depth: Scaled::ZERO,
    }
}

/// `tex.web` §705 `overbar`: `b` with a rule `t` thick above it, `k` of clear
/// space between, and `t` of space above the rule.
fn overbar(b: crate::node::TeXBox, k: Scaled, t: Scaled) -> crate::node::TeXBox {
    let list = alloc::vec![
        Node::Kern(t, crate::node::kern_sub::NORMAL),
        fraction_rule(t),
        Node::Kern(k, crate::node::kern_sub::NORMAL),
        Node::Box(alloc::boxed::Box::new(b)),
    ];
    crate::node::vpack(list, Option::None, Scaled(i32::MAX))
}

/// `tex.web` §715 `rebox`: repack a box to an exact width, centring what is in
/// it with infinitely stretchable glue at each end.
fn rebox(b: crate::node::TeXBox, w: Scaled, metrics: &impl CharMetrics) -> crate::node::TeXBox {
    if b.width == w || b.list.is_empty() {
        let mut b = b;
        b.width = w;
        return b;
    }
    let ss = Glue {
        width: Scaled::ZERO,
        stretch: Scaled(65536),
        stretch_order: 1,
        shrink: Scaled(65536),
        shrink_order: 1,
        subtype: 0,
        canonical_zero: false,
    };
    let mut list = alloc::vec![Node::Glue(ss)];
    if b.kind == crate::node::BoxKind::Vertical {
        list.push(Node::Box(alloc::boxed::Box::new(b)));
    } else {
        // §715 restores the italic correction that §720 deliberately
        // removed from a trivial one-character box's node list. Its width still
        // includes that correction; without the compensating kern, the two
        // `ss_glue` nodes centre only the bare glyph and shift it right by half
        // the missing correction.
        let correction = match b.list.as_slice() {
            [Node::Char { code, font, .. }] => b.width - metrics.width(*font, *code),
            _ => Scaled::ZERO,
        };
        list.extend(b.list);
        if correction != Scaled::ZERO {
            list.push(Node::Kern(correction, crate::node::kern_sub::NORMAL));
        }
    }
    list.push(Node::Glue(ss));
    crate::node::hpack(list, Some(w), metrics)
}

/// A delimiter, as `\delcode` and `\delimiter` encode it: a small variant and a
/// large one, each a family and a character. `tex.web` §683.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Delim {
    pub small_fam: usize,
    pub small_char: u32,
    pub large_fam: usize,
    pub large_char: u32,
}

impl Delim {
    /// §697's test for the null delimiter: all four parts zero, which is what
    /// `\left.` leaves and what a fraction written without `…withdelims` has.
    pub fn is_null(&self) -> bool {
        self.small_fam == 0 && self.small_char == 0 && self.large_fam == 0 && self.large_char == 0
    }

    /// The 24-bit `\delcode`/`\delimiter` encoding: `"sSSlLL`.
    pub fn from_code(v: i64) -> Delim {
        Delim {
            small_fam: ((v >> 20) & 0xF) as usize,
            small_char: ((v >> 12) & 0xFF) as u32,
            large_fam: ((v >> 8) & 0xF) as usize,
            large_char: (v & 0xFF) as u32,
        }
    }
}

/// `tex.web` §709 `char_box`: a box holding one glyph, as wide as the glyph
/// *plus its italic correction*.
fn char_box(font: usize, code: u32, metrics: &impl CharMetrics) -> crate::node::TeXBox {
    crate::node::TeXBox {
        kind: crate::node::BoxKind::Horizontal,
        width: metrics.width(font, code) + metrics.italic(font, code),
        height: metrics.height(font, code),
        depth: metrics.depth(font, code),
        set_num: 0,
        set_den: 1,
        glue_sign: crate::node::GlueSign::Normal,
        glue_order: 0,
        shift: Scaled::ZERO,
        list: alloc::vec![Node::Char {
            code,
            font,
            lig: None
        }],
    }
}

fn height_plus_depth(font: usize, code: u32, metrics: &impl CharMetrics) -> Scaled {
    metrics.height(font, code) + metrics.depth(font, code)
}

/// `tex.web` §706 `var_delimiter`: find or build a delimiter at least `v` tall.
///
/// The search walks the small variant's charlist in the font at size `size` and
/// every larger size, then the large variant's, keeping the tallest glyph seen
/// and stopping the moment one is big enough. A glyph tagged *extensible* wins
/// outright: it can be built to any height out of its recipe's pieces.
///
/// A delimiter that is not found at all — including the null delimiter, which
/// is what `\over` has on both sides — is an empty box of
/// `\nulldelimiterspace`.
fn var_delimiter(
    d: Delim,
    size: usize,
    v: Scaled,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> crate::node::TeXBox {
    let mut best: Option<(usize, u32)> = None;
    let mut w = Scaled::ZERO;
    let mut found = None;
    'search: for (fam, chr) in [(d.small_fam, d.small_char), (d.large_fam, d.large_char)] {
        if fam == 0 && chr == 0 {
            continue;
        }
        // §707: from this size up to text size — a bigger font is the cheaper
        // way to get a bigger delimiter.
        for sz in (0..=size).rev() {
            let Some(g) = (ctx.fonts)(sz, fam) else {
                continue;
            };
            let mut y = chr;
            // §708: follow the charlist. It is a linked list through the font,
            // so it is capped rather than trusted.
            for _ in 0..64 {
                if !metrics.char_exists(g, y) {
                    break;
                }
                if metrics.char_tag(g, y) == 3 {
                    found = Some((g, y));
                    break 'search;
                }
                let u = height_plus_depth(g, y, metrics);
                if u > w {
                    best = Some((g, y));
                    w = u;
                    if u >= v {
                        found = best;
                        break 'search;
                    }
                }
                if metrics.char_tag(g, y) == 2 {
                    y = metrics.char_remainder(g, y);
                } else {
                    break;
                }
            }
        }
    }
    let mut b = match found.or(best) {
        Some((f, c)) if metrics.char_tag(f, c) == 3 => {
            extensible(f, c, v, metrics).unwrap_or_else(|| char_box(f, c, metrics))
        }
        Some((f, c)) => char_box(f, c, metrics),
        None => crate::node::TeXBox {
            kind: crate::node::BoxKind::Horizontal,
            width: ctx.null_delimiter_space,
            height: Scaled::ZERO,
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: crate::node::GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: Vec::new(),
        },
    };
    // §706: a delimiter is centred on the axis.
    b.shift = half(b.height - b.depth) - ctx.axis[size];
    b
}

/// `tex.web` §711–§714: build a delimiter out of its extensible recipe, tall
/// enough to reach `v`. The pieces are stacked bottom upwards, so the box's
/// height is the top piece's and everything else is depth.
fn extensible(
    font: usize,
    code: u32,
    v: Scaled,
    metrics: &impl CharMetrics,
) -> Option<crate::node::TeXBox> {
    let (top, mid, bot, rep) = metrics.exten(font, metrics.char_remainder(font, code))?;
    let u = height_plus_depth(font, rep, metrics);
    let mut w = Scaled::ZERO;
    let width = metrics.width(font, rep) + metrics.italic(font, rep);
    for c in [bot, mid, top] {
        if c != 0 {
            w = w + height_plus_depth(font, c, metrics);
        }
    }
    let mut n = 0usize;
    if u.0 > 0 {
        // Each repeat adds one piece, or two when there is a middle: the recipe
        // is symmetric about it.
        while w < v && n < 1000 {
            w = w + u;
            n += 1;
            if mid != 0 {
                w = w + u;
            }
        }
    }
    let mut list: Vec<Node> = Vec::new();
    let mut height = Scaled::ZERO;
    let mut stack = |c: u32, list: &mut Vec<Node>, height: &mut Scaled| {
        let p = char_box(font, c, metrics);
        *height = p.height;
        list.insert(0, Node::Box(alloc::boxed::Box::new(p)));
    };
    if bot != 0 {
        stack(bot, &mut list, &mut height);
    }
    for _ in 0..n {
        stack(rep, &mut list, &mut height);
    }
    if mid != 0 {
        stack(mid, &mut list, &mut height);
        for _ in 0..n {
            stack(rep, &mut list, &mut height);
        }
    }
    if top != 0 {
        stack(top, &mut list, &mut height);
    }
    Some(crate::node::TeXBox {
        kind: crate::node::BoxKind::Vertical,
        width,
        height,
        depth: w - height,
        set_num: 0,
        set_den: 1,
        glue_sign: crate::node::GlueSign::Normal,
        glue_order: 0,
        shift: Scaled::ZERO,
        list,
    })
}

/// `tex.web` §100's `half`, rounding an odd value away from zero.
fn half(x: Scaled) -> Scaled {
    Scaled(if x.0 % 2 != 0 { (x.0 + 1) / 2 } else { x.0 / 2 })
}

/// `\over`, `\atop`, `\above` — `tex.web` §743–§748.
///
/// The numerator and denominator are set one style smaller, reboxed to a
/// common width, and stacked with the fraction bar on the axis. How far each
/// is displaced starts from the font's `num`/`denom` parameters and is then
/// opened out until the clearance around the bar is enough.
fn make_fraction(
    noad: &Noad,
    thickness: Option<i32>,
    left: Delim,
    right: Delim,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> Node {
    let size = style.size();
    let t = match thickness {
        Some(v) => Scaled(v),
        None => ctx.default_rule_thickness[size],
    };
    let mut x = clean_box(&noad.nucleus, style.num(), ctx, metrics);
    let mut z = clean_box(&noad.denominator, style.denom(), ctx, metrics);
    if x.width < z.width {
        x = rebox(x, z.width, metrics);
    } else {
        z = rebox(z, x.width, metrics);
    }
    let display = style.is_display();
    let (mut shift_up, mut shift_down) = if display {
        (ctx.num[size][0], ctx.denom[size][0])
    } else if t.0 != 0 {
        (ctx.num[size][1], ctx.denom[size][1])
    } else {
        (ctx.num[size][2], ctx.denom[size][1])
    };
    let axis = ctx.axis[size];
    let delta = half(t);
    if t.0 == 0 {
        // §746: with no bar, the two boxes are simply kept apart.
        let clr = if display {
            Scaled(7 * ctx.default_rule_thickness[size].0)
        } else {
            Scaled(3 * ctx.default_rule_thickness[size].0)
        };
        let d = clr - ((shift_up - x.depth) - (z.height - shift_down));
        if d.0 > 0 {
            shift_up = shift_up + half(d);
            shift_down = shift_down + half(d);
        }
    } else {
        // §747: with a bar, each side has to clear it by itself.
        let clr = if display { Scaled(3 * t.0) } else { t };
        let d1 = clr - ((shift_up - x.depth) - (axis + delta));
        let d2 = clr - ((axis - delta) - (z.height - shift_down));
        if d1.0 > 0 {
            shift_up = shift_up + d1;
        }
        if d2.0 > 0 {
            shift_down = shift_down + d2;
        }
    }
    // §748: the vlist, built top down.
    let width = x.width;
    let height = shift_up + x.height;
    let depth = z.depth + shift_down;
    let mut list = alloc::vec![Node::Box(alloc::boxed::Box::new(x.clone()))];
    if t.0 == 0 {
        list.push(Node::Kern(
            (shift_up - x.depth) - (z.height - shift_down),
            crate::node::kern_sub::NORMAL,
        ));
    } else {
        list.push(Node::Kern(
            (shift_up - x.depth) - (axis + delta),
            crate::node::kern_sub::NORMAL,
        ));
        list.push(fraction_rule(t));
        list.push(Node::Kern(
            (axis - delta) - (z.height - shift_down),
            crate::node::kern_sub::NORMAL,
        ));
    }
    list.push(Node::Box(alloc::boxed::Box::new(z)));
    let v = crate::node::TeXBox {
        kind: crate::node::BoxKind::Vertical,
        width,
        height,
        depth,
        set_num: 0,
        set_den: 1,
        glue_sign: crate::node::GlueSign::Normal,
        glue_order: 0,
        shift: Scaled::ZERO,
        list,
    };
    // §749: the delimiters, as tall as `\delimiterfactor` asks. With none —
    // which is every plain `\over` and `\atop` — `var_delimiter` gives an empty
    // box of `\nulldelimiterspace`.
    let d = if style.is_display() {
        ctx.delim[size][0]
    } else {
        ctx.delim[size][1]
    };
    Node::Box(alloc::boxed::Box::new(crate::node::hpack(
        alloc::vec![
            Node::Box(alloc::boxed::Box::new(var_delimiter(
                left, size, d, ctx, metrics
            ))),
            Node::Box(alloc::boxed::Box::new(v)),
            Node::Box(alloc::boxed::Box::new(var_delimiter(
                right, size, d, ctx, metrics
            ))),
        ],
        Option::None,
        metrics,
    )))
}

/// `tex.web` §750–§751 `make_op`: a large operator.
///
/// Two jobs. In display style the glyph is taken one step up its charlist —
/// that is what makes `\sum` bigger in a display than in a line — and it is
/// centred on the axis. And when it takes *limits*, its scripts go above and
/// below rather than beside, stacked on the operator in a vertical box.
///
/// Returns the nucleus's nodes, the italic correction that offsets the scripts
/// when they stay beside it, and whether the scripts were used up here.
fn make_op(
    noad: &Noad,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> (Vec<Node>, Scaled, bool) {
    let size = style.size();
    let limits = match noad.limits {
        Limits::Display => style.is_display(),
        Limits::Limits => true,
        Limits::NoLimits => false,
    };
    let mut nucleus = noad.nucleus.clone();
    let mut delta = Scaled::ZERO;
    if let Field::Char { family, code } = nucleus {
        // §749 `make_op` opens with `fetch(nucleus(q))`, *before* the limit
        // boxes are cleaned — so an operator out of an undefined family is
        // reported ahead of its own superscript, and §723's
        // `math_type(a):=empty` is what stops the nucleus being reported a
        // second time when the box is built.
        let font = fetch_char(size, family, code, ctx, metrics);
        if font.is_none() {
            nucleus = Field::Empty;
        }
        if let Some(f) = font {
            let mut c = code;
            // §750: in display style, one size larger if the font offers one.
            if style.is_display() && metrics.char_tag(f, c) == 2 {
                let bigger = metrics.char_remainder(f, c);
                if metrics.char_exists(f, bigger) {
                    c = bigger;
                }
            }
            delta = metrics.italic(f, c);
            nucleus = Field::Char { family, code: c };
        }
        // §749 tests `math_type(nucleus(q))=math_char` before `fetch`. If the
        // family or glyph is missing, `fetch` empties the field but TeX still
        // calls `clean_box` and centres that empty box on the axis. Dropping it
        // is dimensionally harmless, but makes the diagnostic node list differ.
        let mut x = clean_box(&nucleus, style, ctx, metrics);
        if !noad.subscript.is_empty() && !limits {
            // The correction becomes the offset between the scripts instead,
            // so it must not also be in the width.
            x.width = x.width - delta;
        }
        x.shift = half(x.height - x.depth) - ctx.axis[size];
        nucleus = Field::List(alloc::vec![Node::Box(alloc::boxed::Box::new(x))]);
    }
    if !limits {
        let mut out = Vec::new();
        emit_field(&nucleus, style, ctx, metrics, &mut out);
        return (out, delta, false);
    }
    // §751: the limits go into a vertical box with the operator.
    let x = clean_box(&noad.superscript, style.sup(), ctx, metrics);
    let y = clean_box(&nucleus, style, ctx, metrics);
    let z = clean_box(&noad.subscript, style.sub(), ctx, metrics);
    let w = max2(max2(x.width, y.width), z.width);
    let mut x = rebox(x, w, metrics);
    let y = rebox(y, w, metrics);
    let mut z = rebox(z, w, metrics);
    x.shift = half(delta);
    z.shift = Scaled(-x.shift.0);
    let mut height = y.height;
    let mut depth = y.depth;
    let sp = ctx.big_op_spacing[size];
    let mut list: Vec<Node> = Vec::new();
    if !noad.superscript.is_empty() {
        let shift_up = max2(sp[2] - x.depth, sp[0]);
        height = height + sp[4] + x.height + x.depth + shift_up;
        list.push(Node::Kern(sp[4], crate::node::kern_sub::NORMAL));
        list.push(Node::Box(alloc::boxed::Box::new(x)));
        list.push(Node::Kern(shift_up, crate::node::kern_sub::NORMAL));
    }
    let ydepth = y.depth;
    let yheight = y.height;
    list.push(Node::Box(alloc::boxed::Box::new(y)));
    let _ = (ydepth, yheight);
    if !noad.subscript.is_empty() {
        let shift_down = max2(sp[3] - z.height, sp[1]);
        depth = depth + sp[4] + z.height + z.depth + shift_down;
        list.push(Node::Kern(shift_down, crate::node::kern_sub::NORMAL));
        list.push(Node::Box(alloc::boxed::Box::new(z)));
        list.push(Node::Kern(sp[4], crate::node::kern_sub::NORMAL));
    }
    let v = crate::node::TeXBox {
        kind: crate::node::BoxKind::Vertical,
        width: w,
        height,
        depth,
        set_num: 0,
        set_den: 1,
        glue_sign: crate::node::GlueSign::Normal,
        glue_order: 0,
        shift: Scaled::ZERO,
        list,
    };
    (
        alloc::vec![Node::Box(alloc::boxed::Box::new(v))],
        delta,
        true,
    )
}

/// `tex.web` §738 `make_math_accent`: an accent centred over the nucleus.
///
/// The accent is skewed by the nucleus's kern against the font's `\skewchar`,
/// grown up its charlist while a wider variant still fits over the nucleus, and
/// lowered so it sits on the x-height rather than above whatever the nucleus
/// happens to reach.
fn make_math_accent(
    noad: &Noad,
    family: usize,
    code: u32,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> Option<(Vec<Node>, bool)> {
    // §738 opens with `fetch(accent_chr(q))`, so §723 applies to the accent's
    // own character as much as to a nucleus — and when the fetch fails,
    // `char_exists(cur_i)` is false and the whole accent block is skipped:
    //
    //     begin fetch(accent_chr(q));
    //     if char_exists(cur_i) then
    //       begin i:=cur_i; c:=cur_c; f:=cur_f; … end;
    //     end;
    //
    // *Skipped*, not the noad with it: the nucleus is still converted by the
    // caller. `None` here says so.
    let size = style.size();
    let f = fetch_char(size, family, code, ctx, metrics)?;
    // The skew: how far the nucleus's own kern against the `\skewchar` moves
    // the accent along.
    let mut s = Scaled::ZERO;
    if let Field::Char {
        family: nf,
        code: nc,
    } = noad.nucleus
    {
        if let Some(nfont) = (ctx.fonts)(size, nf) {
            s = metrics.skew(nfont, nc);
        }
    }
    let mut consumed = false;
    let x = clean_box(&noad.nucleus, style.cramped(), ctx, metrics);
    let w = x.width;
    let h = x.height;
    // A wider accent, while it is still no wider than what it covers.
    let mut c = code;
    while metrics.char_tag(f, c) == 2 {
        let y = metrics.char_remainder(f, c);
        if !metrics.char_exists(f, y) || metrics.width(f, y) > w {
            break;
        }
        c = y;
    }
    // §738 says `x_height(f)`: the parameter belongs to the font that
    // supplied the accent, not family 2's math-symbol font. They ordinarily
    // agree, but TRIP gives them deliberately different negative values.
    let x_height = metrics.x_height(f);
    let mut delta = if h < x_height { h } else { x_height };
    // §738: an accented character that carries scripts keeps them *inside* —
    // the accent goes over the character and its scripts together, so they are
    // not attached again outside. Attaching them outside instead put a
    // subscript an italic correction too far right.
    let mut x = x;
    let mut h = h;
    if (!noad.superscript.is_empty() || !noad.subscript.is_empty())
        && matches!(noad.nucleus, Field::Char { .. })
    {
        let inner = Noad {
            nucleus: noad.nucleus.clone(),
            superscript: noad.superscript.clone(),
            subscript: noad.subscript.clone(),
            ..Default::default()
        };
        x = clean_box(&Field::Sub(alloc::vec![inner]), style, ctx, metrics);
        delta = delta + x.height - h;
        h = x.height;
        // `w` is deliberately *not* updated: §738 centres the accent over the
        // *character*, not over the character and its scripts. The box's own
        // width does come from the swapped box, below.
        consumed = true;
    }
    let mut y = char_box(f, c, metrics);
    y.shift = s + half(w - y.width);
    y.width = Scaled::ZERO;
    let xw = x.width;
    let list = alloc::vec![
        Node::Box(alloc::boxed::Box::new(y)),
        Node::Kern(Scaled(-delta.0), crate::node::kern_sub::NORMAL),
        Node::Box(alloc::boxed::Box::new(x))
    ];
    let mut v = crate::node::vpack(list, Option::None, Scaled(i32::MAX));
    v.width = xw;
    if v.height < h {
        // §738: the accent may not make the atom shorter than its nucleus.
        let pad = h - v.height;
        v.list
            .insert(0, Node::Kern(pad, crate::node::kern_sub::NORMAL));
        v.height = h;
    }
    Some((alloc::vec![Node::Box(alloc::boxed::Box::new(v))], consumed))
}

/// Emit one noad's nucleus, then its scripts.
fn translate(
    noad: &Noad,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
    text_char: bool,
    out: &mut Vec<Node>,
) {
    // A fraction is not an atom with a nucleus: it *is* the construction.
    if let Kind::Fraction(t, left, right) = noad.kind {
        out.push(make_fraction(noad, t, left, right, style, ctx, metrics));
        return;
    }
    // §749: a large operator is not an ordinary atom — it is grown, centred on
    // the axis, and may take its scripts as limits above and below.
    //
    // `before` is taken here, not after: §756 measures the nucleus as it was
    // *set*, and for an operator that is the box built below. Taking it after
    // left the scripts measuring an empty list, so they hung off the baseline
    // and sat inside the operator instead of clearing it.
    let before = out.len();
    let mut op_delta = Scaled::ZERO;
    let mut accent_took_scripts = false;
    if noad.class == Class::Op && noad.kind == Kind::Normal {
        let (nodes, delta, consumed) = make_op(noad, style, ctx, metrics);
        out.extend(nodes);
        if consumed {
            return;
        }
        op_delta = delta;
    }
    if noad.class == Class::Op && noad.kind == Kind::Normal {
        // already emitted above
    } else {
        match noad.kind {
            // §734/§735: a rule above or below the nucleus, at three times the
            // default rule thickness of clear space.
            Kind::Over => {
                let t = ctx.default_rule_thickness[style.size()];
                // §734: the contents go under a bar, so they are set *cramped*.
                let b = clean_box(&noad.nucleus, style.cramped(), ctx, metrics);
                out.push(Node::Box(alloc::boxed::Box::new(overbar(
                    b,
                    Scaled(3 * t.0),
                    t,
                ))));
            }
            Kind::Under => {
                let t = ctx.default_rule_thickness[style.size()];
                let x = clean_box(&noad.nucleus, style, ctx, metrics);
                let (xh, _) = (x.height, x.depth);
                let list = alloc::vec![
                    Node::Box(alloc::boxed::Box::new(x)),
                    Node::Kern(Scaled(3 * t.0), crate::node::kern_sub::NORMAL),
                    fraction_rule(t),
                ];
                let mut y = crate::node::vpack(list, Option::None, Scaled(i32::MAX));
                // §735: the rule hangs below the baseline; the box keeps the
                // nucleus's height and takes the rest as depth.
                let total = y.height + y.depth + t;
                y.height = xh;
                y.depth = total - xh;
                out.push(Node::Box(alloc::boxed::Box::new(y)));
            }
            // §737: the radical sign is grown to clear the nucleus, dropped so its
            // own bar sits at the top, and the nucleus goes under a bar of its own.
            Kind::Radical(d) => {
                let t = ctx.default_rule_thickness[style.size()];
                let x = clean_box(&noad.nucleus, style.cramped(), ctx, metrics);
                let mut clr = if style.is_display() {
                    t + Scaled(ctx.x_height[style.size()].0.abs() / 4)
                } else {
                    t + Scaled(t.0.abs() / 4)
                };
                let mut y =
                    var_delimiter(d, style.size(), x.height + x.depth + clr + t, ctx, metrics);
                let delta = y.depth - (x.height + x.depth + clr);
                if delta.0 > 0 {
                    // The sign came out taller than asked for; the slack becomes
                    // extra clearance rather than a gap under the bar.
                    clr = clr + half(delta);
                }
                let yh = y.height;
                y.shift = Scaled(-(x.height + clr).0);
                let bar = overbar(x, clr, yh);
                out.push(Node::Box(alloc::boxed::Box::new(crate::node::hpack(
                    alloc::vec![
                        Node::Box(alloc::boxed::Box::new(y)),
                        Node::Box(alloc::boxed::Box::new(bar))
                    ],
                    Option::None,
                    metrics,
                ))));
            }
            Kind::Choice => {
                if let Some(c) = &noad.choices {
                    let i = if style.is_display() {
                        0
                    } else {
                        style.size() + 1
                    };
                    out.extend(mlist_to_hlist(c[i].clone(), style, ctx, metrics));
                }
            }
            // §734 `make_vcenter`: the box is centred on the axis.
            Kind::VCenter => {
                let mut inner = Vec::new();
                emit_field(&noad.nucleus, style, ctx, metrics, &mut inner);
                for n in inner.iter_mut() {
                    if let Node::Box(b) = n {
                        let delta = b.height + b.depth;
                        b.height = ctx.axis[style.size()] + half(delta);
                        b.depth = delta - b.height;
                    }
                }
                out.extend(inner);
            }
            Kind::Accent(family, code) => {
                match make_math_accent(noad, family, code, style, ctx, metrics) {
                    Some((nodes, consumed)) => {
                        out.extend(nodes);
                        accent_took_scripts = consumed;
                    }
                    // The accent was dropped; the nucleus is not.
                    None => emit_field(&noad.nucleus, style, ctx, metrics, out),
                }
            }
            _ => emit_field(&noad.nucleus, style, ctx, metrics, out),
        }
    }
    let nucleus_width: Scaled = out[before..]
        .iter()
        .map(|n| node_width(n, metrics))
        .fold(Scaled::ZERO, |a, b| a + b);
    let _ = nucleus_width;

    // `tex.web` §749: a character nucleus is followed by its *italic
    // correction* — the letters of a math font lean right, and whatever comes
    // next has to clear the overhang. TeX drops it when there is a subscript
    // (the correction becomes the offset between the two scripts instead), and
    // when the character came from a *text* font, which is what `\rm x` inside
    // a formula does.
    // §749 exactly: the correction is *appended* only when there is no
    // subscript. With one, it becomes the offset between the two scripts
    // instead — the superscript sits that much to the right of the subscript.
    let is_op = noad.class == Class::Op && noad.kind == Kind::Normal;
    // §749's italic correction is for a nucleus that *is* a character. Every
    // construction here — a fraction, a rule, a radical, an accent — has
    // replaced the nucleus with a box, and TeX adds no correction after one.
    let plain_char = noad.kind == Kind::Normal;
    let mut delta = op_delta;
    if !is_op && plain_char {
        if let Field::Char { family, code } = noad.nucleus {
            if let Some(f) = (ctx.fonts)(style.size(), family) {
                let d = metrics.italic(f, code);
                // §755's `(math_type=math_text_char) and (space(cur_f)<>0)`.
                // Both halves: a math font (`space` 0) always corrects, and a
                // text font corrects the character that ends the run.
                if d.0 != 0 && !(text_char && metrics.has_space(f)) {
                    delta = d;
                }
            }
        }
        if noad.subscript.is_empty() && delta.0 != 0 {
            out.push(Node::Kern(delta, crate::node::kern_sub::NORMAL));
            delta = Scaled::ZERO;
        }
    }
    if accent_took_scripts || (noad.superscript.is_empty() && noad.subscript.is_empty()) {
        return;
    }
    // Scripts are set one style smaller and shifted from the baseline. How far
    // is `tex.web` §756–759, and it is not one number: it starts from the
    // nucleus's own height and depth, is bounded below by the font's `sup`/`sub`
    // parameters for the current style, and bounded again so a superscript
    // never sits lower than four-fifths of the x-height.
    let size = style.size();
    // §756: the nucleus's extent, measured from a clean box of it.
    // §756 measures what the nucleus actually *became* — `new_hlist(q)` — not
    // the field it was written as. For `\overline{pq}^2` that is the overbar
    // box, and the superscript clears the bar because of it. A nucleus that
    // translated to a plain character is the special case: its scripts hang off
    // the baseline, so the shift starts at zero.
    let (nuc_h, nuc_d) = if matches!(out.get(before), Some(Node::Char { .. }) | None) {
        (Scaled::ZERO, Scaled::ZERO)
    } else {
        let b = crate::node::hpack(out[before..].to_vec(), Option::None, metrics);
        (b.height, b.depth)
    };
    // `sup_drop` and `sub_drop` are read at the size the scripts will be set
    // in — one smaller than this noad's — not at this noad's own size.
    let script_size = if size == 0 { 1 } else { 2 };
    let mut shift_up = nuc_h - ctx.sup_drop[script_size];
    let mut shift_down = nuc_d + ctx.sub_drop[script_size];
    if shift_up.0 < 0 {
        shift_up = Scaled::ZERO;
    }
    if shift_down.0 < 0 {
        shift_down = Scaled::ZERO;
    }

    if noad.superscript.is_empty() {
        // §757: a subscript alone.
        let mut b = Node::Box(alloc::boxed::Box::new(clean_box(
            &noad.subscript,
            style.sub(),
            ctx,
            metrics,
        )));
        add_script_space(&mut b, ctx.script_space);
        let (h, _) = box_extent(&b);
        // §757: a subscript may not rise above four-fifths of the x-height.
        let clr = h - four_fifths(ctx.x_height[size]);
        let shift = max3(shift_down, ctx.sub[size][0], clr);
        set_shift(&mut b, shift);
        out.push(b);
        return;
    }

    // §758: a superscript. Which `sup` parameter applies depends on the style:
    // `sup3` in a cramped style, `sup1` in display, `sup2` otherwise.
    let mut sup_box = {
        let mut b = Node::Box(alloc::boxed::Box::new(clean_box(
            &noad.superscript,
            style.sup(),
            ctx,
            metrics,
        )));
        add_script_space(&mut b, ctx.script_space);
        b
    };
    // §758: `sup3` in a cramped style, `sup1` in display, `sup2` otherwise.
    let clr_param = if style.is_cramped() {
        ctx.sup[size][2]
    } else if style.is_display() {
        ctx.sup[size][0]
    } else {
        ctx.sup[size][1]
    };
    let (_, sup_d) = box_extent(&sup_box);
    // §758: a superscript may not descend below a *quarter* of the x-height —
    // a different fraction from the subscript's, and the asymmetry is real: a
    // superscript hangs above the axis and a subscript below it.
    shift_up = max3(shift_up, clr_param, sup_d + quarter(ctx.x_height[size]));

    if noad.subscript.is_empty() {
        set_shift(&mut sup_box, Scaled(-shift_up.0));
        out.push(sup_box);
        return;
    }
    // §759: both. They are stacked into one *vertical* box — side by side is
    // not a near miss, it is a different formula — kept far enough apart that
    // they cannot touch, and the whole box is lowered by `shift_down`.
    let mut sub_box = {
        let mut b = Node::Box(alloc::boxed::Box::new(clean_box(
            &noad.subscript,
            style.sub(),
            ctx,
            metrics,
        )));
        add_script_space(&mut b, ctx.script_space);
        b
    };
    shift_down = max2(shift_down, ctx.sub[size][1]);
    let (_, sup_d2) = box_extent(&sup_box);
    let (sub_h, _) = box_extent(&sub_box);
    let drt = ctx.default_rule_thickness[size];
    let mut clr = Scaled(4 * drt.0) - ((shift_up - sup_d2) - (sub_h - shift_down));
    if clr.0 > 0 {
        shift_down = shift_down + clr;
        // §759: having pushed the subscript down, the superscript is raised
        // instead if that leaves it sitting too low.
        clr = four_fifths(ctx.x_height[size]) - (shift_up - sup_d2);
        if clr.0 > 0 {
            shift_up = shift_up + clr;
            shift_down = shift_down - clr;
        }
    }
    // In a vertical list a box's shift is *horizontal*, which is how the
    // superscript ends up `delta` to the right of the subscript.
    set_shift(&mut sup_box, delta);
    let gap = (shift_up - sup_d2) - (sub_h - shift_down);
    let mut v = crate::node::vpack(
        alloc::vec![
            sup_box,
            Node::Kern(gap, crate::node::kern_sub::NORMAL),
            sub_box
        ],
        Option::None,
        Scaled(i32::MAX),
    );
    v.shift = shift_down;
    out.push(Node::Box(alloc::boxed::Box::new(v)));
}

/// Four-fifths of the x-height — the bound `tex.web` §757/§758 puts on how
/// low a script may sit.
fn four_fifths(s: Scaled) -> Scaled {
    Scaled(((s.0.abs() as i64 * 4) / 5) as i32)
}

/// A quarter of the x-height — the bound `tex.web` §758 puts on a superscript.
fn quarter(s: Scaled) -> Scaled {
    Scaled(s.0.abs() / 4)
}

fn max2(a: Scaled, b: Scaled) -> Scaled {
    if a.0 >= b.0 { a } else { b }
}

fn max3(a: Scaled, b: Scaled, c: Scaled) -> Scaled {
    max2(max2(a, b), c)
}

fn set_shift(n: &mut Node, shift: Scaled) {
    if let Node::Box(b) = n {
        b.shift = shift;
    }
}

fn box_extent(n: &Node) -> (Scaled, Scaled) {
    match n {
        Node::Box(b) => (b.height, b.depth),
        _ => (Scaled::ZERO, Scaled::ZERO),
    }
}

/// `tex.web` §756: a script box is widened by `\scriptspace`.
fn add_script_space(n: &mut Node, space: Scaled) {
    if let Node::Box(b) = n {
        b.width = b.width + space;
    }
}

fn emit_field(
    f: &Field,
    style: Style,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
    out: &mut Vec<Node>,
) {
    match f {
        Field::Empty => {}
        Field::Sub(noads) => {
            // `tex.web` §754: a sub-formula nucleus is *packed into a box*, not
            // spliced in. That is what stops the glue inside `${a+b}$` or
            // `$\left(a+b\right)$` from stretching when the line does — it is
            // already set, at its natural width.
            let nodes = mlist_to_hlist(noads.clone(), style, ctx, metrics);
            out.push(Node::Box(alloc::boxed::Box::new(crate::node::hpack(
                nodes,
                Option::None,
                metrics,
            ))));
        }
        Field::Char { family, code } => {
            if let Some(font) = fetch_char(style.size(), *family, *code, ctx, metrics) {
                out.push(Node::Char {
                    code: *code,
                    font,
                    lig: None,
                });
            }
        }
        Field::List(nodes) => out.extend(nodes.iter().cloned()),
    }
}

/// `tex.web` §722–§723 `fetch`: resolve a family and reject a character
/// absent from its TFM. Both failures empty the field; only the diagnostic
/// differs. In particular, an absent glyph is not a zero-width character.
fn fetch_char(
    size: usize,
    family: usize,
    code: u32,
    ctx: &MathContext<'_>,
    metrics: &impl CharMetrics,
) -> Option<usize> {
    match (ctx.fonts)(size, family) {
        Some(font) if font != 0 => {
            if metrics.char_exists(font, code) {
                Some(font)
            } else {
                ctx.font_issues
                    .borrow_mut()
                    .push(MathFontIssue::MissingCharacter { font, code });
                None
            }
        }
        _ => {
            ctx.font_issues
                .borrow_mut()
                .push(MathFontIssue::UndefinedFamily { size, family, code });
            None
        }
    }
}

fn node_width(n: &Node, metrics: &impl CharMetrics) -> Scaled {
    match n {
        Node::Char { code, font, .. } => metrics.width(*font, *code),
        Node::Glue(g) => g.width,
        Node::Kern(k, _) => *k,
        Node::Box(b) => b.width,
        _ => Scaled::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl CharMetrics for Fixed {
        fn width(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(5)
        }
        fn height(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::from_points(7)
        }
        fn depth(&self, _f: usize, _c: u32) -> Scaled {
            Scaled::ZERO
        }
        fn char_exists(&self, _f: usize, _c: u32) -> bool {
            true
        }
    }

    fn ctx() -> MathContext<'static> {
        MathContext {
            // These are mu values. A quad of 18pt below keeps one mu equal to
            // one point in the ordinary test context.
            thin: Glue::fixed(Scaled::from_points(3)),
            med: Glue::fixed(Scaled::from_points(4)),
            thick: Glue::fixed(Scaled::from_points(5)),
            quad: [Scaled::from_points(18); 3],
            // Not font 0: §722's test for an undefined family is
            // `cur_f=null_font`, and `\nullfont` *is* font 0. A test context
            // that answered 0 was saying every family was undefined, so §723
            // dropped every character before it could be spaced.
            fonts: &|_size, _fam| Some(1),
            font_issues: {
                use alloc::boxed::Box;
                Box::leak(Box::new(core::cell::RefCell::new(alloc::vec::Vec::new())))
            },
            axis: [Scaled::from_points(2); 3],
            sup: [[Scaled::from_points(4); 3]; 3],
            sub: [[Scaled::from_points(2); 2]; 3],
            sup_drop: [Scaled::ZERO; 3],
            sub_drop: [Scaled::ZERO; 3],
            x_height: [Scaled::ZERO; 3],
            script_space: Scaled::ZERO,
            default_rule_thickness: [Scaled::from_points(1); 3],
            num: [[Scaled::from_points(5); 3]; 3],
            denom: [[Scaled::from_points(3); 2]; 3],
            null_delimiter_space: Scaled::from_points(1),
            delim: [[Scaled::from_points(10), Scaled::from_points(8)]; 3],
            delimiter_factor: 901,
            delimiter_shortfall: Scaled::from_points(5),
            big_op_spacing: [[Scaled::from_points(1); 5]; 3],
            bin_op_penalty: 700,
            rel_penalty: 500,
        }
    }

    /// `tex.web` §100's `half` rounds an odd value away from zero — one scaled
    /// point, and every glyph in a centred fraction after it.
    #[test]
    fn half_rounds_odd_values_up() {
        assert_eq!(half(Scaled(4)), Scaled(2));
        assert_eq!(half(Scaled(5)), Scaled(3));
        assert_eq!(half(Scaled(-5)), Scaled(-2));
    }

    /// §715 `rebox` centres what it holds with infinite glue at each end.
    #[test]
    fn rebox_centres_what_it_holds() {
        let b = crate::node::hpack(
            alloc::vec![Node::Kern(
                Scaled::from_points(4),
                crate::node::kern_sub::NORMAL
            )],
            None,
            &Fixed,
        );
        let r = rebox(b, Scaled::from_points(10), &Fixed);
        assert_eq!(r.width, Scaled::from_points(10));
        // ss glue at each end, splitting the 6pt of slack
        assert_eq!(r.list.len(), 3);
    }

    /// `tex.web` §683's delimiter encoding: `"sSSlLL`.
    #[test]
    fn a_delimiter_code_splits_into_two_variants() {
        // plain's `\delcode`\(="028300
        let d = Delim::from_code(0x028300);
        assert_eq!(d.small_fam, 0);
        assert_eq!(d.small_char, 0x28);
        assert_eq!(d.large_fam, 3);
        assert_eq!(d.large_char, 0x00);
        // `\sqrt` is `\radical"270370`
        let r = Delim::from_code(0x270370);
        assert_eq!(r.small_fam, 2);
        assert_eq!(r.small_char, 0x70);
        assert_eq!(r.large_fam, 3);
        assert_eq!(r.large_char, 0x70);
    }

    fn atom(class: Class, c: char) -> Noad {
        Noad {
            class,
            nucleus: Field::Char {
                family: 0,
                code: c as u32,
            },
            subscript: Field::Empty,
            superscript: Field::Empty,
            ..Default::default()
        }
    }

    /// The mathcode's top nibble is the class.
    #[test]
    fn a_mathcode_carries_its_class() {
        assert_eq!(Class::from_mathcode(0x0161), Class::Ord);
        assert_eq!(Class::from_mathcode(0x2B), Class::Ord); // no class nibble
        assert_eq!(Class::from_mathcode(0x2200 + 0x2B), Class::Bin);
        assert_eq!(Class::from_mathcode(0x3000 + 0x3D), Class::Rel);
        assert_eq!(Class::from_mathcode(0x4000 + 0x28), Class::Open);
    }

    /// `tex.web` §764's table, spot-checked where it is asymmetric.
    #[test]
    fn the_spacing_table_is_asymmetric_where_tex_says() {
        // A relation is thickly spaced on both sides.
        assert_eq!(spacing(Class::Ord, Class::Rel), Spacing::ThickConditional);
        assert_eq!(spacing(Class::Rel, Class::Ord), Spacing::ThickConditional);
        // Punctuation takes space after it, never before.
        assert_eq!(spacing(Class::Punct, Class::Ord), Spacing::ThinConditional);
        assert_eq!(spacing(Class::Ord, Class::Punct), Spacing::None);
        // An opening delimiter hugs whatever follows.
        assert_eq!(spacing(Class::Open, Class::Ord), Spacing::None);
        // Two ordinary symbols touch: `xy` is not `x y`.
        assert_eq!(spacing(Class::Ord, Class::Ord), Spacing::None);
    }

    /// The rule that makes `$-x$` and `$a-x$` differ.
    #[test]
    fn a_binary_operator_with_nothing_to_its_left_is_unary() {
        let mut l = alloc::vec![atom(Class::Bin, '-'), atom(Class::Ord, 'x')];
        fix_binaries(&mut l);
        assert_eq!(
            l[0].class,
            Class::Ord,
            "leading - is a sign, not an operator"
        );

        let mut l = alloc::vec![
            atom(Class::Ord, 'a'),
            atom(Class::Bin, '-'),
            atom(Class::Ord, 'x')
        ];
        fix_binaries(&mut l);
        assert_eq!(l[1].class, Class::Bin, "between operands it stays binary");

        // And after a relation it is unary again: `$a = -x$`.
        let mut l = alloc::vec![
            atom(Class::Ord, 'a'),
            atom(Class::Rel, '='),
            atom(Class::Bin, '-'),
            atom(Class::Ord, 'x')
        ];
        fix_binaries(&mut l);
        assert_eq!(l[2].class, Class::Ord);
    }

    /// Spacing lands between atoms, and only between them.
    #[test]
    fn glue_appears_between_atoms_by_class() {
        let l = alloc::vec![
            atom(Class::Ord, 'a'),
            atom(Class::Bin, '+'),
            atom(Class::Ord, 'b')
        ];
        let h = mlist_to_hlist(l, Style::Text, &ctx(), &Fixed);
        // a, glue, +, glue, b
        assert_eq!(h.len(), 5, "got {h:?}");
        assert!(matches!(h[1], Node::Glue(_)));
        assert!(matches!(h[3], Node::Glue(_)));

        // `xy` has nothing between it.
        let l = alloc::vec![atom(Class::Ord, 'x'), atom(Class::Ord, 'y')];
        let h = mlist_to_hlist(l, Style::Text, &ctx(), &Fixed);
        assert_eq!(h.len(), 2);
    }

    /// `tex.web` §761's `q<>null` guard suppresses the automatic relation
    /// penalty when the relation is the final math-list atom. The closing math
    /// node is added by the caller and does not satisfy that guard.
    #[test]
    fn a_final_relation_gets_no_inline_math_penalty() {
        let h = mlist_to_hlist_pen(
            alloc::vec![atom(Class::Rel, '=')],
            Style::Text,
            &ctx(),
            &Fixed,
            true,
        );
        assert!(
            h.iter().all(|node| !matches!(node, Node::Penalty(_))),
            "the final relation must not carry relpenalty: {h:?}"
        );
    }

    /// A relation followed by another atom still receives `\relpenalty`.
    #[test]
    fn a_nonfinal_relation_gets_an_inline_math_penalty() {
        let h = mlist_to_hlist_pen(
            alloc::vec![atom(Class::Rel, '='), atom(Class::Ord, 'x')],
            Style::Text,
            &ctx(),
            &Fixed,
            true,
        );
        assert!(
            matches!(h.get(1), Some(Node::Penalty(500))),
            "the nonfinal relation must carry relpenalty: {h:?}"
        );
    }

    /// An explicit penalty immediately after the final relation takes
    /// precedence over the automatic `\relpenalty`, just as it does between
    /// two atoms.
    #[test]
    fn a_trailing_explicit_math_penalty_suppresses_the_automatic_one() {
        let explicit = Noad {
            kind: Kind::Raw,
            nucleus: Field::List(alloc::vec![Node::Penalty(42)]),
            ..Default::default()
        };
        let h = mlist_to_hlist_pen(
            alloc::vec![atom(Class::Rel, '='), explicit],
            Style::Text,
            &ctx(),
            &Fixed,
            true,
        );
        assert_eq!(
            h.iter()
                .filter_map(|n| match n {
                    Node::Penalty(p) => Some(*p),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            alloc::vec![42]
        );
    }

    /// A math choice is replaced by its selected list; it is not an atom of
    /// its own. This is the spacing rule amsmath's multi-integral macros rely
    /// on between two operator atoms.
    #[test]
    fn a_math_choice_is_transparent_to_atom_spacing() {
        let raw_kern = |points| Noad {
            kind: Kind::Raw,
            nucleus: Field::List(alloc::vec![Node::Kern(
                Scaled::from_points(points),
                crate::node::kern_sub::MU,
            )]),
            ..Default::default()
        };
        let choice = Noad {
            kind: Kind::Choice,
            choices: Some(alloc::boxed::Box::new([
                alloc::vec![raw_kern(-3)],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ])),
            ..Default::default()
        };
        let list = alloc::vec![
            atom(Class::Op, 'I'),
            raw_kern(-6),
            choice,
            atom(Class::Op, 'I'),
        ];

        let display = mlist_to_hlist(list.clone(), Style::Display, &ctx(), &Fixed);
        assert_eq!(
            display
                .iter()
                .filter(|node| matches!(node, Node::Glue(_)))
                .count(),
            1,
            "the two operators have one automatic thin space: {display:?}"
        );
        assert_eq!(
            display
                .iter()
                .filter(|node| matches!(node, Node::Kern(..)))
                .count(),
            2,
            "display style selects the extra -3mu kern: {display:?}"
        );

        let text = mlist_to_hlist(list, Style::Text, &ctx(), &Fixed);
        assert_eq!(
            text.iter()
                .filter(|node| matches!(node, Node::Glue(_)))
                .count(),
            1,
            "an empty branch is transparent too: {text:?}"
        );
        assert_eq!(
            text.iter()
                .filter(|node| matches!(node, Node::Kern(..)))
                .count(),
            1,
            "text style does not select the display-only kern: {text:?}"
        );
    }

    /// e-TeX's `\middle` closes the segment on its left and opens the one on
    /// its right. Thus the explicit `\,` in `\middle|\,\lim` is the only thin
    /// space there; treating the delimiter as Close on both faces adds a
    /// second one and widens every centred formula containing this idiom.
    #[test]
    fn a_middle_delimiter_has_open_spacing_on_its_right() {
        let explicit_thin = Noad {
            kind: Kind::Raw,
            nucleus: Field::List(alloc::vec![Node::Glue(Glue {
                width: Scaled::from_points(3),
                subtype: crate::node::glue_sub::MU,
                ..Default::default()
            })]),
            ..Default::default()
        };
        let l = alloc::vec![
            atom(Class::Ord, 'x'),
            Noad {
                class: Class::Close,
                kind: Kind::MiddleDelim(Delim::default()),
                ..Default::default()
            },
            explicit_thin,
            atom(Class::Op, 'y'),
        ];
        let h = mlist_to_hlist(l, Style::Text, &ctx(), &Fixed);
        assert_eq!(
            h.iter().filter(|n| matches!(n, Node::Glue(_))).count(),
            1,
            "only the explicit thin space remains: {h:?}"
        );

        let mut binary = alloc::vec![
            atom(Class::Ord, 'x'),
            Noad {
                class: Class::Close,
                kind: Kind::MiddleDelim(Delim::default()),
                ..Default::default()
            },
            atom(Class::Bin, '-'),
            atom(Class::Ord, 'y'),
        ];
        fix_binaries(&mut binary);
        assert_eq!(binary[2].class, Class::Ord, "a sign after middle is unary");
    }

    #[test]
    fn a_raw_rule_sizes_the_delimiters_around_it() {
        struct DelimiterMetrics;
        impl CharMetrics for DelimiterMetrics {
            fn width(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::from_points(5)
            }
            fn height(&self, font: usize, _code: u32) -> Scaled {
                Scaled::from_points(if font == 4 { 20 } else { 7 })
            }
            fn depth(&self, _font: usize, _code: u32) -> Scaled {
                Scaled::ZERO
            }
            fn char_exists(&self, _font: usize, _code: u32) -> bool {
                true
            }
        }

        let mut c = ctx();
        c.fonts = &|_size, family| Some(family + 1);
        let delimiter = Delim {
            small_fam: 2,
            small_char: b'|' as u32,
            large_fam: 3,
            large_char: b'|' as u32,
        };
        let list = alloc::vec![
            Noad {
                class: Class::Open,
                kind: Kind::LeftDelim(delimiter),
                ..Default::default()
            },
            Noad {
                kind: Kind::Raw,
                nucleus: Field::List(alloc::vec![Node::Rule {
                    width: Scaled::from_points(-1),
                    height: Scaled::from_points(15),
                    depth: Scaled::RUNNING,
                }]),
                ..Default::default()
            },
            Noad {
                class: Class::Close,
                kind: Kind::RightDelim(Delim::default()),
                ..Default::default()
            },
        ];

        let h = mlist_to_hlist(list, Style::Text, &c, &DelimiterMetrics);
        let Node::Box(left) = &h[0] else {
            panic!("left delimiter was not boxed: {h:?}");
        };
        assert!(
            matches!(left.list.as_slice(), [Node::Char { font: 4, .. }]),
            "the tall raw rule must select the large-family delimiter: {h:?}"
        );
    }

    /// §762 receives the enclosing mlist style, not the mutable style that
    /// §730 changes while translating its atoms.  This is visible when a
    /// `\displaystyle` occurs between `\left` and `\right`: the contents after
    /// it use display style, while both delimiters stay on the original axis.
    #[test]
    fn a_style_node_does_not_move_only_the_closing_delimiter() {
        let mut c = ctx();
        c.axis = [
            Scaled::from_points(3),
            Scaled::from_points(2),
            Scaled::from_points(1),
        ];
        let delimiter = Delim {
            small_fam: 1,
            small_char: b'(' as u32,
            large_fam: 1,
            large_char: b'(' as u32,
        };
        let list = alloc::vec![
            Noad {
                class: Class::Open,
                kind: Kind::LeftDelim(delimiter),
                ..Default::default()
            },
            atom(Class::Ord, 'x'),
            Noad {
                kind: Kind::Style(Style::Display),
                ..Default::default()
            },
            atom(Class::Ord, 'y'),
            Noad {
                class: Class::Close,
                kind: Kind::RightDelim(delimiter),
                ..Default::default()
            },
        ];

        let h = mlist_to_hlist(list, Style::Script, &c, &Fixed);
        let shifts: alloc::vec::Vec<_> = h
            .iter()
            .filter_map(|n| match n {
                Node::Box(b) => Some(b.shift),
                _ => None,
            })
            .collect();
        assert_eq!(shifts.len(), 2, "the pair has two delimiter boxes: {h:?}");
        assert_eq!(shifts[0], shifts[1]);
        assert_eq!(shifts[0], Scaled(98_304)); // 7pt/2 - 2pt = 1.5pt
    }

    /// Script styles drop the conditional spaces, which is what stops a
    /// subscript from being spread out.
    #[test]
    fn script_style_suppresses_conditional_spacing() {
        let l = alloc::vec![
            atom(Class::Ord, 'a'),
            atom(Class::Rel, '='),
            atom(Class::Ord, 'b')
        ];
        let text = mlist_to_hlist(l.clone(), Style::Text, &ctx(), &Fixed);
        let script = mlist_to_hlist(l, Style::Script, &ctx(), &Fixed);
        assert_eq!(text.len(), 5);
        assert_eq!(script.len(), 3, "no glue in script style");
    }

    /// Unconditional operator spacing remains present in scripts, but one mu
    /// is based on the script font's quad rather than the text font's quad.
    #[test]
    fn math_glue_uses_the_quad_for_its_style() {
        let mut c = ctx();
        c.quad = [
            Scaled::from_points(18),
            Scaled::from_points(9),
            Scaled::from_points(6),
        ];
        let text = glue_for(Spacing::Thin, Style::Text, &c).unwrap();
        let script = glue_for(Spacing::Thin, Style::Script, &c).unwrap();
        let scriptscript = glue_for(Spacing::Thin, Style::ScriptScript, &c).unwrap();
        assert_eq!(text.width, Scaled::from_points(3));
        assert_eq!(script.width, Scaled(98_304));
        // TeX divides one quad by 18 before multiplying by 3mu, so this is
        // one scaled point below 1pt rather than a single rounded division.
        assert_eq!(scriptscript.width, Scaled(65_535));
        assert_eq!(script.subtype, crate::node::glue_sub::THIN_MU_SKIP);
    }

    /// A superscript is raised and a subscript lowered, both boxed so the
    /// shift does not leak into the enclosing list.
    #[test]
    fn scripts_are_boxed_and_shifted() {
        let mut a = atom(Class::Ord, 'x');
        a.superscript = Field::Char {
            family: 0,
            code: '2' as u32,
        };
        let h = mlist_to_hlist(alloc::vec![a], Style::Text, &ctx(), &Fixed);
        let Some(Node::Box(b)) = h.get(1) else {
            panic!("expected a boxed superscript, got {h:?}");
        };
        assert!(b.shift.0 < 0, "a superscript is raised");

        let mut a = atom(Class::Ord, 'x');
        a.subscript = Field::Char {
            family: 0,
            code: '1' as u32,
        };
        let h = mlist_to_hlist(alloc::vec![a], Style::Text, &ctx(), &Fixed);
        let Some(Node::Box(b)) = h.get(1) else {
            panic!("expected a boxed subscript");
        };
        assert!(b.shift.0 > 0, "a subscript is lowered");
    }

    /// `tex.web` §702's style transitions, all four of them. A subscript is
    /// always cramped; a superscript keeps whatever cramping it inherited; a
    /// numerator likewise; a denominator is always cramped. Cramping is what
    /// makes `\overline{x^2}`'s superscript sit low enough to clear the bar.
    #[test]
    fn style_steps_down_by_texs_rules() {
        assert_eq!(Style::Display.sub(), Style::ScriptCramped);
        assert_eq!(Style::Text.sub(), Style::ScriptCramped);
        assert_eq!(Style::Script.sub(), Style::ScriptScriptCramped);
        assert_eq!(Style::ScriptScript.sub(), Style::ScriptScriptCramped);
        // a superscript keeps the cramping, and only that
        assert_eq!(Style::Text.sup(), Style::Script);
        assert_eq!(Style::TextCramped.sup(), Style::ScriptCramped);
        assert_eq!(Style::Display.sup(), Style::Script);
        // numerator and denominator
        assert_eq!(Style::Display.num(), Style::Text);
        assert_eq!(Style::Display.denom(), Style::TextCramped);
        assert_eq!(Style::Text.num(), Style::Script);
        assert_eq!(Style::Text.denom(), Style::ScriptCramped);
        assert_eq!(Style::ScriptScript.num(), Style::ScriptScript);
        assert_eq!(Style::ScriptScript.denom(), Style::ScriptScriptCramped);
        // cramping never changes the size
        for s in [
            Style::Display,
            Style::Text,
            Style::Script,
            Style::ScriptScript,
        ] {
            assert_eq!(s.size(), s.cramped().size());
            assert!(s.cramped().is_cramped());
        }
    }

    #[test]
    fn an_empty_formula_produces_nothing() {
        assert!(mlist_to_hlist(Vec::new(), Style::Text, &ctx(), &Fixed).is_empty());
    }
}

/// `tex.web` §690–§698: a math list as `\showlists` shows it, before
/// `mlist_to_hlist` has turned any of it into nodes.
///
/// It is its own format, not `show_box`'s. Each noad names itself —
/// `\mathord`, `\fraction, thickness …` — and its three subsidiary fields are
/// shown under it with a *history character* rather than a fixed indent:
/// `.` for the nucleus, `^` for the superscript, `_` for the subscript, and
/// `\` and `/` for a fraction's two halves. The history is what the reader
/// uses to tell a superscript's contents from a nucleus's at the same depth,
/// which a `.`-per-level indent cannot say.
///
/// `nodes` is how already-typeset material inside a field is shown: a `{…}`
/// group that has been packed, or a box. It is `show_box`'s job, handed in so
/// this module does not need to know about it.
pub fn show_math_list(
    list: &[Noad],
    history: &str,
    depth_threshold: usize,
    breadth_max: usize,
    escape: &str,
    nodes: &dyn Fn(&[crate::node::Node], &str) -> alloc::string::String,
) -> alloc::string::String {
    use core::fmt::Write;
    let mut out = alloc::string::String::new();
    // §182's `\showboxbreadth`, which counts *this* level's items.
    let mut shown = 0usize;
    for n in list {
        if shown >= breadth_max {
            let _ = writeln!(out, "{history}etc.");
            return out;
        }
        shown += 1;
        // §690 walks a mixed list of nodes and noads. Glue, kerns, penalties,
        // insertions, adjustments and discretionaries are nodes, so §691
        // sends them straight to `show_node_list`; they have no `\mathord`
        // wrapper and gain no nucleus-history dot. `Kind::Raw` is only this
        // representation's way to retain such a node in an mlist.
        if n.kind == Kind::Raw {
            if let Field::List(l) = &n.nucleus {
                out.push_str(&nodes(l, history));
            }
            continue;
        }
        let head = match (&n.kind, n.class) {
            (Kind::Over, _) => alloc::format!("{escape}overline"),
            (Kind::Under, _) => alloc::format!("{escape}underline"),
            (Kind::VCenter, _) => alloc::format!("{escape}vcenter"),
            (Kind::Style(s), _) => alloc::format!(
                "{escape}{}style",
                match s {
                    Style::Display => "display",
                    Style::Text => "text",
                    Style::Script => "script",
                    Style::ScriptScript => "scriptscript",
                    // §694 names the four uncramped ones; a cramped style is
                    // reached by construction, never written, so it cannot
                    // appear in a list `\showlists` sees.
                    _ => "script",
                }
            ),
            (Kind::Radical(d), _) => alloc::format!("{escape}radical{}", show_delimiter(d)),
            (Kind::Accent(f, c), _) => {
                alloc::format!("{escape}accent{}", show_fam_and_char(*f, *c, escape))
            }
            (Kind::LeftDelim(d), _) => alloc::format!("{escape}left{}", show_delimiter(d)),
            (Kind::RightDelim(d), _) => alloc::format!("{escape}right{}", show_delimiter(d)),
            (Kind::MiddleDelim(d), _) => alloc::format!("{escape}middle{}", show_delimiter(d)),
            // §697: a fraction names its thickness, and its delimiters only
            // when they are not the null one.
            (Kind::Fraction(t, l, r), _) => {
                let mut s = alloc::format!("{escape}fraction, thickness ");
                match t {
                    None => s.push_str("= default"),
                    Some(v) => s.push_str(&crate::scaled::print_scaled(crate::scaled::Scaled(*v))),
                }
                if !l.is_null() {
                    let _ = write!(s, ", left-delimiter {}", show_delimiter(l));
                }
                if !r.is_null() {
                    let _ = write!(s, ", right-delimiter {}", show_delimiter(r));
                }
                s
            }
            (Kind::Choice, _) => alloc::format!("{escape}mathchoice"),
            (_, c) => alloc::format!(
                "{escape}{}",
                match c {
                    Class::Ord => "mathord",
                    Class::Op => "mathop",
                    Class::Bin => "mathbin",
                    Class::Rel => "mathrel",
                    Class::Open => "mathopen",
                    Class::Close => "mathclose",
                    Class::Punct => "mathpunct",
                    Class::Inner => "mathinner",
                }
            ),
        };
        let _ = write!(out, "{history}{head}");
        // §696: `\limits`/`\nolimits` is a subtype, printed on the head's line.
        if matches!(n.kind, Kind::Normal) && n.class == Class::Op {
            match n.limits {
                Limits::Limits => {
                    let _ = write!(out, "{escape}limits");
                }
                Limits::NoLimits => {
                    let _ = write!(out, "{escape}nolimits");
                }
                Limits::Display => {}
            }
        }
        // §692's depth cutoff is reported on the noad's head line. Each
        // occupied subsidiary field below the threshold contributes ` []`;
        // returning silently from `show_field` lost that only evidence that
        // `\mathord{}` differs from an empty ordinary noad.
        if history.chars().count() >= depth_threshold {
            if let Some(ch) = &n.choices {
                for _ in ch.iter().filter(|l| !l.is_empty()) {
                    out.push_str(" []");
                }
            } else {
                for f in [&n.nucleus, &n.denominator, &n.superscript, &n.subscript] {
                    if !f.is_empty() {
                        out.push_str(" []");
                    }
                }
            }
            out.push('\n');
            continue;
        }
        out.push('\n');
        if let Some(ch) = &n.choices {
            // §695: the four lists, each under its own history character.
            for (c, l) in ["D", "T", "S", "s"].iter().zip(ch.iter()) {
                let h = alloc::format!("{history}{c}");
                out.push_str(&show_math_list(
                    l,
                    &h,
                    depth_threshold,
                    breadth_max,
                    escape,
                    nodes,
                ));
            }
            continue;
        }
        // §692's three, in TeX's order. A fraction keeps its numerator in the
        // nucleus and shows the pair with `\` and `/` instead.
        let (nuc, den) = match n.kind {
            Kind::Fraction(..) => ("\\", "/"),
            _ => (".", ""),
        };
        out.push_str(&show_field(
            &n.nucleus,
            history,
            nuc,
            depth_threshold,
            breadth_max,
            escape,
            nodes,
        ));
        if !den.is_empty() {
            out.push_str(&show_field(
                &n.denominator,
                history,
                den,
                depth_threshold,
                breadth_max,
                escape,
                nodes,
            ));
        }
        out.push_str(&show_field(
            &n.superscript,
            history,
            "^",
            depth_threshold,
            breadth_max,
            escape,
            nodes,
        ));
        out.push_str(&show_field(
            &n.subscript,
            history,
            "_",
            depth_threshold,
            breadth_max,
            escape,
            nodes,
        ));
    }
    out
}

/// §692 `print_subsidiary_data`: one field, under the history character `c`.
fn show_field(
    f: &Field,
    history: &str,
    c: &str,
    depth_threshold: usize,
    breadth_max: usize,
    escape: &str,
    nodes: &dyn Fn(&[crate::node::Node], &str) -> alloc::string::String,
) -> alloc::string::String {
    // §692 opens with the depth test, and an occupied field past it is ` []`
    // *on the parent's line* — which is why this returns before the newline
    // the parent has already written.
    if history.chars().count() >= depth_threshold {
        return alloc::string::String::new();
    }
    let h = alloc::format!("{history}{c}");
    match f {
        Field::Empty => alloc::string::String::new(),
        Field::Char { family, code } => {
            alloc::format!("{h}{}\n", show_fam_and_char(*family, *code, escape))
        }
        Field::List(l) => nodes(l, &h),
        // §692: an empty sub-list is `{}`, which is not the same as no field —
        // `x^{}` has a superscript and `x` has none.
        Field::Sub(l) if l.is_empty() => alloc::format!("{h}{{}}\n"),
        Field::Sub(l) => show_math_list(l, &h, depth_threshold, breadth_max, escape, nodes),
    }
}

/// §68 `print_ASCII`: a character outside the printable range is shown in
/// §48's `^^` form, which is how a math family's slot 0 or 127 is quotable at
/// all.
pub fn print_ascii(code: u32) -> alloc::string::String {
    print_ascii_nl(code, -1)
}

/// [`print_ascii`], knowing `\newlinechar`.
///
/// `tex.web` §68's `print_ASCII` calls §59's `print`, and §59 has a case
/// *before* any `^^` form:
///
/// ```text
/// if (@<Character |s| is the current new-line character@>) then
///   if selector<pseudo then
///     begin print_ln; return;
///     end;
/// ```
///
/// So the character equal to `\newlinechar` prints as an actual line break —
/// in a box display, in a `short_display`, everywhere `print` is used. INITEX
/// leaves `\newlinechar` at nought and `\defaulthyphenchar` too, so a font
/// loaded without one hyphenates with character 0 and TeX writes that hyphen
/// as a line break. LaTeX commonly sets `\newlinechar` to `^^J`, so any
/// document that shows a box is affected, not only that corner.
///
/// `nl` is `-1` for "no new-line character", which is what §59's own
/// `new_line_char:=-1` means while a string is being printed — the rule does
/// not apply recursively.
pub fn print_ascii_nl(code: u32, nl: i64) -> alloc::string::String {
    if nl >= 0 && code as i64 == nl {
        return alloc::string::String::from("\n");
    }
    let c = char::from_u32(code).unwrap_or('?');
    if (' '..='~').contains(&c) {
        return alloc::format!("{c}");
    }
    // §49 has *three* arms, not two. Above 127 the character is written as two
    // lowercase hex digits — `^^c8` — because `^^` plus one character can only
    // reach 0..127, and the round trip §352 relies on has to work for every
    // byte a file can hold:
    //
    //     if s<@'100 then begin print_char("^"); print_char("^");
    //       print(s+@'100); end
    //     else if s<@'200 then begin print_char("^"); print_char("^");
    //       print(s-@'100); end
    //     else begin print_char("^"); print_char("^");
    //       print_lc_hex(s div 16); print_lc_hex(s mod 16); end
    if code >= 128 {
        let d = |v: u32| char::from_digit(v, 16).unwrap_or('?');
        return alloc::format!("^^{}{}", d((code / 16) & 0xF), d(code & 0xF));
    }
    let x = if code < 64 { code + 64 } else { code - 64 };
    alloc::format!("^^{}", char::from_u32(x).unwrap_or('?'))
}

/// §691 `print_fam_and_char`.
fn show_fam_and_char(family: usize, code: u32, escape: &str) -> alloc::string::String {
    alloc::format!("{escape}fam{family} {}", print_ascii(code))
}

/// §698 `print_delimiter`: the two halves packed back into the 24-bit code the
/// document wrote, in hex.
fn show_delimiter(d: &Delim) -> alloc::string::String {
    let a = (d.small_fam as i64) * 256 + d.small_char as i64;
    let a = a * 4096 + (d.large_fam as i64) * 256 + d.large_char as i64;
    if a < 0 {
        alloc::format!("{a}")
    } else {
        alloc::format!("\"{a:X}")
    }
}
