//! TFM font metrics.
//!
//! A `.tfm` file carries the numbers TeX needs to typeset with a font — widths,
//! heights, depths, italic corrections, kerns and ligatures — and *nothing*
//! about glyph shapes. That separation is why TeX can typeset without ever
//! seeing an outline, and why the driver, not the engine, deals with embedding.
//!
//! Parsing lives in the engine because TeX82 cannot work without it and the
//! format needs no dependencies. OpenType and shaping belong in `aitex-fonts`.

use crate::scaled::Scaled;
use alloc::vec::Vec;

/// Metrics for one font at a particular size.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TfmFont {
    pub checksum: u32,
    /// The size the font was designed for, usually 10pt.
    pub design_size: Scaled,
    /// The size it is being used at. Widths scale by `at_size / design_size`.
    pub at_size: Scaled,
    /// The TFM header identifies Knuth's classic `TEX TEXT` coding scheme.
    /// pdfTeX's non-selected microtype fallback treats those metrics
    /// differently from extended and generated encodings at scaled sizes.
    #[serde(default)]
    tex_text_coding: bool,
    /// First and last character codes present.
    pub bc: u16,
    pub ec: u16,
    widths: Vec<Scaled>,
    heights: Vec<Scaled>,
    depths: Vec<Scaled>,
    /// Italic corrections, indexed by a character's `italic_index`. `\/` is a
    /// kern of this width — without it a roman letter after an italic word sits
    /// against its overhang.
    italics: Vec<Scaled>,
    /// Per character: indices into the tables above, packed as TeX packs them.
    chars: Vec<CharInfo>,
    /// The ligature/kern program, four bytes per instruction.
    lig_kern: Vec<[u8; 4]>,
    /// Some test and specialist fonts use TeX's retained-operand ligature
    /// operators (op bytes 1..=7).  Ordinary text fonts use only op zero,
    /// which can be shaped by a linear cursor without cloning and splicing a
    /// general work list.  Cached at load time so every word does not rescan
    /// the program table to choose its algorithm.
    #[serde(default)]
    complex_ligatures: bool,
    /// `\pdfnoligatures` was applied to this font. See `set_no_ligatures`.
    #[serde(default)]
    no_ligatures: bool,
    /// Reverse of the ordinary `=:` ligature program, indexed by the produced
    /// glyph. Hyphenation asks this for every character while enforcing TeX's
    /// 63-letter word limit; rescanning every left character's program on each
    /// query made that linear guard accidentally quadratic in the font table.
    #[serde(default)]
    unligatures: Vec<Option<(u32, u32)>>,
    /// pdfTeX's `\lpcode` and `\rpcode`: how far a character may hang into the
    /// left and right margin, in *thousandths of the font's quad*.
    ///
    /// The unit is measured, not assumed. `\lpcode 200` on cmr10's `A` — a
    /// glyph 7.50002pt wide — gives pdfTeX a 2.0pt kern, which is a fifth of
    /// the 10pt em and not of the glyph; at `cmr10 at 20pt`, with quad
    /// 20.00005pt and an `A` 15.00003pt wide, the kern is 4.00002pt. So the
    /// base is `\fontdimen6` and "thousandths of the character's width", which
    /// is the natural reading of the name, is wrong in every font.
    ///
    /// Here rather than beside the engine's other side tables because the
    /// *line breaker* needs them, and all it is given is the font table.
    #[serde(default)]
    lp: alloc::collections::BTreeMap<u32, i64>,
    #[serde(default)]
    rp: alloc::collections::BTreeMap<u32, i64>,
    /// pdfTeX's per-character expansion factor (`\efcode`), in thousandths.
    /// An absent entry has pdfTeX's default value of 1000. Assigning one code
    /// makes pdfTeX allocate a 256-entry table initialized to 1000; the sparse
    /// representation here has the same observable result without the table.
    #[serde(default)]
    ef: alloc::collections::BTreeMap<u32, i64>,
    /// `(stretch, shrink, step, autoexpand)` from `\pdffontexpand`.
    #[serde(default)]
    expansion: Option<(i64, i64, i64, bool)>,
    /// Font-table owners of the protrusion codes on pdfTeX's expanded limit
    /// instances, for stretch and shrink respectively.
    ///
    /// Expanded fonts are looked up globally by `<tfm-name>{+|-}<amount>` and
    /// size. Several independently configured base fonts can therefore share
    /// one expanded instance. Every reuse redirects that instance's lp/rp
    /// tables to the latest base font; earlier bases keep their link to the
    /// same instance. The owner indices preserve that observable aliasing for
    /// marginal-kern variation during line breaking.
    #[serde(default)]
    expanded_protrusion_owners: [Option<usize>; 2],
    /// Whether each limit instance participates in pdfTeX's global expanded-
    /// font lookup. `\pdfcopyfont` descendants instead own private instances.
    #[serde(default)]
    expanded_protrusion_shared: [bool; 2],
    /// `\letterspacefont` is implemented by pdfTeX as a virtual font whose
    /// sole local font is the original source font. If the tracked font is
    /// expanded, pdfTeX immediately propagates those parameters to this base.
    #[serde(default)]
    letterspace_base: Option<usize>,
    /// Interword-spacing and character-kern codes, in primitive order:
    /// `knbscode`, `stbscode`, `shbscode`, `knbccode`, `knaccode`.
    #[serde(default)]
    micro_codes: [alloc::collections::BTreeMap<u32, i64>; 5],
    /// The extensible recipes — four character codes each: top, middle, bottom
    /// and the piece repeated to fill. `tex.web` §546. A delimiter that has to
    /// grow taller than any single glyph is built out of these.
    exten: Vec<[u8; 4]>,
    /// `\skewchar` — the character a math accent's skew is measured against.
    /// It is an engine setting, not part of the TFM, so it is written here when
    /// `\skewchar\font=<n>` is executed.
    skew_char: i32,
    kerns: Vec<Scaled>,
    /// `\fontdimen` parameters: slant, space, stretch, shrink, xheight, quad,
    /// extra space, then math parameters for families 2 and 3.
    params: Vec<Scaled>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
struct CharInfo {
    width_index: u8,
    height_index: u8,
    depth_index: u8,
    italic_index: u8,
    /// 1 means a ligature/kern program starts at `remainder`; 0 means none.
    /// (2 and 3 are charlists and extensibles, for math, and are ignored here.)
    tag: u8,
    remainder: u8,
}

/// What a font says should happen between two adjacent characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LigKern {
    /// Insert `code` between the pair, then retain/delete the two operands and
    /// advance according to TeX's `op_byte` (§545). Ordinary `=:` is op 0;
    /// TRIP deliberately exercises the other seven legal operators.
    Ligature { code: u32, op: u8 },
    /// Move the second character by this much. Almost always negative -- `V`
    /// then `A` are tucked together -- which is why omitting kerns makes text
    /// look uniformly loose rather than obviously broken.
    Kern(Scaled),
}

/// One result of running a string through a TFM ligature/kern program.
///
/// `source` contains indices of input characters consumed into a generated
/// ligature; a generated character may consume none when both operands are
/// retained. `span` is the corresponding half-open input range (or an empty
/// insertion point), which hyphenation uses when it reconstructs a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShapedItem {
    Glyph {
        code: u32,
        /// Literal character provenance supplied by §903's `init_list`.
        /// These characters precede the `source` positions but are outside
        /// the `hu` array being reconstituted.
        prefix: Vec<u32>,
        source: Vec<usize>,
        span: (usize, usize),
        ligature: bool,
        left_hit: bool,
        right_hit: bool,
    },
    Kern {
        width: Scaled,
        at: usize,
    },
}

/// One §902 cut prefix, as produced by TeX's `reconstitute` routine.
pub(crate) struct Reconstituted {
    pub items: Vec<ShapedItem>,
    /// Last input position consumed by this prefix, inclusive.
    pub last: usize,
    /// First candidate break whose hyphen changed this prefix.
    pub hyphen_passed: Option<usize>,
}

/// Translation state saved from the character or ligature immediately before
/// a word when §903 removes that node and reconstitutes it with the word.
#[derive(Clone, Copy)]
pub(crate) struct ReconstituteInit<'a> {
    pub raw: &'a [u32],
    pub ligature: bool,
    pub left_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TfmError {
    TooShort,
    /// Header lengths disagree with the file length — TeX checks this and
    /// refuses to load rather than reading past the end.
    Inconsistent,
    BadRange,
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

/// TeX's `alpha`/`beta` pair for a given at-size (`tex.web` §572).
///
/// The at-size is halved until it fits a bound, tracking the compensation, so
/// the multiplications below cannot overflow a 32-bit word. This is why TeX can
/// scale fonts to arbitrary sizes on 1982 hardware without losing a scaled
/// point.
fn alpha_beta(at: Scaled) -> (i64, i64) {
    let mut z = at.0 as i64;
    let mut alpha = 16i64;
    while z >= 0x80_0000 {
        z /= 2;
        alpha += alpha;
    }
    // §572's `beta:=256 div alpha`. `alpha` doubles once per halving, so five
    // halvings make it 512 and `beta` zero — which needs an at-size of 2^28 sp
    // or more. §1259's "less than 2048pt" (2^27) is exactly what keeps that
    // unreachable, and until this engine enforced it `\font\b cmr10 at 3000pt`
    // divided by zero and took the process down. The clamp is for a malformed
    // TFM whose own design size is out of range, where there is no §1259 to
    // lean on; for anything TeX would accept it changes nothing.
    let beta = (256 / alpha).max(1);
    (alpha * z, beta)
}

/// Convert a TFM `fix_word` to scaled points, as Knuth's `store_scaled`
/// (`tex.web` §572) does: build the value from the four bytes with a division
/// after each. `a == 255` marks a negative value.
///
/// A plain `(fw * at) >> 20` in 64-bit arithmetic gives the same answer for
/// every font and size measured — cmr10 from 10pt to 500pt, including past the
/// 128pt threshold where the halving loop engages. This is kept anyway because
/// it is what TeX specifies, so it matches by construction rather than by
/// coincidence, and byte-exact DVI comparison is the engine's only oracle.
fn fix_word_to_scaled(fw: u32, at: Scaled) -> Scaled {
    let (alpha, beta) = alpha_beta(at);
    let z = {
        let mut z = at.0 as i64;
        while z >= 0x80_0000 {
            z /= 2;
        }
        z
    };
    let a = ((fw >> 24) & 0xFF) as i64;
    let b = ((fw >> 16) & 0xFF) as i64;
    let c = ((fw >> 8) & 0xFF) as i64;
    let d = (fw & 0xFF) as i64;

    let sw = (((((d * z) / 256) + (c * z)) / 256) + (b * z)) / beta;
    if a == 0 {
        Scaled(sw as i32)
    } else if a == 255 {
        Scaled((sw - alpha) as i32)
    } else {
        // Out of range; TeX rejects the font. Clamp rather than wrap.
        Scaled(0)
    }
}

/// pdfTeX's `round_xn_over_d`: `xn_over_d` with a rounding step, normalising `x` but
/// *not* `n`.
///
/// ```text
/// function round_xn_over_d(@!x:scaled; @!n,@!d:integer):scaled;
/// begin if x>=0 then positive:=true
/// else  begin negate(x); positive:=false;
///   end;
/// t:=(x mod @'100000)*n;
/// u:=(x div @'100000)*n+(t div @'100000);
/// v:=(u mod d)*@'100000 + (t mod @'100000);
/// if u div d>=@'100000 then arith_error:=true
/// else u:=@'100000*(u div d) + (v div d);
/// v := v mod d;
/// if 2*v >= d then u := u + 1;
/// if positive then round_xn_over_d:=u
/// else round_xn_over_d:=-u;
/// end;
/// ```
///
/// A protrusion code is routinely negative, and `n` is the code. Then `t`, `u` and `v`
/// come out negative too, `2*v >= d` is false for every negative `v`, and the rounding
/// step never fires — while for a positive code it fires half the time. The asymmetry
/// is real and measurable: on `cmr10 at 10.95pt` (quad 717621sp) a code of −35 gives
/// 0.38324pt where +35 gives −0.38326pt, and −40 gives 0.43799pt where +40 gives
/// −0.438pt.
///
/// Rounding the magnitude and re-applying the sign — the natural thing to write, and
/// what this did — is wrong for every negative code by one scaled point. Verified
/// against `pdftex -ini -etex` over codes −40..40, all 80 exact.
///
/// Rust's `/` and `%` on integers truncate toward zero and take the dividend's sign,
/// which is what Pascal's `div` and `mod` do, so the transcription is direct.
fn round_xn_over_d(x: Scaled, n: i64, d: i64) -> Scaled {
    let positive = x.0 >= 0;
    let x = if positive { x.0 as i64 } else { -(x.0 as i64) };
    let t = (x % 32768) * n;
    let u = (x / 32768) * n + t / 32768;
    let v = (u % d) * 32768 + t % 32768;
    let mut u = 32768 * (u / d) + v / d;
    let v = v % d;
    if 2 * v >= d {
        u += 1;
    }
    Scaled(if positive { u as i32 } else { -u as i32 })
}

impl TfmFont {
    /// Create pdfTeX's `\pdfcopyfont` metric instance.
    ///
    /// The TFM memory, runtime `\fontdimen` values, hyphen/skew state, and
    /// ligature program are copied. PDF-side microtype tables are not: the new
    /// font exists precisely so a package can assign independent protrusion,
    /// expansion, and spacing settings while the PDF writer shares the same
    /// physical font program.
    pub fn pdf_copy(&self) -> Self {
        let mut copy = self.clone();
        copy.lp.clear();
        copy.rp.clear();
        copy.ef.clear();
        copy.expansion = None;
        copy.expanded_protrusion_owners = [None, None];
        copy.expanded_protrusion_shared = [false, false];
        copy.letterspace_base = None;
        for codes in &mut copy.micro_codes {
            codes.clear();
        }
        copy
    }

    /// Heap held by this font's metric tables, for `Fast::memory_report`.
    pub fn heap_bytes(&self) -> usize {
        use core::mem::size_of;
        size_of::<Self>()
            + (self.widths.capacity()
                + self.heights.capacity()
                + self.depths.capacity()
                + self.italics.capacity()
                + self.kerns.capacity()
                + self.params.capacity())
                * size_of::<Scaled>()
            + self.chars.capacity() * size_of::<CharInfo>()
            + (self.lig_kern.capacity() + self.exten.capacity()) * 4
    }

    /// Parse a `.tfm`, scaling to `at_size`. Pass `None` to use the design size.
    pub fn parse(bytes: &[u8], at_size: Option<Scaled>) -> Result<TfmFont, TfmError> {
        // The twelve-halfword directory.
        let lf = be16(bytes, 0).ok_or(TfmError::TooShort)? as usize;
        let lh = be16(bytes, 2).ok_or(TfmError::TooShort)? as usize;
        let bc = be16(bytes, 4).ok_or(TfmError::TooShort)?;
        let ec = be16(bytes, 6).ok_or(TfmError::TooShort)?;
        let nw = be16(bytes, 8).ok_or(TfmError::TooShort)? as usize;
        let nh = be16(bytes, 10).ok_or(TfmError::TooShort)? as usize;
        let nd = be16(bytes, 12).ok_or(TfmError::TooShort)? as usize;
        let ni = be16(bytes, 14).ok_or(TfmError::TooShort)? as usize;
        let nl = be16(bytes, 16).ok_or(TfmError::TooShort)? as usize;
        let nk = be16(bytes, 18).ok_or(TfmError::TooShort)? as usize;
        let ne = be16(bytes, 20).ok_or(TfmError::TooShort)? as usize;
        let np = be16(bytes, 22).ok_or(TfmError::TooShort)? as usize;

        if bc > ec + 1 || ec > 255 {
            return Err(TfmError::BadRange);
        }
        let nc = (ec as usize + 1).saturating_sub(bc as usize);
        // TeX's own consistency check (`tex.web` §541).
        let expect = 6 + lh + nc + nw + nh + nd + ni + nl + nk + ne + np;
        if lf != expect || bytes.len() < lf * 4 {
            return Err(TfmError::Inconsistent);
        }

        // Words, not bytes, from here on.
        let w = |i: usize| be32(bytes, i * 4).ok_or(TfmError::TooShort);

        let checksum = w(6)?;
        // The design size is itself a fix_word, but scaled against unity.
        let design_size = Scaled((w(7)? >> 4) as i32);
        let at = at_size.unwrap_or(design_size);
        let tex_text_coding = if lh >= 12 {
            let length = usize::from(bytes[32].min(39));
            bytes
                .get(33..33 + length)
                .is_some_and(|coding| coding.eq_ignore_ascii_case(b"TEX TEXT"))
        } else {
            false
        };

        let mut off = 6 + lh;
        let mut chars = Vec::with_capacity(nc);
        for i in 0..nc {
            let v = w(off + i)?;
            chars.push(CharInfo {
                width_index: (v >> 24) as u8,
                height_index: ((v >> 20) & 0xF) as u8,
                depth_index: ((v >> 16) & 0xF) as u8,
                italic_index: ((v >> 10) & 0x3F) as u8,
                tag: ((v >> 8) & 0x3) as u8,
                remainder: v as u8,
            });
        }
        off += nc;

        let table = |n: usize, off: &mut usize| -> Result<Vec<Scaled>, TfmError> {
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                v.push(fix_word_to_scaled(w(*off + i)?, at));
            }
            *off += n;
            Ok(v)
        };
        let widths = table(nw, &mut off)?;
        let heights = table(nh, &mut off)?;
        let depths = table(nd, &mut off)?;
        let italics = table(ni, &mut off)?;

        // The ligature/kern program: four bytes per instruction, kept raw
        // because interpreting it needs the pair of characters in hand.
        let mut lig_kern = Vec::with_capacity(nl);
        for i in 0..nl {
            let v = w(off + i)?;
            lig_kern.push([(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        // Only real instructions (skip <= 128) carry an operation byte.
        // Redirects and the boundary declarations use the same two byte
        // positions for an address and must not make a simple font look
        // complex.
        let complex_ligatures = lig_kern
            .iter()
            .any(|[skip, _, op, _]| *skip <= 128 && *op > 0 && *op < 128);
        off += nl;
        let kerns = table(nk, &mut off)?;
        let mut exten = Vec::with_capacity(ne);
        for i in 0..ne {
            let v = w(off + i)?;
            exten.push([(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        off += ne;
        let params = {
            let mut v = Vec::with_capacity(np);
            for i in 0..np {
                let raw = w(off + i)?;
                // Parameter 1 (slant) is a pure ratio, not a length, so it is
                // *not* scaled by the at-size. Getting this wrong tilts every
                // accent in the document.
                v.push(if i == 0 {
                    Scaled((raw as i32) >> 4)
                } else {
                    fix_word_to_scaled(raw, at)
                });
            }
            // §563 allocates `max(np, 7)` words for every real font and
            // initializes the missing basic parameters to zero.  Symbol TFMs
            // commonly declare `np=0` (MnSymbolC is one); TeX nevertheless
            // permits `\fontdimen1` through `\fontdimen7` without treating
            // the font as parameterless.  Keeping the literal TFM count made
            // microtype's standard `\fontdimen6` repair emit an error for
            // every size of those fonts.
            v.resize(v.len().max(7), Scaled::ZERO);
            v
        };

        let mut font = TfmFont {
            checksum,
            design_size,
            at_size: at,
            tex_text_coding,
            bc,
            ec,
            widths,
            heights,
            depths,
            italics,

            chars,
            lig_kern,
            complex_ligatures,
            no_ligatures: false,
            unligatures: Vec::new(),
            lp: alloc::collections::BTreeMap::new(),
            rp: alloc::collections::BTreeMap::new(),
            ef: alloc::collections::BTreeMap::new(),
            expansion: None,
            expanded_protrusion_owners: [None, None],
            expanded_protrusion_shared: [false, false],
            letterspace_base: None,
            micro_codes: core::array::from_fn(|_| alloc::collections::BTreeMap::new()),
            exten,
            skew_char: -1,
            kerns,
            params,
        };
        font.rebuild_unligatures();
        Ok(font)
    }

    #[inline(always)]
    fn info(&self, code: u32) -> Option<&CharInfo> {
        if code < self.bc as u32 || code > self.ec as u32 {
            return None;
        }
        self.chars.get(code as usize - self.bc as usize)
    }

    /// Does this font have a glyph for `code`?
    ///
    /// A zero width index means the slot is empty, which is distinct from a
    /// genuinely zero-width character.
    #[inline(always)]
    pub fn has_char(&self, code: u32) -> bool {
        self.info(code).is_some_and(|c| c.width_index != 0)
    }

    #[inline(always)]
    pub fn width(&self, code: u32) -> Scaled {
        self.info(code)
            .and_then(|c| self.widths.get(c.width_index as usize))
            .copied()
            .unwrap_or(Scaled::ZERO)
    }

    /// Width, height and depth with one character-table lookup. Horizontal
    /// packing asks for all three for every glyph; doing three independent
    /// range checks and `chars` lookups there was pure bookkeeping.
    #[inline(always)]
    pub fn dimensions(&self, code: u32) -> (Scaled, Scaled, Scaled) {
        let Some(c) = self.info(code) else {
            return (Scaled::ZERO, Scaled::ZERO, Scaled::ZERO);
        };
        (
            self.widths
                .get(c.width_index as usize)
                .copied()
                .unwrap_or(Scaled::ZERO),
            self.heights
                .get(c.height_index as usize)
                .copied()
                .unwrap_or(Scaled::ZERO),
            self.depths
                .get(c.depth_index as usize)
                .copied()
                .unwrap_or(Scaled::ZERO),
        )
    }

    pub fn height(&self, code: u32) -> Scaled {
        self.info(code)
            .and_then(|c| self.heights.get(c.height_index as usize))
            .copied()
            .unwrap_or(Scaled::ZERO)
    }

    /// The italic correction for a character — what `\/` inserts.
    pub fn italic(&self, code: u32) -> Scaled {
        self.info(code)
            .and_then(|c| self.italics.get(c.italic_index as usize))
            .copied()
            .unwrap_or(Scaled::ZERO)
    }

    pub fn depth(&self, code: u32) -> Scaled {
        self.info(code)
            .and_then(|c| self.depths.get(c.depth_index as usize))
            .copied()
            .unwrap_or(Scaled::ZERO)
    }

    /// `\fontdimen n`, 1-indexed as TeX numbers them.
    /// A font with no characters, for `\nullfont` and for a `.tfm` that could
    /// not be read. Every glyph measures zero, which is what TeX reports for
    /// `\nullfont` too.
    pub fn empty() -> TfmFont {
        TfmFont {
            checksum: 0,
            design_size: Scaled::from_points(10),
            at_size: Scaled::from_points(10),
            ..Default::default()
        }
    }

    /// `\fontdimen<n><font>=<dimen>`. TeX allows a font's parameters to be
    /// changed after loading, and plain.tex does it to `\fontdimen3` of the
    /// math fonts.
    pub fn set_param(&mut self, n: usize, v: Scaled) {
        if n == 0 {
            return;
        }
        if self.params.len() < n {
            self.params.resize(n, Scaled::ZERO);
        }
        self.params[n - 1] = v;
    }

    /// What goes between `left` and `right`, if anything.
    ///
    /// `tex.web` §545. The program is a list of instructions attached to the
    /// *left* character; each names a right character it applies to, and a
    /// skip count to reach the next candidate. A first instruction with
    /// `skip_byte > 128` is a redirect, which is how fonts with more than 256
    /// instructions address the rest of them.
    pub fn set_skew_char(&mut self, c: i32) {
        self.skew_char = c;
    }

    pub fn skew_char(&self) -> Option<u32> {
        if self.skew_char < 0 {
            None
        } else {
            Some(self.skew_char as u32)
        }
    }

    /// The character's *tag*: 0 none, 1 a ligature/kern program, 2 a charlist
    /// (this glyph has a bigger variant), 3 an extensible recipe.
    pub fn char_tag(&self, code: u32) -> u8 {
        self.info(code).map_or(0, |i| i.tag)
    }

    /// Clear a character's TFM tag, as pdfTeX's `\tagcode<font><char>=-1`
    /// does.  Tracking packages use this on `f` to suppress the font's
    /// `ff`/`fi`/`fl` ligature program without disabling unrelated kerns.
    pub fn clear_char_tag(&mut self, code: u32) {
        if code < self.bc as u32 || code > self.ec as u32 {
            return;
        }
        if let Some(info) = self.chars.get_mut(code as usize - self.bc as usize) {
            info.tag = 0;
            // A produced glyph may be reachable from more than one left
            // character. Rebuild rather than merely deleting this left's
            // entries so the next candidate retains the old scan order.
            self.rebuild_unligatures();
        }
    }

    /// The tag's operand: the next bigger variant for a charlist, the index
    /// into the extensible table for a recipe.
    pub fn char_remainder(&self, code: u32) -> u32 {
        self.info(code).map_or(0, |i| i.remainder as u32)
    }

    /// One extensible recipe: `(top, middle, bottom, repeated)`. A zero code
    /// means that piece is absent.
    pub fn exten(&self, index: u32) -> Option<(u32, u32, u32, u32)> {
        let r = self.exten.get(index as usize)?;
        Some((r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32))
    }

    /// The pair a ligature was made from, if this character is one.
    ///
    /// Every `=:` instruction names the character it produces. The reverse
    /// table is built once when the TFM is parsed (and after the rare
    /// `\tagcode=-1` mutation), because hyphenation asks this for every
    /// represented letter.
    fn rebuild_unligatures(&mut self) {
        let mut reverse = alloc::vec![None; 256];
        for left in self.bc as u32..=self.ec as u32 {
            let Some(info) = self.info(left) else {
                continue;
            };
            if info.tag != 1 {
                continue;
            }
            let mut i = info.remainder as usize;
            let Some(first) = self.lig_kern.get(i) else {
                continue;
            };
            if first[0] > 128 {
                i = 256 * first[2] as usize + first[3] as usize;
            }
            for _ in 0..self.lig_kern.len() {
                let Some(&[skip, next, op, rem]) = self.lig_kern.get(i) else {
                    break;
                };
                if skip <= 128 && op == 0 {
                    // Match the former query-time scan: the first left
                    // character and first reachable instruction win.
                    reverse[rem as usize].get_or_insert((left, next as u32));
                }
                if skip >= 128 {
                    break;
                }
                i += skip as usize + 1;
            }
        }
        self.unligatures = reverse;
    }

    /// The pair a ligature was made from, if this character is one.
    ///
    /// Hyphenation needs this — `office` is `o` `ffi` `c` `e` on the page,
    /// and its patterns are matched against the letters.
    #[inline]
    pub fn unligature(&self, code: u32) -> Option<(u32, u32)> {
        self.unligatures.get(code as usize).copied().flatten()
    }

    /// `\lpcode`/`\rpcode` as *set*, in thousandths. Zero when unset, which is
    /// pdfTeX's default and means "does not protrude".
    pub fn protrude_code(&self, code: u32, right: bool) -> i64 {
        let t = if right { &self.rp } else { &self.lp };
        t.get(&code).copied().unwrap_or(0)
    }

    pub fn set_protrude_code(&mut self, code: u32, right: bool, v: i64) {
        let t = if right { &mut self.rp } else { &mut self.lp };
        if v == 0 {
            t.remove(&code);
        } else {
            t.insert(code, v);
        }
    }

    pub fn expansion_code(&self, code: u32) -> i64 {
        self.ef.get(&code).copied().unwrap_or(1000)
    }

    pub fn set_expansion_code(&mut self, code: u32, v: i64) {
        if v == 1000 {
            self.ef.remove(&code);
        } else {
            self.ef.insert(code, v);
        }
    }

    pub fn set_expansion(&mut self, stretch: i64, shrink: i64, step: i64, auto: bool) {
        self.expansion = Some((stretch, shrink, step, auto));
    }

    pub fn set_expanded_protrusion_owner(&mut self, stretch: bool, owner: usize, shared: bool) {
        let slot = usize::from(!stretch);
        self.expanded_protrusion_owners[slot] = Some(owner);
        self.expanded_protrusion_shared[slot] = shared;
    }

    pub fn expanded_protrusion_owner(&self, stretch: bool) -> Option<usize> {
        self.expanded_protrusion_owners[usize::from(!stretch)]
    }

    pub fn expanded_protrusion_is_shared(&self, stretch: bool) -> bool {
        self.expanded_protrusion_shared[usize::from(!stretch)]
    }

    pub fn letterspace_base(&self) -> Option<usize> {
        self.letterspace_base
    }

    pub fn set_letterspace_base(&mut self, base: usize) {
        self.letterspace_base = Some(base);
    }

    /// Add pdfTeX-style tracking to this freshly loaded metric.
    ///
    /// The primitive adds `amount / 1000` of the font quad to every existing
    /// character width.  pdfTeX clamps the requested amount to -1000..1000;
    /// the other metrics and fontdimens are unchanged. The caller is
    /// responsible for supplying a pristine reload of the base TFM: pdfTeX's
    /// primitive does not inherit runtime `\fontdimen` or microtype changes
    /// from the source font.
    pub fn letterspaced(&self, amount: i64) -> Self {
        let mut font = self.clone();
        let amount = amount.clamp(-1000, 1000);
        font.letterspace_base = None;
        let extra = round_xn_over_d(font.param(6), amount, 1000);
        // Width table entry zero denotes a missing character.  Existing
        // characters use nonzero indices even when several share a width.
        for width in font.widths.iter_mut().skip(1) {
            *width = Scaled(width.0.saturating_add(extra.0));
        }
        font
    }

    /// The leading (and trailing) movement in pdfTeX's synthetic
    /// `\letterspacefont` virtual-character packet.
    ///
    /// This is deliberately computed directly from the em rather than by
    /// halving the tracked width increment: pdfTeX rounds the two quantities
    /// independently (`e/2000` here, `e/1000` in [`Self::letterspaced`]).
    pub fn letterspace_glyph_offset(&self, amount: i64) -> Scaled {
        round_xn_over_d(self.param(6), amount.clamp(-1000, 1000), 2000)
    }

    /// The expansion limits installed by `\pdffontexpand`.
    pub fn expansion_limits(&self) -> Option<(i64, i64, i64)> {
        self.expansion
            .map(|(stretch, shrink, step, _)| (stretch, shrink, step))
    }

    pub(crate) fn expansion_is_auto(&self) -> bool {
        self.expansion.is_some_and(|(_, _, _, auto)| auto)
    }

    /// How much this glyph can grow and shrink at the font's configured
    /// limits, after applying its `\efcode`.
    ///
    /// pdfTeX creates a private TFM whose widths are first rounded at the full
    /// expansion limit, then multiplies that difference by `\efcode`. The two
    /// rounding operations are intentionally kept separate here.
    pub fn expansion_capacity(&self, code: u32) -> (Scaled, Scaled) {
        let Some((stretch, shrink, step, _)) = self.expansion else {
            return (Scaled::ZERO, Scaled::ZERO);
        };
        if step <= 0 {
            return (Scaled::ZERO, Scaled::ZERO);
        }
        let ef = self.expansion_code(code).clamp(0, 1000);
        if ef == 0 {
            return (Scaled::ZERO, Scaled::ZERO);
        }
        let width = self.width(code);
        let stretched = if stretch > 0 {
            round_xn_over_d(width, 1000 + stretch, 1000) - width
        } else {
            Scaled::ZERO
        };
        let shrunk = if shrink > 0 {
            width - round_xn_over_d(width, 1000 - shrink, 1000)
        } else {
            Scaled::ZERO
        };
        (
            round_xn_over_d(stretched, ef, 1000),
            round_xn_over_d(shrunk, ef, 1000),
        )
    }

    /// The glyph-width change at pdfTeX's line expansion ratio (-1000..1000).
    /// The requested font expansion is rounded to the nearest configured step
    /// before its TFM width is calculated, just as `fix_expand_value` does.
    pub fn expansion_delta(&self, code: u32, ratio: i64) -> Scaled {
        let amount = self.expansion_amount(code, ratio);
        if amount == 0 {
            return Scaled::ZERO;
        }
        let width = self.width(code);
        round_xn_over_d(width, 1000 + amount, 1000) - width
    }

    /// The expanded font instance selected for one character. pdfTeX applies
    /// `\efcode` before quantizing to the font step, so two characters in the
    /// same line may use neighboring expanded font instances.
    pub fn expansion_amount(&self, code: u32, ratio: i64) -> i64 {
        let Some((stretch, shrink, step, _)) = self.expansion else {
            return 0;
        };
        if ratio == 0 || step <= 0 {
            return 0;
        }
        let limit = if ratio > 0 { stretch } else { shrink };
        let ef = self.expansion_code(code).clamp(0, 1000);
        if limit <= 0 || ef == 0 {
            return 0;
        }
        let numerator = ratio.saturating_mul(ef).saturating_mul(limit);
        let amount = if numerator >= 0 {
            (numerator + 500_000) / 1_000_000
        } else {
            -((-numerator + 500_000) / 1_000_000)
        };
        let magnitude = amount.unsigned_abs() as i64;
        let rounded = step * ((magnitude + step / 2) / step);
        amount.signum() * rounded.min(limit)
    }

    pub fn expansion_kern_capacity(&self, left: u32, right: u32, base: Scaled) -> (Scaled, Scaled) {
        let Some((stretch, shrink, _, _)) = self.expansion else {
            return (Scaled::ZERO, Scaled::ZERO);
        };
        let ef = self.expansion_code(left).clamp(0, 1000);
        if ef == 0 {
            return (Scaled::ZERO, Scaled::ZERO);
        }
        // `kern_stretch`/`kern_shrink` look the kern up again between the
        // glyphs that neighbor the node *after* ligaturing. That pair can be
        // different from the pair that originally created `base`: in
        // `matter---varying`, for example, the kern before the first hyphen
        // survives while the three hyphens become an em dash. The expanded
        // font has no r/em-dash kern, so pdfTeX compares zero with the retained
        // -0.2pt node. Treating a missing lookup as zero is essential; treating
        // it as "no expansion" shifts paragraph demerits and line breaks.
        let pair = match self.lig_kern(left, right) {
            Some(LigKern::Kern(width)) => width,
            _ => Scaled::ZERO,
        };
        let stretched = round_xn_over_d(pair, 1000 + stretch.max(0), 1000) - base;
        let shrunk = base - round_xn_over_d(pair, 1000 - shrink.max(0), 1000);
        (
            round_xn_over_d(stretched, ef, 1000),
            round_xn_over_d(shrunk, ef, 1000),
        )
    }

    pub fn expansion_kern_delta(&self, left: u32, right: u32, base: Scaled, ratio: i64) -> Scaled {
        let amount = self.expansion_amount(left, ratio);
        if amount == 0 {
            return Scaled::ZERO;
        }
        let pair = match self.lig_kern(left, right) {
            Some(LigKern::Kern(width)) => width,
            _ => Scaled::ZERO,
        };
        round_xn_over_d(pair, 1000 + amount, 1000) - base
    }

    pub fn micro_code(&self, which: usize, code: u32) -> i64 {
        self.micro_codes[which].get(&code).copied().unwrap_or(0)
    }

    pub fn set_micro_code(&mut self, which: usize, code: u32, v: i64) {
        let t = &mut self.micro_codes[which];
        if v == 0 {
            t.remove(&code);
        } else {
            t.insert(code, v);
        }
    }

    /// pdfTeX's microtype interword-glue additions after this character:
    /// natural width, stretch, and shrink, each in thousandths of the quad.
    pub fn micro_spacing(&self, code: u32) -> (Scaled, Scaled, Scaled) {
        let quad = self.param(6);
        (
            round_xn_over_d(quad, self.micro_code(0, code), 1000),
            round_xn_over_d(quad, self.micro_code(1, code), 1000),
            round_xn_over_d(quad, self.micro_code(2, code), 1000),
        )
    }

    /// pdfTeX's automatic kern between two characters (or a list boundary).
    /// `knaccode` belongs to the left character and `knbccode` to the right.
    pub fn micro_kern(
        &self,
        left: Option<u32>,
        right: Option<u32>,
        append: bool,
        prepend: bool,
    ) -> Scaled {
        let quad = self.param(6);
        let mut width = Scaled::ZERO;
        if append {
            if let Some(c) = left {
                width = width + round_xn_over_d(quad, self.micro_code(4, c), 1000);
            }
        }
        if prepend {
            if let Some(c) = right {
                width = width + round_xn_over_d(quad, self.micro_code(3, c), 1000);
            }
        }
        width
    }

    /// How far the character actually hangs: the quad times the code over a
    /// thousand. `\fontdimen6` is the base — see [`Self::protrude_code`].
    pub fn protrusion(&self, code: u32, right: bool) -> Scaled {
        let c = self.protrude_code(code, right);
        if c == 0 {
            return Scaled::ZERO;
        }
        round_xn_over_d(self.param(6), c as i64, 1000)
    }

    /// pdfTeX's `\pdfnoligatures<font>`: disable each character's entire
    /// ligature/kern program. The primitive clears the character's `lig_tag`,
    /// so ordinary TFM kerns disappear along with ligatures. The special left
    /// boundary program is not stored in a character tag and remains active.
    pub fn set_no_ligatures(&mut self, off: bool) {
        self.no_ligatures = off;
    }

    pub(crate) fn ligatures_disabled(&self) -> bool {
        self.no_ligatures
    }

    /// True when every ligature instruction uses TeX's ordinary `=:`, so the
    /// caller may translate a run directly into final nodes. Retained-operand
    /// programs still require the general splice interpreter below.
    pub(crate) fn has_simple_ligature_program(&self) -> bool {
        !self.complex_ligatures
    }

    /// The implicit right-boundary character, when the first program word
    /// declares one with `skip_byte=255` (§545).
    pub(crate) fn boundary_char(&self) -> Option<u32> {
        let [skip, next, ..] = *self.lig_kern.first()?;
        (skip == 255).then_some(next as u32)
    }

    /// The special lig/kern program for the implicit left boundary, encoded
    /// by the last program word (§545).
    pub(crate) fn left_boundary_start(&self) -> Option<usize> {
        let [skip, _, op, rem] = *self.lig_kern.last()?;
        if skip != 255 {
            return None;
        }
        let i = 256 * op as usize + rem as usize;
        (i < self.lig_kern.len()).then_some(i)
    }

    fn lig_kern_from(&self, mut i: usize, right: u32) -> Option<LigKern> {
        // Bounded by the table: a malformed font must not spin here.
        for _ in 0..self.lig_kern.len() {
            let [skip, next, op, rem] = *self.lig_kern.get(i)?;
            if skip <= 128 && next as u32 == right {
                return Some(if op >= 128 {
                    // A kern, indexed by the low bits of the operation.
                    let k = 256 * (op as usize - 128) + rem as usize;
                    LigKern::Kern(*self.kerns.get(k)?)
                } else {
                    LigKern::Ligature {
                        code: rem as u32,
                        op,
                    }
                });
            }
            if skip >= 128 {
                return None; // last instruction for this character
            }
            i += skip as usize + 1;
        }
        None
    }

    pub fn lig_kern(&self, left: u32, right: u32) -> Option<LigKern> {
        if self.no_ligatures {
            return None;
        }
        let info = self.info(left)?;
        if info.tag != 1 {
            return None;
        }
        let mut i = info.remainder as usize;
        // A redirect: the real program starts elsewhere.
        let first = *self.lig_kern.get(i)?;
        if first[0] > 128 {
            i = 256 * first[2] as usize + first[3] as usize;
        }
        self.lig_kern_from(i, right)
    }

    pub(crate) fn left_boundary_lig_kern(&self, right: u32) -> Option<LigKern> {
        self.lig_kern_from(self.left_boundary_start()?, right)
    }

    /// Whether a raw run can be copied straight to the node list.  Most words
    /// contain neither a ligature pair nor a font kern; constructing the full
    /// rewrite work list for them only to reproduce the input costs several
    /// heap allocations per word.
    pub(crate) fn shape_text_needed(
        &self,
        chars: &[u32],
        left_boundary: bool,
        right_boundary: bool,
    ) -> bool {
        let Some((&first, rest)) = chars.split_first() else {
            return false;
        };
        if left_boundary && self.left_boundary_lig_kern(first).is_some() {
            return true;
        }
        if chars
            .iter()
            .copied()
            .zip(rest.iter().copied())
            .any(|(left, right)| self.lig_kern(left, right).is_some())
        {
            return true;
        }
        right_boundary
            && self.boundary_char().is_some_and(|boundary| {
                self.lig_kern(*chars.last().unwrap_or(&first), boundary)
                    .is_some()
            })
    }

    /// TeX §902's `reconstitute`, without the surrounding node-list plumbing.
    ///
    /// Unlike [`Self::shape_text`], this stops after the first cut prefix and
    /// tests candidate hyphens while the ligature cursor is live. That is what
    /// lets §913 synchronize the unbroken and broken translations even when a
    /// retained operand makes the effect of a hyphen ripple through the word.
    pub(crate) fn reconstitute(
        &self,
        chars: &[u32],
        start: usize,
        bchar: Option<u32>,
        hchar: Option<u32>,
        breaks: &[bool],
        init: Option<ReconstituteInit<'_>>,
    ) -> Reconstituted {
        #[derive(Clone)]
        struct LigItem {
            code: u32,
            raw: Option<(u32, usize)>,
        }

        fn raw_glyph(code: u32, at: usize) -> ShapedItem {
            ShapedItem::Glyph {
                code,
                prefix: Vec::new(),
                source: alloc::vec![at],
                span: (at, at + 1),
                ligature: false,
                left_hit: false,
                right_hit: false,
            }
        }

        fn init_glyph(code: u32, at: usize) -> ShapedItem {
            ShapedItem::Glyph {
                code,
                prefix: alloc::vec![code],
                source: Vec::new(),
                span: (at, at),
                ligature: false,
                left_hit: false,
                right_hit: false,
            }
        }

        let n = chars.len().saturating_sub(1);
        if chars.is_empty() || start >= chars.len() {
            return Reconstituted {
                items: Vec::new(),
                last: start,
                hyphen_passed: None,
            };
        }

        let mut out = Vec::new();
        let mut j = start;
        let mut cur_l = (chars[j] != crate::node::LIG_BOUNDARY).then_some(chars[j]);
        let mut cur_r;
        let mut cur_rh;
        let mut hchar = hchar;
        let mut hyphen_passed = None;
        let mut lig_start = 0usize;
        let mut lig_stack: Vec<LigItem> = Vec::new();
        let mut ligature_present = false;
        let mut left_hit = false;
        let mut right_hit = false;

        if start == 0 {
            if let Some(init) = init {
                ligature_present = init.ligature;
                left_hit = init.ligature && init.left_hit;
                out.extend(init.raw.iter().copied().map(|code| init_glyph(code, start)));
            } else if let Some(code) = cur_l {
                out.push(raw_glyph(code, j));
            }
        } else if let Some(code) = cur_l {
            out.push(raw_glyph(code, j));
        }

        let set_right = |j: usize, stack: &[LigItem], hchar: Option<u32>| {
            let right = stack
                .last()
                .map(|p| p.code)
                .or_else(|| if j < n { Some(chars[j + 1]) } else { bchar });
            let rh = if stack.is_empty() && breaks.get(j).copied().unwrap_or(false) {
                hchar
            } else {
                None
            };
            (right, rh)
        };
        (cur_r, cur_rh) = set_right(j, &lig_stack, hchar);

        // `wrap_lig`: replace the raw characters accumulated since `cur_q`
        // by the ligature node that owns them.
        let wrap = |out: &mut Vec<ShapedItem>,
                    lig_start: usize,
                    code: Option<u32>,
                    present: &mut bool,
                    left_hit: &mut bool,
                    right_hit: &mut bool,
                    hit_right: bool,
                    at: usize,
                    stack_empty: bool| {
            if !*present {
                return;
            }
            let mut source = Vec::new();
            let mut prefix = Vec::new();
            let mut spans = Vec::new();
            for item in &out[lig_start..] {
                if let ShapedItem::Glyph {
                    prefix: item_prefix,
                    source: item_source,
                    span,
                    ..
                } = item
                {
                    prefix.extend_from_slice(item_prefix);
                    source.extend_from_slice(item_source);
                    spans.push(*span);
                }
            }
            source.sort_unstable();
            source.dedup();
            let span = if spans.is_empty() {
                (at, at)
            } else {
                (
                    spans.iter().map(|s| s.0).min().unwrap_or(at),
                    spans.iter().map(|s| s.1).max().unwrap_or(at),
                )
            };
            out.truncate(lig_start);
            if let Some(code) = code {
                out.push(ShapedItem::Glyph {
                    code,
                    prefix,
                    source,
                    span,
                    ligature: true,
                    left_hit: core::mem::take(left_hit),
                    right_hit: hit_right && stack_empty && core::mem::take(right_hit),
                });
            }
            *present = false;
        };

        // Valid TFM programs make progress, but a hostile cyclic program must
        // not hold the engine forever.
        let mut fuel = chars.len().saturating_add(1).saturating_mul(4096);
        while fuel > 0 {
            fuel -= 1;
            let lookup = |right: u32| match cur_l {
                Some(left) => self.lig_kern(left, right),
                None => self.left_boundary_lig_kern(right),
            };

            // §903 searches the program for the candidate hyphen first. A hit
            // records the break, removes the hypothetical hyphen, and restarts
            // at the same cursor with the real right character.
            if let Some(test) = cur_rh {
                if lookup(test).is_some() {
                    hyphen_passed.get_or_insert(j);
                    hchar = None;
                    cur_rh = None;
                    continue;
                }
                cur_rh = None;
            }

            let command = cur_r.and_then(lookup);
            if command.is_none() {
                wrap(
                    &mut out,
                    lig_start,
                    cur_l,
                    &mut ligature_present,
                    &mut left_hit,
                    &mut right_hit,
                    true,
                    j,
                    lig_stack.is_empty(),
                );
                if let Some(item) = lig_stack.pop() {
                    lig_start = out.len();
                    cur_l = Some(item.code);
                    ligature_present = true;
                    if let Some((code, at)) = item.raw {
                        out.push(raw_glyph(code, at));
                        j += 1;
                    }
                    (cur_r, cur_rh) = set_right(j, &lig_stack, hchar);
                    continue;
                }
                break;
            }

            // If the real right character has a command but the hypothetical
            // hyphen did not, the translation would still differ.
            if hchar.is_some() && breaks.get(j).copied().unwrap_or(false) {
                hyphen_passed.get_or_insert(j);
                hchar = None;
            }

            match command.unwrap() {
                LigKern::Kern(width) => {
                    // A kern ends this cut prefix.
                    wrap(
                        &mut out,
                        lig_start,
                        cur_l,
                        &mut ligature_present,
                        &mut left_hit,
                        &mut right_hit,
                        true,
                        j,
                        lig_stack.is_empty(),
                    );
                    out.push(ShapedItem::Kern { width, at: j + 1 });
                    if let Some(item) = lig_stack.pop() {
                        lig_start = out.len();
                        cur_l = Some(item.code);
                        ligature_present = true;
                        if let Some((code, at)) = item.raw {
                            out.push(raw_glyph(code, at));
                            j += 1;
                        }
                        (cur_r, cur_rh) = set_right(j, &lig_stack, hchar);
                        continue;
                    }
                    break;
                }
                LigKern::Ligature { code, op } => {
                    if cur_l.is_none() {
                        left_hit = true;
                    }
                    if j == n && lig_stack.is_empty() {
                        right_hit = true;
                    }
                    match op {
                        1 | 5 => {
                            cur_l = Some(code);
                            ligature_present = true;
                        }
                        2 | 6 => {
                            cur_r = Some(code);
                            if let Some(item) = lig_stack.last_mut() {
                                item.code = code;
                            } else {
                                let raw = if j == n {
                                    None
                                } else {
                                    Some((chars[j + 1], j + 1))
                                };
                                lig_stack.push(LigItem { code, raw });
                            }
                        }
                        3 => {
                            lig_stack.push(LigItem { code, raw: None });
                            cur_r = Some(code);
                        }
                        7 | 11 => {
                            wrap(
                                &mut out,
                                lig_start,
                                cur_l,
                                &mut ligature_present,
                                &mut left_hit,
                                &mut right_hit,
                                false,
                                j,
                                lig_stack.is_empty(),
                            );
                            lig_start = out.len();
                            cur_l = Some(code);
                            ligature_present = true;
                        }
                        _ => {
                            cur_l = Some(code);
                            ligature_present = true;
                            if let Some(item) = lig_stack.pop() {
                                if let Some((raw, at)) = item.raw {
                                    out.push(raw_glyph(raw, at));
                                    j += 1;
                                }
                                (cur_r, cur_rh) = set_right(j, &lig_stack, hchar);
                            } else if j == n {
                                break;
                            } else {
                                if let Some(right) = cur_r {
                                    out.push(raw_glyph(right, j + 1));
                                }
                                j += 1;
                                (cur_r, cur_rh) = set_right(j, &lig_stack, hchar);
                            }
                        }
                    }
                    if op > 4 && op != 7 {
                        wrap(
                            &mut out,
                            lig_start,
                            cur_l,
                            &mut ligature_present,
                            &mut left_hit,
                            &mut right_hit,
                            true,
                            j,
                            lig_stack.is_empty(),
                        );
                        if let Some(item) = lig_stack.pop() {
                            lig_start = out.len();
                            cur_l = Some(item.code);
                            ligature_present = true;
                            if let Some((raw, at)) = item.raw {
                                out.push(raw_glyph(raw, at));
                                j += 1;
                            }
                            (cur_r, cur_rh) = set_right(j, &lig_stack, hchar);
                            continue;
                        }
                        break;
                    }
                }
            }
        }

        Reconstituted {
            items: out,
            last: j,
            hyphen_passed,
        }
    }

    /// The common subset of §545: kerns and op-zero ligatures.
    ///
    /// An op-zero ligature consumes both adjacent glyphs, leaves its
    /// replacement under the cursor, and can therefore be combined with the
    /// next input glyph (the `ff` then `i` cascade is the familiar example).
    /// No insertion or retained operand is possible, so a single forward pass
    /// produces the same work list as the general splice-based interpreter.
    fn shape_text_simple(
        &self,
        chars: &[u32],
        left_boundary: bool,
        right_boundary: bool,
    ) -> Vec<ShapedItem> {
        #[derive(Clone)]
        struct Glyph {
            code: u32,
            source: Vec<usize>,
            span: (usize, usize),
            ligature: bool,
            left_hit: bool,
            right_hit: bool,
        }

        enum Item {
            Glyph(Glyph),
            Kern { width: Scaled, at: usize },
        }

        fn raw(code: u32, at: usize) -> Glyph {
            Glyph {
                code,
                source: Vec::new(),
                span: (at, at + 1),
                ligature: false,
                left_hit: false,
                right_hit: false,
            }
        }

        fn append_source(g: &Glyph, out: &mut Vec<usize>) {
            if g.ligature {
                out.extend_from_slice(&g.source);
            } else if g.span.1 == g.span.0 + 1 {
                out.push(g.span.0);
            }
        }

        fn combine(left: Glyph, right: Glyph, code: u32) -> Glyph {
            let mut source =
                Vec::with_capacity(left.source.len().max(1) + right.source.len().max(1));
            append_source(&left, &mut source);
            append_source(&right, &mut source);
            Glyph {
                code,
                source,
                span: (left.span.0.min(right.span.0), left.span.1.max(right.span.1)),
                ligature: true,
                left_hit: left.left_hit || right.left_hit,
                right_hit: left.right_hit || right.right_hit,
            }
        }

        let Some((&first, rest)) = chars.split_first() else {
            return Vec::new();
        };
        let mut work = Vec::with_capacity(chars.len() + 2);
        let first = raw(first, 0);
        if left_boundary {
            match self.left_boundary_lig_kern(first.code) {
                Some(LigKern::Kern(width)) => {
                    work.push(Item::Kern { width, at: 0 });
                    work.push(Item::Glyph(first));
                }
                Some(LigKern::Ligature { code, op: 0 }) => {
                    let mut replacement = first;
                    replacement.code = code;
                    replacement.source.push(0);
                    replacement.ligature = true;
                    replacement.left_hit = true;
                    work.push(Item::Glyph(replacement));
                }
                // `complex_ligatures` routes every nonzero operator to the
                // general interpreter.  Keep malformed or disabled commands
                // inert rather than making this optimization a crash path.
                _ => work.push(Item::Glyph(first)),
            }
        } else {
            work.push(Item::Glyph(first));
        }

        for (offset, &code) in rest.iter().enumerate() {
            let right = raw(code, offset + 1);
            let command = match work.last() {
                Some(Item::Glyph(left)) => self.lig_kern(left.code, right.code),
                _ => None,
            };
            match command {
                Some(LigKern::Kern(width)) => {
                    work.push(Item::Kern {
                        width,
                        at: offset + 1,
                    });
                    work.push(Item::Glyph(right));
                }
                Some(LigKern::Ligature { code, op: 0 }) => {
                    let Some(Item::Glyph(left)) = work.pop() else {
                        unreachable!("a ligature command requires its left glyph")
                    };
                    work.push(Item::Glyph(combine(left, right, code)));
                }
                _ => work.push(Item::Glyph(right)),
            }
        }

        if right_boundary {
            let command = match work.last() {
                Some(Item::Glyph(left)) => self
                    .boundary_char()
                    .and_then(|boundary| self.lig_kern(left.code, boundary)),
                _ => None,
            };
            match command {
                Some(LigKern::Kern(width)) => work.push(Item::Kern {
                    width,
                    at: chars.len(),
                }),
                Some(LigKern::Ligature { code, op: 0 }) => {
                    let Some(Item::Glyph(mut left)) = work.pop() else {
                        unreachable!("a boundary ligature requires its left glyph")
                    };
                    left.code = code;
                    if !left.ligature {
                        left.source.push(left.span.0);
                    }
                    left.ligature = true;
                    left.right_hit = true;
                    work.push(Item::Glyph(left));
                }
                _ => {}
            }
        }

        work.into_iter()
            .map(|item| match item {
                Item::Glyph(g) => ShapedItem::Glyph {
                    code: g.code,
                    prefix: Vec::new(),
                    source: g.source,
                    span: g.span,
                    ligature: g.ligature,
                    left_hit: g.left_hit,
                    right_hit: g.right_hit,
                },
                Item::Kern { width, at } => ShapedItem::Kern { width, at },
            })
            .collect()
    }

    /// Apply §545's ligature programming language to a complete raw character
    /// string. The direct list rewrite mirrors its definition: insert the
    /// replacement, delete operands selected by the two low op bits, then pass
    /// over `op/4` characters to find the next current character.
    pub(crate) fn shape_text(
        &self,
        chars: &[u32],
        left_boundary: bool,
        right_boundary: bool,
    ) -> Vec<ShapedItem> {
        if !self.complex_ligatures {
            return self.shape_text_simple(chars, left_boundary, right_boundary);
        }
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Boundary {
            None,
            Left,
            Right,
        }
        #[derive(Clone)]
        struct Glyph {
            code: u32,
            // An ordinary raw glyph owns exactly `span.0`; keep that implicit
            // so shaping a normal word does not allocate once per character.
            // Generated ligatures use `Some`, including `Some(Vec::new())`
            // when both operands were retained.
            source: Option<Vec<usize>>,
            span: (usize, usize),
            ligature: bool,
            left_hit: bool,
            right_hit: bool,
            boundary: Boundary,
        }
        #[derive(Clone)]
        enum Work {
            Glyph(Glyph),
            Kern { width: Scaled, at: usize },
        }

        fn append_source(glyph: &Glyph, source: &mut Vec<usize>) {
            if let Some(explicit) = &glyph.source {
                source.extend_from_slice(explicit);
            } else if glyph.boundary == Boundary::None && glyph.span.1 == glyph.span.0 + 1 {
                source.push(glyph.span.0);
            }
        }

        let mut work = Vec::with_capacity(chars.len() + 2);
        if left_boundary && self.left_boundary_start().is_some() {
            work.push(Work::Glyph(Glyph {
                code: 256,
                source: None,
                span: (0, 0),
                ligature: false,
                left_hit: false,
                right_hit: false,
                boundary: Boundary::Left,
            }));
        }
        for (i, &code) in chars.iter().enumerate() {
            work.push(Work::Glyph(Glyph {
                code,
                source: None,
                span: (i, i + 1),
                ligature: false,
                left_hit: false,
                right_hit: false,
                boundary: Boundary::None,
            }));
        }
        if right_boundary {
            if let Some(code) = self.boundary_char() {
                work.push(Work::Glyph(Glyph {
                    code,
                    source: None,
                    span: (chars.len(), chars.len()),
                    ligature: false,
                    left_hit: false,
                    right_hit: false,
                    boundary: Boundary::Right,
                }));
            }
        }

        let mut cur = 0usize;
        // Valid programs make progress, but TFM input is untrusted and can
        // deliberately encode a ligature cycle. TeX checks interrupts here;
        // this non-interactive engine instead imposes a generous finite bound.
        let mut fuel = work.len().saturating_add(1).saturating_mul(4096);
        while cur + 1 < work.len() && fuel > 0 {
            fuel -= 1;
            let (left, right) = match (&work[cur], &work[cur + 1]) {
                (Work::Glyph(left), Work::Glyph(right)) => (left.clone(), right.clone()),
                _ => {
                    cur += 1;
                    continue;
                }
            };
            if left.boundary == Boundary::Right || right.boundary == Boundary::Left {
                cur += 1;
                continue;
            }
            let command = if left.boundary == Boundary::Left {
                self.left_boundary_lig_kern(right.code)
            } else {
                self.lig_kern(left.code, right.code)
            };
            match command {
                Some(LigKern::Kern(width)) => {
                    let at = left.span.1.min(chars.len());
                    work.insert(cur + 1, Work::Kern { width, at });
                    cur += 2;
                }
                Some(LigKern::Ligature { code, op }) => {
                    let a = usize::from(op / 4);
                    let keep_left = op & 2 != 0;
                    let keep_right = op & 1 != 0;
                    // §545 permits only a<=b+c. Ignore malformed operations
                    // rather than allowing an out-of-range cursor rewrite.
                    if a > usize::from(keep_left) + usize::from(keep_right) {
                        cur += 1;
                        continue;
                    }
                    let mut source = Vec::new();
                    let mut spans = Vec::new();
                    if !keep_left {
                        append_source(&left, &mut source);
                        spans.push(left.span);
                    }
                    if !keep_right {
                        append_source(&right, &mut source);
                        spans.push(right.span);
                    }
                    source.sort_unstable();
                    source.dedup();
                    let span = if spans.is_empty() {
                        let at = left.span.1.min(chars.len());
                        (at, at)
                    } else {
                        let start = spans.iter().map(|s| s.0).min().unwrap_or(0);
                        let end = spans.iter().map(|s| s.1).max().unwrap_or(start);
                        (start, end)
                    };
                    let replacement = Work::Glyph(Glyph {
                        code,
                        source: Some(source),
                        span,
                        ligature: true,
                        // A boundary subtype belongs to the ligature that
                        // consumed/hit it. Retained operand glyphs keep their
                        // own subtype; propagating it to every later inserted
                        // ligature prints a spurious `|` down the whole run.
                        left_hit: left.boundary == Boundary::Left
                            || (!keep_left && left.left_hit)
                            || (!keep_right && right.left_hit),
                        right_hit: right.boundary == Boundary::Right
                            || (!keep_left && left.right_hit)
                            || (!keep_right && right.right_hit),
                        boundary: Boundary::None,
                    });
                    let mut segment = Vec::with_capacity(3);
                    if keep_left {
                        segment.push(Work::Glyph(left));
                    }
                    segment.push(replacement);
                    if keep_right {
                        segment.push(Work::Glyph(right));
                    }
                    work.splice(cur..=cur + 1, segment);
                    cur = cur.saturating_add(a);
                }
                None => cur += 1,
            }
        }

        work.into_iter()
            .filter_map(|item| match item {
                Work::Glyph(g) if g.boundary != Boundary::None => None,
                Work::Glyph(g) => Some(ShapedItem::Glyph {
                    code: g.code,
                    prefix: Vec::new(),
                    source: g.source.unwrap_or_default(),
                    span: g.span,
                    ligature: g.ligature,
                    left_hit: g.left_hit,
                    right_hit: g.right_hit,
                }),
                Work::Kern { width, at } => Some(ShapedItem::Kern { width, at }),
            })
            .collect()
    }

    /// How many `\fontdimen` parameters the font actually has — TeX's
    /// `font_params`. §1195 refuses to typeset math unless family 2 has at
    /// least `total_mathsy_params` of them and family 3 at least
    /// `total_mathex_params`, and the count is the only way to ask.
    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    pub fn param(&self, n: usize) -> Scaled {
        if n == 0 {
            return Scaled::ZERO;
        }
        self.params.get(n - 1).copied().unwrap_or(Scaled::ZERO)
    }

    /// `\fontdimen2` — the natural interword space.
    pub fn space(&self) -> Scaled {
        self.param(2)
    }
    /// `\fontdimen3` — how much that space may stretch.
    pub fn space_stretch(&self) -> Scaled {
        self.param(3)
    }
    /// `\fontdimen4` — how much it may shrink.
    pub fn space_shrink(&self) -> Scaled {
        self.param(4)
    }
    /// `\fontdimen6` — one em.
    pub fn quad(&self) -> Scaled {
        self.param(6)
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_input_without_panicking() {
        assert_eq!(TfmFont::parse(&[], None), Err(TfmError::TooShort));
        assert_eq!(TfmFont::parse(&[0, 1], None), Err(TfmError::TooShort));
        for n in 0..64 {
            let junk: Vec<u8> = (0..n).map(|i| (i * 37 % 251) as u8).collect();
            let _ = TfmFont::parse(&junk, None); // must not panic
        }
    }

    #[test]
    fn rejects_a_file_whose_lengths_do_not_add_up() {
        // lf claims a size the sub-table lengths contradict; TeX refuses these
        // rather than reading past the end.
        let mut b = alloc::vec![0u8; 128];
        b[0..2].copy_from_slice(&99u16.to_be_bytes()); // lf
        b[2..4].copy_from_slice(&2u16.to_be_bytes()); // lh
        assert_eq!(TfmFont::parse(&b, None), Err(TfmError::Inconsistent));
    }

    #[test]
    fn rejects_an_impossible_character_range() {
        let mut b = alloc::vec![0u8; 128];
        b[0..2].copy_from_slice(&32u16.to_be_bytes());
        b[2..4].copy_from_slice(&2u16.to_be_bytes());
        b[4..6].copy_from_slice(&200u16.to_be_bytes()); // bc
        b[6..8].copy_from_slice(&10u16.to_be_bytes()); // ec < bc - 1
        assert_eq!(TfmFont::parse(&b, None), Err(TfmError::BadRange));
    }

    /// Widths as the reference engine emits them, taken from `dvitype` output
    /// of a real `tex` run:
    ///
    /// ```text
    /// setchar72  h:=0+491521          (H at 10pt)
    /// setchar101 h:=491521+291271     (e at 10pt)
    /// setchar108 h:=782792+182045     (l at 10pt)
    /// setchar111 h:=1146882+327681    (o at 10pt)
    /// setchar72  h:=0+589825          (H at 12pt)
    /// setchar101 h:=589825+349526     (e at 12pt)
    /// ```
    ///
    /// Pinned because these are the numbers that make our DVI comparable to
    /// pdftex's byte for byte. Note `tftopl`'s decimal display rounds — reading
    /// 0.444446 and multiplying gives 291272, one sp adrift. The engine, not the
    /// pretty-printer, is the oracle.
    #[test]
    fn ligatures_and_kerns_match_the_reference_engine() {
        let Ok(bytes) = std::fs::read(cmr10_path()) else {
            return;
        };
        let f = TfmFont::parse(&bytes, None).expect("cmr10 parses");

        // The ligatures that make "office" three glyphs instead of six.
        // Character codes are cmr10's own: 11 is `ff`, 12 `fi`, 14 `ffi`.
        assert_eq!(
            f.lig_kern(b'f' as u32, b'f' as u32),
            Some(LigKern::Ligature { code: 11, op: 0 }),
            "ff"
        );
        assert_eq!(
            f.lig_kern(b'f' as u32, b'i' as u32),
            Some(LigKern::Ligature { code: 12, op: 0 }),
            "fi"
        );
        // And the cascade: the `ff` ligature pairs with `i` to give `ffi`.
        assert_eq!(
            f.lig_kern(11, b'i' as u32),
            Some(LigKern::Ligature { code: 14, op: 0 }),
            "ffi"
        );

        // Kerns are negative: the pair is tucked together. `\showlists` in
        // TeX Live reports exactly these values for cmr10 at its design size.
        let Some(LigKern::Kern(va)) = f.lig_kern(b'V' as u32, b'A' as u32) else {
            panic!("V A must kern");
        };
        assert!(va.0 < 0, "V A tucks together, got {va:?}");
        let Some(LigKern::Kern(to)) = f.lig_kern(b'T' as u32, b'o' as u32) else {
            panic!("T o must kern");
        };
        assert!(to.0 < 0, "T o tucks together, got {to:?}");

        // Most pairs have neither.
        assert_eq!(f.lig_kern(b'x' as u32, b'y' as u32), None);
        // And a character with no program at all must not be searched.
        assert_eq!(f.lig_kern(b'z' as u32, b'z' as u32), None);
    }

    #[test]
    fn long_lig_kern_program_reaches_its_final_entry() {
        let path: alloc::string::String = std::process::Command::new("kpsewhich")
            .arg("ec-lmbx10.tfm")
            .output()
            .ok()
            .map(|o| {
                alloc::string::String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .into()
            })
            .unwrap_or_default();
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let f = TfmFont::parse(&bytes, Some(Scaled(717_619))).expect("ec-lmbx10 parses");
        assert_eq!(
            f.lig_kern(b'y' as u32, b'.' as u32),
            Some(LigKern::Kern(Scaled(-68_772)))
        );
    }

    /// A malformed program must terminate rather than spin.
    /// The reverse of the ligature program: cmr10's `ffi` is `ff` and `i`, and
    /// its `ff` is `f` and `f`. Hyphenation needs it, because the patterns are
    /// matched against the letters and `office` is `o` `ffi` `c` `e`.
    #[test]
    fn a_ligature_says_what_it_was_made_from() {
        let path = cmr10_path();
        if path.is_empty() {
            return;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let Ok(mut f) = TfmFont::parse(&bytes, None) else {
            return;
        };
        assert_eq!(f.unligature(11), Some((b'f' as u32, b'f' as u32)));
        assert_eq!(f.unligature(12), Some((b'f' as u32, b'i' as u32)));
        assert_eq!(f.unligature(13), Some((b'f' as u32, b'l' as u32)));
        assert_eq!(f.unligature(14), Some((11, b'i' as u32)));
        assert_eq!(f.unligature(15), Some((11, b'l' as u32)));
        // An ordinary letter is not a ligature.
        assert_eq!(f.unligature(b'o' as u32), None);
        f.clear_char_tag(b'f' as u32);
        assert_eq!(f.unligature(11), None);
        assert_eq!(f.unligature(12), None);
        assert_eq!(f.unligature(14), Some((11, b'i' as u32)));
    }

    #[test]
    fn lig_kern_on_junk_terminates() {
        for seed in 0..8u32 {
            let junk: Vec<u8> = (0..600)
                .map(|i| ((i * 37 + seed * 11) % 251) as u8)
                .collect();
            if let Ok(f) = TfmFont::parse(&junk, None) {
                for a in 0..40u32 {
                    let _ = f.lig_kern(a, a + 1);
                }
            }
        }
    }

    #[test]
    fn widths_match_the_reference_engine() {
        let Ok(bytes) = std::fs::read(cmr10_path()) else {
            return;
        };
        let f = TfmFont::parse(&bytes, None).expect("cmr10 parses");
        assert_eq!(f.width(b'H' as u32), Scaled(491_521));
        assert_eq!(f.width(b'e' as u32), Scaled(291_271));
        assert_eq!(f.width(b'l' as u32), Scaled(182_045));
        assert_eq!(f.width(b'o' as u32), Scaled(327_681));

        let at12 = TfmFont::parse(&bytes, Some(Scaled::from_points(12))).expect("parses at 12pt");
        assert_eq!(at12.width(b'H' as u32), Scaled(589_825));
        assert_eq!(at12.width(b'e' as u32), Scaled(349_526));
    }

    #[test]
    fn zero_parameter_tfm_is_padded_to_texs_seven_basics() {
        let path = std::process::Command::new("kpsewhich")
            .arg("MnSymbolC10.tfm")
            .output()
            .ok()
            .map(|o| {
                alloc::string::String::from(
                    alloc::string::String::from_utf8_lossy(&o.stdout).trim(),
                )
            })
            .unwrap_or_default();
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let f = TfmFont::parse(&bytes, None).expect("MnSymbolC10 parses");

        assert_eq!(f.param_count(), 7);
        assert_eq!(f.param(6), Scaled::ZERO);
    }

    #[test]
    fn letterspacing_matches_pdftex_rounding_and_limits() {
        let Ok(bytes) = std::fs::read(cmr10_path()) else {
            return;
        };
        let f = TfmFont::parse(&bytes, None).expect("cmr10 parses");
        let tracked = f.letterspaced(100);
        assert_eq!(tracked.width(b'A' as u32), Scaled(557_057));
        assert_eq!(tracked.param(2), f.param(2));
        assert_eq!(
            tracked.lig_kern(b'f' as u32, b'i' as u32),
            f.lig_kern(b'f' as u32, b'i' as u32)
        );
        assert_eq!(f.letterspaced(1001).width(b'A' as u32), Scaled(1_146_882));
        assert_eq!(f.letterspaced(-1001).width(b'A' as u32), Scaled(-163_840));
        assert!(
            !tracked.has_char(255),
            "letterspacing must not create missing glyphs"
        );
    }

    #[test]
    fn pdf_no_ligatures_disables_character_ligature_and_kern_programs() {
        let Ok(bytes) = std::fs::read(cmr10_path()) else {
            return;
        };
        let mut f = TfmFont::parse(&bytes, None).expect("cmr10 parses");
        assert!(matches!(
            f.lig_kern(b'f' as u32, b'i' as u32),
            Some(LigKern::Ligature { .. })
        ));
        assert!(matches!(
            f.lig_kern(b'T' as u32, b'o' as u32),
            Some(LigKern::Kern(_))
        ));
        f.set_no_ligatures(true);
        assert_eq!(f.lig_kern(b'f' as u32, b'i' as u32), None);
        assert_eq!(f.lig_kern(b'T' as u32, b'o' as u32), None);
    }

    #[test]
    fn expansion_remeasures_a_kern_after_its_neighbor_becomes_a_ligature() {
        let Ok(bytes) = std::fs::read(ptmr8t_path()) else {
            return;
        };
        let mut f = TfmFont::parse(&bytes, None).expect("ptmr8t parses");
        let retained = match f.lig_kern(b'r' as u32, b'-' as u32) {
            Some(LigKern::Kern(width)) => width,
            other => panic!("ptmr8t r/hyphen kern missing: {other:?}"),
        };
        // T1 slot 22 is the em dash. TeX builds the r/hyphen kern first, then
        // turns `---` into this ligature without rebuilding the preceding
        // kern. pdfTeX's expansion pass does re-query the new r/em-dash pair.
        assert!(!matches!(
            f.lig_kern(b'r' as u32, 22),
            Some(LigKern::Kern(_))
        ));
        f.set_expansion(20, 20, 1, true);
        f.set_expansion_code(b'r' as u32, 700);
        assert_eq!(
            f.expansion_kern_capacity(b'r' as u32, 22, retained),
            (
                round_xn_over_d(Scaled::ZERO - retained, 700, 1000),
                round_xn_over_d(retained, 700, 1000),
            )
        );
        assert_eq!(
            f.expansion_kern_delta(b'r' as u32, 22, retained, 1000),
            Scaled::ZERO - retained
        );
    }

    /// Locate cmr10 the way TeX would; the test skips if no distribution.
    fn cmr10_path() -> alloc::string::String {
        std::process::Command::new("kpsewhich")
            .arg("cmr10.tfm")
            .output()
            .ok()
            .map(|o| {
                alloc::string::String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .into()
            })
            .unwrap_or_default()
    }

    fn ptmr8t_path() -> alloc::string::String {
        std::process::Command::new("kpsewhich")
            .arg("ptmr8t.tfm")
            .output()
            .ok()
            .map(|o| {
                alloc::string::String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .into()
            })
            .unwrap_or_default()
    }

    #[test]
    fn fix_word_conversion_scales_by_the_at_size() {
        // 0.5 in fix_word is 1<<19; at 10pt that is 5pt.
        let half = 1u32 << 19;
        assert_eq!(
            fix_word_to_scaled(half, Scaled::from_points(10)),
            Scaled::from_points(5)
        );
        // The same fraction at 20pt is 10pt: widths follow the at-size.
        assert_eq!(
            fix_word_to_scaled(half, Scaled::from_points(20)),
            Scaled::from_points(10)
        );
    }

    #[test]
    fn unset_parameters_read_as_zero() {
        // A font with no \fontdimen beyond the basics must not panic when the
        // engine asks for a math parameter.
        let f = TfmFont {
            checksum: 0,
            design_size: Scaled::from_points(10),
            at_size: Scaled::from_points(10),
            tex_text_coding: false,
            bc: 0,
            ec: 0,
            widths: alloc::vec![Scaled::ZERO],
            heights: alloc::vec![Scaled::ZERO],
            depths: alloc::vec![Scaled::ZERO],
            italics: alloc::vec![Scaled::ZERO],
            chars: alloc::vec![CharInfo::default()],
            lig_kern: alloc::vec![],
            complex_ligatures: false,
            no_ligatures: false,
            unligatures: alloc::vec![],
            lp: alloc::collections::BTreeMap::new(),
            rp: alloc::collections::BTreeMap::new(),
            ef: alloc::collections::BTreeMap::new(),
            expansion: None,
            expanded_protrusion_owners: [None, None],
            expanded_protrusion_shared: [false, false],
            letterspace_base: None,
            micro_codes: core::array::from_fn(|_| alloc::collections::BTreeMap::new()),
            exten: alloc::vec![],
            skew_char: -1,
            kerns: alloc::vec![],
            params: alloc::vec![],
        };
        assert_eq!(f.param(22), Scaled::ZERO);
        assert_eq!(f.param(0), Scaled::ZERO);
        assert_eq!(f.space(), Scaled::ZERO);
    }
}
