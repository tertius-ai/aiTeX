//! Category codes.
//!
//! Every character TeX reads carries a *category code* deciding what it means:
//! `\` is escape, `{` is begin-group, `$` is math shift, and so on. Crucially
//! **the table is mutable at runtime** — `\catcode`\``\@=11` is the standard
//! idiom that makes `@` a letter inside package code.
//!
//! That single fact is why TeX cannot be parsed by any generated grammar, and
//! why docs/history/PLAN.md decision 9 rules out a parser generator: expansion can change
//! what a character *means*, so you cannot know where token *n+1* begins
//! without having fully processed tokens *1..n*.

/// The sixteen category codes, `tex.web` §207.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[repr(u8)]
pub enum CatCode {
    /// `\` — starts a control sequence.
    Escape = 0,
    /// `{` — begin group.
    BeginGroup = 1,
    /// `}` — end group.
    EndGroup = 2,
    /// `$` — math shift.
    MathShift = 3,
    /// `&` — alignment tab.
    AlignTab = 4,
    /// Newline. Ends a line; a *second* one makes `\par`.
    EndOfLine = 5,
    /// `#` — macro parameter.
    Parameter = 6,
    /// `^` — superscript, and the `^^` escape.
    Superscript = 7,
    /// `_` — subscript.
    Subscript = 8,
    /// Ignored completely (NUL by default).
    Ignored = 9,
    /// Space. Runs of spaces collapse to one.
    Space = 10,
    /// A letter. Only these may appear in multi-character control sequences.
    Letter = 11,
    /// Anything printable that is not a letter.
    Other = 12,
    /// `~` — a single character that acts as a control sequence.
    Active = 13,
    /// `%` — comment to end of line.
    Comment = 14,
    /// Invalid input (DEL by default); TeX complains if it sees one.
    Invalid = 15,
}

impl CatCode {
    pub fn from_u8(n: u8) -> Option<CatCode> {
        use CatCode::*;
        Some(match n {
            0 => Escape,
            1 => BeginGroup,
            2 => EndGroup,
            3 => MathShift,
            4 => AlignTab,
            5 => EndOfLine,
            6 => Parameter,
            7 => Superscript,
            8 => Subscript,
            9 => Ignored,
            10 => Space,
            11 => Letter,
            12 => Other,
            13 => Active,
            14 => Comment,
            15 => Invalid,
            _ => return None,
        })
    }
}

/// Category code assignments for the whole Unicode range.
///
/// Stored as a flat table over Latin-1 plus a sorted list of exceptions above
/// it: real documents change a handful of codes (`@`, `~`, `_`), never
/// thousands, so a full 0x110000-entry table would waste megabytes to no end.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CatCodeTable {
    // serde impls arrays only up to length 32, so the 256-entry table goes
    // through a small shim that serializes it as a sequence (any length) and
    // collects it back. No extra dependency.
    #[serde(with = "low_table_serde")]
    low: [CatCode; 256],
    high: alloc::vec::Vec<(u32, CatCode)>,
}

mod low_table_serde {
    use super::CatCode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(a: &[CatCode; 256], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(a.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[CatCode; 256], D::Error> {
        let v: alloc::vec::Vec<CatCode> = Deserialize::deserialize(d)?;
        v.try_into().map_err(|_| {
            serde::de::Error::custom("catcode table must have exactly 256 low entries")
        })
    }
}

impl CatCodeTable {
    /// TeX's *initial* state, before any format is loaded (`tex.web` §232).
    ///
    /// Deliberately spartan: only `\`, letters, `%`, and whitespace are special.
    /// `{`, `}`, `$`, `&`, `#`, `^`, `_`, `~` are all just [`CatCode::Other`]
    /// here — plain.tex and latex.ltx assign those themselves, which is exactly
    /// why `trip.tex` opens by setting them by hand.
    pub fn initex() -> Self {
        let mut low = [CatCode::Other; 256];
        // Unicode-native, as XeTeX and LuaTeX are: any alphabetic character is
        // a letter, not just ASCII A–Z. TeX82 could not see these at all, and
        // pdfTeX treats them as bytes, but an engine that claims Unicode
        // support must let `\héllo` be a control word.
        //
        // This covers Latin-1 too (é is U+00E9 = 233, inside this table), which
        // is exactly the case a naive `n < 256 -> Other` rule gets wrong.
        for (i, slot) in low.iter_mut().enumerate() {
            if char::from_u32(i as u32).is_some_and(char::is_alphabetic) {
                *slot = CatCode::Letter;
            }
        }
        low[b'\\' as usize] = CatCode::Escape;
        // Only `\r` (13), tex.web's default `\endlinechar`, gets EndOfLine.
        // Character 10 (line feed) is `Other` by the same default -- it is
        // this table's job to say what catcode a *character* has, and
        // `tex.web` never gives line feed one of its own. Physical
        // end-of-line detection while reading a file is a different concern,
        // handled directly on the raw byte in `token.rs`, not through this
        // table: giving char 10 `EndOfLine` here made every *ordinary*
        // appearance of it -- `^^J`, `\char10`, a string built with it --
        // behave as a line break instead of a character. expl3's line
        // wrapping in l3iow uses `^^J` as a literal macro-parameter
        // delimiter, and it was silently rewritten to a space before the
        // macro ever saw it, which is why the parameter text stored for
        // `\__iow_wrap_fix_newline:w` was a space and no call could ever
        // match it.
        low[b'\r' as usize] = CatCode::EndOfLine;
        low[b'%' as usize] = CatCode::Comment;
        low[b' ' as usize] = CatCode::Space;
        low[0] = CatCode::Ignored;
        low[127] = CatCode::Invalid;
        Self {
            low,
            high: alloc::vec::Vec::new(),
        }
    }

    /// The conventional plain-TeX / LaTeX assignments, as `\dospecials` leaves
    /// them. This is what a document actually runs under.
    pub fn plain() -> Self {
        let mut t = Self::initex();
        t.set('{', CatCode::BeginGroup);
        t.set('}', CatCode::EndGroup);
        t.set('$', CatCode::MathShift);
        t.set('&', CatCode::AlignTab);
        t.set('#', CatCode::Parameter);
        t.set('^', CatCode::Superscript);
        t.set('_', CatCode::Subscript);
        t.set('~', CatCode::Active);
        t.set('\t', CatCode::Space);
        t
    }

    pub fn get(&self, c: char) -> CatCode {
        let n = c as u32;
        if n < 256 {
            self.low[n as usize]
        } else {
            match self.high.binary_search_by_key(&n, |&(k, _)| k) {
                Ok(i) => self.high[i].1,
                // Same rule as the low table: alphabetic means letter. CJK,
                // Greek and Cyrillic are letters; punctuation and symbols
                // (U+2028, arrows, emoji) are not.
                Err(_) => {
                    if c.is_alphabetic() {
                        CatCode::Letter
                    } else {
                        CatCode::Other
                    }
                }
            }
        }
    }

    pub fn set(&mut self, c: char, code: CatCode) {
        let n = c as u32;
        if n < 256 {
            self.low[n as usize] = code;
        } else {
            match self.high.binary_search_by_key(&n, |&(k, _)| k) {
                Ok(i) => self.high[i].1 = code,
                Err(i) => self.high.insert(i, (n, code)),
            }
        }
    }
}

impl Default for CatCodeTable {
    fn default() -> Self {
        // §232's table, not plain's. `{`, `}`, `$`, `&`, `#`, `^`, `_` and `~`
        // are *other* until a format says otherwise, and `\t` is not a space:
        // plain.tex is what makes them what everyone thinks they are, and an
        // engine that presets them answers `\ifnum\catcode`\~=13` wrongly and
        // restores a category code it never saved to the wrong value.
        Self::initex()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initex_leaves_braces_as_ordinary_characters() {
        // The detail `trip.tex` depends on: before a format is loaded, `{` is
        // not a grouping character and must be assigned explicitly.
        let t = CatCodeTable::initex();
        assert_eq!(t.get('{'), CatCode::Other);
        assert_eq!(t.get('}'), CatCode::Other);
        assert_eq!(t.get('$'), CatCode::Other);
        assert_eq!(t.get('\\'), CatCode::Escape);
        assert_eq!(t.get('%'), CatCode::Comment);
        assert_eq!(t.get('A'), CatCode::Letter);
    }

    #[test]
    fn plain_assigns_the_familiar_specials() {
        let t = CatCodeTable::plain();
        assert_eq!(t.get('{'), CatCode::BeginGroup);
        assert_eq!(t.get('}'), CatCode::EndGroup);
        assert_eq!(t.get('$'), CatCode::MathShift);
        assert_eq!(t.get('&'), CatCode::AlignTab);
        assert_eq!(t.get('#'), CatCode::Parameter);
        assert_eq!(t.get('~'), CatCode::Active);
        assert_eq!(
            t.get('@'),
            CatCode::Other,
            "@ is Other until \\makeatletter"
        );
    }

    /// `\makeatletter` is the single most common runtime catcode change, and
    /// the reason a static grammar cannot describe TeX.
    #[test]
    fn makeatletter_changes_what_a_character_means() {
        let mut t = CatCodeTable::plain();
        assert_eq!(t.get('@'), CatCode::Other);
        t.set('@', CatCode::Letter);
        assert_eq!(t.get('@'), CatCode::Letter);
    }

    /// Unicode-native behaviour: alphabetic is a letter at every code point,
    /// including Latin-1. `é` is U+00E9 = 233, which a naive "below 256 is
    /// ASCII" rule gets wrong.
    #[test]
    fn unicode_letters_are_letters_at_every_code_point() {
        let t = CatCodeTable::plain();
        for c in ['é', 'ü', 'ñ', 'α', 'β', 'Ж', '中', 'あ', 'ℵ'] {
            assert_eq!(t.get(c), CatCode::Letter, "{c:?} should be a letter");
        }
        // Non-alphabetic Unicode stays Other — punctuation and symbols must not
        // silently become part of control words.
        for c in ['—', '§', '→', '€', '\u{2028}'] {
            assert_eq!(t.get(c), CatCode::Other, "{c:?} should be Other");
        }
    }

    #[test]
    fn high_characters_can_be_reassigned_and_stay_sorted() {
        let mut t = CatCodeTable::plain();
        for c in ['\u{2028}', '\u{00e9}', '\u{1000}', '\u{0100}'] {
            t.set(c, CatCode::Active);
        }
        for c in ['\u{2028}', '\u{1000}', '\u{0100}'] {
            assert_eq!(t.get(c), CatCode::Active, "{c:?} should be active");
        }
        assert!(
            t.high.windows(2).all(|w| w[0].0 < w[1].0),
            "exceptions stay sorted"
        );
    }

    #[test]
    fn catcode_numbers_match_texs_own() {
        assert_eq!(CatCode::from_u8(0), Some(CatCode::Escape));
        assert_eq!(CatCode::from_u8(11), Some(CatCode::Letter));
        assert_eq!(CatCode::from_u8(15), Some(CatCode::Invalid));
        assert_eq!(CatCode::from_u8(16), None);
        assert_eq!(CatCode::Letter as u8, 11);
    }
}
