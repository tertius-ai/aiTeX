//! Alignment — `\halign`, `tex.web` §768–§812.
//!
//! An alignment is a table whose column widths are not known until every row
//! has been read. TeX handles that by setting each entry into an *unset* box —
//! packed to its natural size but remembering its glue — and going back at the
//! end to re-pack every entry to its column's final width. That two-pass shape
//! is the whole algorithm; everything else is bookkeeping.
//!
//! The preamble `u_1 # v_1 & u_2 # v_2 & ... \cr` gives each column a template.
//! An entry is set as `u_j <what the document wrote> v_j`, which is how
//! `\halign{\bf#\hfil&...}` makes a whole column bold without the document
//! repeating itself. The `#` is where the entry goes; `\omit` skips the
//! template for one entry.
//!
//! We do not need a separate unset-node type: an entry's node list is kept and
//! `hpack`ed to the column width at the end, which is what converting an unset
//! box to a set one amounts to. Decision 2 in docs/history/PLAN.md (real Rust types rather
//! than Knuth's `mem[]` array) is what makes that simplification available.
//!
//! Spans, transposed `\valign`s, and independently varying `\tabskip` glues
//! all use that same two-pass representation.  The span pass is deliberately
//! kept here beside the final setting pass: TeX resolves constraints by their
//! ending column, not by the order in which rows happened to be read.

use crate::node::{CharMetrics, Glue, Node, TeXBox, hpack};
use crate::scaled::Scaled;
use crate::token::{TokKind, Token};
use alloc::vec::Vec;

/// One column's template: the text before the `#` and after it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Column {
    pub u: Vec<Token>,
    pub v: Vec<Token>,
}

/// One entry of a row.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Cell {
    /// The material the entry set, already in node form.
    pub list: Vec<Node>,
    /// Natural size, measured when the entry was closed: the width in an
    /// `\halign`, the height-plus-depth in a `\valign`. It is what the
    /// entry's column (or row) is sized from.
    pub natural: Scaled,
    /// Height and depth of an `\halign` entry when it was packed.  Rows are
    /// retained until every column width is known, but TeX has already put the
    /// completed unset row on its internal vertical list at this point.  A
    /// following `\noalign` may therefore read `\prevdepth`, so these two
    /// dimensions are part of the live alignment state rather than something
    /// that can wait for the final setting pass.
    #[serde(default)]
    pub height: Scaled,
    #[serde(default)]
    pub depth: Scaled,
    /// How many columns the entry covers. `\span` adds one each time, which is
    /// how plain's `\multispan{n}` is built: `\omit` then n-1 `\span\omit`.
    pub span: usize,
    /// Insertions, marks, and adjustments lifted out while an `\halign` entry
    /// is packaged. TeX appends them after the finished row (§799); a `\valign`
    /// uses `vpackage` and therefore leaves this empty.
    #[serde(default)]
    pub migrating: Vec<Node>,
}

/// A row, or the vertical material `\noalign` contributed between rows.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Row {
    Cells(Vec<Cell>),
    /// `\noalign` material, and whether it reset `\prevdepth`.
    ///
    /// A flag rather than the depth itself. Rows cannot be contributed until
    /// every column width is known, so all of this is spliced together at the
    /// end — and during the body `\prevdepth` is still whatever preceded the
    /// alignment, since no row has been placed yet. Its *value* while the
    /// `\noalign` runs therefore means nothing; what matters is whether the body
    /// *assigned* `\prevdepth` — a rule sets it to `ignore_depth`, which is what
    /// suppresses the interline glue before the next row (`tex.web` §1056).
    /// `None` means it did not, and the row below is spaced from the row above
    /// as if the `\noalign` were not there: `\noalign{\kern7pt}` adds seven
    /// points *and* keeps the `\baselineskip`.
    NoAlign {
        list: Vec<Node>,
        resets_depth: Option<Scaled>,
    },
}

/// An alignment in progress.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Align {
    pub cols: Vec<Column>,
    pub rows: Vec<Row>,
    /// `\tabskip` at each boundary: before the first column, between every
    /// pair, and after the last, so there is one more than there are columns
    /// (`tex.web` §768).
    ///
    /// Per boundary rather than one value, because the preamble may reset it
    /// as it goes: `\halign{#\tabskip=5pt&#\tabskip=9pt&#\tabskip=13pt\cr}`
    /// gives the four boundaries the initial value and then 5, 9 and 13. An
    /// assignment written before a `&` is the glue *at* that `&`.
    pub tabskips: Vec<Glue>,
    /// `\halign to <dimen>` / `spread <dimen>`, applied to each row.
    pub to: Option<Scaled>,
    pub spread: Option<Scaled>,
    // -- state while reading the body ------------------------------------
    /// Entries of the row being read.
    pub cells: Vec<Cell>,
    /// Which column the current entry belongs to.
    pub col: usize,
    /// `\omit` seen for the current entry, so its template is skipped.
    pub omit: bool,
    /// Extra columns the current entry covers, from `\span`.
    pub span: usize,
    /// The delimiter whose v-template is running was `\span`. The v-template
    /// may open a nested alignment, so this cannot be engine-global state.
    #[serde(default)]
    pub span_pending: bool,
    /// Whether that delimiter ends the row (`\cr`) instead of one entry (`&`).
    /// This likewise has to survive arbitrary nested v-template work.
    #[serde(default)]
    pub row_ends: bool,
    /// Group depth at which the current entry began. `&` and `\cr` are only
    /// column and row separators at that depth — inside a `{...}` written by
    /// the entry they are ordinary tokens. `tex.web` tracks the same thing
    /// with `align_state`.
    pub cell_depth: usize,
    /// The interline parameters in force as each row was *finished*.
    ///
    /// `tex.web` §807 appends a row to the enclosing vertical list the moment
    /// `fin_row` completes it, so the `\baselineskip`, `\lineskip` and
    /// `\lineskiplimit` a row is set with are the ones current in the body.
    /// We cannot append that early — no column width is known until the last
    /// row is in — so the values are captured when TeX would have used them
    /// and applied when the rows finally go out. Reading them at the end
    /// instead picks up whatever the alignment's own closing groups have
    /// restored, which is how amsmath's `\openup\jot` was lost and every
    /// `align` came out with its rows 12pt apart where TeX sets them 15.
    pub row_params: Vec<(Scaled, Scaled, Scaled)>,
    /// A repeating preamble: `\halign{&<template>\cr}`. `tex.web` §783 gives
    /// an `&` that arrives before any `#`, with the template so far empty and
    /// no loop yet recorded, this meaning — the columns from here on repeat for
    /// as long as the rows need them. §793 lengthens the preamble one period at
    /// a time as entries arrive.
    ///
    /// `(first, period)`: the column the loop starts at, and how many columns
    /// are in it. `None` is an ordinary fixed preamble.
    pub loop_from: Option<(usize, usize)>,
    /// `\valign` rather than `\halign` — `tex.web` §768: "the same procedure
    /// with the roles of horizontal and vertical interchanged".
    ///
    /// The preamble describes *rows*; `\cr` ends a **column**; each entry is a
    /// vbox set to its row's height, and the finished columns go side by side
    /// into the enclosing horizontal list. Everything else — templates,
    /// `\omit`, `\span`, `\tabskip` per boundary, `\noalign` — is unchanged,
    /// which is why this is a flag rather than a second implementation.
    pub vertical: bool,
    /// §216's `mode_line` for the alignment's own semantic level.  An
    /// active alignment also has row and span levels inside this one; all of
    /// them are visible to §218 even though this implementation stores the
    /// unfinished table in one Rust value.
    #[serde(default)]
    pub entered_at: i64,
    /// The auxiliary field inherited when `init_align` pushes its level.
    /// `\valign` negates horizontal mode and keeps the enclosing space factor;
    /// `\halign` analogously keeps the enclosing vertical `\prevdepth`.
    #[serde(default)]
    pub enclosing_space_factor: i64,
    #[serde(default)]
    pub enclosing_prev_depth: Scaled,
}

impl Align {
    /// The `\prevdepth` TeX's alignment list has immediately before the next
    /// row.  This is observable inside `\noalign`: tabto uses it to return to
    /// the baseline after its one-row display alignment.
    pub fn prev_depth(&self) -> Scaled {
        let mut depth = self.enclosing_prev_depth;
        for row in &self.rows {
            match row {
                Row::Cells(cells) if !self.vertical => {
                    depth = cells
                        .iter()
                        .map(|cell| cell.depth)
                        .max()
                        .unwrap_or(Scaled::ZERO);
                }
                Row::NoAlign {
                    resets_depth: Some(value),
                    ..
                } if !self.vertical => depth = *value,
                _ => {}
            }
        }
        depth
    }

    /// The glue at boundary `i`, of which there is one more than there are
    /// columns. Out-of-range asks for a column that does not exist, which a
    /// row with more entries than the preamble declared can do.
    pub fn tabskip(&self, i: usize) -> Glue {
        self.tabskips
            .get(i)
            .copied()
            .or_else(|| self.tabskips.last().copied())
            .unwrap_or_default()
            .sub(crate::node::glue_sub::TAB_SKIP)
    }

    pub fn new(
        cols: Vec<Column>,
        tabskips: Vec<Glue>,
        to: Option<Scaled>,
        spread: Option<Scaled>,
    ) -> Self {
        Self {
            cols,
            rows: Vec::new(),
            tabskips,
            to,
            spread,
            cells: Vec::new(),
            col: 0,
            omit: false,
            span: 0,
            span_pending: false,
            row_ends: false,
            cell_depth: 0,
            row_params: Vec::new(),
            loop_from: None,
            vertical: false,
            entered_at: 0,
            enclosing_space_factor: 1000,
            enclosing_prev_depth: crate::fastset::IGNORE_DEPTH,
        }
    }

    /// `tex.web` §793 "lengthen the preamble periodically": an entry in a
    /// column the preamble does not reach copies the next template of the loop,
    /// and its `\tabskip`, so a repeating preamble covers any number of columns.
    ///
    /// Without a loop the preamble simply ends, and an entry past it gets an
    /// empty template — which is what TeX does too, after complaining.
    fn ensure_col(&mut self, i: usize) {
        let Some((first, period)) = self.loop_from else {
            return;
        };
        if period == 0 {
            return;
        }
        while self.cols.len() <= i {
            let src = first + (self.cols.len() - first) % period;
            let col = self.cols[src].clone();
            self.cols.push(col);
            // §793 appends the copied column *after* the preamble's final
            // tabskip and then puts a fresh copy of the loop's own following
            // glue after it:
            //
            //     begin link(q):=new_null_box; p:=link(q); {a new alignrecord}
            //     info(p):=end_span; width(p):=null_flag; cur_loop:=link(cur_loop);
            //     @<Copy the templates from node |cur_loop| into node |p|@>;
            //     cur_loop:=link(cur_loop);
            //     link(p):=new_glue(glue_ptr(cur_loop));
            //
            // So the boundary that was "after the last column" stays where it
            // is and becomes the one *between* the old last column and the new
            // one; the new glue goes on the end. Inserting instead of pushing
            // shifted every boundary from there on by one, and amsmath's
            // `align` — whose preamble is `& … \tabskip\z@skip & … \tabskip
            // \alignsep@` and whose rows carry one column more than the
            // preamble declares — came out `\minalignsep` too wide on every
            // row.
            let g = self.tabskip(src + 1);
            self.tabskips.push(g);
        }
    }

    /// The template around the current entry, unless `\omit` cancelled it.
    /// Would moving to the next column run off the end of the preamble?
    ///
    /// §792: an `&` or `\span` past the last column is "Extra alignment tab has
    /// been changed to `\cr`" — the tab ends the *row* instead. A preamble with
    /// a `&&` repeat never runs out, because `ensure_col` grows it.
    pub fn past_last_column(&self) -> bool {
        if let Some((_, period)) = self.loop_from {
            if period != 0 {
                return false;
            }
        }
        self.col + 1 >= self.cols.len()
    }

    pub fn template(&mut self) -> (Vec<Token>, Vec<Token>) {
        // §791's `fin_col` lengthens the preamble *before* it looks at what the
        // entry contains, so `\omit` does not exempt a column from being
        // created. The alignrecord is what carries the column's width and the
        // tabskip glue that follows it, and an omitted entry occupies one just
        // the same. Returning early left the preamble short, so the boundary
        // after an omitted entry fell back to the preamble's last tabskip —
        // which is exactly the shape of amsmath's `align`, whose tag column is
        // `\omit\kern-\alignsep@`.
        self.ensure_col(self.col);
        if self.omit {
            return (Vec::new(), Vec::new());
        }
        match self.cols.get(self.col) {
            Some(c) => (c.u.clone(), c.v.clone()),
            None => (Vec::new(), Vec::new()),
        }
    }
}

/// Split a preamble's tokens into columns.
///
/// `&` separates columns and `#` separates a column's two halves, both only at
/// brace depth zero: `\halign{\hbox{a&b}#&...}` has one `&` that is part of the
/// first template and one that separates the columns.
pub fn parse_preamble(
    tokens: &[Token],
) -> (Vec<Column>, Vec<usize>, Option<usize>, Vec<PreambleError>) {
    let mut diags = Vec::new();
    let mut cols = Vec::new();
    let mut ends = Vec::new();
    let mut loop_from = None;
    let mut cur = Column::default();
    let mut seen_hash = false;
    let mut depth = 0i32;
    for (i, t) in tokens.iter().enumerate() {
        // §783: spaces are eliminated from the beginning of each u-template.
        // Do this here as well as in the live scanner because this parser is a
        // public, directly tested representation boundary.
        if !seen_hash && cur.u.is_empty() && t.is_space() {
            continue;
        }
        match t.kind() {
            TokKind::Char(_, crate::catcode::CatCode::BeginGroup) => {
                depth += 1;
            }
            TokKind::Char(_, crate::catcode::CatCode::EndGroup) => {
                depth -= 1;
            }
            TokKind::Char(_, crate::catcode::CatCode::AlignTab) if depth == 0 => {
                // `tex.web` §783: an `&` with nothing before it in this template
                // and no loop recorded yet is not a missing `#` — it starts a
                // *repeating* preamble (`cur_loop := cur_align`). amsmath's
                // `\align@preamble` opens with one, which is how its alignments
                // stretch to however many fields a row has; rejecting it
                // abandoned the whole `\halign`, and with it the group its `{`
                // had opened, so `\end{align}` closed `\begin{document}`'s.
                if !seen_hash && cur.u.is_empty() && loop_from.is_none() {
                    loop_from = Some(cols.len());
                    continue;
                }
                // §783 does not give up on a template with no `#`: it says so,
                // puts the `&` back, and ends the template there — which leaves
                // `u` holding what was scanned and `v` empty, because §784 then
                // reads the `&` again and stops at once.
                //
                //     begin print_err("Missing # inserted in alignment preamble");
                //     help3("There should be exactly one # between &'s, when an")
                //     ("\halign or \valign is being set up. In this case you had")
                //     ("none, so I've put one in; maybe that will work.");
                //     back_error; goto done1;
                //     end
                //
                // Abandoning the whole alignment instead left the `{` its
                // `\halign` had opened with no `}`, and everything after it in
                // the document one group too deep.
                if !seen_hash {
                    diags.push(PreambleError::MissingHash);
                }
                cols.push(core::mem::take(&mut cur));
                ends.push(i);
                seen_hash = false;
                continue;
            }
            // `tex.web` §783 scans the template until the *first* `#`, and does not
            // care how many braces it is inside — only `&` and `\cr` are held to the
            // outer level (TeX's `align_state`). LaTeX's `tabular` puts its `#`
            // inside a group (`{\hskip 1sp\ignorespaces #\unskip\hfil}`), so
            // requiring depth 0 found no `#` at all and abandoned the alignment.
            TokKind::Char(_, crate::catcode::CatCode::Parameter) => {
                // §784 keeps the first `#` and ignores the rest:
                //
                //     begin print_err("Only one # is allowed per tab");
                //     help3(…)("more than one, so I'm ignoring all but the first.");
                //     error; goto continue;
                //     end
                //
                // `goto continue` — the token is dropped and the scan carries
                // on. trip.tex line 333 has `\d#\d` where `\d` expands to `##`.
                if seen_hash {
                    diags.push(PreambleError::TwoHashes);
                    continue;
                }
                seen_hash = true;
                continue;
            }
            _ => {}
        }
        if seen_hash {
            cur.v.push(t.clone());
        } else {
            cur.u.push(t.clone());
        }
    }
    if !seen_hash {
        diags.push(PreambleError::MissingHash);
    }
    cols.push(cur);
    ends.push(tokens.len());
    (cols, ends, loop_from, diags)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PreambleError {
    /// A column without a `#`. TeX says "Missing # inserted in alignment
    /// preamble".
    MissingHash,
    /// Two `#`s in one column.
    TwoHashes,
}

/// Set one row of entries to the known column widths.
///
/// `None` for `\noalign` material, which is not a row and is spliced in as it
/// stands.
/// Set one `\valign` column: its entries stacked vertically, each vpacked to
/// its row's height, with `\tabskip` at every boundary — the transpose of
/// [`set_row`], and `tex.web` §810 with the axes swapped.
pub fn set_column(
    align: &Align,
    row: &Row,
    heights: &[Scaled],
    metrics: &impl CharMetrics,
    max_depth: Scaled,
) -> Option<TeXBox> {
    let Row::Cells(cells) = row else { return None };
    let prototype = column_prototype(align, heights, max_depth);
    let mut vlist = alloc::vec![Node::Glue(align.tabskip(0))];
    let mut actual_cells = Vec::new();
    let mut idx = 0usize;
    for cell in cells {
        let k = cell.span.max(1);
        let first = heights.get(idx).copied().unwrap_or(cell.natural);
        let mut target = first;
        for j in 1..k {
            let g = align.tabskip(idx + j);
            target = target + independently_set_glue(&g, &prototype);
            target = target + heights.get(idx + j).copied().unwrap_or(Scaled::ZERO);
        }
        // Packed with `\boxmaxdepth` at zero so the entry's depth folds into
        // its height: the row size is the total extent, so a box that kept a
        // depth would push everything after it down by that much.
        let mut boxed = crate::node::vpack(cell.list.clone(), Some(target), Scaled::ZERO);
        // §810: a spanning entry's declared extent is its *first* row. The
        // swallowed tabskips and empty rows are explicit siblings below.
        boxed.height = first;
        actual_cells.push(vlist.len());
        vlist.push(Node::Box(alloc::boxed::Box::new(boxed)));
        for j in 1..k {
            vlist.push(Node::Glue(align.tabskip(idx + j)));
            vlist.push(Node::Box(alloc::boxed::Box::new(empty_vbox(
                heights.get(idx + j).copied().unwrap_or(Scaled::ZERO),
            ))));
        }
        idx += k;
        vlist.push(Node::Glue(align.tabskip(idx)));
    }
    let _ = metrics;
    let mut column = crate::node::vpack(vlist, None, max_depth);
    column.height = prototype.height;
    column.glue_sign = prototype.glue_sign;
    column.glue_order = prototype.glue_order;
    column.set_num = prototype.set_num;
    column.set_den = prototype.set_den;
    // §810's vertical branch, the transpose of §811: a `\valign` cell takes the
    // *column's* width, as an `\halign` cell takes the row's height and depth.
    // Same reason — a cell is a window onto the column — and the same symptom
    // if it is missed: a cell holding a running `\hrule` was 0pt wide and the
    // rule had nothing to run to.
    let w = column.width;
    for i in actual_cells {
        if let Some(Node::Box(b)) = column.list.get_mut(i) {
            b.width = w;
        }
    }
    Some(column)
}

/// §806's transposed prototype for a `\valign`. Besides supplying the glue
/// setting to every finished column, this is the one constrained pack whose
/// badness TeX reports for the whole alignment.
pub fn column_prototype(align: &Align, heights: &[Scaled], max_depth: Scaled) -> TeXBox {
    let mut prototype_list = Vec::new();
    prototype_list.push(Node::Glue(align.tabskip(0)));
    for (i, &h) in heights.iter().enumerate() {
        prototype_list.push(Node::Box(alloc::boxed::Box::new(empty_vbox(h))));
        prototype_list.push(Node::Glue(align.tabskip(i + 1)));
    }
    let prototype_natural = crate::node::vpack(prototype_list.clone(), None, max_depth).height;
    let prototype_target = match (align.to, align.spread) {
        (Some(t), _) => Some(t),
        (None, Some(sp)) => Some(prototype_natural + sp),
        (None, None) => None,
    };
    crate::node::vpack(prototype_list, prototype_target, max_depth)
}

/// Each row of a `\valign` is as tall as its tallest entry — [`column_widths`]
/// transposed.
pub fn row_heights(align: &mut Align) -> Vec<Scaled> {
    column_widths(align)
}

pub fn set_row(
    align: &Align,
    row: &Row,
    widths: &[Scaled],
    metrics: &impl CharMetrics,
) -> Option<TeXBox> {
    let Row::Cells(cells) = row else { return None };
    let prototype = row_prototype(align, widths, metrics);
    let mut hlist = Vec::new();
    let mut actual_cells = Vec::new();
    // `\tabskip` before the first column, between each pair, and after the
    // last — the boundary's own glue, not one value throughout.
    hlist.push(Node::Glue(align.tabskip(0)));
    let mut col = 0usize;
    for cell in cells {
        let k = cell.span.max(1);
        let first = widths.get(col).copied().unwrap_or(cell.natural);
        let mut target = first;
        for j in 1..k {
            let g = align.tabskip(col + j);
            target = target + independently_set_glue(&g, &prototype);
            target = target + widths.get(col + j).copied().unwrap_or(Scaled::ZERO);
        }
        let mut boxed = hpack(cell.list.clone(), Some(target), metrics);
        // A span retains the first column as the cell box's declared width.
        // The remaining columns have to be real siblings: besides matching
        // TeX's node list, this prevents glue rounding inside the cell from
        // changing the reference point of every later entry.
        boxed.width = first;
        actual_cells.push(hlist.len());
        hlist.push(Node::Box(alloc::boxed::Box::new(boxed)));
        for j in 1..k {
            hlist.push(Node::Glue(align.tabskip(col + j)));
            hlist.push(Node::Box(alloc::boxed::Box::new(empty_hbox(
                widths.get(col + j).copied().unwrap_or(Scaled::ZERO),
            ))));
        }
        col += k;
        hlist.push(Node::Glue(align.tabskip(col)));
    }
    let mut row = hpack(hlist, None, metrics);
    row.width = prototype.width;
    row.glue_sign = prototype.glue_sign;
    row.glue_order = prototype.glue_order;
    row.set_num = prototype.set_num;
    row.set_den = prototype.set_den;
    // §811: every cell in a finished row takes the *row's* height and depth —
    //
    //     height(r):=height(q); depth(r):=depth(q)
    //
    // — not its own. A cell is a window onto the row, which is why a `\vrule`
    // with a running height fills the row's full height however short the cell
    // that holds it: the rule stays running, and the cell it is measured
    // against grows. We left each cell at its natural size, so a cell holding
    // only a rule was 0pt tall and the rule had nothing to run to.
    let (h, d) = (row.height, row.depth);
    for i in actual_cells {
        if let Some(Node::Box(b)) = row.list.get_mut(i) {
            b.height = h;
            b.depth = d;
        }
    }
    Some(row)
}

/// §806's prototype for a `\halign`. TeX constrains this box once, reports
/// that pack's badness as an alignment warning, and copies its glue setting to
/// every row.
pub fn row_prototype(align: &Align, widths: &[Scaled], metrics: &impl CharMetrics) -> TeXBox {
    // §806's packed preamble is the prototype for *every* row. In particular,
    // a short row inherits the full alignment's glue set instead of computing
    // a new ratio from the smaller number of tabskips it happens to contain.
    let mut prototype_list = alloc::vec![Node::Glue(align.tabskip(0))];
    for (i, &w) in widths.iter().enumerate() {
        prototype_list.push(Node::Box(alloc::boxed::Box::new(empty_hbox(w))));
        prototype_list.push(Node::Glue(align.tabskip(i + 1)));
    }
    let prototype_natural = hpack(prototype_list.clone(), None, metrics).width;
    let prototype_target = match (align.to, align.spread) {
        (Some(t), _) => Some(t),
        (None, Some(s)) => Some(prototype_natural + s),
        (None, None) => None,
    };
    hpack(prototype_list, prototype_target, metrics)
}

/// Resolve ordinary and spanning width constraints in TeX's column order.
pub fn column_widths(align: &mut Align) -> Vec<Scaled> {
    let n = align
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Cells(c) => Some(c.iter().map(|e| e.span.max(1)).sum::<usize>()),
            Row::NoAlign { .. } => None,
        })
        .max()
        .unwrap_or(0)
        .max(align.cols.len());
    // §787 and §793 start every alignrecord at `width(cur_align):=null_flag`,
    // and §801 raises it with `if w>width(cur_align)`. `null_flag` is
    // −2^30, not zero, so a column whose entries are all *narrower than
    // nothing* takes their width: amsmath's tag column holds one
    // `\kern-\alignsep@` and is −10pt wide, and every `align` row is that much
    // narrower than the sum of its parts. A zero floor made the rows
    // `\minalignsep` too wide.
    //
    // A column no row occupies keeps `null_flag` in TeX and §805 turns it into
    // zero, which is what `unwrap_or(ZERO)` does here.
    // `constraints[i][j]` is TeX's w_ij: the widest entry beginning in
    // column i and ending in column j. Keeping the two indices is essential;
    // updating the final column immediately as rows arrive makes a long span
    // seen before a shorter one permanently too wide.
    let mut constraints: Vec<Vec<Option<Scaled>>> = alloc::vec![alloc::vec![None; n]; n];
    for row in &align.rows {
        if let Row::Cells(cells) = row {
            let mut col = 0usize;
            for cell in cells {
                let k = cell.span.max(1);
                let end = col.saturating_add(k).saturating_sub(1);
                if col < n && end < n && constraints[col][end].is_none_or(|w| cell.natural > w) {
                    constraints[col][end] = Some(cell.natural);
                }
                col += k;
            }
        }
    }

    // §805, written directly from Knuth's formula:
    //
    //   w_j = max_i (w_ij - sum_{i <= k < j}(t_k + w_k)).
    //
    // Resolving by ending column makes all widths to the left final before a
    // longer constraint uses them. If no constraint ends at j, TeX makes the
    // column and its following tabskip zero.
    let mut widths = alloc::vec![Scaled::ZERO; n];
    for end in 0..n {
        let mut best: Option<Scaled> = None;
        for start in 0..=end {
            let Some(mut candidate) = constraints[start][end] else {
                continue;
            };
            for k in start..end {
                candidate = candidate - widths[k] - align.tabskip(k + 1).width;
            }
            if best.is_none_or(|w| candidate > w) {
                best = Some(candidate);
            }
        }
        if let Some(w) = best {
            widths[end] = w;
        } else if let Some(g) = align.tabskips.get_mut(end + 1) {
            *g = Glue::default();
        }
    }
    widths
}

/// TeX computes a spanning cell's target using each swallowed tabskip's set
/// width independently (§811), rather than the outer box's running glue
/// accumulator. A fresh setter spells that single-glue rounding exactly.
fn independently_set_glue(g: &Glue, prototype: &TeXBox) -> Scaled {
    let mut setter = crate::node::GlueSetter::new();
    setter.width(
        g,
        prototype.glue_sign,
        prototype.glue_order,
        prototype.set_num,
        prototype.set_den,
    )
}

fn empty_hbox(width: Scaled) -> TeXBox {
    TeXBox {
        kind: crate::node::BoxKind::Horizontal,
        width,
        height: Scaled::ZERO,
        depth: Scaled::ZERO,
        set_num: 0,
        set_den: 1,
        glue_sign: crate::node::GlueSign::Normal,
        glue_order: 0,
        shift: Scaled::ZERO,
        list: Vec::new(),
    }
}

fn empty_vbox(height: Scaled) -> TeXBox {
    TeXBox {
        kind: crate::node::BoxKind::Vertical,
        width: Scaled::ZERO,
        height,
        depth: Scaled::ZERO,
        set_num: 0,
        set_den: 1,
        glue_sign: crate::node::GlueSign::Normal,
        glue_order: 0,
        shift: Scaled::ZERO,
        list: Vec::new(),
    }
}

/// A box's natural width, for measuring an entry as it closes.
pub fn natural_width(list: &[Node], metrics: &impl CharMetrics) -> Scaled {
    hpack(list.to_vec(), None, metrics).width
}

/// Not used yet, but the type the driver will want when `\span` lands.
pub type Unset = TeXBox;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catcode::CatCode;
    use crate::intern::Interner;
    use core::cell::RefCell;

    std::thread_local! {
        static INTERNER: RefCell<Interner> = RefCell::new(Interner::new());
    }

    fn ch(c: char, k: CatCode) -> Token {
        Token::Char(c, k)
    }
    fn cs(n: &str) -> Token {
        INTERNER.with(|i| Token::ControlSequence(i.borrow_mut().intern(n)))
    }

    /// `\halign{u#v&x#y\cr}` — two columns, each with a template either side
    /// of the `#`.
    #[test]
    fn a_preamble_splits_into_columns() {
        let toks = alloc::vec![
            ch('u', CatCode::Letter),
            ch('#', CatCode::Parameter),
            ch('v', CatCode::Letter),
            ch('&', CatCode::AlignTab),
            ch('x', CatCode::Letter),
            ch('#', CatCode::Parameter),
            ch('y', CatCode::Letter),
        ];
        let (cols, _, _, _) = parse_preamble(&toks);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].u, alloc::vec![ch('u', CatCode::Letter)]);
        assert_eq!(cols[0].v, alloc::vec![ch('v', CatCode::Letter)]);
        assert_eq!(cols[1].u, alloc::vec![ch('x', CatCode::Letter)]);
        assert_eq!(cols[1].v, alloc::vec![ch('y', CatCode::Letter)]);
    }

    /// A column may have an empty template on either side, which is the
    /// commonest case of all: `\halign{#&#\cr}`.
    #[test]
    fn empty_templates_are_fine() {
        let toks = alloc::vec![
            ch('#', CatCode::Parameter),
            ch('&', CatCode::AlignTab),
            ch('#', CatCode::Parameter),
        ];
        let (cols, _, _, _) = parse_preamble(&toks);
        assert_eq!(cols.len(), 2);
        assert!(cols[0].u.is_empty() && cols[0].v.is_empty());
    }

    /// `&` and `#` inside braces belong to the template, not to the preamble's
    /// structure.
    #[test]
    fn separators_inside_braces_are_ordinary() {
        let toks = alloc::vec![
            cs("hbox"),
            ch('{', CatCode::BeginGroup),
            ch('&', CatCode::AlignTab),
            ch('}', CatCode::EndGroup),
            ch('#', CatCode::Parameter),
        ];
        let (cols, _, _, _) = parse_preamble(&toks);
        assert_eq!(cols.len(), 1, "the braced & did not split a column");
        assert_eq!(cols[0].u.len(), 4);
    }

    /// The reported end of each column is what maps a `\tabskip` assignment
    /// onto a boundary, so it has to be the position of the `&`.
    #[test]
    fn column_ends_are_reported() {
        let toks = alloc::vec![
            ch('#', CatCode::Parameter),
            ch('&', CatCode::AlignTab),
            ch('a', CatCode::Letter),
            ch('#', CatCode::Parameter),
        ];
        let (cols, ends, _, _) = parse_preamble(&toks);
        assert_eq!(cols.len(), 2);
        // The `&` sits at index 1 once the two `#`s are dropped... it is the
        // index *in the input slice*, which is what the caller compares its
        // recorded change positions against.
        assert_eq!(ends, alloc::vec![1, 4]);
    }

    /// `tex.web` §783 and §784 both *recover*: the first says a `#` was missing
    /// and carries on with the template it scanned, the second says only one is
    /// allowed and drops the rest. Neither abandons the alignment, so both come
    /// back as diagnostics beside a usable preamble.
    #[test]
    fn a_column_without_a_hash_is_reported_and_recovered() {
        let toks = alloc::vec![ch('u', CatCode::Letter)];
        let (cols, _, _, diags) = parse_preamble(&toks);
        assert_eq!(diags, alloc::vec![PreambleError::MissingHash]);
        assert_eq!(cols.len(), 1, "the column is still there");
        assert_eq!(cols[0].u.len(), 1);
        assert!(cols[0].v.is_empty());

        let toks = alloc::vec![ch('#', CatCode::Parameter), ch('#', CatCode::Parameter)];
        let (cols, _, _, diags) = parse_preamble(&toks);
        assert_eq!(diags, alloc::vec![PreambleError::TwoHashes]);
        assert_eq!(cols.len(), 1);
        assert!(cols[0].u.is_empty() && cols[0].v.is_empty());
    }
}
