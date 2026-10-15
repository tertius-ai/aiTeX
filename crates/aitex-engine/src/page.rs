//! The page builder — docs/history/PLAN.md E2, and the part every previous
//! reimplementation stalled on.
//!
//! The hard thing is not choosing where to break. It is that `\output` is a
//! *TeX macro*, not a callback: it runs in the middle of page building, can
//! inspect `\pagegoal` and `\pagetotal`, can `\vsplit`, and can `\unvbox255` to
//! push material **back** into the contribution list — which the page builder
//! then reprocesses. That reentrancy is the whole difficulty, so it is modelled
//! explicitly here rather than assumed away.
//!
//! The flow (`tex.web` §980–1028):
//!
//! ```text
//!   contributions --> page builder --> page so far (box 255)
//!         ^                                  |
//!         |                            \output runs
//!         +------ \unvbox255 puts it back ---+
//! ```

use crate::node::{Glue, Node};
use crate::scaled::Scaled;
use alloc::vec::Vec;

/// A penalty at or below this forces a break; at or above `INF_PENALTY` forbids
/// one. `tex.web` §157.
pub const EJECT_PENALTY: i32 = -10_000;
pub const INF_PENALTY: i32 = 10_000;

/// How badly a page fits. `AWFUL_BAD` is worse than any real badness.
pub const AWFUL_BAD: i32 = 0x3FFF_FFFF;
/// `tex.web` §108's `inf_bad`: the largest value [`PageBuilder::badness`] ever
/// returns. TeX's badness *saturates* here — a line stretched four times its
/// glue and one stretched forty times are both simply "as bad as it gets", and
/// `\tolerance=10000` therefore accepts any line that is not overfull.
///
/// This used to be conflated with [`DEPLORABLE`], and badness could come back as
/// 100000. Anything above 10000 then failed a `\tolerance` test that TeX passes,
/// so a paragraph with no comfortable break was declared unsettable and fell
/// through to the emergency pass — which sets overfull lines. The two constants
/// are different things in `tex.web`: one bounds a badness, the other is a page
/// *cost*.
pub const INF_BAD: i32 = 10_000;

/// `tex.web` §1005's `deplorable`: "more than `inf_bad`, but less than
/// `awful_bad`" — the page-breaking cost of a break that is legal but terrible.
pub const DEPLORABLE: i32 = 100_000;

/// Retained name for [`DEPLORABLE`].
pub const DEADLY: i32 = DEPLORABLE;

/// What the builder decided to do with a contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Decision {
    /// Moved onto the page.
    Contributed,
    /// A legal breakpoint that is not yet the best one seen.
    RecordedBreakpoint,
    /// The page is complete; `\output` should now run.
    FirePage,
}

/// The three registers that govern an insertion class, as `tex.web` §1008–§1010
/// reads them, plus what is already sitting in `\box n`.
///
/// TeX reads `\count n`, `\dimen n` and `\skip n` when an insertion of class
/// `n` first reaches the page, so the host syncs them in just before building.
/// `\count n` is a magnification: 1000 means the insertion takes its own height
/// on the page, 500 that it takes half (a two-column format sets it so, because
/// a footnote spanning both columns costs each column half its height).
/// `\dimen n` caps how much of the class one page may hold, and `\skip n` is
/// the glue `\output` will put above the first of them — charged against the
/// page whether or not `\output` has run yet.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InsParams {
    pub count: i32,
    pub dimen: Scaled,
    pub skip: Glue,
    /// Height + depth of whatever `\box n` already holds (§1009's `height(r)`
    /// seed): material a previous page left behind is still in the queue.
    pub box_size: Scaled,
    /// §993 `ensure_vbox` found an `\hbox` in `\box n`. The register has
    /// already been emptied where that was noticed; what waits is the message,
    /// because §1010 issues it from *here* — after the freeze that says which
    /// page it is about and before the split trace of the same insertion.
    pub not_a_vbox: bool,
    /// §1009: `\skip n` shrinks infinitely. Reported after the correction glue
    /// has been added to the page, which is the order §1010 has it in.
    pub infinite_shrink: bool,
}

impl Default for InsParams {
    fn default() -> Self {
        Self {
            count: 0,
            dimen: Scaled::ZERO,
            skip: Glue::fixed(Scaled::ZERO),
            box_size: Scaled::ZERO,
            not_a_vbox: false,
            infinite_shrink: false,
        }
    }
}

/// A line of the page builder's narration, or a fault it found on the way.
///
/// §1005-§1011 print and complain in one stream, and the order is the whole
/// point: `%% goal height` comes from the freeze, then §1010's two complaints
/// about the class, then the `% split` trace of the insertion that provoked
/// them. The builder is a layer below the engine and cannot raise an error, so
/// it leaves a marker where the error belongs and the engine turns it into one
/// as it drains.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PageLog {
    Text(alloc::string::String),
    /// §993, for class n.
    NotAVbox(u16),
    /// §1009, for class n.
    InfiniteShrink(u16),
}

/// `tex.web`'s `page_ins_node`: what the builder knows about one insertion class
/// on the page it is currently building.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct InsState {
    /// `height(r)`: how much of this class the page has taken on so far.
    pub height: Scaled,
    /// `last_ins_ptr`: the most recent insertion of the class that fits.
    pub last_ins: Option<usize>,
    /// `best_ins_ptr`: the same, as of the best breakpoint seen — the one that
    /// counts, since that is where the page will actually be cut.
    pub best_ins: Option<usize>,
    /// `type(r) = split_up`: an insertion of this class had to be broken, so
    /// every later one waits for the next page.
    pub split_up: bool,
    /// Which insertion was broken, and before which of its nodes.
    pub broken: Option<usize>,
    pub split_at: usize,
}

/// State of the page under construction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PageBuilder {
    /// Material not yet examined. `\output` can prepend to this.
    pub contributions: Vec<Node>,
    /// `\topskip` — glue above the first box on every page.
    pub topskip: Glue,
    /// `tex.web` §1012's mark state, generalized by e-TeX to numbered classes.
    /// `top_marks[n]` is the mark in force when the page began — the last one
    /// on the previous page — while `first_marks[n]` and `bot_marks[n]` are the
    /// first and last on this page. A class with no mark of its own inherits
    /// its top mark for both, which makes running heads persist.
    pub top_marks: alloc::collections::BTreeMap<u16, Vec<crate::token::Token>>,
    pub first_marks: alloc::collections::BTreeMap<u16, Vec<crate::token::Token>>,
    pub bot_marks: alloc::collections::BTreeMap<u16, Vec<crate::token::Token>>,
    /// §979: the first and last mark in each class in the part a `\vsplit`
    /// took off. Cleared by every `\vsplit`, whether or not it finds any.
    pub split_first_marks: alloc::collections::BTreeMap<u16, Vec<crate::token::Token>>,
    pub split_bot_marks: alloc::collections::BTreeMap<u16, Vec<crate::token::Token>>,
    /// `\skip<n>` for the insertion class in use — the space `\output` will put
    /// above the insertions. Charged against the page's height when the first
    /// insertion arrives. One value, not one per class: only `\footins` is
    /// exercised, and pretending otherwise would be a guess.
    pub insert_skip: Glue,
    /// Material committed to the current page (becomes `\box255`).
    pub page: Vec<Node>,
    /// e-TeX's `page_disc`: leading glue, kern and penalty nodes removed while
    /// the current page was empty and retained by positive `\savingvdiscards`.
    pub page_discards: Vec<Node>,
    /// The live value of `\savingvdiscards > 0`, supplied by the engine before
    /// each page-builder pass so assignments take effect immediately.
    pub saving_vdiscards: bool,
    /// `\pagegoal` — the height we are aiming for.
    pub page_goal: Scaled,
    /// `\pagetotal` — height accumulated so far.
    pub page_total: Scaled,
    /// `\pageshrink` — how much the page's glue may shrink.
    pub page_shrink: Scaled,
    /// Stretch available, by infinity order.
    pub page_stretch: [Scaled; 4],
    pub page_depth: Scaled,
    /// `\outputpenalty` — the penalty at the break the page was fired at, or
    /// `INF_PENALTY` when the break was not a penalty node (`tex.web` §1013).
    /// LaTeX's output routine is a switch on it: `\clearpage` ends with
    /// `\penalty-10001`, and `\output` reads that as "this page is the tail of a
    /// `\clearpage`, discard it" rather than shipping a blank sheet.
    pub output_penalty: i32,
    /// Cost of the best breakpoint seen, and where it was.
    pub least_cost: i32,
    pub best_break: Option<usize>,
    /// True once something has been contributed: glue at the very top of a page
    /// is discarded, which is why a page never starts with blank space.
    ///
    /// This is `tex.web`'s `page_contents = box_there`. A whatsit or a mark does
    /// *not* set it (§1000) — the first real box still gets its `\topskip`.
    /// §1004: infinitely-shrinkable glue reached the page and was neutralised.
    /// The host reports it once; TeX reports it per occurrence.
    pub infinite_shrink: bool,
    /// §1004 reports *per offending glue*, as it reaches the page. `page.rs`
    /// cannot report anything itself, so occurrences are counted here and
    /// drained by the caller, which has `report`.
    pub infinite_shrink_pending: u32,
    pub started: bool,
    /// `page_contents <> empty`: a box, a rule or an insertion has reached the
    /// page, so `\pagegoal` and the rest of `page_so_far` are frozen (§987
    /// `freeze_page_specs`). `\vsize` may change afterwards without moving the
    /// goal of the page already under way, which is what makes an output routine
    /// able to set up the *next* page's height while this one is still open.
    pub frozen: bool,
    /// How many times `\output` has fired, to catch a routine that never
    /// consumes its material.
    pub output_count: u32,
    /// `\count`/`\dimen`/`\skip <n>` per insertion class, synced by the host
    /// before each build.
    pub ins_params: alloc::collections::BTreeMap<u16, InsParams>,
    /// What the builder has decided about each class on this page.
    pub ins: alloc::collections::BTreeMap<u16, InsState>,
    /// The same, as it stood when the page fired — what the host needs to move
    /// the material into `\box n`. Kept separate because firing clears the live
    /// state for the next page.
    pub fired_ins: alloc::collections::BTreeMap<u16, InsState>,
    /// `\insertpenalties`: while building, the accumulated `\floatingpenalty`
    /// of insertions that had to wait (`tex.web` §1005 charges it to every
    /// break, and 10000 of it makes a break impossible).
    pub insert_penalties: i32,
    /// `best_size`: `\pagegoal` as it stood at the best breakpoint. `\box255`
    /// is packed to *this*, not to `\vsize` — the difference is exactly the
    /// room the page's insertions took.
    pub best_size: Scaled,
    /// `best_size` of the page just fired.
    pub fired_size: Scaled,
    /// The live `\pagegoal` when the page fired.  This is deliberately not
    /// `fired_size`: a later insertion can reduce the live goal after TeX has
    /// already remembered an earlier best breakpoint.  `\box255` is packed to
    /// `fired_size`, while the output routine still reads this value.
    pub fired_goal: Scaled,
    /// §982's `page_so_far` as it stood *at the firing*, kept because `fire_up`
    /// zeroes the live accumulator on its way out and `\output` still has to be
    /// able to read what the page it was handed came to. Snapshotted beside
    /// `fired_size` for the same reason and at the same moment.
    pub fired_total: Scaled,
    pub fired_depth: Scaled,
    pub fired_shrink: Scaled,
    pub fired_stretch: [Scaled; 4],
    /// `\maxdepth`, frozen with the goal (§987 `freeze_page_specs`).
    pub page_max_depth: Scaled,
    /// `\tracingpages`: log every breakpoint the builder weighs.
    pub trace: bool,
    pub trace_ins_debug: bool,
    /// `AITEX_TRACING_PAGES` — on for the whole run whatever the document says.
    pub trace_forced: bool,
    pub trace_nodes: bool,
    pub log: Vec<PageLog>,
    /// `tex.web` §991: what the builder last took off the contribution list.
    ///
    /// Once the page builder has drained the contribution list there is no
    /// "last node" left to ask about, but `\lastskip` and friends still have to
    /// answer — §424 answers from here. LaTeX depends on it: `\addvspace` and
    /// `\addpenalty` are how every `\section`, list and float spaces itself,
    /// and both begin by reading `\lastskip`.
    pub last_glue: Glue,
    pub last_penalty: i32,
    pub last_kern: Scaled,
    /// §424's node-type code, or -1 for "nothing has been moved".
    pub last_node_type: i32,
}

impl PageBuilder {
    pub fn new(page_goal: Scaled) -> Self {
        Self {
            contributions: Vec::new(),
            topskip: Glue::fixed(Scaled::ZERO),
            top_marks: alloc::collections::BTreeMap::new(),
            first_marks: alloc::collections::BTreeMap::new(),
            bot_marks: alloc::collections::BTreeMap::new(),
            split_first_marks: alloc::collections::BTreeMap::new(),
            split_bot_marks: alloc::collections::BTreeMap::new(),
            insert_skip: Glue::fixed(Scaled::ZERO),
            page: Vec::new(),
            page_discards: Vec::new(),
            saving_vdiscards: false,
            page_goal,
            page_total: Scaled::ZERO,
            page_shrink: Scaled::ZERO,
            page_stretch: [Scaled::ZERO; 4],
            page_depth: Scaled::ZERO,
            output_penalty: INF_PENALTY,
            least_cost: AWFUL_BAD,
            best_break: None,
            infinite_shrink: false,
            infinite_shrink_pending: 0,
            started: false,
            frozen: false,
            output_count: 0,
            ins_params: alloc::collections::BTreeMap::new(),
            ins: alloc::collections::BTreeMap::new(),
            fired_ins: alloc::collections::BTreeMap::new(),
            insert_penalties: 0,
            best_size: page_goal,
            fired_size: page_goal,
            fired_goal: page_goal,
            fired_total: Scaled::ZERO,
            fired_depth: Scaled::ZERO,
            fired_shrink: Scaled::ZERO,
            fired_stretch: [Scaled::ZERO; 4],
            page_max_depth: Scaled::MAX,
            trace: false,
            trace_ins_debug: false,
            trace_forced: false,
            trace_nodes: false,
            log: Vec::new(),
            last_glue: Glue::fixed(Scaled::ZERO),
            last_penalty: 0,
            last_kern: Scaled::ZERO,
            last_node_type: -1,
        }
    }

    /// Badness of stretching `t` to fill `s` — `tex.web` §108, verbatim.
    ///
    /// Approximates `100 * (t/s)^3`, and the constants are not adjustable. The
    /// magic number is **297**, because 297³ = 99.94 × 2¹⁸, which is what makes
    /// the cube fit in a 32-bit word while landing on the right scale. Using
    /// 100 instead (as I first did) returns 0 for every ratio below about 1.4 —
    /// every page looks perfect and the builder never prefers one break to
    /// another.
    ///
    /// The three branches keep `r` in range without overflowing: below
    /// 7230584 the multiply is safe; above it, divide first.
    pub fn badness(t: Scaled, s: Scaled) -> i32 {
        if t.0 == 0 {
            return 0;
        }
        if s.0 <= 0 {
            return INF_BAD;
        }
        let (t, s) = (t.0 as i64, s.0 as i64);
        let r = if t <= 7_230_584 {
            (t * 297) / s
        } else if s >= 1_663_497 {
            t / (s / 297)
        } else {
            t
        };
        // 1290^3 < 2^31 < 1291^3, so beyond this the cube would overflow.
        if r > 1290 {
            INF_BAD
        } else {
            (((r * r * r + 0x20000) / 0x40000) as i32).min(INF_BAD)
        }
    }

    /// `tex.web` §985 `print_totals`: the page so far, in the form
    /// `<height> plus <x> plus <x>fil … minus <x>`, with each order printed
    /// only when it is non-zero. `\tracingpages`'s `t=`.
    pub fn print_totals(&self) -> alloc::string::String {
        let mut s = crate::scaled::print_scaled(self.page_total);
        for (i, name) in ["", "fil", "fill", "filll"].iter().enumerate() {
            if self.page_stretch[i].0 != 0 {
                s.push_str(" plus ");
                s.push_str(&crate::scaled::print_scaled(self.page_stretch[i]));
                s.push_str(name);
            }
        }
        if self.page_shrink.0 != 0 {
            s.push_str(" minus ");
            s.push_str(&crate::scaled::print_scaled(self.page_shrink));
        }
        s
    }

    /// The badness of the page as it stands — `\tracingpages`'s `b`.
    fn badness_here(&self) -> i32 {
        if self.page_total < self.page_goal {
            if self.page_stretch[1].0 != 0
                || self.page_stretch[2].0 != 0
                || self.page_stretch[3].0 != 0
            {
                0
            } else {
                Self::badness(self.page_goal - self.page_total, self.page_stretch[0])
            }
        } else if self.page_total - self.page_goal > self.page_shrink {
            AWFUL_BAD
        } else {
            Self::badness(self.page_total - self.page_goal, self.page_shrink)
        }
    }

    /// Cost of breaking here, given the penalty at this point.
    ///
    /// `tex.web` §1005. An insertion that does not fit, or a page that cannot
    /// shrink enough, is `AWFUL_BAD` — meaning "never break here" rather than
    /// "expensive".
    pub fn cost(&self, penalty: i32) -> i32 {
        let b = if self.page_total < self.page_goal {
            if self.page_stretch[1].0 != 0
                || self.page_stretch[2].0 != 0
                || self.page_stretch[3].0 != 0
            {
                0 // infinite stretch fills any shortfall
            } else {
                Self::badness(self.page_goal - self.page_total, self.page_stretch[0])
            }
        } else if self.page_total - self.page_goal > self.page_shrink {
            AWFUL_BAD
        } else {
            Self::badness(self.page_total - self.page_goal, self.page_shrink)
        };

        let c = if b < AWFUL_BAD {
            if penalty <= EJECT_PENALTY {
                penalty
            } else if b < INF_BAD {
                // Insertions that had to wait are charged here: a break that
                // strands a footnote is worse than one that does not.
                b + penalty + self.insert_penalties
            } else {
                // A legal break, but a terrible one: `tex.web` §1005 charges a
                // flat `deplorable` rather than the badness.
                DEPLORABLE
            }
        } else {
            b
        };
        // §1005: enough stranded insertions and the break is out of the question.
        if self.insert_penalties >= 10_000 {
            AWFUL_BAD
        } else {
            c
        }
    }

    /// Is this node a legal breakpoint?
    ///
    /// Glue breaks only when preceded by a *non-discardable* item — otherwise
    /// every run of glue would offer several equivalent breaks.
    fn is_breakpoint(&self, i: usize, node: &Node) -> bool {
        match node {
            Node::Glue(_) | Node::Leaders { .. } => self.page.last().is_some_and(|p| {
                !matches!(
                    p,
                    Node::Glue(_) | Node::Leaders { .. } | Node::Kern(_, _) | Node::Penalty(_)
                )
            }),
            Node::Kern(_, _) => matches!(
                self.contributions.get(i + 1),
                Some(Node::Glue(_) | Node::Leaders { .. })
            ),
            Node::Penalty(p) => *p < INF_PENALTY,
            _ => false,
        }
    }

    /// Move one contribution onto the page, recording breakpoints as it goes.
    pub fn step(&mut self) -> Option<Decision> {
        if self.contributions.is_empty() {
            return None;
        }
        // Take the head *out* instead of copying it. Cloning a `Node::Box`
        // copies the box's entire list, recursively, and the page builder looks
        // at every node of every page — that clone and the drop that followed
        // it were ~50 M instructions in a 4000-paragraph run. The slot is left
        // holding a sentinel rather than removed, so the index arithmetic in
        // `is_breakpoint` still refers to the node *after* this one, and the
        // two paths that do not consume the head put it back.
        let mut node = core::mem::replace(&mut self.contributions[0], Node::Penalty(0));
        // §997 records this for *every* node it takes off the list, before
        // deciding what to do with it — discarded ones included.
        self.record_last(&node);
        if self.trace_nodes {
            self.log.push(PageLog::Text(alloc::format!(
                "  . {}",
                match &node {
                    Node::Glue(g) => alloc::format!(
                        "glue w={} s={}",
                        crate::scaled::print_scaled(g.width),
                        crate::scaled::print_scaled(g.stretch)
                    ),
                    Node::Kern(k, _) => alloc::format!("kern {}", crate::scaled::print_scaled(*k)),
                    Node::Penalty(p) => alloc::format!("penalty {p}"),
                    Node::Box(b) => alloc::format!(
                        "box h={} d={}",
                        crate::scaled::print_scaled(b.height),
                        crate::scaled::print_scaled(b.depth)
                    ),
                    Node::Ins(i) => alloc::format!("ins {}", i.number),
                    other => alloc::format!("{:?}", core::mem::discriminant(other)),
                }
            )));
        }

        // §1000 dispatches an insertion before anything else, and it does not
        // make the page non-empty: TeX's `page_contents` goes to `inserts_only`,
        // which still discards glue at the top and still gives the first *box*
        // its `\topskip`. A page whose first arrival is a held-over footnote
        // otherwise loses that glue and sets its first line too high.
        if let Node::Ins { .. } = &node {
            self.contributions.remove(0);
            self.frozen = true;
            self.contribute_insertion(&node);
            self.page.push(node);
            return Some(Decision::Contributed);
        }

        // Discardable material at the top of a page vanishes, which is why a
        // page never begins with stray vertical space.
        if !self.started {
            if matches!(
                node,
                Node::Glue(_) | Node::Leaders { .. } | Node::Kern(_, _) | Node::Penalty(_)
            ) {
                self.contributions.remove(0);
                if self.saving_vdiscards {
                    self.page_discards.push(node);
                }
                return Some(Decision::Contributed);
            }
            // §1000: a whatsit or a mark contributes without making the page
            // non-empty — the first *box* still gets its `\topskip`. It does
            // make the glue that follows a legal breakpoint, which is what
            // LaTeX's `\clearpage` relies on.
            if matches!(
                node,
                Node::Language { .. } | Node::Whatsit(_) | Node::WriteOut(_) | Node::Mark { .. }
            ) {
                self.contributions.remove(0);
                self.page.push(node);
                return Some(Decision::Contributed);
            }
            // The first box on a page is preceded by `\topskip` glue whose
            // natural width is `\topskip` *minus the box's height*, so the
            // first baseline sits a fixed distance from the top however tall
            // the line is (`tex.web` §1001). Without it every page holds a
            // little more than TeX would put on it, and the difference
            // accumulates into an extra page over a long document.
            self.started = true;
            self.frozen = true;
            // §1000 dispatches `hlist_node`, `vlist_node` *and* `rule_node` to
            // the same branch, and §1001 subtracts `height(p)` whichever it is.
            // A rule counted as "not a box" cost exactly `\topskip` at the top
            // of the page, because LaTeX's `\vspace*` is `\hrule height\z@`
            // followed by the skip — precisely so the space survives a page
            // break. `book.cls`'s `\@makechapterhead` opens with
            // `\vspace*{50\p@}`, so every chapter head sat 10pt too high.
            let first_height = match &node {
                Node::Box(b) => Some(b.height),
                // A running height in a vertical list is the rule's default,
                // 0.4pt for `\hrule` — but only `\hrule` reaches a vertical
                // list, and by then the dimension has been resolved.
                Node::Rule { height, .. } if *height != Scaled::RUNNING => Some(*height),
                Node::Rule { .. } => Some(Scaled::ZERO),
                _ => None,
            };
            if let Some(h) = first_height {
                let mut g = self.topskip.sub(crate::node::glue_sub::TOP_SKIP);
                g.width = if self.topskip.width > h {
                    self.topskip.width - h
                } else {
                    Scaled::ZERO
                };
                // This node is not consumed: `\topskip` goes *in front* of it.
                self.contributions[0] = node;
                self.contributions.insert(0, Node::Glue(g));
                return Some(Decision::Contributed);
            }
        }

        // §1000 `kern_node: if link(p)=null then return`. A kern is a legal
        // breakpoint exactly when *glue* follows it, so until something does,
        // TeX cannot classify it — and rather than guess, it stops and leaves
        // the kern on the contribution list for the next call to look at again.
        //
        // We guessed "not a breakpoint" and moved it, which loses the
        // breakpoint outright once the glue arrives. trip.tex's `\kern-50pt`
        // ahead of a paragraph is one: TeX weighs a break there and we never
        // did, so a whole `% t=1080.0 … c=967` line was missing from our page
        // trace and the page after it broke somewhere else.
        if matches!(node, Node::Kern(..)) && self.contributions.len() == 1 {
            self.contributions[0] = node;
            return None;
        }

        if self.is_breakpoint(0, &node) {
            let penalty = match &node {
                Node::Penalty(p) => *p,
                _ => 0,
            };
            let c = self.cost(penalty);
            // `\tracingpages`: TeX's own line, so the two engines' page
            // decisions can be read side by side.
            if self.trace {
                // §1011, in TeX's own words so the two engines' page decisions
                // diff line for line:
                //
                //   % t=20.0 plus 1.0fil g=2000.0 b=0 p=0 c=0#
                //
                // `*` stands for `awful_bad` in both `b` and `c`, and the `#`
                // marks a break at least as good as the best so far — which is
                // the only part of the line that says what the builder *did*.
                let b = self.badness_here();
                let bs = if b >= AWFUL_BAD {
                    alloc::string::String::from("*")
                } else {
                    alloc::format!("{b}")
                };
                let cs = if c >= AWFUL_BAD {
                    alloc::string::String::from("*")
                } else {
                    alloc::format!("{c}")
                };
                self.log.push(PageLog::Text(alloc::format!(
                    "% t={} g={} b={} p={} c={}{}{}",
                    self.print_totals(),
                    crate::scaled::print_scaled(self.page_goal),
                    bs,
                    penalty,
                    cs,
                    if c <= self.least_cost { "#" } else { "" },
                    // `AITEX_TRACING_NODES` adds *which node* the breakpoint is
                    // — after the `#`, so the line still diffs against TeX's
                    // when it is off. "p=125 where TeX has p=0" is not a lead
                    // until you know whether the node is even the same kind.
                    if self.trace_nodes {
                        alloc::format!("  <{}>", node_kind_name(&node))
                    } else {
                        alloc::string::String::new()
                    }
                )));
            }
            if c <= self.least_cost {
                self.best_break = Some(self.page.len());
                self.least_cost = c;
                // §1005: the goal *at this break* is what `\box255` will be
                // packed to, and each class remembers which of its insertions
                // had made it onto the page by now.
                self.best_size = self.page_goal;
                for r in self.ins.values_mut() {
                    r.best_ins = r.last_ins;
                }
            }
            // Forced, or no longer able to improve: fire the page.
            if c == AWFUL_BAD || penalty <= EJECT_PENALTY {
                // The break node stays on the list: §1013 reads it back to set
                // `\outputpenalty`, and `fire_up` neutralises it there.
                self.contributions[0] = node;
                return Some(Decision::FirePage);
            }
            self.contributions.remove(0);
            self.accumulate(&mut node);
            self.page.push(node);
            return Some(Decision::RecordedBreakpoint);
        }

        self.contributions.remove(0);
        self.accumulate(&mut node);
        self.page.push(node);
        Some(Decision::Contributed)
    }

    /// `tex.web` §1008–§1010: an insertion reaches the page.
    ///
    /// The page does not grow — `\pagegoal` shrinks, by the room `\output`
    /// will need for this insertion once it places it. Three registers decide
    /// how much: `\count n` magnifies the height, `\dimen n` caps the class's
    /// total, and `\skip n` is the glue above the first one, charged when the
    /// first arrives. When the insertion will not fit whole it is broken with
    /// `vert_break` and the tail waits for the next page; every later insertion
    /// of that class waits too, because they must stay in order.
    fn contribute_insertion(&mut self, node: &Node) {
        let Node::Ins(ins) = node else {
            return;
        };
        let (number, list, height, depth, float_cost) = (
            &ins.number,
            &ins.list,
            &ins.height,
            &ins.depth,
            &ins.float_cost,
        );
        let at = self.page.len();
        let par = self.ins_params.get(number).copied().unwrap_or_default();
        let scale = |h: Scaled| -> Scaled {
            if par.count == 1000 {
                h
            } else {
                Scaled(
                    crate::scaled::x_over_n(h, 1000)
                        .0
                        .0
                        .saturating_mul(par.count),
                )
            }
        };
        // §1009: the first insertion of its class on this page pays for the
        // class — the glue above it, and whatever a previous page left in the box.
        if !self.ins.contains_key(number) {
            let r = InsState {
                height: par.box_size,
                ..InsState::default()
            };
            self.ins.insert(*number, r);
            // §1010 calls `ensure_vbox(n)` here, before it touches the goal.
            if par.not_a_vbox {
                self.log.push(PageLog::NotAVbox(*number));
            }
            let q = par.skip;
            self.page_goal = self.page_goal - scale(par.box_size) - q.width;
            self.page_stretch[q.stretch_order as usize] =
                self.page_stretch[q.stretch_order as usize] + q.stretch;
            self.page_shrink = self.page_shrink + q.shrink;
            // ...and complains about the shrink only after adding it, which is
            // what "has been made finite" in the help means.
            if par.infinite_shrink {
                self.log.push(PageLog::InfiniteShrink(*number));
            }
        }
        let r = self.ins.get(number).copied().unwrap_or_default();
        if r.split_up {
            // §1010: the class is already broken across this page's foot, so
            // this insertion belongs to the next page whole.
            self.insert_penalties += float_cost;
            return;
        }
        let delta = self.page_goal - self.page_total - self.page_depth + self.page_shrink;
        let h = scale(*height);
        if (h <= Scaled::ZERO || h <= delta) && *height + r.height <= par.dimen {
            self.page_goal = self.page_goal - h;
            let r = self.ins.entry(*number).or_default();
            r.height = r.height + *height;
            r.last_ins = Some(at);
            return;
        }
        // §1010: it does not fit. Find how much of it does.
        let mut w = if par.count <= 0 {
            Scaled::MAX
        } else {
            let w = self.page_goal - self.page_total - self.page_depth;
            if par.count == 1000 {
                w
            } else {
                // §1010 divides *first* and then multiplies —
                // `x_over_n(w,count(n))*1000`, the same shape as §1009's
                // `h:=x_over_n(height(p),1000)*count(n)` — and the order is
                // not interchangeable. `xn_over_d(w,1000,count)` is exact
                // where this deliberately is not: it drops the remainder of
                // the division before scaling it up, so the answer is a
                // multiple of 1000sp and always a little *short*.
                //
                // Measured against real TeX on four independent inputs, all
                // exact:
                //
                //     w        count   x_over_n*1000   xn_over_d
                //     90pt     500     179.99268       180.0
                //     93pt     500     185.98938       186.0
                //     100pt    500     199.99695       200.0
                //     100pt    250     399.9939        400.0
                //
                // TeX prints the left column. This is the 120sp from the
                // historical split-depth investigation, which had guessed at
                // `page_depth` on the assumption the arithmetic was exact.
                Scaled(
                    crate::scaled::x_over_n(w, par.count)
                        .0
                        .0
                        .saturating_mul(1000),
                )
            }
        };
        if w > par.dimen - r.height {
            w = par.dimen - r.height;
        }
        let (q, best_hd) = vert_break_full(list, w, *depth);
        // §1010's own line, so a split can be diffed against TeX's the way a
        // page break already can:
        //
        //     % split100 to 803.0,267.7 p=-101
        //
        // `to` is how much room the class was given, then how much of the
        // insertion actually fits, then the penalty the break is charged.
        if self.trace_ins_debug {
            self.log.push(PageLog::Text(alloc::format!(
                "%% ins{} dimen={} rheight={} goal={} total={} depth={} count={} [sp goal={} total={} depth={} w={}]",
                number,
                crate::scaled::print_scaled(par.dimen),
                crate::scaled::print_scaled(r.height),
                crate::scaled::print_scaled(self.page_goal),
                crate::scaled::print_scaled(self.page_total),
                crate::scaled::print_scaled(self.page_depth),
                par.count,
                self.page_goal.0,
                self.page_total.0,
                self.page_depth.0,
                (self.page_goal - self.page_total - self.page_depth).0
            )));
        }
        if self.trace {
            let pen = if q >= list.len() {
                EJECT_PENALTY
            } else {
                match &list[q] {
                    Node::Penalty(p) => *p,
                    _ => 0,
                }
            };
            self.log.push(PageLog::Text(alloc::format!(
                "% split{} to {},{} p={}",
                number,
                crate::scaled::print_scaled(w),
                crate::scaled::print_scaled(best_hd),
                pen
            )));
        }
        let r = self.ins.entry(*number).or_default();
        r.height = r.height + best_hd;
        self.page_goal = self.page_goal - scale(best_hd);
        r.split_up = true;
        r.broken = Some(at);
        r.split_at = q;
        r.last_ins = Some(at);
        // A break that strands the rest of an insertion is charged for it; a
        // break at the very end of the list (nothing left over) is charged
        // `eject_penalty`, which is TeX's way of saying "get this page out".
        self.insert_penalties += if q >= list.len() {
            EJECT_PENALTY
        } else {
            match &list[q] {
                Node::Penalty(p) => *p,
                _ => 0,
            }
        };
    }

    /// `tex.web` §1003: a box deeper than `\maxdepth` does not hang past the
    /// bottom of the page — the excess counts against the page's *height*
    /// instead. Without it a page holding one deep box (a `tabular`, a display,
    /// a tall rule) looks emptier than it is by exactly that excess, and takes
    /// on material TeX would have pushed to the next page. In a long document
    /// that happens once and every page after it is different.
    fn clamp_depth(&mut self) {
        if self.page_depth > self.page_max_depth {
            self.page_total = self.page_total + self.page_depth - self.page_max_depth;
            self.page_depth = self.page_max_depth;
        }
    }

    /// §991, run as each node leaves the contribution list.
    fn record_last(&mut self, node: &Node) {
        self.last_penalty = 0;
        self.last_kern = Scaled::ZERO;
        self.last_glue = Glue::fixed(Scaled::ZERO);
        self.last_node_type = node_type_code(node);
        match node {
            Node::Glue(g) | Node::Leaders { glue: g, .. } => self.last_glue = *g,
            Node::Penalty(p) => self.last_penalty = *p,
            Node::Kern(k, _) => self.last_kern = *k,
            _ => {}
        }
    }

    /// What `\lastskip`, `\lastkern` and `\lastpenalty` should see when the
    /// contribution list is empty — a node of the recorded kind, for the three
    /// kinds that carry a value.
    pub fn last_moved(&self) -> Option<Node> {
        match self.last_node_type {
            11 => Some(Node::Glue(self.last_glue)),
            12 => Some(Node::Kern(self.last_kern, crate::node::kern_sub::NORMAL)),
            13 => Some(Node::Penalty(self.last_penalty)),
            _ => None,
        }
    }

    fn accumulate(&mut self, node: &mut Node) {
        match node {
            Node::Box(b) => {
                self.page_total = self.page_total + self.page_depth + b.height;
                self.page_depth = b.depth;
                self.clamp_depth();
            }
            Node::Rule { height, depth, .. } => {
                self.page_total = self.page_total + self.page_depth + *height;
                self.page_depth = *depth;
                self.clamp_depth();
            }
            Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                self.page_total = self.page_total + self.page_depth + g.width;
                self.page_depth = Scaled::ZERO;
                self.page_stretch[g.stretch_order as usize] =
                    self.page_stretch[g.stretch_order as usize] + g.stretch;
                // §1004: glue that shrinks *infinitely* must not reach the page.
                // TeX reports it, forces the order to normal, and then adds the
                // shrink as if it had always been finite — so `\pageshrink`
                // stays a finite number that `\output` can reason about. We
                // added it without the check, which left the page claiming
                // infinite shrinkability; LaTeX's mark code tests for exactly
                // that and refused.
                if g.shrink_order != 0 && g.shrink != Scaled::ZERO {
                    self.infinite_shrink = true;
                    self.infinite_shrink_pending += 1;
                    // §1004 replaces the node's glue spec after §997 has
                    // recorded `\lastskip`. The amount stays unchanged; only
                    // its order becomes finite, both for page arithmetic and
                    // for every later observer of the page list.
                    g.shrink_order = 0;
                }
                self.page_shrink = self.page_shrink + g.shrink;
            }
            Node::Kern(k, _) => {
                self.page_total = self.page_total + self.page_depth + *k;
                self.page_depth = Scaled::ZERO;
            }
            _ => {}
        }
    }

    /// Run until the page is ready or the contributions run out.
    pub fn build(&mut self) -> Option<Decision> {
        let _p = crate::profile::Phase::enter(crate::profile::P_PAGE);
        while !self.contributions.is_empty() {
            let decision = self.step();
            match decision {
                Some(Decision::FirePage) => return Some(Decision::FirePage),
                Some(_) => {}
                None => break,
            }
            // §1004 reports an infinitely-shrinkable glue at the instant that
            // contribution reaches the page, before the builder examines the
            // next contribution. Return control to the host at that boundary;
            // it owns diagnostics and will resume this same build immediately.
            // Without the pause a following forced penalty wrote its §1005
            // cost line before the earlier glue error was printed.
            if self.infinite_shrink_pending != 0 {
                return None;
            }
        }
        None
    }

    /// Split the page at the best recorded breakpoint.
    ///
    /// Returns the material for `\box255`; anything after the break goes back
    /// to the front of the contribution list, because it belongs to the *next*
    /// page and must be reconsidered there.

    /// `tex.web` §1012 and §1014: fire_up rolls every mark class forward.
    /// `top_marks` becomes the previous page's `bot_marks`; then the page's own
    /// list is walked, its first mark per class setting `first_marks` and its
    /// last setting `bot_marks`. A class with no mark on this page reports its
    /// top mark for all three.
    fn roll_marks(&mut self, shipped: &[Node]) {
        self.top_marks = core::mem::take(&mut self.bot_marks);
        self.first_marks = self.top_marks.clone();
        self.bot_marks = self.top_marks.clone();
        let mut seen = alloc::collections::BTreeSet::new();
        for n in shipped {
            if let Node::Mark { class, tokens } = n {
                if seen.insert(*class) {
                    self.first_marks.insert(*class, tokens.clone());
                }
                self.bot_marks.insert(*class, tokens.clone());
            }
        }
    }

    pub fn fire_up(&mut self) -> Vec<Node> {
        let at = self
            .best_break
            .unwrap_or(self.page.len())
            .min(self.page.len());
        // §1013: the break node decides `\outputpenalty`. It is the node the page
        // was cut at — still at the head of the contributions when the break was
        // forced, and the first of the leftovers when it was merely the best one.
        // §1013 also *neutralises* it: `penalty(best_page_break) := inf_penalty`.
        // The break node goes back on the contribution list, and an output
        // routine that returns material ahead of it would otherwise meet the
        // same forcing penalty again, and again. Until now the node happened to
        // be discarded as leading matter on the next, empty page — which stops
        // being true the moment a routine hands anything back.
        let node = match self.page.get_mut(at) {
            Some(n) => Some(n),
            None => self.contributions.first_mut(),
        };
        self.output_penalty = match node {
            Some(Node::Penalty(p)) => {
                let was = *p;
                *p = INF_PENALTY;
                was
            }
            _ => INF_PENALTY,
        };
        let leftover = self.page.split_off(at);
        let boxed = core::mem::take(&mut self.page);
        self.roll_marks(&boxed);

        // §1017 hands *everything* after the break back to the contribution
        // list — the glue the page broke at included. It is §1000 that drops it,
        // when it arrives at a page that is still empty, and only then. Dropping
        // it here instead is the same thing for a page that starts empty and a
        // different thing for one that does not: an output routine can `\unvbox`
        // material back ahead of it, and then TeX keeps the glue and we had
        // already destroyed it.
        let mut rest = leftover;
        rest.append(&mut self.contributions);
        self.contributions = rest;

        // The insertion decisions belong to the page just cut; the next page
        // starts with none. `\insertpenalties` is reset here too — during
        // `\output` it means something else entirely (§1014: the *number* of
        // insertions held over), and the host fills that in as it distributes.
        self.fired_ins = core::mem::take(&mut self.ins);
        self.fired_size = self.best_size;
        self.fired_goal = self.page_goal;
        self.fired_total = self.page_total;
        self.fired_depth = self.page_depth;
        self.fired_shrink = self.page_shrink;
        self.fired_stretch = self.page_stretch;
        self.insert_penalties = 0;
        self.page_total = Scaled::ZERO;
        self.page_depth = Scaled::ZERO;
        self.page_shrink = Scaled::ZERO;
        self.page_stretch = [Scaled::ZERO; 4];
        self.least_cost = AWFUL_BAD;
        self.best_break = None;
        self.started = false;
        self.frozen = false;
        self.output_count += 1;
        boxed
    }

    /// Ship everything on the page, ignoring the best breakpoint.
    ///
    /// This is what `\end` does: there is no next page to defer material to, so
    /// splitting at the cheapest break would silently discard the remainder.
    /// Using the ordinary [`PageBuilder::fire_up`] here loses the last partial
    /// line of every document.
    pub fn fire_up_final(&mut self) -> Vec<Node> {
        self.best_break = None;
        self.output_penalty = INF_PENALTY;
        let mut boxed = core::mem::take(&mut self.page);
        // Anything still unexamined belongs on this page too.
        boxed.append(&mut self.contributions);
        self.roll_marks(&boxed);
        // Every insertion on this page is placed on it: there is no next page
        // to hold one over to, so no class is split and none waits.
        self.fired_ins = core::mem::take(&mut self.ins);
        for r in self.fired_ins.values_mut() {
            r.best_ins = Some(boxed.len());
            r.split_up = false;
            r.broken = None;
        }
        self.fired_size = self.best_size;
        self.fired_goal = self.page_goal;
        self.fired_total = self.page_total;
        self.fired_depth = self.page_depth;
        self.fired_shrink = self.page_shrink;
        self.fired_stretch = self.page_stretch;
        self.insert_penalties = 0;
        self.page_total = Scaled::ZERO;
        self.page_depth = Scaled::ZERO;
        self.page_shrink = Scaled::ZERO;
        self.page_stretch = [Scaled::ZERO; 4];
        self.least_cost = AWFUL_BAD;
        self.started = false;
        self.frozen = false;
        self.output_count += 1;
        boxed
    }

    /// `\unvbox255` — the output routine giving material back.
    ///
    /// This is the reentrancy that makes the page builder hard: the returned
    /// nodes are *prepended*, so the builder sees them again and may break at a
    /// different point this time.
    /// `tex.web` §1017: insertions the fired page could not take go at the head
    /// of the **new page**, not on the contribution list — and stay there only
    /// for as long as `\output` runs. §1026 moves them off again.
    ///
    /// §1017 ends `if q<>hold_head then begin link(page_head):=link(hold_head);
    /// page_tail:=q; end` — after the new page has been started, so
    /// `page_contents` stays `empty` and the glue that arrives next is still
    /// discarded and the first box still gets its `\topskip`. The page list is
    /// non-empty all the same, which is why §986 has a
    /// `### current page: (held over for next output)` to print at all.
    ///
    /// Putting them on the contributions instead let the page builder examine
    /// them again — a second `\pagegoal` reduction for an insertion already
    /// charged — and moved them behind whatever the output routine handed back.
    pub fn hold_over(&mut self, mut nodes: Vec<Node>) {
        nodes.append(&mut self.page);
        self.page = nodes;
    }

    pub fn unvbox_back(&mut self, mut nodes: Vec<Node>) {
        nodes.append(&mut self.contributions);
        self.contributions = nodes;
        self.started = false;
        self.frozen = false;
    }
}

/// Interword-style glue for page tests.
pub fn vglue(width: Scaled, stretch: Scaled, shrink: Scaled) -> Glue {
    Glue {
        width,
        stretch,
        stretch_order: 0,
        shrink,
        shrink_order: 0,
        subtype: 0,
        canonical_zero: false,
    }
}

/// Glue between two lines so their *baselines* sit `baselineskip` apart
/// (`tex.web` §679, `append_to_vlist`).
///
/// The subtlety is `prev_depth`. `\baselineskip` is measured baseline to
/// baseline, so the descenders of the line above eat into it. Computing
/// `baselineskip - height` and forgetting the depth leaves the *visual*
/// spacing correct while over-counting every line's contribution to
/// `\pagetotal` — measured here as 47 lines on a page that holds 54.
///
/// When the lines are too tall for that to leave a sensible gap, TeX abandons
/// the calculation and uses `\lineskip`, which is why closely-set large type
/// does not overlap.
pub fn baselineskip_glue(
    baselineskip: Scaled,
    prev_depth: Scaled,
    next_height: Scaled,
    lineskip_limit: Scaled,
    lineskip: Scaled,
) -> Glue {
    let d = baselineskip - prev_depth - next_height;
    if d < lineskip_limit {
        Glue::fixed(lineskip)
    } else {
        Glue {
            width: d,
            stretch: Scaled::ZERO,
            stretch_order: 0,
            shrink: Scaled::ZERO,
            shrink_order: 0,
            subtype: 0,
            canonical_zero: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{BoxKind, GlueSign, TeXBox};
    use alloc::vec;

    /// `tex.web` §970: the break is where the cost is least, and a forced
    /// penalty ends the search at once.
    #[test]
    fn vert_break_picks_the_cheapest_place() {
        // Four 10pt lines with ordinary glue between them; aiming for 20pt
        // should take two of them.
        let g = || {
            Node::Glue(vglue(
                Scaled::from_points(1),
                Scaled::from_points(1),
                Scaled::ZERO,
            ))
        };
        let list = vec![line(10), g(), line(10), g(), line(10), g(), line(10)];
        assert_eq!(vert_break(&list, Scaled::from_points(21), Scaled::ZERO), 3);
        // A forced break wins wherever it is.
        let list = vec![line(10), Node::Penalty(-10_000), line(10), g(), line(10)];
        assert_eq!(vert_break(&list, Scaled::from_points(100), Scaled::ZERO), 1);
    }

    /// §968: the remainder starts with `\splittopskip`, less the height of the
    /// box it precedes, so the first baseline lands the same distance down
    /// however tall that box is.
    #[test]
    fn prune_page_top_puts_splittopskip_first() {
        let list = vec![
            Node::Glue(vglue(Scaled::from_points(5), Scaled::ZERO, Scaled::ZERO)),
            Node::Penalty(100),
            line(4),
            line(10),
        ];
        let top = vglue(Scaled::from_points(6), Scaled::ZERO, Scaled::ZERO);
        let out = prune_page_top(list, top);
        assert_eq!(out.len(), 3, "the leading glue and penalty go");
        match &out[0] {
            Node::Glue(g) => assert_eq!(g.width, Scaled::from_points(2)),
            _ => panic!("expected the split-top glue first"),
        }
        // A box taller than `\splittopskip` gets none at all.
        let out = prune_page_top(vec![line(10)], top);
        match &out[0] {
            Node::Glue(g) => assert_eq!(g.width, Scaled::ZERO),
            _ => panic!("expected glue"),
        }
    }

    #[test]
    fn prune_page_top_keeps_trailing_whatsits_without_a_box() {
        let list = vec![
            Node::Glue(vglue(Scaled::from_points(5), Scaled::ZERO, Scaled::ZERO)),
            Node::Whatsit(alloc::string::String::from("color pop")),
            Node::Penalty(100),
        ];
        let (out, discards) = prune_page_top_saved(
            list,
            vglue(Scaled::from_points(6), Scaled::ZERO, Scaled::ZERO),
            true,
        );
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Node::Whatsit(s) if s == "color pop"));
        assert_eq!(discards.len(), 2, "glue and penalty are still discarded");
    }

    fn line(height_pt: i32) -> Node {
        Node::Box(alloc::boxed::Box::new(TeXBox {
            kind: BoxKind::Horizontal,
            width: Scaled::from_points(345),
            height: Scaled::from_points(height_pt),
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list: vec![],
        }))
    }

    fn builder(goal_pt: i32) -> PageBuilder {
        PageBuilder::new(Scaled::from_points(goal_pt))
    }

    #[test]
    fn glue_at_the_top_of_a_page_is_discarded() {
        // Why a page never starts with blank space.
        let mut b = builder(100);
        b.contributions = vec![
            Node::Glue(vglue(Scaled::from_points(50), Scaled::ZERO, Scaled::ZERO)),
            line(10),
        ];
        b.step();
        assert_eq!(b.page_total, Scaled::ZERO, "leading glue must not count");
        // Run to exhaustion rather than counting steps: how many a box costs
        // is an implementation detail, and `\topskip` changed it.
        b.build();
        assert_eq!(b.page_total, Scaled::from_points(10));
    }

    #[test]
    fn page_total_accumulates_heights_and_depths() {
        let mut b = builder(100);
        b.contributions = vec![line(10), line(12)];
        b.build();
        assert_eq!(b.page_total, Scaled::from_points(22));
    }

    #[test]
    fn a_forced_penalty_fires_the_page_immediately() {
        let mut b = builder(1000);
        b.contributions = vec![line(10), Node::Penalty(EJECT_PENALTY), line(10)];
        assert_eq!(
            b.build(),
            Some(Decision::FirePage),
            "\\eject must break here"
        );
        assert!(b.best_break.is_some());
    }

    #[test]
    fn an_infinite_penalty_is_never_a_breakpoint() {
        let mut b = builder(1000);
        b.contributions = vec![line(10), Node::Penalty(INF_PENALTY), line(10)];
        assert_eq!(b.build(), None, "\\nobreak must not break");
    }

    #[test]
    fn glue_only_breaks_after_a_box() {
        // Two glues in a row offer one breakpoint, not two.
        let mut b = builder(1000);
        b.contributions = vec![
            line(10),
            Node::Glue(vglue(Scaled::from_points(5), Scaled::ZERO, Scaled::ZERO)),
            Node::Glue(vglue(Scaled::from_points(5), Scaled::ZERO, Scaled::ZERO)),
        ];
        // Count the breakpoints rather than assert on step order: how many
        // steps a box costs is an implementation detail, and `\topskip`
        // changed it.
        let mut breaks = 0;
        while let Some(d) = b.step() {
            if d == Decision::RecordedBreakpoint {
                breaks += 1;
            }
        }
        assert_eq!(breaks, 1, "two glues in a row offer one breakpoint");
    }

    #[test]
    fn overfull_page_costs_awful_bad_and_fires() {
        let mut b = builder(20);
        b.contributions = vec![
            line(15),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
            line(15),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
        ];
        assert_eq!(b.build(), Some(Decision::FirePage));
    }

    #[test]
    fn fire_up_returns_the_page_and_keeps_the_remainder() {
        let mut b = builder(25);
        b.contributions = vec![
            line(10),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
            line(10),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
            line(10),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
        ];
        b.build();
        let page = b.fire_up();
        assert!(!page.is_empty(), "something was shipped");
        assert!(
            !b.contributions.is_empty(),
            "the rest carries to the next page"
        );
        assert_eq!(b.page_total, Scaled::ZERO, "accounting resets");
        assert_eq!(b.output_count, 1);
    }

    #[test]
    fn fire_up_keeps_live_goal_distinct_from_best_break_size() {
        let mut b = builder(100);
        b.page = vec![line(10)];
        b.best_break = Some(1);
        b.best_size = Scaled::from_points(80);
        // An insertion arriving after the remembered break can reduce the
        // live goal without changing the size TeX chose for \box255.
        b.page_goal = Scaled::from_points(35);

        let _ = b.fire_up();

        assert_eq!(b.fired_size, Scaled::from_points(80));
        assert_eq!(b.fired_goal, Scaled::from_points(35));
    }

    /// The glue a page broke at does not begin the next page — but it is §1000
    /// that drops it, on arrival at a page that is still empty, and not
    /// `fire_up`. §1017 hands *everything* after the break back to the
    /// contribution list, so the glue is still there to be looked at; an output
    /// routine that `\unvbox`es material back ahead of it makes the page
    /// non-empty, and then TeX keeps the glue after all.
    #[test]
    fn discardables_after_a_break_do_not_start_the_next_page() {
        let mut b = builder(15);
        b.contributions = vec![
            line(10),
            Node::Glue(vglue(Scaled::from_points(30), Scaled::ZERO, Scaled::ZERO)),
            line(10),
        ];
        b.build();
        b.fire_up();
        assert!(
            matches!(b.contributions.first(), Some(Node::Glue(_))),
            "§1017 hands the glue back with everything else after the break"
        );
        b.build();
        // The page does begin with glue — its `\topskip` — so the thing to check
        // is that the 30pt we broke at is nowhere on it.
        assert!(
            !b.page.iter().any(|n| matches!(n, Node::Glue(g)
                if g.width == Scaled::from_points(30))),
            "§1000 drops it on arrival at an empty page, so no page begins with it"
        );
    }

    /// The reentrancy that defines this milestone: `\output` hands material
    /// back with `\unvbox255`, and the builder must reconsider it rather than
    /// treating the page as settled.
    #[test]
    fn unvbox255_returns_material_for_reconsideration() {
        let mut b = builder(25);
        b.contributions = vec![line(10), line(10), line(10)];
        b.build();
        let page = b.fire_up();
        let before = b.contributions.len();

        b.unvbox_back(page);
        assert!(b.contributions.len() > before, "material came back");
        assert!(
            !b.started,
            "the page restarts, so leading glue is discarded again"
        );

        // And the builder can run again over it without complaint.
        let _ = b.build();
    }

    /// At \end there is no next page, so nothing may be held back.
    #[test]
    fn the_final_page_ships_everything() {
        let mut b = builder(1000);
        b.contributions = vec![
            line(10),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
            line(10),
        ];
        b.build();
        let page = b.fire_up_final();
        let boxes = page.iter().filter(|n| matches!(n, Node::Box(_))).count();
        assert_eq!(boxes, 2, "both lines must be shipped");
        assert!(b.contributions.is_empty(), "nothing left behind");
    }

    /// The bug this replaced: fire_up splits at the best breakpoint and returns
    /// the rest to the contribution list. Correct mid-document, silently lossy
    /// at the end of the job.
    #[test]
    fn ordinary_fire_up_does_hold_material_back() {
        let mut b = builder(1000);
        b.contributions = vec![
            line(10),
            Node::Glue(vglue(Scaled::ZERO, Scaled::ZERO, Scaled::ZERO)),
            line(10),
        ];
        b.build();
        let page = b.fire_up();
        let shipped = page.iter().filter(|n| matches!(n, Node::Box(_))).count();
        assert!(
            shipped < 2 || !b.contributions.is_empty(),
            "fire_up may defer material; fire_up_final must not"
        );
    }

    #[test]
    fn badness_is_zero_when_it_fits_and_deadly_when_it_cannot() {
        assert_eq!(
            PageBuilder::badness(Scaled::ZERO, Scaled::from_points(1)),
            0
        );
        assert_eq!(
            PageBuilder::badness(Scaled::from_points(1), Scaled::ZERO),
            INF_BAD
        );
        let small = PageBuilder::badness(Scaled::from_points(1), Scaled::from_points(100));
        let large = PageBuilder::badness(Scaled::from_points(50), Scaled::from_points(100));
        assert!(
            small < large,
            "stretching further must cost more ({small} vs {large})"
        );

        // Verified against TeX Live by \showbox on boxes with known glue set:
        //   glue set 1.00 -> "Loose \hbox (badness 100)"
        //   glue set 0.50 -> "Loose \hbox (badness 12)"
        //   glue set 0.25 -> "Loose \hbox (badness 2)"
        for (stretch_pt, expect) in [(100, 100), (50, 12), (25, 2)] {
            assert_eq!(
                PageBuilder::badness(Scaled::from_points(stretch_pt), Scaled::from_points(100)),
                expect,
                "glue set {}/100 should be badness {expect}",
                stretch_pt
            );
        }
        // Well beyond it saturates at `inf_bad` rather than overflowing — and at
        // `inf_bad`, not at `deplorable`: `\tolerance=10000` has to accept it.
        assert_eq!(
            PageBuilder::badness(Scaled::from_points(10_000), Scaled::from_points(1)),
            INF_BAD
        );
    }

    #[test]
    fn baselineskip_accounts_for_the_previous_depth() {
        let bs = Scaled::from_points(12);
        let lim = Scaled::from_points(0);
        let ls = Scaled::from_points(1);
        // A 7pt-tall line under a 2pt-deep one leaves 3pt of glue, not 5.
        let g = baselineskip_glue(bs, Scaled::from_points(2), Scaled::from_points(7), lim, ls);
        assert_eq!(g.width, Scaled::from_points(3));
        // Baseline separation is then depth + glue + height = 12pt exactly.
        assert_eq!(
            Scaled::from_points(2) + g.width + Scaled::from_points(7),
            bs
        );
    }

    #[test]
    fn oversized_lines_fall_back_to_lineskip() {
        // Without this, tall lines would be given negative glue and overlap.
        let g = baselineskip_glue(
            Scaled::from_points(12),
            Scaled::from_points(8),
            Scaled::from_points(10),
            Scaled::from_points(1),
            Scaled::from_points(3),
        );
        assert_eq!(g.width, Scaled::from_points(3), "lineskip takes over");
    }

    #[test]
    fn empty_input_is_not_an_error() {
        let mut b = builder(100);
        assert_eq!(b.build(), None);
        assert!(b.fire_up().is_empty());
    }
}

/// `tex.web` §970 `vert_break`: where to break a vertical list aiming for
/// height `h`, with at most `d` of depth left hanging below.
///
/// The same cost rule as the page builder: badness against the goal, plus the
/// penalty at the break, and a forced break wins outright. Returns the index to
/// break *before*; `list.len()` means the whole list fits.
pub fn vert_break(list: &[Node], h: Scaled, d: Scaled) -> usize {
    vert_break_full(list, h, d).0
}

/// [`vert_break`], also reporting `best_height_plus_depth` — how tall the piece
/// before the break actually is. `tex.web` §970 computes it on the way past and
/// §1010 needs it: an insertion's split-off head is charged to `\pagegoal` at
/// exactly that height.
pub fn vert_break_full(list: &[Node], h: Scaled, d: Scaled) -> (usize, Scaled) {
    let (at, height, _) = vert_break_full_traced(list, h, d, false);
    (at, height)
}

/// [`vert_break_full`] with TeX's `\tracingpages` narration. `\vsplit` uses
/// the same vertical breaker as insertions and the main page, and pdfTeX logs
/// those internal splits too. Keeping that trace available is more than a
/// diagnostic nicety: LaTeX's output routine uses several small `\vsplit`s to
/// enforce widow/club policy, so this is the shortest exact oracle when a line
/// lands on the wrong page.
pub fn vert_break_full_traced(
    list: &[Node],
    h: Scaled,
    d: Scaled,
    trace: bool,
) -> (usize, Scaled, Vec<alloc::string::String>) {
    let mut least = AWFUL_BAD;
    let mut best = list.len();
    let mut best_hd = Scaled::ZERO;
    let mut height = Scaled::ZERO;
    let mut prev_dp = Scaled::ZERO;
    let mut stretch = [Scaled::ZERO; 4];
    let mut shrink = Scaled::ZERO;
    let mut log = Vec::new();
    if trace {
        log.push(alloc::format!(
            "%% goal height={}, max depth={}",
            crate::scaled::print_scaled(h),
            crate::scaled::print_scaled(d)
        ));
    }
    let mut i = 0usize;
    loop {
        // §972: is this a legal breakpoint, and what does it cost?
        let pi = if i >= list.len() {
            EJECT_PENALTY
        } else {
            match &list[i] {
                Node::Box(_) | Node::Rule { .. } => {
                    let (bh, bd) = match &list[i] {
                        Node::Box(b) => (b.height, b.depth),
                        Node::Rule { height, depth, .. } => (
                            if *height == Scaled::RUNNING {
                                Scaled::ZERO
                            } else {
                                *height
                            },
                            if *depth == Scaled::RUNNING {
                                Scaled::ZERO
                            } else {
                                *depth
                            },
                        ),
                        _ => unreachable!(),
                    };
                    height = height + prev_dp + bh;
                    prev_dp = bd;
                    if prev_dp > d {
                        height = height + prev_dp - d;
                        prev_dp = d;
                    }
                    i += 1;
                    continue;
                }
                // Glue is a breakpoint only after something non-discardable.
                Node::Glue(_) | Node::Leaders { .. } if i > 0 && !discardable(&list[i - 1]) => 0,
                Node::Kern(_, _)
                    if matches!(list.get(i + 1), Some(Node::Glue(_) | Node::Leaders { .. })) =>
                {
                    0
                }
                Node::Penalty(p) => *p,
                _ => {
                    // Not a breakpoint: it still contributes its size.
                    match &list[i] {
                        Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                            height = height + prev_dp + g.width;
                            prev_dp = Scaled::ZERO;
                            stretch[g.stretch_order as usize] =
                                stretch[g.stretch_order as usize] + g.stretch;
                            shrink = shrink + g.shrink;
                        }
                        Node::Kern(k, _) => {
                            height = height + prev_dp + *k;
                            prev_dp = Scaled::ZERO;
                        }
                        _ => {}
                    }
                    i += 1;
                    continue;
                }
            }
        };
        if pi < INF_PENALTY {
            // §974: how badly the page-so-far fits the goal.
            let b = if height < h {
                if stretch[1] != Scaled::ZERO
                    || stretch[2] != Scaled::ZERO
                    || stretch[3] != Scaled::ZERO
                {
                    0
                } else {
                    PageBuilder::badness(h - height, stretch[0])
                }
            } else if height - h > shrink {
                AWFUL_BAD
            } else {
                PageBuilder::badness(height - h, shrink)
            };
            let cost = if b < AWFUL_BAD {
                if pi <= EJECT_PENALTY {
                    pi
                } else if b < INF_BAD {
                    b + pi
                } else {
                    DEPLORABLE
                }
            } else {
                b
            };
            if trace {
                let mut totals = crate::scaled::print_scaled(height);
                for (order, name) in ["", "fil", "fill", "filll"].iter().enumerate() {
                    if stretch[order] != Scaled::ZERO {
                        totals.push_str(" plus ");
                        totals.push_str(&crate::scaled::print_scaled(stretch[order]));
                        totals.push_str(name);
                    }
                }
                if shrink != Scaled::ZERO {
                    totals.push_str(" minus ");
                    totals.push_str(&crate::scaled::print_scaled(shrink));
                }
                let badness = if b >= AWFUL_BAD {
                    alloc::string::String::from("*")
                } else {
                    alloc::format!("{b}")
                };
                let shown_cost = if cost >= AWFUL_BAD {
                    alloc::string::String::from("*")
                } else {
                    alloc::format!("{cost}")
                };
                log.push(alloc::format!(
                    "% t={} g={} b={} p={} c={}{}",
                    totals,
                    crate::scaled::print_scaled(h),
                    badness,
                    pi,
                    shown_cost,
                    if cost <= least { "#" } else { "" }
                ));
            }
            if cost <= least {
                least = cost;
                best = i;
                best_hd = height + prev_dp;
            }
            if b == AWFUL_BAD || pi <= EJECT_PENALTY {
                return (best, best_hd, log);
            }
        }
        if i >= list.len() {
            return (best, best_hd, log);
        }
        // The glue or kern we broke at (or considered) still counts if we go on.
        match &list[i] {
            Node::Glue(g) | Node::Leaders { glue: g, .. } => {
                height = height + prev_dp + g.width;
                prev_dp = Scaled::ZERO;
                stretch[g.stretch_order as usize] = stretch[g.stretch_order as usize] + g.stretch;
                shrink = shrink + g.shrink;
            }
            Node::Kern(k, _) => {
                height = height + prev_dp + *k;
                prev_dp = Scaled::ZERO;
            }
            _ => {}
        }
        i += 1;
    }
}

/// `tex.web` §968 `prune_page_top`: what a broken-off remainder starts with.
///
/// The discardable material at the top goes, and `\splittopskip` is put before
/// the first box — less that box's height, so the first baseline sits exactly
/// `\splittopskip` below the top however tall the box is.
pub fn prune_page_top(list: Vec<Node>, split_top_skip: Glue) -> Vec<Node> {
    prune_page_top_saved(list, split_top_skip, false).0
}

/// e-TeX's extension of §968: optionally return the glue, kern and penalty
/// nodes removed before the first box as `split_disc`, in their original order.
pub fn prune_page_top_saved(
    list: Vec<Node>,
    split_top_skip: Glue,
    save_discards: bool,
) -> (Vec<Node>, Vec<Node>) {
    let mut out: Vec<Node> = Vec::new();
    let mut discards: Vec<Node> = Vec::new();
    let mut it = list.into_iter();
    let mut pending: Vec<Node> = Vec::new();
    for n in it.by_ref() {
        match n {
            Node::Box(_) | Node::Rule { .. } => {
                let h = match &n {
                    Node::Box(b) => b.height,
                    Node::Rule { height, .. } if *height != Scaled::RUNNING => *height,
                    _ => Scaled::ZERO,
                };
                let mut g = split_top_skip.sub(crate::node::glue_sub::SPLIT_TOP_SKIP);
                g.width = if g.width > h {
                    g.width - h
                } else {
                    Scaled::ZERO
                };
                out.append(&mut pending);
                out.push(Node::Glue(g));
                out.push(n);
                break;
            }
            // Marks, insertions and whatsits survive the pruning.
            Node::Mark { .. }
            | Node::Ins { .. }
            | Node::Language { .. }
            | Node::Whatsit(_)
            | Node::WriteOut(_) => pending.push(n),
            Node::Glue(_) | Node::Leaders { .. } | Node::Kern(_, _) | Node::Penalty(_)
                if save_discards =>
            {
                discards.push(n)
            }
            _ => {}
        }
    }
    // §968 advances past marks, insertions and whatsits while it looks for the
    // first box.  They remain linked even when the remainder ends before a box
    // is found.  Dropping `pending` here lost a trailing colour special (and,
    // more importantly, could lose marks that output routines use to steer
    // later splits) whenever a `\vsplit` left only non-discardable zero-size
    // material behind.
    out.append(&mut pending);
    out.extend(it);
    (out, discards)
}

fn discardable(n: &Node) -> bool {
    matches!(
        n,
        Node::Glue(_) | Node::Leaders { .. } | Node::Kern(_, _) | Node::Penalty(_)
    )
}

/// `tex.web` §424's node-type codes: 0 char, 1 hlist, 2 vlist, 3 rule, 4 ins,
/// 5 mark, 6 adjust, 8 disc, 9 whatsit, 10 math, 11 glue, 12 kern, 13 penalty.
pub fn node_type_code(n: &Node) -> i32 {
    match n {
        Node::Char { .. } | Node::NativeRun(_) => 0,
        Node::Box(b) => {
            if b.kind == crate::node::BoxKind::Horizontal {
                1
            } else {
                2
            }
        }
        Node::Rule { .. } => 3,
        Node::Ins { .. } => 4,
        Node::Mark { .. } => 5,
        Node::Adjust { .. } => 6,
        Node::Disc { .. } => 8,
        Node::Language { .. } | Node::Whatsit(_) | Node::WriteOut(_) => 9,
        Node::Math { .. } => 10,
        Node::Glue(_) | Node::Leaders { .. } => 11,
        Node::Kern(_, _) => 12,
        Node::Penalty(_) => 13,
    }
}

/// A breakpoint node's kind, for `AITEX_TRACING_NODES`.
fn node_kind_name(n: &Node) -> &'static str {
    match n {
        Node::Box(_) => "box",
        Node::Rule { .. } => "rule",
        Node::Glue(_) => "glue",
        Node::Kern(_, _) => "kern",
        Node::Penalty(_) => "penalty",
        Node::Ins(_) => "ins",
        Node::Mark { .. } => "mark",
        Node::Leaders { .. } => "leaders",
        Node::Language { .. } | Node::Whatsit(_) | Node::WriteOut(_) => "whatsit",
        _ => "other",
    }
}
