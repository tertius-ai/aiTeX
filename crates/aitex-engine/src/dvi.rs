//! DVI output.
//!
//! DVI is the format TeX82 actually emits: positioned glyphs, rules, font
//! selections and `\special`s, and nothing else. It is deliberately the first
//! backend we implement (docs/history/PLAN.md decision 3) because it is *the differential
//! test oracle* — comparing our DVI against `pdftex -output-format=dvi` isolates
//! typesetting logic from font embedding, compression and PDF object numbering.
//!
//! The format is a byte stream of opcodes, documented normatively by
//! `dvitype.web`. Positions are in scaled points throughout, so nothing here is
//! approximate.

use crate::scaled::Scaled;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

// Opcodes we emit. Numbering is from `dvitype.web` and is not negotiable.
const SET_CHAR_0: u8 = 0;
const SET1: u8 = 128;
const SET_RULE: u8 = 132;
const PUT_RULE: u8 = 137;
const BOP: u8 = 139;
const EOP: u8 = 140;
const PUSH: u8 = 141;
const POP: u8 = 142;
// Movement opcodes come in four widths; using the narrowest that fits is most
// of the size difference between a naive writer and TeX's.
const W0: u8 = 147;
const W1: u8 = 148;
const X0: u8 = 152;
const X1: u8 = 153;
const Y0: u8 = 161;
const Y1: u8 = 162;
const Z0: u8 = 166;
const Z1: u8 = 167;
const FNT_NUM_0: u8 = 171;
const FNT1: u8 = 235;
const FNT2: u8 = 236;
const FNT3: u8 = 237;
const FNT4: u8 = 238;
const XXX1: u8 = 239;
const FNT_DEF1: u8 = 243;
const FNT_DEF2: u8 = 244;
const FNT_DEF3: u8 = 245;
const FNT_DEF4: u8 = 246;
const PRE: u8 = 247;
const POST: u8 = 248;
const POST_POST: u8 = 249;
const DVI_ID: u8 = 2;
/// Trailing padding byte; the postamble is padded with these to a 4-byte
/// boundary, minimum four.
const PAD: u8 = 223;

/// A font as the DVI stream refers to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FontDef {
    /// TFM checksum, so a reader can detect a mismatched font.
    pub checksum: u32,
    /// Size the font is used at.
    pub at_size: Scaled,
    /// Size the font was designed for.
    pub design_size: Scaled,
    /// Font name, e.g. `cmr10`.
    pub name: String,
}

/// A representational limit of the DVI format was exceeded.
///
/// DVI stores both its page count and the backward page chain in fixed-width
/// fields.  Returning an error is important here: truncating either value can
/// leave a file which looks valid to a casual reader but seeks to the wrong
/// page or reports a wrapped page count.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DviError {
    /// The postamble page count is an unsigned 16-bit integer.
    TooManyPages { pages: u32 },
    /// `bop`, `post`, and `post_post` offsets are signed 32-bit integers.
    FileOffsetTooLarge { offset: usize },
    /// Font identifiers are unsigned 32-bit integers in DVI.
    FontNumberTooLarge { number: usize },
}

impl fmt::Display for DviError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyPages { pages } => {
                write!(f, "DVI page limit exceeded: {pages} pages (maximum 65535)")
            }
            Self::FileOffsetTooLarge { offset } => write!(
                f,
                "DVI file-offset limit exceeded at byte {offset} (maximum 2147483647)"
            ),
            Self::FontNumberTooLarge { number } => write!(
                f,
                "DVI font-number limit exceeded: {number} (maximum 4294967295)"
            ),
        }
    }
}

/// Writes a DVI byte stream.
///
/// Tracks the running position so the postamble can report the maxima a
/// previewer needs to allocate.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DviWriter {
    out: Vec<u8>,
    fonts: Vec<FontDef>,
    /// Byte offset of the most recent `bop`, for the backward chain.
    last_bop: i32,
    pages: u32,
    max_v: Scaled,
    max_h: Scaled,
    /// TeX's validated, frozen `\mag`, repeated in `pre` and `post`.
    magnification: u32,
    stack_depth: u16,
    max_stack: u16,
    cur_font: Option<usize>,
    in_page: bool,
    /// Movement not yet emitted. Consecutive `down`s collapse into one, which
    /// matters because a line contributes height, depth and glue separately.
    pending_h: Scaled,
    pending_v: Scaled,
    /// `(w, x, y, z, horizontal-MRU, vertical-MRU)` saved by each `push` and
    /// restored by its `pop`.
    #[allow(clippy::type_complexity)]
    reg_stack: Vec<(
        Option<Scaled>,
        Option<Scaled>,
        Option<Scaled>,
        Option<Scaled>,
        bool,
        bool,
    )>,
    /// DVI's two horizontal and two vertical movement registers. A repeated
    /// distance costs one byte; retaining the two most recently used values
    /// also covers the alternating indents and baseline skips common in real
    /// LaTeX output.
    reg_w: Option<Scaled>,
    reg_x: Option<Scaled>,
    reg_y: Option<Scaled>,
    reg_z: Option<Scaled>,
    reg_h_second_mru: bool,
    reg_v_second_mru: bool,
    /// First format-limit error.  Page bytes may continue to be accumulated so
    /// shipout need not branch on every operation, but `finish` will never
    /// return those bytes as an apparently valid artifact.
    error: Option<DviError>,
}

impl DviWriter {
    pub fn new() -> Self {
        Self {
            last_bop: -1,
            ..Default::default()
        }
    }

    /// Bytes already committed to the resumable DVI prefix.
    pub fn output_len(&self) -> usize {
        self.out.len()
    }

    /// Exact bytes committed before a live-document checkpoint. The writer's
    /// registers, font table, backpointer, and maxima still travel in the
    /// cloned writer; these bytes let a host prove that an edited execution
    /// has regenerated the identical externally visible prefix.
    pub fn committed_bytes(&self) -> &[u8] {
        &self.out
    }

    fn u8(&mut self, b: u8) {
        self.out.push(b);
    }
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_be_bytes());
    }

    /// Emit an unsigned font number using the narrowest of DVI's four operand
    /// widths. `base` is either `fnt1` or `fnt_def1`.
    fn font_number(&mut self, base: u8, num: usize) {
        let value = match u32::try_from(num) {
            Ok(value) => value,
            Err(_) => {
                self.error
                    .get_or_insert(DviError::FontNumberTooLarge { number: num });
                u32::MAX
            }
        };
        let width = if value <= u8::MAX as u32 {
            1usize
        } else if value <= u16::MAX as u32 {
            2
        } else if value <= 0x00ff_ffff {
            3
        } else {
            4
        };
        let opcode = match (base, width) {
            (FNT1, 1) => FNT1,
            (FNT1, 2) => FNT2,
            (FNT1, 3) => FNT3,
            (FNT1, 4) => FNT4,
            (FNT_DEF1, 1) => FNT_DEF1,
            (FNT_DEF1, 2) => FNT_DEF2,
            (FNT_DEF1, 3) => FNT_DEF3,
            (FNT_DEF1, 4) => FNT_DEF4,
            _ => unreachable!("font-number base and width are internally bounded"),
        };
        self.u8(opcode);
        let bytes = value.to_be_bytes();
        self.out.extend_from_slice(&bytes[4 - width..]);
    }
    fn i32(&mut self, v: i32) {
        self.out.extend_from_slice(&v.to_be_bytes());
    }

    /// Emit a signed value in the narrowest of 1..4 bytes that holds it.
    fn signed(&mut self, base: u8, v: i32) {
        let (n, op) = if (-0x80..0x80).contains(&v) {
            (1, base)
        } else if (-0x8000..0x8000).contains(&v) {
            (2, base + 1)
        } else if (-0x80_0000..0x80_0000).contains(&v) {
            (3, base + 2)
        } else {
            (4, base + 3)
        };
        self.u8(op);
        let b = v.to_be_bytes();
        self.out.extend_from_slice(&b[4 - n..]);
    }

    /// Select one of a movement direction's two DVI registers. The return is
    /// `(repeat, first)`: a repeat uses the one-byte zero-operand opcode, while
    /// a replacement uses the width-minimized setting opcode. This is a tiny
    /// two-entry LRU, enough to retain alternating line and box movements
    /// without TeX's linked-list backpatching machinery.
    fn movement_register(
        first: &mut Option<Scaled>,
        second: &mut Option<Scaled>,
        second_mru: &mut bool,
        value: Scaled,
    ) -> (bool, bool) {
        if *first == Some(value) {
            *second_mru = false;
            return (true, true);
        }
        if *second == Some(value) {
            *second_mru = true;
            return (true, false);
        }
        if first.is_none() || *second_mru {
            *first = Some(value);
            *second_mru = false;
            (false, true)
        } else {
            *second = Some(value);
            *second_mru = true;
            (false, false)
        }
    }

    /// Flush any accumulated movement.
    ///
    /// Three savings, in the order TeX applies them: collapse consecutive
    /// movements, reuse a register when the distance repeats, and otherwise
    /// pick the narrowest opcode.
    fn flush_movement(&mut self) {
        let v = core::mem::replace(&mut self.pending_v, Scaled::ZERO);
        if v.0 != 0 {
            match Self::movement_register(
                &mut self.reg_y,
                &mut self.reg_z,
                &mut self.reg_v_second_mru,
                v,
            ) {
                (true, true) => self.u8(Y0),
                (true, false) => self.u8(Z0),
                (false, true) => self.signed(Y1, v.0),
                (false, false) => self.signed(Z1, v.0),
            }
        }
        let h = core::mem::replace(&mut self.pending_h, Scaled::ZERO);
        if h.0 != 0 {
            match Self::movement_register(
                &mut self.reg_w,
                &mut self.reg_x,
                &mut self.reg_h_second_mru,
                h,
            ) {
                (true, true) => self.u8(W0),
                (true, false) => self.u8(X0),
                (false, true) => self.signed(W1, h.0),
                (false, false) => self.signed(X1, h.0),
            }
        }
    }

    /// `pre`. The `num`/`den` pair defines the unit: 25400000/473628672 is
    /// TeX's standard, making one DVI unit 1 sp exactly.
    pub fn preamble(&mut self, comment: &str, magnification: u32) {
        assert_ne!(magnification, 0, "DVI magnification must be positive");
        self.magnification = magnification;
        self.u8(PRE);
        self.u8(DVI_ID);
        self.u32(25_400_000);
        self.u32(473_628_672);
        self.u32(magnification);
        let c = comment.as_bytes();
        let n = c.len().min(255);
        self.u8(n as u8);
        self.out.extend_from_slice(&c[..n]);
    }

    /// Declare a font and return its number.
    pub fn define_font(&mut self, def: FontDef) -> usize {
        let num = self.fonts.len();
        self.emit_font_def(num, &def);
        self.fonts.push(def);
        num
    }

    fn emit_font_def(&mut self, num: usize, def: &FontDef) {
        self.font_number(FNT_DEF1, num);
        self.u32(def.checksum);
        self.i32(def.at_size.0);
        self.i32(def.design_size.0);
        // Area is empty; the name follows.
        self.u8(0);
        let name = def.name.as_bytes();
        self.u8(name.len() as u8);
        self.out.extend_from_slice(name);
    }

    /// `bop`. The ten counters are `\count0`..`\count9`; `\count0` is the page
    /// number a previewer displays.
    pub fn begin_page(&mut self, counts: [i32; 10]) {
        if self.pages == u16::MAX as u32 {
            self.error.get_or_insert(DviError::TooManyPages {
                pages: self.pages + 1,
            });
        }
        let here = match i32::try_from(self.out.len()) {
            Ok(offset) => offset,
            Err(_) => {
                self.error.get_or_insert(DviError::FileOffsetTooLarge {
                    offset: self.out.len(),
                });
                // The value is never exposed because `finish` returns the
                // recorded error.  Keeping a sentinel lets shipout unwind
                // normally instead of panicking midway through a document.
                i32::MAX
            }
        };
        self.u8(BOP);
        for c in counts {
            self.i32(c);
        }
        self.i32(self.last_bop);
        self.last_bop = here;
        self.pages += 1;
        self.in_page = true;
        // Font selection does not survive a page boundary.
        self.cur_font = None;
        self.stack_depth = 0;
        // Movement registers, like font selection, do not cross bop.
        self.pending_h = Scaled::ZERO;
        self.pending_v = Scaled::ZERO;
        self.reg_w = None;
        self.reg_x = None;
        self.reg_y = None;
        self.reg_z = None;
        self.reg_h_second_mru = false;
        self.reg_v_second_mru = false;
    }

    pub fn end_page(&mut self) {
        self.pending_h = Scaled::ZERO;
        self.pending_v = Scaled::ZERO;
        self.u8(EOP);
        self.in_page = false;
    }

    pub fn push(&mut self) {
        self.flush_movement();
        self.u8(PUSH);
        // `push` saves h, v, w, x, y and z; `pop` restores them. The `w`/`y`
        // registers we track for the one-byte repeat forms are those same
        // registers, so they have to be saved and restored alongside — emitting
        // `y0` after a `pop` moves by whatever `y` held *outside* the group,
        // which is a different distance entirely. This is invisible until a box
        // is drawn inside a vertical list and something follows it.
        self.reg_stack.push((
            self.reg_w,
            self.reg_x,
            self.reg_y,
            self.reg_z,
            self.reg_h_second_mru,
            self.reg_v_second_mru,
        ));
        self.stack_depth += 1;
        self.max_stack = self.max_stack.max(self.stack_depth);
    }

    pub fn pop(&mut self) {
        // Movement inside a group that never reaches an opcode is discarded by
        // the pop anyway, so dropping it here is both correct and smaller.
        self.pending_h = Scaled::ZERO;
        self.pending_v = Scaled::ZERO;
        let (w, x, y, z, h_second_mru, v_second_mru) = self
            .reg_stack
            .pop()
            .unwrap_or((None, None, None, None, false, false));
        self.reg_w = w;
        self.reg_x = x;
        self.reg_y = y;
        self.reg_z = z;
        self.reg_h_second_mru = h_second_mru;
        self.reg_v_second_mru = v_second_mru;
        self.u8(POP);
        self.stack_depth = self.stack_depth.saturating_sub(1);
    }

    /// Select a font, skipping the opcode if it is already current.
    pub fn select_font(&mut self, num: usize) {
        if self.cur_font == Some(num) {
            return;
        }
        if num < 64 {
            self.u8(FNT_NUM_0 + num as u8);
        } else {
            self.font_number(FNT1, num);
        }
        self.cur_font = Some(num);
    }

    /// Typeset a character and advance the reference point.
    ///
    /// Codes below 128 have a single-byte opcode, which is why ordinary text
    /// costs one byte per character.
    pub fn set_char(&mut self, code: u32) {
        // Most calls are the second and later glyphs of a text run.  Calling
        // the full movement routine for each one performed two replacements
        // and register checks merely to rediscover two zero accumulators.
        if self.pending_h.0 != 0 || self.pending_v.0 != 0 {
            self.flush_movement();
        }
        if code < 128 {
            self.u8(SET_CHAR_0 + code as u8);
        } else {
            self.u8(SET1);
            self.u8(code as u8);
        }
    }

    /// `put_rule` — draw a rule *without* moving the reference point.
    ///
    /// `tex.web` §633: a rule in a **vertical** list is `put`, one in a
    /// horizontal list is `set`. Using `set` in a vertical list advances `h` by
    /// the rule's width, so everything drawn afterwards on that page starts a
    /// full measure to the right of where TeX puts it.
    pub fn put_rule(&mut self, height: Scaled, width: Scaled) {
        self.flush_movement();
        self.u8(PUT_RULE);
        self.i32(height.0);
        self.i32(width.0);
    }

    pub fn set_rule(&mut self, height: Scaled, width: Scaled) {
        self.flush_movement();
        self.u8(SET_RULE);
        self.i32(height.0);
        self.i32(width.0);
    }

    /// Move right. Accumulated and emitted lazily so consecutive moves merge.
    pub fn right(&mut self, by: Scaled) {
        self.pending_h = self.pending_h + by;
    }

    /// Move down. Accumulated and emitted lazily.
    pub fn down(&mut self, by: Scaled) {
        self.pending_v = self.pending_v + by;
    }

    /// `\special` — arbitrary text passed through for the driver to interpret.
    /// This is how `hyperref` and `graphicx` work without engine support.
    pub fn special(&mut self, text: &str) {
        self.flush_movement();
        // TeX's string pool stores character codes as bytes. Rust stores the
        // same U+0080..U+00FF values as two-byte UTF-8 sequences, which must
        // not leak into a DVI xxx command: `^^80` is byte 80, not c2 80.
        // Preserve Unicode support above TeX's byte range as UTF-8 fallback.
        let mut bytes = Vec::with_capacity(text.len());
        for c in text.chars() {
            if (c as u32) <= u8::MAX as u32 {
                bytes.push(c as u8);
            } else {
                let mut encoded = [0; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
            }
        }
        // DVI has four `xxx` opcodes, whose one- through four-byte operands
        // carry the special's length.  Hyperref's pdfmark prologue and document
        // information routinely exceed 255 bytes; truncating them to `xxx1`
        // turns valid package output into incomplete PostScript before the PDF
        // driver even sees it.
        let n = bytes.len().min(u32::MAX as usize);
        let len = n as u32;
        let width = if len <= u8::MAX as u32 {
            1
        } else if len <= u16::MAX as u32 {
            2
        } else if len <= 0x00ff_ffff {
            3
        } else {
            4
        };
        self.u8(XXX1 + width - 1);
        let encoded = len.to_be_bytes();
        self.out.extend_from_slice(&encoded[4 - width as usize..]);
        self.out.extend_from_slice(&bytes[..n]);
    }

    /// Record the extent of a page, for the postamble's maxima.
    pub fn note_extent(&mut self, height: Scaled, width: Scaled) {
        self.max_v = self.max_v.max(height);
        self.max_h = self.max_h.max(width);
    }

    /// `post`, the font definitions again, then `post_post`.
    ///
    /// Fonts are repeated after `post` so a previewer reading the file backwards
    /// can load them without scanning the whole body.
    pub fn finish(mut self) -> Result<Vec<u8>, DviError> {
        assert_ne!(
            self.magnification, 0,
            "DVI preamble must be emitted before finish"
        );
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        let pages =
            u16::try_from(self.pages).map_err(|_| DviError::TooManyPages { pages: self.pages })?;
        let post_at = i32::try_from(self.out.len()).map_err(|_| DviError::FileOffsetTooLarge {
            offset: self.out.len(),
        })?;
        self.u8(POST);
        self.i32(self.last_bop);
        self.u32(25_400_000);
        self.u32(473_628_672);
        self.u32(self.magnification);
        self.i32(self.max_v.0);
        self.i32(self.max_h.0);
        self.u16(self.max_stack.max(1));
        self.u16(pages);

        let fonts = core::mem::take(&mut self.fonts);
        for (i, f) in fonts.iter().enumerate() {
            self.emit_font_def(i, f);
        }

        self.u8(POST_POST);
        self.i32(post_at);
        self.u8(DVI_ID);
        // Pad with 223s to a four-byte boundary, at least four of them.
        let mut pads = 4;
        while !(self.out.len() + pads).is_multiple_of(4) {
            pads += 1;
        }
        for _ in 0..pads {
            self.u8(PAD);
        }
        Ok(self.out)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Vec<u8> {
        let mut w = DviWriter::new();
        w.preamble("aitex test", 1000);
        let f = w.define_font(FontDef {
            checksum: 0x4BF16079,
            at_size: Scaled::from_points(10),
            design_size: Scaled::from_points(10),
            name: "cmr10".into(),
        });
        w.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        w.down(Scaled::from_points(100));
        w.right(Scaled::from_points(100));
        w.select_font(f);
        for c in b"Hi" {
            w.set_char(*c as u32);
        }
        w.end_page();
        w.note_extent(Scaled::from_points(792), Scaled::from_points(612));
        w.finish().expect("minimal DVI is representable")
    }

    #[test]
    fn starts_with_pre_and_the_standard_unit() {
        let d = minimal();
        assert_eq!(d[0], PRE);
        assert_eq!(d[1], DVI_ID);
        // 25400000/473628672 makes one DVI unit exactly one scaled point.
        assert_eq!(u32::from_be_bytes([d[2], d[3], d[4], d[5]]), 25_400_000);
        assert_eq!(u32::from_be_bytes([d[6], d[7], d[8], d[9]]), 473_628_672);
    }

    #[test]
    fn magnification_is_repeated_in_pre_and_post() {
        let mut w = DviWriter::new();
        w.preamble("magnified", 2000);
        w.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        w.end_page();
        let d = w.finish().expect("small DVI is representable");

        assert_eq!(u32::from_be_bytes([d[10], d[11], d[12], d[13]]), 2000);
        let pads = d.iter().rev().take_while(|&&b| b == PAD).count();
        let post_post = d.len() - pads - 6;
        let post = i32::from_be_bytes([
            d[post_post + 1],
            d[post_post + 2],
            d[post_post + 3],
            d[post_post + 4],
        ]) as usize;
        assert_eq!(d[post], POST);
        assert_eq!(
            u32::from_be_bytes([d[post + 13], d[post + 14], d[post + 15], d[post + 16]]),
            2000
        );
    }

    #[test]
    fn ends_with_post_post_and_at_least_four_pads() {
        let d = minimal();
        assert_eq!(*d.last().expect("non-empty"), PAD);
        let pads = d.iter().rev().take_while(|&&b| b == PAD).count();
        assert!(pads >= 4, "need at least four 223 bytes, got {pads}");
        assert_eq!(d.len() % 4, 0, "total length must be a multiple of four");
        assert_eq!(d[d.len() - pads - 1], DVI_ID);
    }

    #[test]
    fn post_post_points_back_at_post() {
        let d = minimal();
        let pads = d.iter().rev().take_while(|&&b| b == PAD).count();
        let pp = d.len() - pads - 6; // post_post, i32 pointer, id byte
        assert_eq!(d[pp], POST_POST);
        let ptr = i32::from_be_bytes([d[pp + 1], d[pp + 2], d[pp + 3], d[pp + 4]]) as usize;
        assert_eq!(d[ptr], POST, "the pointer must land on post");
    }

    #[test]
    fn first_page_has_no_predecessor() {
        let d = minimal();
        let bop = d.iter().position(|&b| b == BOP).expect("has a page");
        // ten counters, then the back pointer
        let p = bop + 1 + 40;
        assert_eq!(i32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]), -1);
    }

    #[test]
    fn ascii_characters_cost_one_byte() {
        let mut w = DviWriter::new();
        w.set_char(b'A' as u32);
        assert_eq!(w.bytes(), b"A");
    }

    #[test]
    fn special_preserves_tex_character_codes_as_bytes() {
        let mut w = DviWriter::new();
        w.special("\u{80}x");
        assert_eq!(w.bytes(), &[XXX1, 2, 0x80, b'x']);
    }

    #[test]
    fn long_special_uses_a_wide_length_without_truncation() {
        let mut w = DviWriter::new();
        let text = "x".repeat(300);
        w.special(&text);
        assert_eq!(&w.bytes()[..3], &[XXX1 + 1, 1, 44]);
        assert_eq!(w.bytes().len(), 303);
        assert_eq!(&w.bytes()[3..], text.as_bytes());
    }

    #[test]
    fn redundant_font_selection_is_skipped() {
        let mut w = DviWriter::new();
        w.select_font(0);
        let after_first = w.bytes().len();
        w.select_font(0);
        assert_eq!(
            w.bytes().len(),
            after_first,
            "must not re-emit the same font"
        );
        w.select_font(1);
        assert!(w.bytes().len() > after_first);
    }

    #[test]
    fn wide_font_numbers_use_wide_definition_and_selection_opcodes() {
        let mut selection = DviWriter::new();
        selection.select_font(256);
        selection.select_font(65_536);
        selection.select_font(16_777_216);
        assert_eq!(
            selection.bytes(),
            &[FNT2, 1, 0, FNT3, 1, 0, 0, FNT4, 1, 0, 0, 0,]
        );

        let mut definitions = DviWriter::new();
        let font = FontDef {
            checksum: 0,
            at_size: Scaled::ZERO,
            design_size: Scaled::ZERO,
            name: "f".into(),
        };
        for _ in 0..256 {
            definitions.define_font(font.clone());
        }
        let before = definitions.bytes().len();
        assert_eq!(definitions.define_font(font), 256);
        assert_eq!(&definitions.bytes()[before..before + 3], &[FNT_DEF2, 1, 0]);

        // Keep every width constant live and pinned to the normative sequence.
        assert_eq!([FNT1, FNT2, FNT3, FNT4], [235, 236, 237, 238]);
        assert_eq!(
            [FNT_DEF1, FNT_DEF2, FNT_DEF3, FNT_DEF4],
            [243, 244, 245, 246]
        );
    }

    #[test]
    fn font_selection_is_reset_at_a_page_boundary() {
        // DVI font state does not survive bop, so a naive "already selected"
        // cache would silently emit unfonted characters on later pages.
        let mut w = DviWriter::new();
        w.begin_page([0; 10]);
        w.select_font(0);
        w.end_page();
        w.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let before = w.bytes().len();
        w.select_font(0);
        assert!(
            w.bytes().len() > before,
            "font must be re-selected on a new page"
        );
    }

    #[test]
    fn zero_movements_are_omitted() {
        let mut w = DviWriter::new();
        w.right(Scaled::ZERO);
        w.down(Scaled::ZERO);
        w.set_char(b'x' as u32);
        assert_eq!(w.bytes(), b"x", "no movement opcode for a zero move");
    }

    /// Consecutive movements collapse into one. A typeset line contributes its
    /// height, its depth and the following glue as three separate calls, and
    /// emitting three opcodes for one baseline step is most of the difference
    /// between a naive writer and tex's output.
    #[test]
    fn consecutive_movements_merge() {
        let mut w = DviWriter::new();
        w.down(Scaled::from_points(7));
        w.down(Scaled::from_points(2));
        w.down(Scaled::from_points(3));
        w.set_char(b'x' as u32);
        let b = w.bytes();
        // 12pt is 786432 sp, which fits the signed three-byte form: opcode
        // Y1+2, three operand bytes, then the character.
        assert_eq!(b[0], Y1 + 2, "12pt fits three bytes");
        assert_eq!(
            i32::from_be_bytes([0, b[1], b[2], b[3]]),
            12 * 65536,
            "operand is the merged 7+2+3 pt"
        );
        assert_eq!(b[4], b'x');
        assert_eq!(b.len(), 5, "three separate downs became one opcode");
    }

    #[test]
    fn a_repeated_distance_costs_one_byte() {
        let mut w = DviWriter::new();
        for _ in 0..3 {
            w.down(Scaled::from_points(12));
            w.set_char(b'x' as u32);
        }
        let b = w.bytes();
        // First step costs 4 (y3 + 3 operand) + 1 char; each repeat costs
        // y0 + char = 2. Naively it would be 3 x 6 = 18.
        assert_eq!(b.len(), 5 + 2 + 2, "repeats must use the y register");
        assert_eq!(b[5], Y0);
        assert_eq!(b[7], Y0);
    }

    #[test]
    fn alternating_distances_use_both_movement_registers() {
        let mut w = DviWriter::new();
        for distance in [10, 12, 10, 12] {
            w.down(Scaled::from_points(distance));
            w.set_char(b'x' as u32);
        }
        let b = w.bytes();
        assert_eq!(b[0], Y1 + 2, "first distance initializes y");
        assert_eq!(b[5], Z1 + 2, "second distance initializes z");
        assert_eq!(b[10], Y0, "first distance reuses y");
        assert_eq!(b[12], Z0, "second distance reuses z");
        assert_eq!(
            b.len(),
            14,
            "later alternating movements cost one byte each"
        );
    }

    #[test]
    fn pop_restores_both_movement_registers() {
        let mut w = DviWriter::new();
        for distance in [10, 12] {
            w.right(Scaled::from_points(distance));
            w.set_char(b'x' as u32);
        }
        w.push();
        w.right(Scaled::from_points(14));
        w.set_char(b'x' as u32);
        w.pop();
        let before = w.bytes().len();
        w.right(Scaled::from_points(10));
        w.set_char(b'x' as u32);
        assert_eq!(&w.bytes()[before..], &[W0, b'x']);
    }

    #[test]
    fn narrow_movements_use_narrow_opcodes() {
        let mut w = DviWriter::new();
        w.down(Scaled(100)); // fits in one signed byte
        w.set_char(b'x' as u32);
        assert_eq!(w.bytes()[0], Y1, "one-byte form");
        assert_eq!(w.bytes().len(), 3);
    }

    #[test]
    fn movement_registers_do_not_survive_a_page_boundary() {
        // DVI resets w/x/y/z at bop; reusing a stale register would misplace
        // everything on the next page.
        let mut w = DviWriter::new();
        w.preamble("x", 1000);
        w.begin_page([0; 10]);
        w.down(Scaled::from_points(12));
        w.set_char(b'a' as u32);
        w.end_page();
        w.begin_page([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let before = w.bytes().len();
        w.down(Scaled::from_points(12));
        w.set_char(b'b' as u32);
        // Must re-set the register, not emit a bare y0.
        assert!(
            w.bytes().len() - before > 2,
            "register must be re-established"
        );
    }

    #[test]
    fn push_and_pop_track_max_stack() {
        let mut w = DviWriter::new();
        w.preamble("x", 1000);
        w.begin_page([0; 10]);
        w.push();
        w.push();
        w.pop();
        w.push();
        w.end_page();
        let d = w.finish().expect("small DVI is representable");
        // max_stack lives just before the two-byte page count in `post`.
        let post = d.iter().rposition(|&b| b == POST).expect("has post");
        let ms = u16::from_be_bytes([d[post + 25], d[post + 26]]);
        assert_eq!(ms, 2);
    }

    #[test]
    fn page_count_is_recorded() {
        let mut w = DviWriter::new();
        w.preamble("x", 1000);
        for i in 0..3 {
            w.begin_page([i, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            w.end_page();
        }
        let d = w.finish().expect("small DVI is representable");
        let post = d.iter().rposition(|&b| b == POST).expect("has post");
        assert_eq!(u16::from_be_bytes([d[post + 27], d[post + 28]]), 3);
    }

    #[test]
    fn page_count_limit_is_checked_instead_of_wrapping() {
        let mut w = DviWriter::new();
        w.preamble("x", 1000);
        w.pages = u16::MAX as u32 + 1;
        assert_eq!(w.finish(), Err(DviError::TooManyPages { pages: 65_536 }));
    }

    #[test]
    fn file_offset_limit_is_reported_instead_of_truncated() {
        let mut w = DviWriter::new();
        w.preamble("x", 1000);
        w.error = Some(DviError::FileOffsetTooLarge {
            offset: i32::MAX as usize + 1,
        });
        assert_eq!(
            w.finish(),
            Err(DviError::FileOffsetTooLarge {
                offset: 2_147_483_648
            })
        );
    }
}
