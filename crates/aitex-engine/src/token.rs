//! Tokens and the tokenizer — TeX's "mouth".
//!
//! Turns characters into tokens, consulting a live [`CatCodeTable`] that the
//! document itself can change mid-stream. Faithfulness here is not optional:
//! the state machine, the `^^` notation, and the three states TeX keeps between
//! lines all have observable consequences that `trip.tex` checks.

use crate::catcode::{CatCode, CatCodeTable};
use crate::intern::{Interner, Symbol};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

/// Stack storage for the usual short control word.  Source tokenization sees
/// the same names repeatedly (`\section`, `\par`, ...); allocating a temporary
/// `String` before every interner lookup made the allocator part of the mouth's
/// steady-state cost.  TeX permits arbitrarily long names, so the inline bytes
/// spill to a `String` without imposing a semantic limit.
struct ControlName {
    inline: [u8; 64],
    len: usize,
    spill: Option<String>,
}

impl ControlName {
    fn new() -> Self {
        Self {
            inline: [0; 64],
            len: 0,
            spill: None,
        }
    }

    fn push(&mut self, c: char) {
        if let Some(s) = self.spill.as_mut() {
            s.push(c);
            return;
        }
        let mut encoded = [0u8; 4];
        let bytes = c.encode_utf8(&mut encoded).as_bytes();
        if self.len + bytes.len() <= self.inline.len() {
            self.inline[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
            return;
        }
        let prefix = core::str::from_utf8(&self.inline[..self.len])
            .expect("control-name bytes came from Unicode scalars");
        let mut s = String::with_capacity(self.len + bytes.len() + 16);
        s.push_str(prefix);
        s.push(c);
        self.spill = Some(s);
    }

    fn as_str(&self) -> &str {
        self.spill.as_deref().unwrap_or_else(|| {
            core::str::from_utf8(&self.inline[..self.len])
                .expect("control-name bytes came from Unicode scalars")
        })
    }
}

/// A single token: either a character with its category, or a control sequence,
/// packed into a `u32` exactly as `tex.web` packs a token into one halfword
/// (§289, `cs_token_flag`).
///
/// **Layout.** Bit 31 tags the two cases:
/// * control sequence — bit 31 set, bits 0..31 the interned [`Symbol`] id
///   (so at most `2^31` control sequences, orders of magnitude more than any run
///   defines);
/// * character — bit 31 clear, bits 0..21 the Unicode scalar (`char` needs only
///   21), bits 21..25 the [`CatCode`] (0..16, four bits). A character can never
///   set bit 31 because the largest scalar `0x10FFFF` and largest catcode `15`
///   together occupy only 25 bits.
///
/// It was an `enum { Char(char, CatCode), ControlSequence(Symbol) }` — a clean
/// API but **8 bytes**, because `Symbol` is a full `u32` and leaves no niche for
/// a discriminant. Halving it to 4 halves the bandwidth of every token-list copy
/// (a `latex.ltx` load moves ~1B tokens through expansion) and halves token-list
/// memory. The old variant *constructors* `Token::Char(..)` /
/// `Token::ControlSequence(..)` are preserved below as associated functions, so
/// every construction site is unchanged; only *pattern* sites move to
/// [`kind`](Self::kind) / the accessors.
///
/// `Copy`: a token is one `u32`, so a token list clones by `memcpy` and moving a
/// token through the input stack, a macro body, or a `\the` result costs nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Token(u32);

/// The unpacked view of a [`Token`], for `match`. Same shape the old enum had,
/// so a pattern site becomes `match t.kind() { TokKind::Char(c, cat) => .. }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Char(char, CatCode),
    Cs(Symbol),
}

const CS_BIT: u32 = 0x8000_0000;
const CAT_SHIFT: u32 = 21;
const CHAR_MASK: u32 = 0x001F_FFFF; // 21 bits — the whole Unicode scalar range.
const CAT_MASK: u32 = 0xF;

impl Token {
    /// A character token carrying the catcode it had *when it was read*. TeX
    /// freezes the category at tokenization time; changing `\catcode` later does
    /// not retroactively alter tokens already made. Named `Char` (not
    /// `snake_case`) so existing `Token::Char(c, cat)` construction sites — of
    /// which there are hundreds — compile unchanged against the packed rep.
    #[allow(non_snake_case)]
    #[inline(always)]
    pub const fn Char(c: char, cat: CatCode) -> Token {
        Token(((cat as u32) << CAT_SHIFT) | (c as u32))
    }

    /// A control sequence — `\foo`, `\!`, or an active character promoted to
    /// behave like one — identified by its interned name. Named `ControlSequence`
    /// to keep construction sites unchanged (see [`Char`](Self::Char)).
    #[allow(non_snake_case)]
    #[inline(always)]
    pub fn ControlSequence(sym: Symbol) -> Token {
        // Bit 31 is the cs tag, so a symbol id must fit in 31 bits. Ids are a
        // monotonic per-name counter (`Interner`); `latex.ltx` interns ~10^5,
        // nowhere near 2^31. A breach would alias a cs onto a char token.
        debug_assert!(
            sym.0 < CS_BIT,
            "symbol id {} overflows the 31-bit token field",
            sym.0
        );
        Token(CS_BIT | sym.0)
    }

    #[inline(always)]
    pub const fn is_cs(self) -> bool {
        self.0 & CS_BIT != 0
    }

    /// The raw packed word. For engines that store tokens in a flat `u32` arena
    /// (a body/token-list arena) and decode them in the hot loop.
    #[inline(always)]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Reconstruct a token from its raw packed word — the inverse of [`raw`](Self::raw).
    #[inline(always)]
    pub const fn from_raw(w: u32) -> Token {
        Token(w)
    }

    /// The interned name, if this is a control sequence.
    #[inline(always)]
    pub const fn as_cs(self) -> Option<Symbol> {
        if self.is_cs() {
            Some(Symbol(self.0 & !CS_BIT))
        } else {
            None
        }
    }

    /// The scalar and catcode, if this is a character token.
    #[inline(always)]
    pub fn as_char(self) -> Option<(char, CatCode)> {
        if self.is_cs() {
            None
        } else {
            // Both halves are valid by construction: the scalar came from a
            // `char` and the catcode from a `CatCode`.
            let c = char::from_u32(self.0 & CHAR_MASK)?;
            let cat = CatCode::from_u8(((self.0 >> CAT_SHIFT) & CAT_MASK) as u8)?;
            Some((c, cat))
        }
    }

    /// The unpacked view, for `match`.
    #[inline(always)]
    pub fn kind(self) -> TokKind {
        if let Some(sym) = self.as_cs() {
            TokKind::Cs(sym)
        } else {
            let (c, cat) = self.as_char().unwrap();
            TokKind::Char(c, cat)
        }
    }

    #[inline(always)]
    pub fn is_space(self) -> bool {
        !self.is_cs() && (self.0 >> CAT_SHIFT) & CAT_MASK == CatCode::Space as u32
    }

    /// A catcode-13 character. Tested on every character token the expansion loop
    /// reads (an active character has a meaning, like a control sequence), so it is a
    /// shift-and-compare rather than a `char`/`CatCode` reconstruction.
    #[inline(always)]
    pub fn is_active(self) -> bool {
        !self.is_cs() && (self.0 >> CAT_SHIFT) & CAT_MASK == CatCode::Active as u32
    }

    #[inline(always)]
    pub fn cat(self) -> Option<CatCode> {
        if self.is_cs() {
            None
        } else {
            CatCode::from_u8(((self.0 >> CAT_SHIFT) & CAT_MASK) as u8)
        }
    }
}

impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.kind() {
            TokKind::Char(c, cat) => write!(f, "Char({c:?}, {cat:?})"),
            TokKind::Cs(s) => write!(f, "ControlSequence({s:?})"),
        }
    }
}

/// Where the tokenizer is within a line (`tex.web` §303).
///
/// This is what makes leading whitespace vanish, a single newline become a
/// space, and a blank line become `\par`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum State {
    /// Start of line: spaces are skipped, and an end-of-line yields `\par`.
    NewLine,
    /// Just read a control word or a space: further spaces are skipped, and an
    /// end-of-line is skipped too.
    SkipBlanks,
    /// Mid-line: a space is significant, and an end-of-line becomes a space.
    MidLine,
}

/// Reads characters into tokens under a mutable catcode regime.
pub struct Tokenizer<'a> {
    input: &'a [char],
    pos: usize,
    state: State,
    catcodes: &'a CatCodeTable,
    /// Shared name table: a control sequence's name is interned here as it is
    /// read, so the produced [`Token`] carries a `Copy` [`Symbol`]. Behind a
    /// `RefCell` because the eqtb and every reader share one interner, and the
    /// tokenizer only touches it to intern (never while anything else holds it).
    interner: &'a RefCell<Interner>,
    /// Current line, 1-indexed — TeX reports errors by line, so we track it.
    pub line: u32,
    /// The next character read begins a new line — see `next_char`.
    pending_line_start: bool,
    /// Character already appended to the currently loaded physical line.
    /// `input_ln` freezes its value when the line is loaded; changing
    /// `\endlinechar` while tokens from that line remain does not rewrite the
    /// buffer retroactively.
    line_endchar: i32,
    /// Live value to append when the next physical line is loaded.
    endlinechar: i32,
    /// How many category-15 characters this call stepped over. `tex.web` §346
    /// reports each one and reads on, and the reader has the error stream that
    /// this does not.
    pub invalid_seen: u32,
    /// §352 contractions performed while producing this token, and whether
    /// each occurred inside control-sequence scanning (the one path where TeX
    /// physically shortens its line buffer).
    contractions: Vec<(usize, usize, char, bool)>,
    in_control_sequence: bool,
}

impl<'a> Tokenizer<'a> {
    /// Tokenize a borrowed slice of characters, from the start.
    ///
    /// Borrowing rather than owning is the difference between reading a file
    /// in O(n) and O(n²): the input stack re-enters this once per token to
    /// pick up any catcode change, and collecting the remaining characters
    /// into a fresh `Vec`/`String` each time made every token cost a copy of
    /// the whole rest of the file. The catcodes are read, never written, so
    /// they are shared immutably too -- no per-token clone.
    pub fn new(
        input: &'a [char],
        catcodes: &'a CatCodeTable,
        interner: &'a RefCell<Interner>,
    ) -> Self {
        Self::new_at(input, 0, catcodes, interner)
    }

    /// As [`Tokenizer::new`], but starting `pos` characters in -- so the
    /// per-token file reader can resume exactly where it left off without
    /// re-slicing the input.
    pub fn new_at(
        input: &'a [char],
        pos: usize,
        catcodes: &'a CatCodeTable,
        interner: &'a RefCell<Interner>,
    ) -> Self {
        Self {
            input,
            pos,
            state: State::NewLine,
            catcodes,
            interner,
            invalid_seen: 0,
            contractions: Vec::new(),
            in_control_sequence: false,
            line: 1,
            pending_line_start: false,
            line_endchar: 13,
            endlinechar: 13,
        }
    }

    /// Intern a control-sequence name into the shared table.
    fn cs(&self, name: &str) -> Token {
        Token::ControlSequence(self.interner.borrow_mut().intern(name))
    }

    /// `\endlinechar` — the character TeX appends to each input line.
    ///
    /// Normally 13, whose catcode 5 makes a line ending act as a space (or a
    /// `\par` after a blank line). Setting it outside 0..=255 appends *nothing*,
    /// so a construct can span lines with no token between them at all — which
    /// is what LaTeX does for six hundred lines after `\endlinechar=-1`.
    pub fn set_endlinechar(&mut self, c: i32) {
        self.endlinechar = c;
        self.line_endchar = c;
    }

    /// Restore the character frozen onto the current file line. The lazy file
    /// reader recreates a tokenizer per token, so this travels with its file
    /// state independently of the now-live `\endlinechar` parameter.
    pub fn set_line_endchar(&mut self, c: i32) {
        self.line_endchar = c;
    }

    pub fn line_endchar(&self) -> i32 {
        self.line_endchar
    }

    /// Characters consumed so far.
    ///
    /// The input stack needs this to advance a file position by exactly what
    /// one token cost, so the next token can be read under whatever catcodes
    /// are in force by then.
    pub fn consumed(&self) -> usize {
        self.pos
    }

    pub fn contractions(&self) -> &[(usize, usize, char, bool)] {
        &self.contractions
    }

    /// The line-state machine's current position.
    ///
    /// The input stack must carry this between tokens. Recreating a tokenizer
    /// per token resets it to `NewLine`, which makes every newline a `\par`
    /// and swallows every space as leading whitespace.
    pub fn state(&self) -> State {
        self.state
    }

    pub fn set_state(&mut self, s: State) {
        self.state = s;
    }

    /// Whether the next raw character begins a physical line. The lazy file
    /// reader recreates a tokenizer for every token, so this one-token bridge
    /// must travel with [`State`] rather than being reset by `new_at`.
    pub fn pending_line_start(&self) -> bool {
        self.pending_line_start
    }

    pub fn set_pending_line_start(&mut self, pending: bool) {
        self.pending_line_start = pending;
    }

    pub fn set_line(&mut self, l: u32) {
        self.line = l;
    }

    fn peek_raw(&self, offset: usize) -> Option<char> {
        self.input.get(self.pos + offset).copied()
    }

    /// Resolve TeX's `^^` notation at the current position (`tex.web` §352).
    ///
    /// `^^A` means character 1, `^^@` means NUL, and `^^` followed by two
    /// lowercase hex digits means that byte. The superscript character is
    /// whatever currently has [`CatCode::Superscript`], not literally `^`, and
    /// both occurrences must be the *same* character. §352 restarts at the
    /// replacement in the shortened input buffer, so one contraction can form
    /// the first half of another: with `q` as a superscript character,
    /// `qq1qM` becomes `qqM` and then carriage return.
    fn expand_double_superscript(&mut self) -> Option<char> {
        let start = self.pos;
        let mut c = self.peek_raw(0)?;
        let mut next = self.pos + 1;
        let mut changed = false;
        loop {
            if self.catcodes.get(c) != CatCode::Superscript
                || self.input.get(next).copied() != Some(c)
            {
                break;
            }
            let Some(a) = self.input.get(next + 1).copied() else {
                break;
            };

            // Two lowercase hex digits take precedence over the single-char
            // form. `next` points at the second superscript character; the
            // first is the virtual replacement from the preceding turn after
            // the initial contraction.
            if let Some(b) = self.input.get(next + 2).copied()
                && is_lower_hex(a)
                && is_lower_hex(b)
            {
                c = char::from_u32(hex_val(a) * 16 + hex_val(b))?;
                next += 3;
                changed = true;
                continue;
            }

            let code = a as u32;
            if code >= 128 {
                break;
            }
            // Flip bit 6: `^^A` -> 0x01, `^^?` -> 0x7f.
            let v = if code < 64 { code + 64 } else { code - 64 };
            c = char::from_u32(v)?;
            next += 2;
            changed = true;
        }
        if changed {
            self.pos = next;
            self.contractions
                .push((start, next, c, self.in_control_sequence));
            Some(c)
        } else {
            None
        }
    }

    /// Read one character while TeX's control-sequence scanner is active.
    /// Unlike ordinary `^^` expansion, §355 records contractions in `buffer`.
    fn next_control_char(&mut self) -> Option<(char, CatCode)> {
        self.in_control_sequence = true;
        let out = self.next_char();
        self.in_control_sequence = false;
        out
    }

    /// Next character, with `^^` already resolved.
    fn next_char(&mut self) -> Option<(char, CatCode)> {
        // A loop, not recursion: with `\endlinechar` out of range every line
        // ending is skipped without producing a token, and a run of them
        // recursed once per line. latex.ltx has enough of them to overflow the
        // stack.
        loop {
            // §331: *reading a line* leaves the scanner in `new_line`, and it
            // does so whatever the previous line's ending turned out to be —
            // a `\par`-making `^^M`, a space, a character the document made
            // `\catcode 9` and so ignored, or nothing at all because
            // `\endlinechar` was out of range. The end-of-line token belongs to
            // the line that is finishing and is delivered in that line's state;
            // the *next* character starts the new one, which is what this flag
            // carries across.
            //
            // Getting it only for the out-of-range case left two documents
            // wrong in the same way. `hyperref` reads `pd1enc.def` under
            // `\endlinechar=-1`, where
            //
            //     \ProvidesFile{pd1enc.def}
            //       [2024-01-20 v7.01h %
            //
            // handed `\@ifnextchar` a space instead of the `[`. And `listings`
            // reads its aspect files under `\catcode`\^^M=9`, where the same
            // continuation-line indentation became spurious spaces.
            if self.pending_line_start {
                self.pending_line_start = false;
                self.state = State::NewLine;
                // §360 appends the parameter's value as `input_ln` loads the
                // line, rather than consulting it later when `get_next`
                // reaches the end of the buffer.
                self.line_endchar = self.endlinechar;
            }
            if let Some(c) = self.expand_double_superscript() {
                return Some((c, self.catcodes.get(c)));
            }
            let c = self.peek_raw(0)?;
            self.pos += 1;
            // A line ending *is* `\endlinechar`, whatever that currently is. When
            // it is out of range the line simply ends with nothing, so the next
            // line continues the construct.
            //
            // What counts as an ending is `input_ln`'s business, not any
            // catcode's. web2c reads until it meets `\n` or `\r`, and swallows
            // the `\n` of a `\r\n` pair with it, so a DOS file has exactly one
            // ending per line like every other file — the terminator never
            // reaches the scanner as a character at all.
            //
            // Reading `\r` as an ordinary character instead gave it its default
            // catcode, which is `EndOfLine` (13 is `\endlinechar`), so a CRLF
            // line ended *twice*: the `\r` as the line's own ending and the
            // `\n` as `\endlinechar` on top of it. The second one arrived with
            // the state the first had just reset to `new_line`, which is
            // §344's blank-line case — a `\par` after every single line.
            //
            // Whole arXiv submissions are CRLF. 2608.18006v1 is one, and its
            //
            //     \hypersetup{
            //       colorlinks=true,
            //
            // put a `\par` inside a macro argument, so kvsetkeys was handed the
            // key `\par colorlinks` and hyperref's option list was lost. The
            // `\par` also started a paragraph before `\begin{document}`, which
            // is what `\everypar{\@nodocument}` exists to catch:
            //
            //     ! LaTeX Error: Missing \begin{document}.
            //
            // reported against the closing brace of the `\hypersetup`.
            if c == '\n' || c == '\r' {
                if c == '\r' && self.peek_raw(0) == Some('\n') {
                    self.pos += 1;
                }
                // The line ended, whether or not it contributed a token. Not
                // counting it stops the line number dead — latex.ltx sets
                // `\endlinechar=-1` on line 191, and listings makes `^^M`
                // catcode 9, so every location reported after either was frozen.
                self.line += 1;
                self.pending_line_start = true;
                match u32::try_from(self.line_endchar)
                    .ok()
                    .filter(|v| *v < 256)
                    .and_then(char::from_u32)
                {
                    Some(e) => return Some((e, self.catcodes.get(e))),
                    // Nothing to deliver, so the new line starts immediately:
                    // the flag is consumed at the top of the next turn.
                    None => continue,
                }
            }
            return Some((c, self.catcodes.get(c)));
        }
    }

    fn skip_to_end_of_line(&mut self) {
        // The raw physical line terminator, not whatever catcode character 10
        // happens to carry. A `%` comment ends where the *file's* line ends,
        // which is a property of how the source is split into lines -- the
        // same raw `'\n'` `next_char` substitutes for `\endlinechar` above --
        // not of any character's catcode. Checking `catcode.get(c) ==
        // EndOfLine` here used to work only because char 10 was wrongly given
        // that catcode by default; fixing that (char 10 is `Other`, matching
        // `tex.web`) would have made a comment run to the end of the file.
        while let Some(c) = self.peek_raw(0) {
            if c == '\n' || c == '\r' {
                break;
            }
            self.pos += 1;
        }
    }

    /// Produce the next token, or `None` at end of input.
    pub fn next_token(&mut self) -> Option<Token> {
        loop {
            let (c, cat) = self.next_char()?;

            match cat {
                CatCode::Ignored => continue,

                CatCode::Escape => {
                    // A control sequence: one non-letter, or a run of letters.
                    // When the escape character is the line's appended
                    // `\endlinechar`, §354 is already past `limit`: it yields
                    // the null control sequence and must not take the first
                    // character of the next physical line as its name.
                    if self.pending_line_start {
                        self.state = State::SkipBlanks;
                        return Some(self.cs(""));
                    }
                    let Some((first, fcat)) = self.next_control_char() else {
                        // `\` at end of input yields the null control sequence.
                        self.state = State::SkipBlanks;
                        return Some(self.cs(""));
                    };
                    if fcat == CatCode::Letter {
                        let mut name = ControlName::new();
                        name.push(first);
                        while let Some(c2) = self.peek_raw(0) {
                            // Re-read through next_char so `^^` works inside a
                            // control word, as TeX allows.
                            // A *speculative* read: if what comes next is not
                            // a letter the control word ended, and everything
                            // this call did has to be undone — including the
                            // line count, because `next_char` counts a line as
                            // it crosses the newline and a control word at the
                            // end of a line reads that newline to find out that
                            // it is not a letter. Restoring only `pos` left the
                            // counter one line ahead for the rest of the file.
                            let save = (
                                self.pos,
                                self.line,
                                self.pending_line_start,
                                self.contractions.len(),
                                self.line_endchar,
                            );
                            let Some((c3, cat3)) = self.next_control_char() else {
                                break;
                            };
                            if cat3 == CatCode::Letter {
                                name.push(c3);
                            } else {
                                (self.pos, self.line, self.pending_line_start) =
                                    (save.0, save.1, save.2);
                                self.line_endchar = save.4;
                                // Scanning ahead found the terminator, not part
                                // of the control word. TeX restores `loc` and
                                // leaves that ordinary `^^` spelling for the
                                // next token and for §311's source context.
                                self.contractions.truncate(save.3);
                                break;
                            }
                            let _ = c2;
                        }
                        // After a control *word*, spaces are skipped.
                        self.state = State::SkipBlanks;
                        return Some(self.cs(name.as_str()));
                    }
                    // Control *symbol*: a single character. `\ ` (backslash
                    // space) is the one that keeps the following spaces.
                    self.state = if fcat == CatCode::Space {
                        State::SkipBlanks
                    } else {
                        State::MidLine
                    };
                    let mut encoded = [0u8; 4];
                    return Some(self.cs(first.encode_utf8(&mut encoded)));
                }

                CatCode::Comment => {
                    self.skip_to_end_of_line();
                    // The end-of-line itself is consumed without producing a
                    // token: a comment swallows the line break. §345 does it by
                    // `loc:=limit+1`, and `limit` is where `\endlinechar` was
                    // put, so the ending goes with the rest of the line.
                    //
                    // The whole terminator, not one character of it: on a CRLF
                    // file taking only the `\r` left the `\n` to end the line a
                    // second time, which is the same doubled ending `next_char`
                    // guards against — reached by the comment path instead.
                    if let Some(c) = self.peek_raw(0) {
                        self.pos += 1;
                        if c == '\r' && self.peek_raw(0) == Some('\n') {
                            self.pos += 1;
                        }
                        self.line += 1;
                        self.pending_line_start = true;
                    }
                    continue;
                }

                CatCode::EndOfLine => {
                    // `next_char` has already counted the line — it does that
                    // for *every* ending now, not only this catcode's.
                    // Anything left on the line is discarded (`tex.web` §343).
                    let prior = self.state;
                    self.state = State::NewLine;
                    match prior {
                        // Blank line: a paragraph break.
                        State::NewLine => return Some(self.cs("par")),
                        // Line break mid-text behaves as a space.
                        State::MidLine => return Some(Token::Char(' ', CatCode::Space)),
                        // Right after a control word: swallowed entirely.
                        State::SkipBlanks => continue,
                    }
                }

                CatCode::Space => {
                    match self.state {
                        // Leading and repeated spaces disappear.
                        State::NewLine | State::SkipBlanks => continue,
                        State::MidLine => {
                            self.state = State::SkipBlanks;
                            // Every space token is a plain U+0020, whatever
                            // character produced it.
                            return Some(Token::Char(' ', CatCode::Space));
                        }
                    }
                }

                // §346 `@<Decry the invalid character and goto restart@>`: the
                // character is reported and *then* forgotten, and the reading
                // restarts. Returning here rather than reading on is what makes
                // the report land where TeX's does — with the position just
                // past the offending character and nothing after it consumed.
                // The reader has the error stream; this has only the count.
                CatCode::Invalid => {
                    self.invalid_seen += 1;
                    return None;
                }

                _ => {
                    self.state = State::MidLine;
                    return Some(Token::Char(c, cat));
                }
            }
        }
    }

    /// Tokenize to exhaustion. Convenient for tests; the real engine pulls
    /// lazily because expansion can change the catcodes mid-stream.
    pub fn collect_all(&mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while let Some(t) = self.next_token() {
            out.push(t);
        }
        out
    }
}

/// Tokenize a whole string in one shot -- the ergonomic form for callers that
/// have a `&str` and want every token, now that [`Tokenizer`] borrows a
/// `&[char]`. Collects the characters once (the cost the per-token file reader
/// avoids, and which a one-shot caller pays anyway).
pub fn tokenize_all(
    src: &str,
    catcodes: &CatCodeTable,
    interner: &RefCell<Interner>,
) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    Tokenizer::new(&chars, catcodes, interner).collect_all()
}

fn is_lower_hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

fn hex_val(c: char) -> u32 {
    if c.is_ascii_digit() {
        c as u32 - '0' as u32
    } else {
        c as u32 - 'a' as u32 + 10
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // All helpers below share one interner, so a name tokenized by `toks` and
    // the same name built by `cs` resolve to the identical `Symbol` and compare
    // equal. The value of the id is irrelevant -- only that both sides agree.
    std::thread_local! {
        static INTERNER: RefCell<Interner> = RefCell::new(Interner::new());
    }

    // The behaviours below were verified against TeX Live 2024 with
    // `\detokenize`, not merely read out of tex.web:
    //
    //   \foo   bar  ->  [\foo bar]   spaces after a control word vanish
    //   a   b       ->  [a b]        runs of spaces collapse
    //   a\nb        ->  [a b]        a single newline is a space
    //   ^^41        ->  [A]          lowercase hex pair
    //   x^^4A       ->  [xtA]        ^^4 is 0x74 't'; uppercase A is NOT hex
    fn toks(src: &str) -> Vec<Token> {
        let cc = CatCodeTable::plain();
        INTERNER.with(|i| tokenize_all(src, &cc, i))
    }

    fn cs(name: &str) -> Token {
        INTERNER.with(|i| Token::ControlSequence(i.borrow_mut().intern(name)))
    }

    fn ch(c: char, cat: CatCode) -> Token {
        Token::Char(c, cat)
    }

    #[test]
    fn control_words_absorb_letters_only() {
        assert_eq!(toks("\\foo"), vec![cs("foo")]);
        assert_eq!(toks("\\foo1"), vec![cs("foo"), ch('1', CatCode::Other)]);
    }

    #[test]
    fn control_symbols_are_a_single_character() {
        assert_eq!(toks("\\!"), vec![cs("!")]);
        // `\\` is a control symbol whose name is a backslash, not an escape.
        assert_eq!(toks("\\\\"), vec![cs("\\")]);
    }

    #[test]
    fn an_escape_endline_is_the_null_control_sequence() {
        let mut cc = CatCodeTable::plain();
        cc.set('\r', CatCode::Escape);
        let chars: Vec<char> = "\n\\relax".chars().collect();
        INTERNER.with(|i| {
            let mut t = Tokenizer::new(&chars, &cc, i);
            assert_eq!(t.next_token(), Some(cs("")));
            assert_eq!(t.next_token(), Some(cs("relax")));
        });
    }

    /// Spaces after a control *word* vanish; after a control *symbol* they do
    /// not. This asymmetry is why `\TeX\ works` needs the backslash.
    #[test]
    fn spaces_after_control_words_are_skipped_but_not_after_symbols() {
        assert_eq!(
            toks("\\foo   bar"),
            vec![
                cs("foo"),
                ch('b', CatCode::Letter),
                ch('a', CatCode::Letter),
                ch('r', CatCode::Letter)
            ]
        );
        assert_eq!(
            toks("\\! x"),
            vec![cs("!"), ch(' ', CatCode::Space), ch('x', CatCode::Letter)]
        );
    }

    #[test]
    fn runs_of_spaces_collapse_and_leading_spaces_vanish() {
        assert_eq!(
            toks("a   b"),
            vec![
                ch('a', CatCode::Letter),
                ch(' ', CatCode::Space),
                ch('b', CatCode::Letter)
            ]
        );
        assert_eq!(toks("   a"), vec![ch('a', CatCode::Letter)]);
    }

    #[test]
    fn single_newline_is_a_space_and_a_blank_line_is_par() {
        assert_eq!(
            toks("a\nb"),
            vec![
                ch('a', CatCode::Letter),
                ch(' ', CatCode::Space),
                ch('b', CatCode::Letter)
            ]
        );
        assert_eq!(
            toks("a\n\nb"),
            vec![
                ch('a', CatCode::Letter),
                ch(' ', CatCode::Space),
                cs("par"),
                ch('b', CatCode::Letter)
            ]
        );
    }

    #[test]
    fn comments_swallow_the_rest_of_the_line_including_the_break() {
        // No space token appears: the comment ate the newline too.
        assert_eq!(
            toks("a% comment\nb"),
            vec![ch('a', CatCode::Letter), ch('b', CatCode::Letter)]
        );
    }

    #[test]
    fn double_superscript_notation_is_resolved() {
        // ^^A is control-A (0x01); ^^@ is NUL; ^^41 is 'A' via lowercase hex.
        assert_eq!(toks("^^41"), vec![ch('A', CatCode::Letter)]);
        let t = toks("^^I");
        assert_eq!(t, vec![], "^^I is a tab, which is a skipped leading space");
        // Uppercase hex is NOT the hex form: ^^4 is a single-char escape.
        assert_eq!(
            toks("x^^4A"),
            vec![
                ch('x', CatCode::Letter),
                ch('\u{74}', CatCode::Letter),
                ch('A', CatCode::Letter)
            ]
        );

        // §352 shortens the input buffer and examines the replacement again.
        // The first `qq1` becomes `q`, which immediately combines with the
        // remaining `qM` to make carriage return.
        let mut cc = CatCodeTable::plain();
        cc.set('q', CatCode::Superscript);
        let chars: Vec<char> = "qq1qM".chars().collect();
        INTERNER.with(|i| {
            let mut t = Tokenizer::new(&chars, &cc, i);
            assert_eq!(t.next_char(), Some(('\r', CatCode::EndOfLine)));
            assert_eq!(t.consumed(), chars.len());
        });
    }

    #[test]
    fn catcode_changes_take_effect_immediately() {
        // The property that rules out a static grammar: reassigning `@` mid
        // stream changes how later characters tokenize.
        let cc = CatCodeTable::plain();
        let chars: Vec<char> = "\\a@b".chars().collect();
        INTERNER.with(|iv| {
            let mut t = Tokenizer::new(&chars, &cc, iv);
            assert_eq!(t.next_token(), Some(cs("a")));
            assert_eq!(t.next_token(), Some(ch('@', CatCode::Other)));

            let mut cc2 = CatCodeTable::plain();
            cc2.set('@', CatCode::Letter);
            let mut t2 = Tokenizer::new(&chars, &cc2, iv);
            assert_eq!(
                t2.next_token(),
                Some(cs("a@b")),
                "@ as a letter joins the control word"
            );
        });
    }

    #[test]
    fn tokens_freeze_the_catcode_they_were_read_with() {
        let mut cc = CatCodeTable::plain();
        let chars: Vec<char> = "{}".chars().collect();
        let iv = RefCell::new(Interner::new());
        let mut t = Tokenizer::new(&chars, &cc, &iv);
        let first = t.next_token();
        assert_eq!(first, Some(ch('{', CatCode::BeginGroup)));
        // The produced token carries the catcode it was read with. Changing the
        // table for the *next* token -- exactly what the input stack does when
        // it re-enters the tokenizer at the saved position -- must not reach
        // back and alter the `{` already made.
        let pos = t.consumed();
        cc.set('}', CatCode::Other);
        let mut t2 = Tokenizer::new_at(&chars, pos, &cc, &iv);
        assert_eq!(t2.next_token(), Some(ch('}', CatCode::Other)));
        assert_eq!(first, Some(ch('{', CatCode::BeginGroup)));
    }

    #[test]
    fn line_numbers_track_for_error_reporting() {
        let cc = CatCodeTable::plain();
        let chars: Vec<char> = "a\nb\nc".chars().collect();
        let iv = RefCell::new(Interner::new());
        let mut t = Tokenizer::new(&chars, &cc, &iv);
        assert_eq!(t.line, 1);
        t.collect_all();
        assert_eq!(t.line, 3);
    }

    #[test]
    fn active_characters_stay_characters_at_this_stage() {
        // `~` is Active: still a Char token here. Promotion to a control
        // sequence happens during expansion, not tokenization.
        assert_eq!(toks("~"), vec![ch('~', CatCode::Active)]);
    }

    /// An *ignored* character (category 9) simply disappears. An *invalid* one
    /// (category 15) does not: `tex.web` §346 reports it and restarts the read,
    /// so the tokenizer stops there and hands the count to whoever has an error
    /// stream — `collect_all` therefore ends at the first one.
    #[test]
    fn an_ignored_character_is_dropped_and_an_invalid_one_stops_the_read() {
        assert_eq!(
            toks("a\u{0}b"),
            vec![ch('a', CatCode::Letter), ch('b', CatCode::Letter)]
        );
        let cc = CatCodeTable::plain();
        let chars: Vec<char> = "a\u{7f}b".chars().collect();
        INTERNER.with(|i| {
            let mut tk = Tokenizer::new(&chars, &cc, i);
            assert_eq!(tk.collect_all(), vec![ch('a', CatCode::Letter)]);
            assert_eq!(tk.invalid_seen, 1, "and it says so");
        });
    }

    #[test]
    fn unicode_letters_form_control_words() {
        assert_eq!(toks("\\héllo"), vec![cs("héllo")]);
    }

    /// The tokenizer must never panic, however hostile the input — TeX
    /// processes untrusted files (docs/history/PLAN.md §9).
    #[test]
    fn never_panics_on_adversarial_input() {
        let cases = [
            "\\", "^^", "^^^", "%", "\\\\\\", "{{{{", "$$$$", "^^^^^^", "\\\u{0}", "~~~~",
            "\n\n\n\n", "%\n%\n%", "^^z", "^^ff", "\u{7f}",
        ];
        let iv = RefCell::new(Interner::new());
        for src in cases {
            let cc = CatCodeTable::plain();
            let _ = tokenize_all(src, &cc, &iv);
        }
        // And a long pathological run.
        let long: String = core::iter::repeat_n('^', 10_000).collect();
        let cc = CatCodeTable::plain();
        let _ = tokenize_all(&long, &cc, &iv);
    }
}
