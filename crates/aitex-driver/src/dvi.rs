//! Reading a DVI file, as `dvitype` reads one.
//!
//! The opcodes are `tex.web` §583–§586 and the DVI format description in
//! `dvitype.web`. This is the inverse of `aitex-engine`'s `dvi.rs`, and it is a
//! separate implementation on purpose: a reader that shared the writer's
//! assumptions would agree with it about a file neither had got right.

// Parsed DVI and native engine shipout deliberately meet at this contract.
// Keeping one item/page vocabulary means the PDF driver can consume engine
// events directly while this independent parser remains a semantic oracle.
pub use aitex_page::{Item, Page};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontDef {
    pub name: String,
    /// `at` size in scaled points.
    pub at: i64,
    /// The font's design size, scaled points.
    pub design: i64,
    pub checksum: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub pages: Vec<Page>,
    pub fonts: std::collections::BTreeMap<u32, FontDef>,
    /// `num`/`den`/`mag` from the preamble. TeX writes 25400000/473628672,
    /// which makes one DVI unit exactly one scaled point.
    pub num: u32,
    pub den: u32,
    pub mag: u32,
}

#[derive(Debug)]
pub enum Error {
    Truncated,
    NotDvi,
    BadOpcode(u8),
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, Error> {
        let v = *self.b.get(self.p).ok_or(Error::Truncated)?;
        self.p += 1;
        Ok(v)
    }
    /// `n` bytes, big-endian, unsigned.
    fn un(&mut self, n: usize) -> Result<u32, Error> {
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 8) | u32::from(self.u8()?);
        }
        Ok(v)
    }
    /// `n` bytes, big-endian, *signed* — the first byte carries the sign.
    fn sn(&mut self, n: usize) -> Result<i64, Error> {
        let first = self.u8()?;
        let mut v = if first >= 0x80 {
            i64::from(first) - 256
        } else {
            i64::from(first)
        };
        for _ in 1..n {
            v = (v << 8) | i64::from(self.u8()?);
        }
        Ok(v)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let e = self.p.checked_add(n).ok_or(Error::Truncated)?;
        let s = self.b.get(self.p..e).ok_or(Error::Truncated)?;
        self.p = e;
        Ok(s)
    }
}

/// The position state DVI keeps, and that `push`/`pop` save and restore
/// (§586). `w x y z` are the "same as last time" registers that make a DVI
/// file compact — and the reason a byte comparison against TeX's own output is
/// not a fair test of an engine.
#[derive(Clone, Copy, Default)]
struct State {
    h: i64,
    v: i64,
    w: i64,
    x: i64,
    y: i64,
    z: i64,
}

const SET1: u8 = 128;
const SET_RULE: u8 = 132;
const PUT1: u8 = 133;
const PUT_RULE: u8 = 137;
const NOP: u8 = 138;
const BOP: u8 = 139;
const EOP: u8 = 140;
const PUSH: u8 = 141;
const POP: u8 = 142;
const RIGHT1: u8 = 143;
const W0: u8 = 147;
const X0: u8 = 152;
const DOWN1: u8 = 157;
const Y0: u8 = 161;
const Z0: u8 = 166;
const FNT_NUM_0: u8 = 171;
const FNT1: u8 = 235;
const XXX1: u8 = 239;
const FNT_DEF1: u8 = 243;
const PRE: u8 = 247;
const POST: u8 = 248;

/// Parse without font metrics. Character items are still present, but a caller
/// that needs the absolute position after a `setchar` must use
/// [`parse_with_widths`], because DVI stores that advance implicitly in the
/// selected font's TFM rather than as a movement opcode.
pub fn parse(bytes: &[u8]) -> Result<Document, Error> {
    parse_with_widths(bytes, |_, _| 0)
}

/// Parse while resolving each implicit `setchar` advance from the selected
/// font. `putchar` never advances. A PDF renderer needs this form: without it,
/// every glyph in an ordinary DVI word has the same horizontal coordinate.
pub fn parse_with_widths(
    bytes: &[u8],
    mut width: impl FnMut(&FontDef, u32) -> i64,
) -> Result<Document, Error> {
    let mut r = Reader { b: bytes, p: 0 };
    let mut doc = Document::default();

    if r.u8()? != PRE {
        return Err(Error::NotDvi);
    }
    let _id = r.u8()?;
    doc.num = r.un(4)?;
    doc.den = r.un(4)?;
    doc.mag = r.un(4)?;
    let k = r.u8()? as usize;
    let _comment = r.bytes(k)?;

    let mut st = State::default();
    let mut stack: Vec<State> = Vec::new();
    let mut font: u32 = 0;
    let mut page: Option<Page> = None;

    loop {
        let op = match r.u8() {
            Ok(o) => o,
            // A DVI file that stops without `post` is truncated, not corrupt in
            // a way worth refusing: everything read so far is still a document.
            Err(_) => break,
        };
        match op {
            0..=127 => {
                // set_char_i: typeset and move right by the character's width.
                // The width lives in the TFM, which this crate does not read —
                // the caller supplies metrics, so the *reader* records only
                // what the file says.
                if let Some(p) = page.as_mut() {
                    let advance = doc
                        .fonts
                        .get(&font)
                        .map_or(0, |def| width(def, u32::from(op)));
                    p.items.push(Item::ClassicGlyph {
                        font: aitex_page::FontInstanceId(font),
                        code: u32::from(op),
                        origin_x: aitex_page::Sp(st.h),
                        origin_y: aitex_page::Sp(st.v),
                        advance: aitex_page::Sp(advance),
                    });
                    st.h += advance;
                }
            }
            SET1..=131 | PUT1..=136 => {
                let advances = op < PUT1;
                let n = if advances {
                    (op - SET1 + 1) as usize
                } else {
                    (op - PUT1 + 1) as usize
                };
                let code = r.un(n)?;
                if let Some(p) = page.as_mut() {
                    let advance = if advances {
                        doc.fonts.get(&font).map_or(0, |def| width(def, code))
                    } else {
                        0
                    };
                    p.items.push(Item::ClassicGlyph {
                        font: aitex_page::FontInstanceId(font),
                        code,
                        origin_x: aitex_page::Sp(st.h),
                        origin_y: aitex_page::Sp(st.v),
                        advance: aitex_page::Sp(advance),
                    });
                    st.h += advance;
                }
            }
            SET_RULE | PUT_RULE => {
                let height = r.sn(4)?;
                let width = r.sn(4)?;
                if let Some(p) = page.as_mut() {
                    p.items.push(Item::Rule {
                        x: aitex_page::Sp(st.h),
                        baseline_y: aitex_page::Sp(st.v),
                        width: aitex_page::Sp(width),
                        height: aitex_page::Sp(height),
                    });
                }
                if op == SET_RULE {
                    st.h += width;
                }
            }
            NOP => {}
            BOP => {
                let mut counts = [0i32; 10];
                for c in counts.iter_mut() {
                    *c = r.sn(4)? as i32;
                }
                let _prev = r.sn(4)?;
                st = State::default();
                stack.clear();
                font = 0;
                page = Some(Page {
                    items: Vec::new(),
                    counts,
                    ..Page::default()
                });
            }
            EOP => {
                if let Some(p) = page.take() {
                    doc.pages.push(p);
                }
            }
            PUSH => stack.push(st),
            POP => {
                if let Some(s) = stack.pop() {
                    st = s;
                }
            }
            RIGHT1..=146 => {
                let n = (op - RIGHT1 + 1) as usize;
                st.h += r.sn(n)?;
            }
            W0 => st.h += st.w,
            148..=151 => {
                let n = (op - 148 + 1) as usize;
                st.w = r.sn(n)?;
                st.h += st.w;
            }
            X0 => st.h += st.x,
            153..=156 => {
                let n = (op - 153 + 1) as usize;
                st.x = r.sn(n)?;
                st.h += st.x;
            }
            DOWN1..=160 => {
                let n = (op - DOWN1 + 1) as usize;
                st.v += r.sn(n)?;
            }
            Y0 => st.v += st.y,
            162..=165 => {
                let n = (op - 162 + 1) as usize;
                st.y = r.sn(n)?;
                st.v += st.y;
            }
            Z0 => st.v += st.z,
            167..=170 => {
                let n = (op - 167 + 1) as usize;
                st.z = r.sn(n)?;
                st.v += st.z;
            }
            FNT_NUM_0..=234 => font = u32::from(op - FNT_NUM_0),
            FNT1..=238 => {
                let n = (op - FNT1 + 1) as usize;
                font = r.un(n)?;
            }
            XXX1..=242 => {
                let n = (op - XXX1 + 1) as usize;
                let len = r.un(n)? as usize;
                let text = r.bytes(len)?.to_vec();
                if let Some(p) = page.as_mut() {
                    p.items.push(Item::Special {
                        x: aitex_page::Sp(st.h),
                        y: aitex_page::Sp(st.v),
                        bytes: text,
                    });
                }
            }
            FNT_DEF1..=246 => {
                let n = (op - FNT_DEF1 + 1) as usize;
                let num = r.un(n)?;
                let checksum = r.un(4)?;
                let at = r.sn(4)?;
                let design = r.sn(4)?;
                let a = r.u8()? as usize;
                let l = r.u8()? as usize;
                let name = r.bytes(a + l)?;
                doc.fonts.insert(
                    num,
                    FontDef {
                        name: String::from_utf8_lossy(name).into_owned(),
                        at,
                        design,
                        checksum,
                    },
                );
            }
            POST => break,
            other => return Err(Error::BadOpcode(other)),
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest legal DVI: a preamble and nothing else. Reading it must not
    /// be an error — a file can be truncated and still say what it said.
    #[test]
    fn a_preamble_alone_parses() {
        let mut b = vec![PRE, 2];
        b.extend_from_slice(&25_400_000u32.to_be_bytes());
        b.extend_from_slice(&473_628_672u32.to_be_bytes());
        b.extend_from_slice(&1000u32.to_be_bytes());
        b.push(0);
        let d = parse(&b).expect("parses");
        assert_eq!(d.num, 25_400_000);
        assert!(d.pages.is_empty());
    }

    /// `push`/`pop` restore *every* register, not just h and v — §586 saves the
    /// whole state, and a reader that kept `w` across a `pop` would place every
    /// character after it a word's width off.
    #[test]
    fn pop_restores_the_movement_registers() {
        let mut b = vec![PRE, 2];
        b.extend_from_slice(&25_400_000u32.to_be_bytes());
        b.extend_from_slice(&473_628_672u32.to_be_bytes());
        b.extend_from_slice(&1000u32.to_be_bytes());
        b.push(0);
        b.push(BOP);
        for _ in 0..10 {
            b.extend_from_slice(&0i32.to_be_bytes());
        }
        b.extend_from_slice(&(-1i32).to_be_bytes());
        b.push(PUSH);
        b.push(148); // w1
        b.push(50);
        b.push(POP);
        b.push(W0); // w is 0 again after the pop, so this must not move
        b.push(b'A');
        b.push(EOP);
        let d = parse(&b).expect("parses");
        assert_eq!(
            d.pages[0].items[0],
            Item::ClassicGlyph {
                font: aitex_page::FontInstanceId(0),
                code: 65,
                origin_x: aitex_page::Sp(0),
                origin_y: aitex_page::Sp(0),
                advance: aitex_page::Sp(0),
            }
        );
    }

    #[test]
    fn font_widths_advance_setchar_but_not_putchar() {
        let mut b = vec![PRE, 2];
        b.extend_from_slice(&25_400_000u32.to_be_bytes());
        b.extend_from_slice(&473_628_672u32.to_be_bytes());
        b.extend_from_slice(&1000u32.to_be_bytes());
        b.push(0);
        b.push(FNT_DEF1);
        b.push(0); // font number
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&1000i32.to_be_bytes());
        b.extend_from_slice(&1000i32.to_be_bytes());
        b.extend_from_slice(&[0, 1, b'f']);
        b.push(BOP);
        for _ in 0..10 {
            b.extend_from_slice(&0i32.to_be_bytes());
        }
        b.extend_from_slice(&(-1i32).to_be_bytes());
        b.push(FNT_NUM_0);
        b.push(b'A');
        b.extend_from_slice(&[PUT1, b'C']);
        b.push(b'B');
        b.push(EOP);
        b.push(POST);

        let doc = parse_with_widths(&b, |_, _| 500).expect("parses");
        let positions: Vec<i64> = doc.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::ClassicGlyph { origin_x, .. } => Some(origin_x.0),
                _ => None,
            })
            .collect();
        assert_eq!(positions, [0, 500, 500]);
    }

    #[test]
    fn wide_font_numbers_round_trip_through_definitions_and_selections() {
        let mut b = vec![PRE, 2];
        b.extend_from_slice(&25_400_000u32.to_be_bytes());
        b.extend_from_slice(&473_628_672u32.to_be_bytes());
        b.extend_from_slice(&1000u32.to_be_bytes());
        b.push(0);
        b.extend_from_slice(&[FNT_DEF1 + 1, 1, 0]); // fnt_def2 256
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&1000i32.to_be_bytes());
        b.extend_from_slice(&1000i32.to_be_bytes());
        b.extend_from_slice(&[0, 1, b'f']);
        b.push(BOP);
        for _ in 0..10 {
            b.extend_from_slice(&0i32.to_be_bytes());
        }
        b.extend_from_slice(&(-1i32).to_be_bytes());
        b.extend_from_slice(&[FNT1 + 1, 1, 0]); // fnt2 256
        b.push(b'A');
        b.push(EOP);
        b.push(POST);

        let doc = parse(&b).expect("wide font opcodes parse");
        assert_eq!(
            doc.fonts.get(&256).map(|font| font.name.as_str()),
            Some("f")
        );
        assert!(matches!(
            doc.pages[0].items[0],
            Item::ClassicGlyph {
                font: aitex_page::FontInstanceId(256),
                code: 65,
                ..
            }
        ));
    }
}
