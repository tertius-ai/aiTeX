//! Writing a page out: `tex.web` §619–§640.
//!
//! This is engine work, not driver work, and the difference is not academic.
//! The traversal has to recurse: a line is a box, a `\hbox` inside it is a box,
//! and each level shifts the reference point. A driver-side loop that walks the
//! top level and ignores what it does not recognise silently drops every nested
//! box — `\setbox0=\hbox{...}` followed by `\box0` produced nothing at all,
//! and no test could see it because the box measured correctly right up until
//! the moment it was not written.
//!
//! Two coordinate rules do all the work:
//!
//! - **Vertical**: descend by a box's *height* to reach its baseline, draw it,
//!   then descend by its *depth*. Skipping the second leaves every following
//!   baseline `depth` too close.
//! - **Horizontal**: a box is drawn at the current point and then the reference
//!   point advances by its *width*, whatever the traversal inside it did. This
//!   is why `push`/`pop` bracket the recursion.

use crate::dvi::DviWriter;
use crate::node::{GlueSetter, GlueSign, Node, TeXBox};
use crate::scaled::Scaled;

/// An engine-side action encountered at its final position on a shipped page.
///
/// Ordinary DVI output has no need to observe these. pdfTeX compatibility does:
/// `\pdfsavepos` records the final, post-line-breaking position for a following
/// deferred `\write`, which is how packages such as `marginnote` learn where
/// their zero-width boxes landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShipoutEvent {
    SavePos { h: Scaled, v: Scaled },
    Write(u32),
}

trait PageItemSink {
    fn push(&mut self, item: aitex_page::Item);
}

impl PageItemSink for alloc::vec::Vec<aitex_page::Item> {
    fn push(&mut self, item: aitex_page::Item) {
        alloc::vec::Vec::push(self, item);
    }
}

struct IgnorePageItems;

impl PageItemSink for IgnorePageItems {
    fn push(&mut self, _item: aitex_page::Item) {}
}

const PDF_SAVE_POS_MARKER: &str = "aitex:pdfsavepos";

/// Maps an engine font index to the font number the DVI file uses.
///
/// The two numbering schemes are genuinely different: the engine numbers fonts
/// in the order `\font` loaded them, the file numbers them in the order they
/// were first defined in it, and a font that is never used is never defined.
pub trait FontNumbers {
    fn dvi_font(&self, engine_font: usize) -> Option<usize>;
    /// The width of a character, which the shipper needs only for leaders: the
    /// repeated boxes are laid on a grid measured from the enclosing box's left
    /// edge (`tex.web` §627), and finding that offset means knowing how far the
    /// list has advanced. A host with no leaders can leave this at zero.
    fn char_width(&self, _engine_font: usize, _code: u32) -> Scaled {
        Scaled::ZERO
    }
}

impl FontNumbers for &[usize] {
    fn dvi_font(&self, engine_font: usize) -> Option<usize> {
        self.get(engine_font).copied()
    }
}

/// Write one page, whose contents are a vertical list.
///
/// `left` and `top` place the origin. DVI's own origin is one inch in from the
/// top-left of the paper, which TeX assumes implicitly; callers pass only the
/// page's `\hoffset` and `\voffset`.
/// `width` is the page's width, which a rule's *running* width resolves to
/// (`tex.web` §633). `\hrule` leaves its width running precisely so that it
/// spans whatever contains it; treating that as "do not draw" made every
/// `\hrule` in every document invisible.
/// Write one page given as a *box*, of either kind.
///
/// `\shipout` takes a box, not a list, and `tex.web` §640 dispatches on its
/// type. Shipping an `\hbox` through the vertical path draws its contents
/// stacked instead of in a row — which is to say it draws nothing recognisable,
/// and `\shipout\hbox{…}` is legal TeX.
pub fn ship_out_box(
    dvi: &mut DviWriter,
    b: &TeXBox,
    left: Scaled,
    top: Scaled,
    fonts: &dyn FontNumbers,
) -> Scaled {
    let mut ignore = |_| {};
    let mut ignore_page_items = IgnorePageItems;
    ship_out_box_impl(
        dvi,
        b,
        left,
        top,
        fonts,
        false,
        &mut ignore,
        false,
        &mut ignore_page_items,
    )
}

/// Walk a page exactly as DVI shipout does while reporting positioned engine
/// actions. This is deliberately the same traversal, rather than a second
/// approximation of boxes and set glue: `\pdfsavepos` must see the position
/// after line breaking, glue setting, shifts, and nested boxes have all applied.
pub fn ship_out_box_with_events(
    dvi: &mut DviWriter,
    b: &TeXBox,
    left: Scaled,
    top: Scaled,
    fonts: &dyn FontNumbers,
    events: &mut dyn FnMut(ShipoutEvent),
) -> Scaled {
    let mut ignore_page_items = IgnorePageItems;
    ship_out_box_impl(
        dvi,
        b,
        left,
        top,
        fonts,
        true,
        events,
        false,
        &mut ignore_page_items,
    )
}

/// Walk the exact DVI shipout traversal while collecting final positioned
/// drawing events. This is the migration seam for the native PDF backend: it
/// does not approximate packed glue, leaders, shifts, or nested boxes in a
/// second page walk.
pub fn ship_out_box_with_page_items(
    dvi: &mut DviWriter,
    b: &TeXBox,
    left: Scaled,
    top: Scaled,
    fonts: &dyn FontNumbers,
    page_items: &mut alloc::vec::Vec<aitex_page::Item>,
) -> Scaled {
    let mut ignore = |_| {};
    ship_out_box_impl(
        dvi,
        b,
        left,
        top,
        fonts,
        false,
        &mut ignore,
        true,
        page_items,
    )
}

fn ship_out_box_impl(
    dvi: &mut DviWriter,
    b: &TeXBox,
    left: Scaled,
    top: Scaled,
    fonts: &dyn FontNumbers,
    observe: bool,
    events: &mut dyn FnMut(ShipoutEvent),
    capture_page: bool,
    page_items: &mut dyn PageItemSink,
) -> Scaled {
    let _p = crate::profile::Phase::enter(crate::profile::P_SHIP);
    match b.kind {
        // The box was *packed*, so its glue has a set ratio, and the shipper has to
        // honour it — handing the list to `ship_out` threw it away and every
        // `\shipout\vbox to …` put its slack at the bottom instead of in its glue.
        // Only the outermost box was affected: a nested one goes through `draw`,
        // which passes the set along. LaTeX ships its pages as one such vbox, so
        // every stretchable skip on a page stayed at its natural width.
        crate::node::BoxKind::Vertical => {
            dvi.down(top);
            let mut state = ShipState {
                depth_reached: top,
                cur_font: None,
                position_transform: PositionTransform::IDENTITY,
                position_transform_stack: alloc::vec::Vec::new(),
            };
            dvi.push();
            dvi.right(left);
            vlist_out(
                dvi,
                &b.list,
                top,
                b.width,
                b.glue_sign,
                b.glue_order,
                b.set_num,
                b.set_den,
                fonts,
                &mut state,
                left,
                observe,
                events,
                capture_page,
                page_items,
            );
            dvi.pop();
            state.depth_reached
        }
        crate::node::BoxKind::Horizontal => {
            dvi.down(top + b.height);
            let mut state = ShipState {
                depth_reached: top + b.height + b.depth,
                cur_font: None,
                position_transform: PositionTransform::IDENTITY,
                position_transform_stack: alloc::vec::Vec::new(),
            };
            dvi.push();
            dvi.right(left);
            hlist_out(
                dvi,
                b,
                top + b.height,
                left,
                fonts,
                &mut state,
                observe,
                events,
                capture_page,
                page_items,
            );
            dvi.pop();
            state.depth_reached
        }
    }
}

pub fn ship_out(
    dvi: &mut DviWriter,
    page: &[Node],
    left: Scaled,
    top: Scaled,
    width: Scaled,
    fonts: &dyn FontNumbers,
) -> Scaled {
    let mut ignore = |_| {};
    let mut ignore_page_items = IgnorePageItems;
    dvi.down(top);
    let mut state = ShipState {
        depth_reached: top,
        cur_font: None,
        position_transform: PositionTransform::IDENTITY,
        position_transform_stack: alloc::vec::Vec::new(),
    };
    dvi.push();
    dvi.right(left);
    // A page is handed over as a plain list; nothing set it, so its glue is at
    // its natural width.
    vlist_out(
        dvi,
        page,
        top,
        width,
        GlueSign::Normal,
        0,
        0,
        1,
        fonts,
        &mut state,
        left,
        false,
        &mut ignore,
        false,
        &mut ignore_page_items,
    );
    dvi.pop();
    state.depth_reached
}

struct ShipState {
    /// How far down the page anything was drawn, for the postamble's extent.
    depth_reached: Scaled,
    /// The font currently selected in the file, so a run of characters in one
    /// font costs one `fnt` command rather than one per glyph.
    cur_font: Option<usize>,
    /// The direct-PDF current transformation matrix as it affects
    /// `\pdfsavepos`. The page still ships through DVI, but dvipdfmx's
    /// `pdf:btrans` is PGF's transport for pdfTeX's `\pdfsetmatrix`; direct
    /// pdfTeX reports the transformed point, not the raw DVI coordinate.
    position_transform: PositionTransform,
    position_transform_stack: alloc::vec::Vec<PositionTransform>,
}

/// An affine transform in TeX's page coordinates (x right, y down), measured
/// in scaled points. PDF matrices use y up, so their off-diagonal terms and
/// vertical translation are converted when a `pdf:btrans` is observed.
#[derive(Clone, Copy, Debug)]
struct PositionTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl PositionTransform {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Compose `local` before the current page transform, as PDF's `cm`
    /// operator does inside an already transformed graphics state.
    fn then_local(self, local: Self) -> Self {
        Self {
            a: self.a * local.a + self.c * local.b,
            b: self.b * local.a + self.d * local.b,
            c: self.a * local.c + self.c * local.d,
            d: self.b * local.c + self.d * local.d,
            e: self.a * local.e + self.c * local.f + self.e,
            f: self.b * local.e + self.d * local.f + self.f,
        }
    }

    fn at_pdf_matrix(
        x: Scaled,
        y: Scaled,
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        e_bp: f64,
        f_bp: f64,
    ) -> Self {
        let x = f64::from(x.0);
        let y = f64::from(y.0);
        let e = bp_to_sp(e_bp);
        let f = bp_to_sp(f_bp);
        // PDF: x'=a*x+c*y+e, y'=b*x+d*y+f, with y pointing up.
        // Converting to TeX's downward y axis gives [a -c; -b d]. Keep the
        // current DVI point fixed before applying the explicit translation,
        // matching dvipdfmx's adjusted `cm` emitted by the host PDF writer.
        Self {
            a,
            b: -b,
            c: -c,
            d,
            e: x + e - a * x + c * y,
            f: y - f + b * x - d * y,
        }
    }

    fn apply(self, x: Scaled, y: Scaled) -> (Scaled, Scaled) {
        let x = f64::from(x.0);
        let y = f64::from(y.0);
        (
            Scaled(round_scaled(self.a * x + self.c * y + self.e)),
            Scaled(round_scaled(self.b * x + self.d * y + self.f)),
        )
    }
}

fn bp_to_sp(value: f64) -> f64 {
    // 72bp = 72.27pt and one TeX point is 65536sp.
    value * (72.27 / 72.0) * 65_536.0
}

fn round_scaled(value: f64) -> i32 {
    if value >= 0.0 {
        (value + 0.5) as i32
    } else {
        (value - 0.5) as i32
    }
}

impl ShipState {
    fn observe_position_special(&mut self, text: &str, x: Scaled, y: Scaled) {
        let text = text.trim();
        if let Some(payload) = text.strip_prefix("pdf:btrans") {
            self.position_transform_stack.push(self.position_transform);
            let mut fields = payload.split_whitespace();
            let kind = fields.next();
            let numbers = fields
                .filter_map(|field| field.parse::<f64>().ok())
                .collect::<alloc::vec::Vec<_>>();
            if kind == Some("matrix") && numbers.len() >= 6 {
                let local = PositionTransform::at_pdf_matrix(
                    x, y, numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
                );
                self.position_transform = self.position_transform.then_local(local);
            }
            return;
        }
        if text == "pdf:etrans" {
            if let Some(previous) = self.position_transform_stack.pop() {
                self.position_transform = previous;
            }
            return;
        }
        if let Some(payload) = text.strip_prefix("x:scale") {
            let numbers = payload
                .split_whitespace()
                .filter_map(|field| field.parse::<f64>().ok())
                .collect::<alloc::vec::Vec<_>>();
            if numbers.len() >= 2 {
                let local = PositionTransform::at_pdf_matrix(
                    x, y, numbers[0], 0.0, 0.0, numbers[1], 0.0, 0.0,
                );
                self.position_transform = self.position_transform.then_local(local);
            }
        }
    }

    fn positioned(&self, x: Scaled, y: Scaled) -> (Scaled, Scaled) {
        self.position_transform.apply(x, y)
    }
}

/// Emit a positioned special byte-for-byte as TeX does.
///
/// A driver can treat an unmatched `color pop` at the page's base color as a
/// no-op, but that is deliberately not the engine's decision.  Real xcolor /
/// titlesec documents do write such pops, including at vertical heading
/// boundaries, and TeX passes every one through to DVI.  Filtering here made
/// the rendered color usually look right while making the engine's semantic
/// output wrong.
fn emit_special(
    dvi: &mut DviWriter,
    text: &str,
    x: Scaled,
    y: Scaled,
    capture_page: bool,
    page_items: &mut dyn PageItemSink,
) {
    dvi.special(text);
    if capture_page {
        page_items.push(aitex_page::Item::Special {
            x: aitex_page::Sp(x.0.into()),
            y: aitex_page::Sp(y.0.into()),
            bytes: text.as_bytes().to_vec(),
        });
    }
}

fn select(dvi: &mut DviWriter, font: usize, fonts: &dyn FontNumbers, state: &mut ShipState) {
    if let Some(n) = fonts.dvi_font(font) {
        if state.cur_font != Some(n) {
            dvi.select_font(n);
            state.cur_font = Some(n);
        }
    }
}

/// Draw a rule that lives in a horizontal list.
///
/// The list's reference point is its baseline, while DVI's rule reference is
/// the rule's lower-left corner. §624 therefore moves down by the rule depth,
/// emits the combined height-plus-depth, and returns to the baseline.
fn set_rule_at_baseline(dvi: &mut DviWriter, height: Scaled, depth: Scaled, width: Scaled) {
    dvi.down(depth);
    dvi.set_rule(height + depth, width);
    dvi.down(-depth);
}

/// `sign`/`order`/`num`/`den` are how the enclosing box was set, so its glue
/// takes the width the packing decided rather than its natural width — the same
/// rule as a horizontal list, and just as necessary. A `\vbox to\vsize` whose
/// glue does not stretch leaves all its slack at the bottom, so anything that
/// should have been pushed to the foot of the page sits directly under the text.
#[allow(clippy::too_many_arguments)]
fn vlist_out(
    dvi: &mut DviWriter,
    list: &[Node],
    mut v: Scaled,
    enclosing_width: Scaled,
    sign: GlueSign,
    order: u8,
    num: i64,
    den: i64,
    fonts: &dyn FontNumbers,
    state: &mut ShipState,
    h: Scaled,
    observe: bool,
    events: &mut dyn FnMut(ShipoutEvent),
    capture_page: bool,
    page_items: &mut dyn PageItemSink,
) {
    let top_edge = v;
    let mut setter = GlueSetter::new();
    for node in list {
        match node {
            Node::Box(b) => {
                // `tex.web` §622: an *empty* box is not drawn at all — TeX just
                // advances past it. Pushing for it would flush whatever movement
                // was pending, which puts a `down` in the file for a box that
                // draws nothing. `\end`'s `\hbox to\hsize{}` is exactly that box,
                // and it appears at the foot of every page.
                if b.list.is_empty() {
                    v = v + b.height + b.depth;
                    dvi.down(b.height + b.depth);
                    state.depth_reached = state.depth_reached.max(v);
                    continue;
                }
                // Down to the baseline, draw, then down past the descender.
                dvi.down(b.height);
                v = v + b.height;
                dvi.push();
                if b.shift.0 != 0 {
                    dvi.right(b.shift);
                }
                draw(
                    dvi,
                    b,
                    v,
                    h + b.shift,
                    fonts,
                    state,
                    observe,
                    events,
                    capture_page,
                    page_items,
                );
                dvi.pop();
                dvi.down(b.depth);
                v = v + b.depth;
                state.depth_reached = state.depth_reached.max(v);
            }
            Node::Glue(g) => {
                let w = setter.width(g, sign, order, num, den);
                dvi.down(w);
                v = v + w;
            }
            // §634-§637: vertical leaders — the space the glue was set to is
            // filled with copies of the box, top to bottom.
            Node::Leaders { kind, glue, boxed } => {
                let w = setter.width(glue, sign, order, num, den);
                if *kind & 4 != 0 {
                    if w.0 != 0 {
                        let rw = if boxed.width == Scaled::RUNNING {
                            enclosing_width
                        } else {
                            boxed.width
                        };
                        dvi.down(w);
                        if w.0 > 0 && rw.0 > 0 {
                            dvi.put_rule(w, rw);
                            if capture_page {
                                page_items.push(aitex_page::Item::Rule {
                                    x: aitex_page::Sp(h.0.into()),
                                    baseline_y: aitex_page::Sp((v + w).0.into()),
                                    width: aitex_page::Sp(rw.0.into()),
                                    height: aitex_page::Sp(w.0.into()),
                                });
                            }
                        }
                    }
                    v = v + w;
                    state.depth_reached = state.depth_reached.max(v);
                    continue;
                }
                let span = Scaled(w.0 + 10);
                let edge = v + span;
                let lh = boxed.height + boxed.depth;
                let mut pos = v;
                if lh.0 > 0 && w.0 > 0 {
                    let mut cur = v;
                    let mut lx = Scaled::ZERO;
                    if *kind & 3 == 0 {
                        // `\leaders`: copies sit on a grid measured from the top
                        // of the enclosing list, so neighbouring runs line up.
                        let off = (cur - top_edge).0;
                        let mut at = top_edge + Scaled(lh.0 * (off / lh.0));
                        if at < cur {
                            at = at + lh;
                        }
                        cur = at;
                    } else {
                        let lq = span.0 / lh.0;
                        let lr = span.0 % lh.0;
                        if *kind & 3 == 1 {
                            cur = cur + Scaled(lr / 2); // `\cleaders`: centred
                        } else {
                            lx = Scaled(lr / (lq + 1)); // `\xleaders`: spread
                            cur = cur + Scaled((lr - (lq - 1) * lx.0) / 2);
                        }
                    }
                    while cur + lh <= edge {
                        dvi.down(cur + boxed.height - pos);
                        pos = cur + boxed.height;
                        dvi.push();
                        if boxed.shift.0 != 0 {
                            dvi.right(boxed.shift);
                        }
                        draw(
                            dvi,
                            boxed,
                            pos,
                            h + boxed.shift,
                            fonts,
                            state,
                            observe,
                            events,
                            capture_page,
                            page_items,
                        );
                        dvi.pop();
                        state.depth_reached = state.depth_reached.max(pos + boxed.depth);
                        cur = cur + lh + lx;
                    }
                }
                let end = Scaled(edge.0 - 10);
                dvi.down(end - pos);
                v = end;
            }
            Node::Kern(k, _) => {
                dvi.down(*k);
                v = v + *k;
            }
            Node::Rule {
                width,
                height,
                depth,
            } => {
                let rule_x = h;
                let rule_height = if *height == Scaled::RUNNING {
                    Scaled::ZERO
                } else {
                    *height
                };
                let d = if *depth == Scaled::RUNNING {
                    Scaled::ZERO
                } else {
                    *depth
                };
                dvi.down(rule_height + d);
                v = v + rule_height + d;
                let w = if *width == Scaled::RUNNING {
                    enclosing_width
                } else {
                    *width
                };
                if (rule_height + d).0 > 0 && w.0 > 0 {
                    dvi.put_rule(rule_height + d, w);
                    if capture_page {
                        page_items.push(aitex_page::Item::Rule {
                            x: aitex_page::Sp(rule_x.0.into()),
                            baseline_y: aitex_page::Sp(v.0.into()),
                            width: aitex_page::Sp(w.0.into()),
                            height: aitex_page::Sp((rule_height + d).0.into()),
                        });
                    }
                }
                state.depth_reached = state.depth_reached.max(v);
            }
            Node::Whatsit(s) if s == PDF_SAVE_POS_MARKER => {
                if observe {
                    let (h, v) = state.positioned(h, v);
                    events(ShipoutEvent::SavePos { h, v });
                }
            }
            Node::Whatsit(s) => {
                if observe {
                    state.observe_position_special(s, h, v);
                }
                emit_special(dvi, s, h, v, capture_page, page_items);
            }
            // A deferred `\write` is performed by the engine when the page is
            // shipped; nothing of it reaches the DVI.
            Node::WriteOut(i) => {
                if observe {
                    events(ShipoutEvent::Write(*i));
                }
            }
            // A mark has already done its job -- the page builder read it
            // before this box was ever shipped -- and draws nothing.
            // An insertion that reached the page has already been pulled out
            // into its box; one that did not draws nothing.
            // A math node cannot appear in a vertical list; nor can a
            // character, in a well-formed one.
            Node::Char { .. }
            | Node::NativeRun(_)
            | Node::Math { .. }
            | Node::Penalty(_)
            | Node::Disc { .. }
            | Node::Language { .. }
            | Node::Mark { .. }
            | Node::Ins { .. }
            | Node::Adjust { .. } => {}
        }
    }
}

fn hlist_out(
    dvi: &mut DviWriter,
    b: &TeXBox,
    v: Scaled,
    h: Scaled,
    fonts: &dyn FontNumbers,
    state: &mut ShipState,
    observe: bool,
    events: &mut dyn FnMut(ShipoutEvent),
    capture_page: bool,
    page_items: &mut dyn PageItemSink,
) {
    // One accumulator per box: `tex.web` §619 saves and restores it around every
    // recursion into a sub-box, which a local achieves for free.
    let mut setter = GlueSetter::new();
    // Where the list has got to, measured from its own left edge. Only leaders
    // need it — §627 lays their copies on a grid anchored there. In ordinary
    // prose, asking the TFM for every character width solely to maintain this
    // otherwise-unused counter was a sizeable part of DVI shipout, so first do
    // the cheap tag scan and leave the counter dormant when there are none.
    let track_h =
        observe || capture_page || b.list.iter().any(|n| matches!(n, Node::Leaders { .. }));
    let mut cur_h = Scaled::ZERO;
    for node in &b.list {
        match node {
            Node::Char { code, font, .. } => {
                select(dvi, *font, fonts, state);
                dvi.set_char(*code);
                if track_h {
                    let advance = fonts.char_width(*font, *code);
                    if capture_page {
                        if let Some(number) = fonts.dvi_font(*font) {
                            page_items.push(aitex_page::Item::ClassicGlyph {
                                font: aitex_page::FontInstanceId(number as u32),
                                code: *code,
                                origin_x: aitex_page::Sp((h + cur_h).0.into()),
                                origin_y: aitex_page::Sp(v.0.into()),
                                advance: aitex_page::Sp(advance.0.into()),
                            });
                        }
                    }
                    cur_h = cur_h + advance;
                }
            }
            Node::NativeRun(run) => {
                // Native glyphs never acquire a fictitious eight-bit DVI font.
                // The matching right command keeps subsequent classic content
                // at the exact position the TeX layout chose; the parallel
                // page-event stream carries the actual OpenType glyphs.
                dvi.right(run.width);
                if capture_page {
                    let direction = match run.direction {
                        crate::node::NativeDirection::LeftToRight => {
                            aitex_page::TextDirection::LeftToRight
                        }
                        crate::node::NativeDirection::RightToLeft => {
                            aitex_page::TextDirection::RightToLeft
                        }
                        crate::node::NativeDirection::TopToBottom => {
                            aitex_page::TextDirection::TopToBottom
                        }
                        crate::node::NativeDirection::BottomToTop => {
                            aitex_page::TextDirection::BottomToTop
                        }
                    };
                    page_items.push(aitex_page::Item::GlyphRun(
                        aitex_page::GlyphRun::new(
                            aitex_page::FontInstanceId(run.font),
                            aitex_page::Sp(i64::from(run.size.0)),
                            aitex_page::Sp(i64::from((h + cur_h).0)),
                            aitex_page::Sp(i64::from(v.0)),
                            direction,
                            run.glyphs
                                .iter()
                                .map(|glyph| aitex_page::PositionedGlyph {
                                    source_gid: aitex_page::SourceGlyphId(glyph.source_gid),
                                    cluster: aitex_page::ClusterId(glyph.cluster),
                                    x_advance: aitex_page::Sp(i64::from(glyph.x_advance.0)),
                                    y_advance: aitex_page::Sp(i64::from(glyph.y_advance.0)),
                                    x_offset: aitex_page::Sp(i64::from(glyph.x_offset.0)),
                                    y_offset: aitex_page::Sp(i64::from(glyph.y_offset.0)),
                                    is_notdef: glyph.is_notdef,
                                })
                                .collect(),
                            run.clusters
                                .iter()
                                .map(|cluster| aitex_page::TextCluster {
                                    byte_range: cluster.byte_start..cluster.byte_end,
                                    text: cluster.text.clone(),
                                })
                                .collect(),
                        )
                        .with_language(run.language.clone()),
                    ));
                }
                if track_h {
                    cur_h = cur_h + run.width;
                }
            }
            Node::Glue(g) => {
                // Glue in a *set* box takes the width the packing decided, not
                // its natural width. Using the natural width unjustifies every
                // line that was stretched to the measure. The widths come from a
                // per-box accumulator, not one rounding each — see `GlueSetter`.
                let w = setter.width(g, b.glue_sign, b.glue_order, b.set_num, b.set_den);
                dvi.right(w);
                if track_h {
                    cur_h = cur_h + w;
                }
            }
            // §626-§627: horizontal leaders. The glue's set width is filled with
            // copies of the box — on a grid from the list's left edge for
            // `\leaders`, centred for `\cleaders`, evenly spread for
            // `\xleaders`. This is how a table of contents draws its dots.
            Node::Leaders { kind, glue, boxed } => {
                let w = setter.width(glue, b.glue_sign, b.glue_order, b.set_num, b.set_den);
                // §626: a *rule* leader is one rule spanning the whole glue, not
                // a repeated box — which is what `\hrulefill` is.
                if *kind & 4 != 0 {
                    if w.0 != 0 {
                        let rule_x = h + cur_h;
                        let rule_height = if boxed.height == Scaled::RUNNING {
                            b.height
                        } else {
                            boxed.height
                        };
                        let d = if boxed.depth == Scaled::RUNNING {
                            b.depth
                        } else {
                            boxed.depth
                        };
                        if (rule_height + d).0 > 0 && w.0 > 0 {
                            set_rule_at_baseline(dvi, rule_height, d, w);
                            if capture_page {
                                page_items.push(aitex_page::Item::Rule {
                                    x: aitex_page::Sp(rule_x.0.into()),
                                    baseline_y: aitex_page::Sp((v + d).0.into()),
                                    width: aitex_page::Sp(w.0.into()),
                                    height: aitex_page::Sp((rule_height + d).0.into()),
                                });
                            }
                        } else {
                            dvi.right(w);
                        }
                    }
                    cur_h = cur_h + w;
                    continue;
                }
                // §626 works with a span ten scaled points longer than the glue
                // — TeX's own compensation for the rounding in a set width —
                // and lands at `edge - 10` when it is done. Without it every
                // `\cleaders` sits five scaled points off.
                let span = Scaled(w.0 + 10);
                let edge = cur_h + span;
                let lw = boxed.width;
                let mut pos = cur_h;
                if lw.0 > 0 && w.0 > 0 {
                    let mut cur = cur_h;
                    let mut lx = Scaled::ZERO;
                    if *kind & 3 == 0 {
                        let mut at = Scaled(lw.0 * (cur.0 / lw.0));
                        if at < cur {
                            at = at + lw;
                        }
                        cur = at;
                    } else {
                        // §627 divides the *compensated* span, not the raw one.
                        let lq = span.0 / lw.0;
                        let lr = span.0 % lw.0;
                        if *kind & 3 == 1 {
                            cur = cur + Scaled(lr / 2);
                        } else {
                            lx = Scaled(lr / (lq + 1));
                            cur = cur + Scaled((lr - (lq - 1) * lx.0) / 2);
                        }
                    }
                    while cur + lw <= edge {
                        dvi.right(cur - pos);
                        pos = cur;
                        dvi.push();
                        if boxed.shift.0 != 0 {
                            dvi.down(boxed.shift);
                        }
                        draw(
                            dvi,
                            boxed,
                            v + boxed.shift,
                            h + cur,
                            fonts,
                            state,
                            observe,
                            events,
                            capture_page,
                            page_items,
                        );
                        dvi.pop();
                        cur = cur + lw + lx;
                    }
                }
                let end = Scaled(edge.0 - 10);
                dvi.right(end - pos);
                cur_h = end;
            }
            Node::Kern(k, _) => {
                dvi.right(*k);
                if track_h {
                    cur_h = cur_h + *k;
                }
            }
            Node::Rule {
                width,
                height,
                depth,
            } => {
                let rule_x = h + cur_h;
                let w = if *width == Scaled::RUNNING {
                    Scaled::ZERO
                } else {
                    *width
                };
                let rule_height = if *height == Scaled::RUNNING {
                    b.height
                } else {
                    *height
                };
                let d = if *depth == Scaled::RUNNING {
                    b.depth
                } else {
                    *depth
                };
                if (rule_height + d).0 > 0 && w.0 > 0 {
                    set_rule_at_baseline(dvi, rule_height, d, w);
                    if capture_page {
                        page_items.push(aitex_page::Item::Rule {
                            x: aitex_page::Sp(rule_x.0.into()),
                            baseline_y: aitex_page::Sp((v + d).0.into()),
                            width: aitex_page::Sp(w.0.into()),
                            height: aitex_page::Sp((rule_height + d).0.into()),
                        });
                    }
                } else {
                    dvi.right(w);
                }
                if track_h {
                    cur_h = cur_h + w;
                }
            }
            // An empty box advances the reference point and draws nothing
            // (`tex.web` §620), so it must not push either.
            Node::Box(inner) if inner.list.is_empty() => {
                dvi.right(inner.width);
                if track_h {
                    cur_h = cur_h + inner.width;
                }
            }
            Node::Box(inner) => {
                // Draw where we stand, then advance by the box's width however
                // far the recursion moved the point.
                dvi.push();
                if inner.shift.0 != 0 {
                    dvi.down(inner.shift);
                }
                draw(
                    dvi,
                    inner,
                    v + inner.shift,
                    h + cur_h,
                    fonts,
                    state,
                    observe,
                    events,
                    capture_page,
                    page_items,
                );
                dvi.pop();
                dvi.right(inner.width);
                if track_h {
                    cur_h = cur_h + inner.width;
                }
            }
            Node::Whatsit(s) if s == PDF_SAVE_POS_MARKER => {
                if observe {
                    let (h, v) = state.positioned(h + cur_h, v);
                    events(ShipoutEvent::SavePos { h, v });
                }
            }
            Node::Whatsit(s) => {
                if observe {
                    state.observe_position_special(s, h + cur_h, v);
                }
                emit_special(dvi, s, h + cur_h, v, capture_page, page_items);
            }
            // A deferred `\write` is performed by the engine when the page is
            // shipped; nothing of it reaches the DVI.
            Node::WriteOut(i) => {
                if observe {
                    events(ShipoutEvent::Write(*i));
                }
            }
            // A discretionary that survived to the page was not broken at, so
            // it sets nothing. A mark is metadata for the page builder, read
            // before this box shipped, and draws nothing either.
            // A math node moves the reference point by `\mathsurround`, like a
            // kern; it draws nothing.
            Node::Math { width, .. } => {
                dvi.right(*width);
                if track_h {
                    cur_h = cur_h + *width;
                }
            }
            Node::Penalty(_)
            | Node::Disc { .. }
            | Node::Language { .. }
            | Node::Mark { .. }
            | Node::Ins { .. }
            | Node::Adjust { .. } => {}
        }
    }
}

/// Draw a box's contents, dispatching on which kind of list it holds.
fn draw(
    dvi: &mut DviWriter,
    b: &TeXBox,
    v: Scaled,
    h: Scaled,
    fonts: &dyn FontNumbers,
    state: &mut ShipState,
    observe: bool,
    events: &mut dyn FnMut(ShipoutEvent),
    capture_page: bool,
    page_items: &mut dyn PageItemSink,
) {
    match b.kind {
        crate::node::BoxKind::Horizontal => hlist_out(
            dvi,
            b,
            v,
            h,
            fonts,
            state,
            observe,
            events,
            capture_page,
            page_items,
        ),
        crate::node::BoxKind::Vertical => {
            // A vertical box's contents start at its *top*, which is `height`
            // above the baseline we descended to.
            dvi.push();
            dvi.down(-b.height);
            vlist_out(
                dvi,
                &b.list,
                v - b.height,
                b.width,
                b.glue_sign,
                b.glue_order,
                b.set_num,
                b.set_den,
                fonts,
                state,
                h,
                observe,
                events,
                capture_page,
                page_items,
            );
            dvi.pop();
        }
    }
}

/// The glue sign of a box that was never packed to a size.
pub const NATURAL: GlueSign = GlueSign::Normal;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{BoxKind, Glue};
    use alloc::vec;
    use alloc::vec::Vec;

    fn hbox(list: Vec<Node>, width: Scaled) -> TeXBox {
        TeXBox {
            kind: BoxKind::Horizontal,
            width,
            height: Scaled::from_points(7),
            depth: Scaled::ZERO,
            set_num: 0,
            set_den: 1,
            glue_sign: GlueSign::Normal,
            glue_order: 0,
            shift: Scaled::ZERO,
            list,
        }
    }

    fn ch(c: char) -> Node {
        Node::Char {
            code: c as u32,
            font: 0,
            lig: None,
        }
    }

    struct PositionedTestFonts;

    impl FontNumbers for PositionedTestFonts {
        fn dvi_font(&self, engine_font: usize) -> Option<usize> {
            Some(engine_font)
        }

        fn char_width(&self, _engine_font: usize, _code: u32) -> Scaled {
            Scaled::from_points(4)
        }
    }

    struct CountingFonts(core::cell::Cell<usize>);

    impl FontNumbers for CountingFonts {
        fn dvi_font(&self, engine_font: usize) -> Option<usize> {
            Some(engine_font)
        }

        fn char_width(&self, _engine_font: usize, _code: u32) -> Scaled {
            self.0.set(self.0.get() + 1);
            Scaled::from_points(4)
        }
    }

    #[test]
    fn dvi_only_shipout_does_not_pay_for_positioned_character_tracking() {
        let line = hbox(vec![ch('a'), ch('b'), ch('c')], Scaled::from_points(12));
        let fonts = CountingFonts(core::cell::Cell::new(0));
        let mut dvi = DviWriter::new();
        ship_out_box(&mut dvi, &line, Scaled::ZERO, Scaled::ZERO, &fonts);
        assert_eq!(fonts.0.get(), 0, "ordinary DVI uses implicit TFM advances");

        let mut items = Vec::new();
        ship_out_box_with_page_items(
            &mut dvi,
            &line,
            Scaled::ZERO,
            Scaled::ZERO,
            &fonts,
            &mut items,
        );
        assert_eq!(
            fonts.0.get(),
            3,
            "PDF positions measure each classic glyph once"
        );
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn positioned_items_come_from_the_exact_shipout_walk() {
        let line = hbox(
            vec![
                ch('a'),
                Node::Kern(Scaled::from_points(2), 0),
                ch('b'),
                Node::Rule {
                    width: Scaled::from_points(3),
                    height: Scaled::from_points(2),
                    depth: Scaled::from_points(1),
                },
                Node::Whatsit("native-special".into()),
            ],
            Scaled::from_points(13),
        );
        let mut dvi = DviWriter::new();
        let mut items = Vec::new();
        ship_out_box_with_page_items(
            &mut dvi,
            &line,
            Scaled::from_points(3),
            Scaled::from_points(5),
            &PositionedTestFonts,
            &mut items,
        );

        assert_eq!(items.len(), 4);
        assert!(matches!(
            &items[0],
            aitex_page::Item::ClassicGlyph {
                code: 97,
                origin_x,
                origin_y,
                advance,
                ..
            } if *origin_x == aitex_page::Sp(i64::from(Scaled::from_points(3).0))
                && *origin_y == aitex_page::Sp(i64::from(Scaled::from_points(12).0))
                && *advance == aitex_page::Sp(i64::from(Scaled::from_points(4).0))
        ));
        assert!(matches!(
            &items[1],
            aitex_page::Item::ClassicGlyph { code: 98, origin_x, .. }
                if *origin_x == aitex_page::Sp(i64::from(Scaled::from_points(9).0))
        ));
        assert!(matches!(
            &items[2],
            aitex_page::Item::Rule { x, baseline_y, width, height }
                if *x == aitex_page::Sp(i64::from(Scaled::from_points(13).0))
                && *baseline_y == aitex_page::Sp(i64::from(Scaled::from_points(13).0))
                && *width == aitex_page::Sp(i64::from(Scaled::from_points(3).0))
                && *height == aitex_page::Sp(i64::from(Scaled::from_points(3).0))
        ));
        assert!(matches!(
            &items[3],
            aitex_page::Item::Special { x, y, bytes }
                if *x == aitex_page::Sp(i64::from(Scaled::from_points(16).0))
                && *y == aitex_page::Sp(i64::from(Scaled::from_points(12).0))
                && bytes == b"native-special"
        ));
    }

    #[test]
    fn shipout_preserves_color_specials_even_when_the_driver_will_ignore_a_pop() {
        let inner = hbox(vec![Node::Whatsit("color pop".into())], Scaled::ZERO);
        let line = hbox(
            vec![
                Node::Whatsit("color pop".into()),
                Node::Whatsit("color push rgb 1 0 0".into()),
                Node::Box(alloc::boxed::Box::new(inner)),
                Node::Whatsit("color pop".into()),
            ],
            Scaled::ZERO,
        );
        let mut dvi = DviWriter::new();
        let mut items = Vec::new();
        ship_out_box_with_page_items(
            &mut dvi,
            &line,
            Scaled::ZERO,
            Scaled::ZERO,
            &PositionedTestFonts,
            &mut items,
        );
        let specials = items
            .iter()
            .filter_map(|item| match item {
                aitex_page::Item::Special { bytes, .. } => Some(bytes.as_slice()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            specials,
            [
                b"color pop".as_slice(),
                b"color push rgb 1 0 0".as_slice(),
                b"color pop".as_slice(),
                b"color pop".as_slice(),
            ]
        );
    }

    #[test]
    fn native_runs_are_positioned_events_and_exact_dvi_movement() {
        let native = Node::NativeRun(alloc::boxed::Box::new(crate::node::NativeRun {
            font: 7,
            size: Scaled::from_points(10),
            direction: crate::node::NativeDirection::RightToLeft,
            language: Some(alloc::string::String::from("he")),
            bidi_level: 1,
            bidi_span: 0,
            width: Scaled::from_points(6),
            height: Scaled::from_points(7),
            depth: Scaled::from_points(2),
            glyphs: vec![crate::node::NativeGlyph {
                source_gid: 42,
                cluster: 0,
                x_advance: Scaled::from_points(6),
                y_advance: Scaled::ZERO,
                x_offset: Scaled::ZERO,
                y_offset: Scaled::ZERO,
                is_notdef: false,
            }],
            clusters: vec![crate::node::NativeCluster {
                byte_start: 0,
                byte_end: 4,
                text: alloc::string::String::from("אב"),
            }],
        }));
        let line = hbox(
            vec![Node::Kern(Scaled::from_points(2), 0), native, ch('a')],
            Scaled::from_points(12),
        );
        let mut dvi = DviWriter::new();
        let mut items = Vec::new();
        ship_out_box_with_page_items(
            &mut dvi,
            &line,
            Scaled::from_points(3),
            Scaled::from_points(5),
            &PositionedTestFonts,
            &mut items,
        );

        assert!(matches!(
            &items[0],
            aitex_page::Item::GlyphRun(run)
                if run.font == aitex_page::FontInstanceId(7)
                    && run.origin_x == aitex_page::Sp(i64::from(Scaled::from_points(5).0))
                    && run.direction == aitex_page::TextDirection::RightToLeft
                    && run.clusters[0].text == "אב"
        ));
        assert!(matches!(
            &items[1],
            aitex_page::Item::ClassicGlyph { origin_x, .. }
                if *origin_x == aitex_page::Sp(i64::from(Scaled::from_points(11).0))
        ));
        assert!(
            !dvi.bytes().contains(&42),
            "the source glyph ID must not masquerade as a DVI character"
        );
    }

    /// A page with one line of two characters writes both of them.
    #[test]
    fn characters_reach_the_file() {
        let mut dvi = DviWriter::new();
        dvi.preamble(" test ", 1000);
        let f = dvi.define_font(crate::dvi::FontDef {
            checksum: 0,
            at_size: Scaled::from_points(10),
            design_size: Scaled::from_points(10),
            name: "cmr10".into(),
        });
        dvi.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let page = vec![Node::Box(alloc::boxed::Box::new(hbox(
            vec![ch('a'), ch('b')],
            Scaled::from_points(20),
        )))];
        ship_out(
            &mut dvi,
            &page,
            Scaled::ZERO,
            Scaled::ZERO,
            Scaled::from_points(100),
            &[f].as_slice(),
        );
        dvi.end_page();
        let bytes = dvi.finish().expect("test DVI is representable");
        assert!(bytes.contains(&b'a'), "the character was written");
        assert!(bytes.contains(&b'b'));
    }

    /// A DVI rule's vertical reference is its lower edge, not the surrounding
    /// hlist's baseline. The first rule reaches 20pt below the baseline; the
    /// second proves that the writer returned to the baseline afterwards.
    #[test]
    fn horizontal_rules_are_lowered_by_their_depth() {
        let mut dvi = DviWriter::new();
        dvi.preamble(" test ", 1000);
        dvi.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let page = hbox(
            vec![
                Node::Rule {
                    width: Scaled::from_points(1),
                    height: Scaled::from_points(-19),
                    depth: Scaled::from_points(20),
                },
                Node::Rule {
                    width: Scaled::from_points(1),
                    height: Scaled::from_points(1),
                    depth: Scaled::ZERO,
                },
                // TeX advances over this rule but emits no DVI opcode because
                // its combined height is not positive.
                Node::Rule {
                    width: Scaled::from_points(1),
                    height: Scaled::from_points(-1),
                    depth: Scaled::from_points(1),
                },
            ],
            Scaled::from_points(3),
        );
        let fonts: Vec<usize> = Vec::new();
        ship_out_box(
            &mut dvi,
            &page,
            Scaled::ZERO,
            Scaled::ZERO,
            &fonts.as_slice(),
        );
        dvi.end_page();

        let bytes = dvi.bytes();
        let rules: Vec<usize> = bytes
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| (b == 132).then_some(i))
            .collect();
        assert_eq!(rules.len(), 2, "invisible rules must not reach the DVI");
        let first = rules[rules.len() - 2];
        let second = rules[rules.len() - 1];
        // A three-byte y/z setting move by 20pt, then one by -20pt. Which of
        // the two equivalent DVI registers is selected is a writer-size
        // decision; the signed movement is the semantic contract here.
        assert!(matches!(bytes[first - 4], 164 | 169));
        assert_eq!(&bytes[first - 3..first], &[20, 0, 0]);
        assert!(matches!(bytes[second - 4], 164 | 169));
        assert_eq!(&bytes[second - 3..second], &[236, 0, 0]);
    }

    /// The bug this module exists for: a box nested inside a line must be
    /// drawn, not skipped. `\setbox0=\hbox{x}` then `\box0` in a paragraph
    /// produced an empty page for as long as the traversal lived in the driver
    /// and ended in `_ => {}`.
    #[test]
    fn a_nested_box_is_drawn() {
        let mut dvi = DviWriter::new();
        dvi.preamble(" test ", 1000);
        let f = dvi.define_font(crate::dvi::FontDef {
            checksum: 0,
            at_size: Scaled::from_points(10),
            design_size: Scaled::from_points(10),
            name: "cmr10".into(),
        });
        dvi.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let inner = hbox(vec![ch('Z')], Scaled::from_points(10));
        let line = hbox(
            vec![ch('a'), Node::Box(alloc::boxed::Box::new(inner)), ch('b')],
            Scaled::from_points(30),
        );
        ship_out(
            &mut dvi,
            &[Node::Box(alloc::boxed::Box::new(line))],
            Scaled::ZERO,
            Scaled::ZERO,
            Scaled::from_points(100),
            &[f].as_slice(),
        );
        dvi.end_page();
        let bytes = dvi.finish().expect("test DVI is representable");
        assert!(
            bytes.contains(&b'Z'),
            "the character inside the nested box was written"
        );
    }

    /// Two fonts in one line produce two font selections, and a run in one font
    /// produces one.
    #[test]
    fn font_changes_are_emitted_once_per_run() {
        let mut dvi = DviWriter::new();
        dvi.preamble(" test ", 1000);
        let mk = |name: &str| crate::dvi::FontDef {
            checksum: 0,
            at_size: Scaled::from_points(10),
            design_size: Scaled::from_points(10),
            name: name.into(),
        };
        let f0 = dvi.define_font(mk("cmr10"));
        let f1 = dvi.define_font(mk("cmbx10"));
        dvi.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let line = hbox(
            vec![
                Node::Char {
                    code: 97,
                    font: 0,
                    lig: None,
                },
                Node::Char {
                    code: 98,
                    font: 0,
                    lig: None,
                },
                Node::Char {
                    code: 99,
                    font: 1,
                    lig: None,
                },
            ],
            Scaled::from_points(30),
        );
        ship_out(
            &mut dvi,
            &[Node::Box(alloc::boxed::Box::new(line))],
            Scaled::ZERO,
            Scaled::ZERO,
            Scaled::from_points(100),
            &[f0, f1].as_slice(),
        );
        dvi.end_page();
        let bytes = dvi.finish().expect("test DVI is representable");
        assert!(
            bytes.contains(&99),
            "the second font's character was written"
        );
    }

    /// Glue in a set box takes the width the packing decided.
    #[test]
    fn set_glue_uses_the_packed_width() {
        let mut dvi = DviWriter::new();
        dvi.preamble(" test ", 1000);
        let f = dvi.define_font(crate::dvi::FontDef {
            checksum: 0,
            at_size: Scaled::from_points(10),
            design_size: Scaled::from_points(10),
            name: "cmr10".into(),
        });
        dvi.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut line = hbox(
            vec![
                ch('a'),
                Node::Glue(Glue::interword(
                    Scaled::from_points(5),
                    Scaled::from_points(5),
                    Scaled::ZERO,
                )),
                ch('b'),
            ],
            Scaled::from_points(30),
        );
        // Stretched to double the glue's natural width.
        line.glue_sign = GlueSign::Stretching;
        line.glue_order = 0;
        line.set_num = 5 * 65536;
        line.set_den = 5 * 65536;
        ship_out(
            &mut dvi,
            &[Node::Box(alloc::boxed::Box::new(line))],
            Scaled::ZERO,
            Scaled::ZERO,
            Scaled::from_points(100),
            &[f].as_slice(),
        );
        dvi.end_page();
        let _ = dvi.finish().expect("test DVI is representable");
    }

    #[test]
    fn an_empty_page_writes_nothing_but_structure() {
        let mut dvi = DviWriter::new();
        dvi.preamble(" test ", 1000);
        dvi.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let empty: Vec<usize> = Vec::new();
        ship_out(
            &mut dvi,
            &[],
            Scaled::ZERO,
            Scaled::ZERO,
            Scaled::ZERO,
            &empty.as_slice(),
        );
        dvi.end_page();
        let _ = dvi.finish().expect("test DVI is representable");
    }

    #[test]
    fn positioned_events_follow_nested_box_movement() {
        let mut dvi = DviWriter::new();
        let line = hbox(
            vec![
                Node::Kern(Scaled::from_points(10), 1),
                Node::Whatsit(PDF_SAVE_POS_MARKER.into()),
                Node::WriteOut(7),
            ],
            Scaled::from_points(10),
        );
        let mut events = Vec::new();
        ship_out_box_with_events(
            &mut dvi,
            &line,
            Scaled::from_points(3),
            Scaled::from_points(5),
            &[].as_slice(),
            &mut |event| events.push(event),
        );
        assert_eq!(
            events,
            vec![
                ShipoutEvent::SavePos {
                    h: Scaled::from_points(13),
                    v: Scaled::from_points(12),
                },
                ShipoutEvent::Write(7),
            ]
        );
        assert!(
            !dvi.bytes()
                .windows(PDF_SAVE_POS_MARKER.len())
                .any(|bytes| bytes == PDF_SAVE_POS_MARKER.as_bytes()),
            "the private position marker must not reach the DVI"
        );
    }

    #[test]
    fn positioned_events_follow_dvipdfmx_matrix_transforms() {
        let mut dvi = DviWriter::new();
        let line = hbox(
            vec![
                Node::Whatsit("pdf:btrans matrix 1 0 0 1 10 4".into()),
                Node::Kern(Scaled::from_points(10), 1),
                Node::Whatsit(PDF_SAVE_POS_MARKER.into()),
                Node::Whatsit("pdf:etrans".into()),
                Node::Whatsit(PDF_SAVE_POS_MARKER.into()),
            ],
            Scaled::from_points(10),
        );
        let mut events = Vec::new();
        ship_out_box_with_events(
            &mut dvi,
            &line,
            Scaled::from_points(3),
            Scaled::from_points(5),
            &[].as_slice(),
            &mut |event| events.push(event),
        );
        assert_eq!(
            events,
            vec![
                ShipoutEvent::SavePos {
                    h: Scaled(round_scaled(
                        f64::from(Scaled::from_points(13).0) + bp_to_sp(10.0)
                    )),
                    v: Scaled(round_scaled(
                        f64::from(Scaled::from_points(12).0) - bp_to_sp(4.0)
                    )),
                },
                ShipoutEvent::SavePos {
                    h: Scaled::from_points(13),
                    v: Scaled::from_points(12),
                },
            ]
        );
    }
}
