//! Macro definition and argument matching — TeX's "gullet".
//!
//! `\def\foo#1#2{...}` stores a *parameter text* and a *replacement text*. On
//! use, the parameter text is matched against what follows and the arguments
//! are substituted into the replacement.
//!
//! Two subtleties do most of the work here, and both are observable:
//!
//! * **Delimited parameters.** `\def\upto#1-#2.{}` matches `\upto 3-7.` by
//!   scanning for the literal `-` and `.`. This is why TeX macros can parse
//!   things that look like syntax, and it is not optional sugar.
//! * **Outer brace stripping.** An undelimited argument that is entirely one
//!   group loses exactly one level of braces: `\foo{ab}` binds `#1` to `ab`,
//!   while `\foo{{ab}}` binds it to `{ab}`. Getting this wrong changes the
//!   meaning of enormous amounts of real LaTeX.

use crate::catcode::CatCode;
use crate::intern::{Interner, Symbol};
use crate::token::{TokKind, Token};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

/// The gullet's macro store, indexed by interned [`Symbol`] instead of a
/// `BTreeMap`.
///
/// This is the same flat-array move as the eqtb (`tex.web` keeps macro bodies
/// *in* the eqtb, region 2), applied where it turned out to matter most:
/// `is_expandable` looks a name up here on **every** gullet token, and expl3
/// defines ~40,000 macros, so a `BTreeMap<Symbol, _>` meant an O(log n)
/// cache-missing pointer-chase ~950 million times loading `latex.ltx`. A `Vec`
/// slot is one array index, O(1) regardless of how many macros exist -- which is
/// why plain TeX (a few hundred macros) was already fast and only the expl3
/// workload was slow. The plurality of memory is `None` (non-macro symbols); that
/// is the deliberate space-for-speed trade of a direct-indexed table.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct MacroTable {
    slots: Vec<Option<Rc<MacroDef>>>,
}

impl MacroTable {
    pub fn get(&self, sym: &Symbol) -> Option<&Rc<MacroDef>> {
        self.slots.get(sym.0 as usize).and_then(|o| o.as_ref())
    }

    pub fn contains_key(&self, sym: &Symbol) -> bool {
        self.slots.get(sym.0 as usize).is_some_and(|o| o.is_some())
    }

    pub fn insert(&mut self, sym: Symbol, def: Rc<MacroDef>) -> Option<Rc<MacroDef>> {
        let i = sym.0 as usize;
        if self.slots.len() <= i {
            self.slots.resize(i + 1, None);
        }
        self.slots[i].replace(def)
    }

    pub fn remove(&mut self, sym: &Symbol) -> Option<Rc<MacroDef>> {
        self.slots.get_mut(sym.0 as usize).and_then(|o| o.take())
    }

    /// The defined macros, as `(symbol, def)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (Symbol, &Rc<MacroDef>)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, o)| o.as_ref().map(|d| (Symbol(i as u32), d)))
    }
}

/// What went wrong while matching a macro's arguments.
///
/// Distinct from `expand::ExpandError`, which is about expansion *control*
/// (unbalanced conditionals, runaway expansion). This one is about a macro
/// call not matching its parameter text.
/// One item in a parameter text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Param {
    /// A parameter marker followed by `n` — normally `#n`, but TeX preserves
    /// the actual character whose current catcode is `mac_param`.
    Arg(u8, char),
    /// A literal token that must match, delimiting the preceding argument.
    Literal(Token),
    /// `#{` — `tex.web` §476. The left brace is appended to the parameter
    /// text *as a delimiter*, and is left in the input so it goes on to open
    /// the group it would have opened anyway.
    ///
    /// It does **not** introduce a parameter. It delimits the one before it:
    /// `\def\a#1#{...}` gives `#1` everything up to the brace, so
    /// `\a AB{x}` binds `#1` to `AB`. Reading it as a parameter of its own
    /// instead makes `#1` undelimited — it takes `A` and drops the rest — which
    /// is how expl3's `\__prg_generate_conditional_parm:NNNpnn #1#2#3#4#` lost
    /// the `1` of the parameter text it was handed.
    ///
    /// With no parameter before it (`\def\f#{...}`) it is a bare requirement
    /// that a `{` come next, binding nothing.
    BraceLiteral,
}

/// A macro's parameter text and body.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MacroDef {
    pub params: Vec<Param>,
    pub body: Vec<BodyItem>,
    /// `\long` macros accept `\par` in their arguments; others reject it.
    pub long: bool,
    /// e-TeX's `\protected`: this macro does *not* expand inside `\edef`,
    /// `\write` or `\message`. LaTeX builds every robust command out of it,
    /// so without it `\DeclareRobustCommand` is undefined and most of the
    /// format never loads.
    pub protected: bool,
}

/// Somewhere tokens come from, one at a time.
///
/// Macro arguments must be read *lazily*. A window pulled up front is
/// tokenized under today's catcodes and then frozen, and TeX documents
/// routinely change a catcode and use it within the same macro call.
/// plain.tex's `\newdimen\zz` expands `\alloc@`, which takes five arguments:
/// a window big enough for them freezes some three hundred tokens the engine
/// has not reached yet, so a `\catcode` executed a line later cannot affect
/// text TeX had not read when it ran.
pub trait TokenSource {
    fn next(&mut self) -> Option<Token>;
    /// Put a token back at the front. Only ever called with a token this
    /// source just produced.
    fn back(&mut self, t: Token);
    /// Whether this is a real continuation (vs [`NoSource`]) — a stream over a
    /// fixed token list has none. Lets [`Stream::is_closed`] answer without an
    /// `Option`, so the source can be held by value and dispatched statically.
    fn is_source(&self) -> bool {
        true
    }
}

/// A [`TokenSource`] that yields nothing — the "no continuation" case for a
/// [`Stream`] over a fixed token list. A zero-sized type, so a `Stream<NoSource>`
/// carries no source pointer and its `next` is a compile-time `None`.
pub struct NoSource;
impl TokenSource for NoSource {
    fn next(&mut self) -> Option<Token> {
        None
    }
    fn back(&mut self, _t: Token) {}
    fn is_source(&self) -> bool {
        false
    }
}

/// Forwarding impl so a `&mut dyn TokenSource` (or any `&mut S`) is itself a
/// `TokenSource` — this is what lets the few dynamic-boundary callers keep a
/// `Stream<&mut dyn TokenSource>` while the hot paths monomorphise over the
/// concrete source and inline its `next`.
impl<T: TokenSource + ?Sized> TokenSource for &mut T {
    fn next(&mut self) -> Option<Token> {
        (**self).next()
    }
    fn back(&mut self, t: Token) {
        (**self).back(t)
    }
    fn is_source(&self) -> bool {
        (**self).is_source()
    }
}

/// The gullet's view of what comes next: a buffer that refills on demand.
///
/// The laziness is the point. A fixed window of upcoming tokens is tokenized
/// under today's catcodes and then frozen, so plain.tex line 521's
/// `\catcode`\^^M=\active` could not affect the `\let^^M\par` two lines
/// later if both arrived in the same pull -- the `^^M` would already be an
/// end-of-line, get swallowed, and the `\let` would silently eat the `}` that
/// closes the group.
pub struct Stream<S: TokenSource = NoSource> {
    buf: Vec<Token>,
    /// Read cursor: `buf[pos]` is the next token, `buf[..pos]` is consumed and
    /// never looked at again. Consuming advances `pos` instead of shifting the
    /// whole buffer, so reading an n-token stream is O(n), not O(n²) -- the
    /// same fix as the input stack's token levels, but here it is the gullet's
    /// scan buffer, which a deep expl3 expansion drives hardest.
    pos: usize,
    /// The continuation, held **by value and monomorphised** — not `&mut dyn`.
    /// For the hot path `S` is `InputSource`, so `fill`'s `src.next()` is a
    /// static, inlinable call into `Input::next_token_with` rather than a virtual
    /// call per token; a fixed-list stream uses [`NoSource`] and carries nothing.
    src: S,
}

/// Placeholder left in a consumed slot -- never read, since the cursor has
/// passed it. Lets a token be moved out without cloning.
const CONSUMED: Token = Token::Char('\0', CatCode::Ignored);

impl Stream<NoSource> {
    /// A stream over a fixed token list -- a macro body, a `\message` group.
    pub fn from_tokens(buf: Vec<Token>) -> Self {
        Self {
            buf,
            pos: 0,
            src: NoSource,
        }
    }
}

impl<S: TokenSource> Stream<S> {
    /// A stream that continues into `src` once `buf` runs out.
    pub fn new(buf: Vec<Token>, src: S) -> Self {
        Self { buf, pos: 0, src }
    }

    /// Number of tokens still pending (buffered but not yet consumed).
    fn pending(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn fill(&mut self, n: usize) {
        while self.pending() < n {
            match self.src.next() {
                Some(t) => self.buf.push(t),
                None => break,
            }
        }
    }

    pub fn first(&mut self) -> Option<&Token> {
        self.fill(1);
        self.buf.get(self.pos)
    }

    /// The `i`th token without consuming anything, pulling from the source as
    /// far as needed. Keyword matching needs this: `fil`, `fill` and `filll`
    /// differ only in a trailing `l`, so a scanner that commits letter by
    /// letter cannot tell `1fil` from `1filX` without being able to look first.
    pub fn peek(&mut self, i: usize) -> Option<&Token> {
        crate::counters::bump(&crate::counters::STREAM_PEEK);
        self.fill(i + 1);
        self.buf.get(self.pos + i)
    }

    /// Does this stream end where its buffer ends, or does it continue into
    /// a source? The difference decides whether running out of tokens means
    /// "the input is exhausted" or "this token list is exhausted".
    pub fn is_closed(&self) -> bool {
        !self.src.is_source()
    }

    pub fn is_empty(&mut self) -> bool {
        self.fill(1);
        self.pending() == 0
    }

    /// Same contract as `Vec::remove` relative to the cursor: `remove(0)` takes
    /// the next token in O(1). A non-zero index is rare and falls back to a shift.
    pub fn remove(&mut self, i: usize) -> Token {
        crate::counters::bump(&crate::counters::STREAM_REMOVE);
        self.fill(i + 1);
        if i == 0 {
            let t = self.buf[self.pos];
            self.pos += 1;
            t
        } else {
            self.buf.remove(self.pos + i)
        }
    }

    pub fn insert(&mut self, i: usize, t: Token) {
        if i == 0 && self.pos > 0 {
            // Reuse the slot just before the cursor: no shift.
            self.pos -= 1;
            self.buf[self.pos] = t;
        } else {
            self.buf.insert(self.pos + i, t);
        }
    }

    /// Put a whole token list at the front, in order -- what a macro expansion
    /// does. Written into the consumed prefix when it fits, so the pending
    /// tokens are not shifted at all; otherwise spliced in.
    pub fn prepend(&mut self, tokens: impl IntoIterator<Item = Token>) {
        let toks: Vec<Token> = tokens.into_iter().collect();
        if toks.len() <= self.pos {
            let start = self.pos - toks.len();
            for (k, t) in toks.into_iter().enumerate() {
                self.buf[start + k] = t;
            }
            self.pos = start;
        } else {
            self.buf.splice(self.pos..self.pos, toks);
        }
    }

    /// As [`prepend`](Self::prepend) but from a borrowed slice -- the source
    /// stays owned by the caller (a reusable expansion buffer), so a macro
    /// expansion is copied in without the intermediate `Vec` `prepend` consumes.
    /// `Token` is `Copy`, so this is a `memcpy` into the consumed prefix (no
    /// buffer growth) whenever the prefix has room.
    pub fn prepend_from(&mut self, src: &[Token]) {
        if src.len() <= self.pos {
            let start = self.pos - src.len();
            self.buf[start..self.pos].copy_from_slice(src);
            self.pos = start;
        } else {
            self.buf.splice(self.pos..self.pos, src.iter().copied());
        }
    }

    /// Reserve `n` token slots at the front and return them for the caller to
    /// fill — what a macro expansion writes into directly. Reuses the consumed
    /// prefix when it has room (no shift, no growth), exactly as
    /// [`prepend_from`](Self::prepend_from) does; otherwise splices placeholder
    /// slots in. The cursor is left at the start of the reserved region, so the
    /// filled tokens are read next, in order.
    pub fn prepend_reserve(&mut self, n: usize) -> &mut [Token] {
        if n <= self.pos {
            self.pos -= n;
        } else {
            // Grow by the shortfall and slide the live tail right, rather than
            // `splice`-ing a `repeat(CONSUMED)` iterator in: `splice` builds and
            // tears down an iterator adapter per expansion (a top ALLOC/COPY
            // caller on the latex.ltx load), where this is one `resize` + one
            // `copy_within` (memmove). The reserved slots are overwritten in
            // full by the caller, so their placeholder value is irrelevant.
            let need = n - self.pos;
            let old = self.buf.len();
            self.buf.resize(old + need, CONSUMED);
            self.buf.copy_within(self.pos..old, n);
            self.pos = 0;
        }
        &mut self.buf[self.pos..self.pos + n]
    }

    /// Everything buffered but never consumed, for the caller to push back.
    pub fn into_remaining(mut self) -> Vec<Token> {
        self.buf.split_off(self.pos)
    }

    /// Like [`into_remaining`](Self::into_remaining), but recycle the spent scan
    /// buffer into `pool` rather than dropping it -- so the next expansion
    /// context reuses this allocation instead of a fresh one. The returned tail
    /// is the unconsumed tokens (usually empty after a complete scan, so no
    /// allocation), which the caller hands back to the input.
    pub fn recycle(mut self, pool: &mut Vec<Vec<Token>>) -> Vec<Token> {
        let tail = self.buf.split_off(self.pos);
        self.buf.clear();
        if pool.len() < 64 {
            self.pos = 0;
            pool.push(self.buf);
        }
        tail
    }
}

impl<S: TokenSource> TokenSource for Stream<S> {
    fn next(&mut self) -> Option<Token> {
        crate::counters::bump(&crate::counters::STREAM_NEXT);
        self.fill(1);
        if self.pending() == 0 {
            None
        } else {
            let t = self.buf[self.pos];
            self.pos += 1;
            Some(t)
        }
    }
    fn back(&mut self, t: Token) {
        self.insert(0, t);
    }
}

/// A [`TokenSource`] over an already-tokenized slice.
///
/// Correct for the gullet, whose stream is a token list by construction, and
/// for tests. `pos` reports how much was consumed.
pub struct SliceSource<'a> {
    toks: &'a [Token],
    pub pos: usize,
}

impl<'a> SliceSource<'a> {
    pub fn new(toks: &'a [Token], pos: usize) -> Self {
        Self { toks, pos }
    }
}

impl TokenSource for SliceSource<'_> {
    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn back(&mut self, _t: Token) {
        self.pos -= 1;
    }
}

/// One item in a replacement text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BodyItem {
    Token(Token),
    /// `#n` — substitute argument *n*.
    Arg(u8),
}

/// A macro under expansion, read **in place** — `tex.web` §389
/// `begin_token_list(body, macro)` with `out_param`.
///
/// The body is the `Rc`-shared, never-copied `MacroDef`; the bound arguments sit
/// beside it, and a `#n` in the body ([`BodyItem::Arg`]) is read straight from
/// `args[n-1]`, with no substitution copy. Shared by the input stack (executing
/// a macro) and the gullet's scan buffer (expanding one) so neither copies the
/// body: invoking `\foo` reads its cells in place instead of writing a fresh
/// expansion into a `Vec` every call — which for the expl3 load was 794M token
/// copies.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MacroFrame {
    pub def: Rc<MacroDef>,
    pub args: Vec<Vec<Token>>,
    /// Cursor into `def.body`.
    pub pos: usize,
    /// When mid-argument: `(arg index, position within it)`.
    pub arg: Option<(usize, usize)>,
}

impl MacroFrame {
    pub fn new(def: Rc<MacroDef>, args: Vec<Vec<Token>>) -> Self {
        Self {
            def,
            args,
            pos: 0,
            arg: None,
        }
    }

    /// The next token of the expansion, advancing in place — from the argument
    /// currently being substituted (`out_param`) if one is live, otherwise the
    /// body, where a `#k` begins reading `args[k-1]` and a literal token is
    /// yielded directly. Everything here is `Copy`; nothing is allocated or
    /// moved. `None` means the frame is spent.
    #[inline]
    pub fn next(&mut self) -> Option<Token> {
        loop {
            if let Some((n, i)) = self.arg {
                match self.args.get(n).and_then(|a| a.get(i)).copied() {
                    Some(t) => {
                        self.arg = Some((n, i + 1));
                        return Some(t);
                    }
                    None => self.arg = None, // argument exhausted
                }
            }
            match self.def.body.get(self.pos) {
                None => return None, // body exhausted
                Some(BodyItem::Token(t)) => {
                    let t = *t;
                    self.pos += 1;
                    return Some(t);
                }
                Some(BodyItem::Arg(k)) => {
                    self.pos += 1;
                    let idx = (*k as usize).saturating_sub(1);
                    match self.args.get(idx) {
                        Some(a) if !a.is_empty() => {
                            self.arg = Some((idx, 1));
                            return Some(a[0]);
                        }
                        _ => {} // empty argument: skip, keep reading
                    }
                }
            }
        }
    }

    /// Whether the frame has no more tokens to yield — the body is at its end and
    /// no argument substitution is still in progress. Checked right after a
    /// `next()` so an exhausted frame is dropped before the token it yielded is
    /// processed (`tex.web` end_token_list), which is what lets a tail call — a
    /// body ending in another macro — not leave a dead frame underneath.
    #[inline]
    pub fn is_exhausted(&self) -> bool {
        let body_done = self.pos >= self.def.body.len();
        let arg_done = match self.arg {
            Some((n, i)) => self.args.get(n).is_none_or(|a| i >= a.len()),
            None => true,
        };
        body_done && arg_done
    }

    /// Take the bound arguments out for recycling once the frame is spent.
    pub fn take_args(&mut self) -> Vec<Vec<Token>> {
        core::mem::take(&mut self.args)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacroError {
    /// The parameter text did not match the input.
    /// A delimiter in the parameter text did not match the input.
    ///
    /// `found` is what the input actually held. It used to be reported under
    /// the name `expected`, which made every one of these diagnostics say the
    /// opposite of the truth.
    NoMatch {
        expected: Token,
        found: Option<Token>,
    },
    /// Input ran out mid-argument. TeX says "File ended while scanning use of".
    EndOfInput,
    /// `\par` inside a non-`\long` macro's argument — TeX's runaway check.
    ParInArgument,
    /// `#` followed by something that is not a digit or `#`.
    BadParameter,
}

/// Parse a parameter text such as `#1-#2.` into [`Param`]s.
///
/// Parameters must be numbered consecutively from 1; TeX rejects `#1#3`.
pub fn parse_params(tokens: &[Token]) -> Result<Vec<Param>, MacroError> {
    let mut out = Vec::new();
    parse_params_into(tokens, &mut out)?;
    Ok(out)
}

/// Allocation-reusing form of [`parse_params`]. The destination is cleared
/// before use and also on an error, matching the all-or-nothing owned API.
pub fn parse_params_into(tokens: &[Token], out: &mut Vec<Param>) -> Result<(), MacroError> {
    out.clear();
    let mut expected_next = 1u8;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].kind() {
            TokKind::Char(marker, CatCode::Parameter) => {
                let Some(next) = tokens.get(i + 1) else {
                    // Nothing follows the `#` because the parameter text ended
                    // at the body's opening brace -- which is exactly TeX's
                    // `#{`. The brace is a delimiter and stays in the input.
                    out.push(Param::BraceLiteral);
                    i += 1;
                    continue;
                };
                match next.kind() {
                    TokKind::Char(d, _) if d.is_ascii_digit() => {
                        let n = d as u8 - b'0';
                        if n != expected_next {
                            out.clear();
                            return Err(MacroError::BadParameter);
                        }
                        expected_next += 1;
                        out.push(Param::Arg(n, marker));
                        i += 2;
                    }
                    _ => {
                        out.clear();
                        return Err(MacroError::BadParameter);
                    }
                }
            }
            _ => {
                out.push(Param::Literal(tokens[i].clone()));
                i += 1;
            }
        }
    }
    Ok(())
}

/// §476's ceiling: "You already have nine parameters". A caller that is not
/// defining a macro, and so has no arity to check against, passes this.
pub const MAX_PARAMS: u8 = 9;

/// Parse a replacement text, resolving `#n` and `##`.
///
/// `nparams` is §479's `t`: how many parameters the macro being defined
/// actually has. A `#n` above it is **not** a parameter reference — §479 says
/// so, and says what to do instead:
///
/// ```text
/// if cur_cmd<>mac_param then
///   if (cur_tok<=zero_token)or(cur_tok>t) then
///     begin print_err("Illegal parameter number in definition of ");
///     …  back_error; cur_tok:=s;
///     end
///   else cur_tok:=out_param_token-"0"+cur_chr;
/// ```
///
/// `cur_tok:=s` restores the `#` so it stands for itself. The scanner already
/// does that (`Fast::absorb_balanced_params`), and this function used to undo
/// it: it collapsed every `#`+digit into `Arg(n)` without knowing the arity, so
/// a macro with no parameters got a reference to a parameter it did not have
/// and expansion substituted nothing.
///
/// ```text
/// \long\def\dd#1{\gdef\inner{#1}}  \dd{P#1Q}
///   tex:  \inner -> P##1Q      \the\toks0 = P##1Q
///   was:  \inner -> P#1Q       \the\toks0 = PQ     <- the token vanished
/// ```
///
/// Note what the rule is *not*. `tools/packages.sh` recorded this as a
/// distinction between an argument-supplied `#` and a literal one, and the fix
/// that follows from that reading — doubling every argument-supplied `#` —
/// was measured and took `latex_check` from 35/35 to 0/35. There is no such
/// distinction. It is the arity test and nothing else, which is why
/// `\newcommand` passing a body containing `#1` into `\def\foo#1{…}` was never
/// affected: there `t >= 1`.
///
/// Callers that are not defining a macro (a token list from `\the`, a test
/// fixture) pass [`MAX_PARAMS`], which restores the old unconditional
/// behaviour for them.
pub fn parse_body(tokens: &[Token], nparams: u8) -> Result<Vec<BodyItem>, MacroError> {
    let mut out = Vec::new();
    parse_body_into(tokens, nparams, &mut out)?;
    Ok(out)
}

/// Allocation-reusing form of [`parse_body`].
pub fn parse_body_into(
    tokens: &[Token],
    nparams: u8,
    out: &mut Vec<BodyItem>,
) -> Result<(), MacroError> {
    out.clear();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].kind() {
            TokKind::Char(_, CatCode::Parameter) => {
                let Some(next) = tokens.get(i + 1) else {
                    out.clear();
                    return Err(MacroError::BadParameter);
                };
                match next.kind() {
                    // §479's `(cur_tok<=zero_token)or(cur_tok>t)` — `#0` is out
                    // of range at both ends, since TeX numbers parameters from
                    // one.
                    TokKind::Char(d, _)
                        if d.is_ascii_digit() && d != '0' && (d as u8 - b'0') <= nparams =>
                    {
                        out.push(BodyItem::Arg(d as u8 - b'0'));
                        i += 2;
                    }
                    // `##` stands for a literal `#`, which is how a macro can
                    // define another macro that itself takes arguments.
                    TokKind::Char(c, CatCode::Parameter) => {
                        out.push(BodyItem::Token(Token::Char(c, CatCode::Parameter)));
                        i += 2;
                    }
                    // A lone parameter character followed by neither a digit nor
                    // another one. In source that is an error, but a body is not always
                    // source: tokens deposited by `\unexpanded`/`\the` bypass TeX's
                    // doubling rules and can legitimately carry a bare catcode-6
                    // character. expl3's `\char_generate:nn` builds exactly that -- its
                    // per-catcode table has an entry for category 6 -- and rejecting it
                    // here emptied the whole token list (`parse_body` errors were
                    // swallowed by `unwrap_or_default`), silently truncating the table
                    // to its last few entries.
                    _ => {
                        out.push(BodyItem::Token(tokens[i]));
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(BodyItem::Token(tokens[i].clone()));
                i += 1;
            }
        }
    }
    Ok(())
}

fn is_begin_group(t: &Token) -> bool {
    matches!(t.kind(), TokKind::Char(_, CatCode::BeginGroup))
}

fn is_end_group(t: &Token) -> bool {
    matches!(t.kind(), TokKind::Char(_, CatCode::EndGroup))
}

fn is_par(t: &Token, par: crate::intern::Symbol) -> bool {
    matches!(t.kind(), TokKind::Cs(n) if n == par)
}

/// Read one *undelimited* argument: a single token, or a whole brace group with
/// Read an *undelimited* argument: one token, or one balanced group with its
/// outer braces stripped.
/// Read up to the next unmatched left brace, leaving the brace in the source.
///
/// The brace is *not* consumed: `tex.web` §476 puts it back so it opens the
/// group the caller wrote. That is the whole difference from a delimited
/// argument, and it is why this cannot reuse `read_delimited`.
fn read_to_brace<S: TokenSource + ?Sized>(
    src: &mut S,
    long: bool,
    par: crate::intern::Symbol,
    out: &mut Vec<Token>,
) -> Result<(), MacroError> {
    out.clear();
    loop {
        let Some(t) = src.next() else {
            return Err(MacroError::EndOfInput);
        };
        if is_begin_group(&t) {
            src.back(t);
            return Ok(());
        }
        if !long && is_par(&t, par) {
            return Err(MacroError::ParInArgument);
        }
        out.push(t);
    }
}

fn read_undelimited<S: TokenSource + ?Sized>(
    src: &mut S,
    long: bool,
    par: crate::intern::Symbol,
    out: &mut Vec<Token>,
) -> Result<(), MacroError> {
    out.clear();
    // Leading spaces are skipped when looking for an argument.
    let first = loop {
        match src.next() {
            Some(t) if t.is_space() => continue,
            Some(t) => break t,
            None => return Err(MacroError::EndOfInput),
        }
    };
    if !long && is_par(&first, par) {
        return Err(MacroError::ParInArgument);
    }
    if !is_begin_group(&first) {
        out.push(first);
        return Ok(());
    }
    // Balanced group: strip exactly one level.
    let mut depth = 1usize;
    while let Some(t) = src.next() {
        if is_begin_group(&t) {
            depth += 1;
        } else if is_end_group(&t) {
            depth -= 1;
            if depth == 0 {
                return Ok(());
            }
        } else if !long && is_par(&t, par) {
            return Err(MacroError::ParInArgument);
        }
        out.push(t);
    }
    Err(MacroError::EndOfInput)
}

/// Read a *delimited* argument: everything up to the next occurrence of
/// `delim` at brace depth zero.
///
/// The delimiter is consumed. Braces around the whole argument are stripped, as
/// for undelimited arguments.
/// Read a delimited argument, where the delimiter may be several tokens.
///
/// `tex.web` §392 keeps a partial-match buffer and *backs up* when a delimiter
/// starts to match and then does not finish: the tokens matched so far turn out
/// to belong to the argument after all, and scanning continues. Committing to
/// the first token of the delimiter instead makes any macro whose delimiter is
/// a sequence fail whenever that sequence's first token appears alone earlier.
///
/// expl3 is full of these. `\__prg_generate_conditional_test:w` is delimited by
/// `\prg_return_true: \else: \prg_return_false: \fi:` and is deliberately fed
/// bodies that contain `\prg_return_true:` on its own — that is exactly how it
/// tells a "fast" conditional from a slow one.
fn read_delimited<S: TokenSource + ?Sized>(
    src: &mut S,
    delims: &[Token],
    long: bool,
    par: crate::intern::Symbol,
    out: &mut Vec<Token>,
    // Tokens that have matched a prefix of the delimiter so far. They belong to
    // the argument if the match does not complete. Caller-owned + recycled so a
    // delimited match allocates nothing.
    partial: &mut Vec<Token>,
) -> Result<(), MacroError> {
    out.clear();
    partial.clear();
    let mut depth = 0usize;
    while let Some(t) = src.next() {
        if depth == 0 && !delims.is_empty() {
            if t == delims[partial.len()] {
                partial.push(t);
                if partial.len() == delims.len() {
                    strip_outer_braces(out);
                    return Ok(());
                }
                continue;
            }
            if !partial.is_empty() {
                // The delimiter started and did not finish: everything buffered
                // was argument text, and `t` may still begin a fresh match.
                out.append(partial);
                if t == delims[0] {
                    partial.push(t);
                    if delims.len() == 1 {
                        partial.clear();
                        strip_outer_braces(out);
                        return Ok(());
                    }
                    continue;
                }
            }
        }
        if is_begin_group(&t) {
            depth += 1;
        } else if is_end_group(&t) {
            if depth == 0 {
                return Err(MacroError::NoMatch {
                    expected: delims.first().cloned().unwrap_or_else(|| t.clone()),
                    found: Some(t.clone()),
                });
            }
            depth -= 1;
        } else if !long && is_par(&t, par) {
            return Err(MacroError::ParInArgument);
        }
        out.push(t);
    }
    Err(MacroError::EndOfInput)
}

/// Strip one outer level of braces from a fully-scanned argument, in place.
///
/// `{ab}` as a complete argument loses its braces (`ab`), but only when the
/// braces are genuinely the outermost pair enclosing balanced content -- the
/// same rule as an undelimited argument. Operates on the caller's buffer so no
/// new `Vec` is allocated.
fn strip_outer_braces(out: &mut Vec<Token>) {
    if out.len() >= 2
        && is_begin_group(&out[0])
        && is_end_group(&out[out.len() - 1])
        && balanced(&out[1..out.len() - 1])
    {
        out.remove(0);
        out.pop();
    }
}

fn balanced(ts: &[Token]) -> bool {
    let mut d = 0i64;
    for t in ts {
        if is_begin_group(t) {
            d += 1;
        } else if is_end_group(t) {
            d -= 1;
            if d < 0 {
                return false;
            }
        }
    }
    d == 0
}

impl MacroDef {
    /// Match this macro's parameter text against `input`, starting at `pos`.
    ///
    /// On success `pos` has advanced past everything consumed, and the result
    /// holds the bound arguments in order.
    pub fn match_args(
        &self,
        input: &[Token],
        pos: &mut usize,
        par: crate::intern::Symbol,
        pool: &mut Vec<Vec<Token>>,
    ) -> Result<Vec<Vec<Token>>, MacroError> {
        let mut src = SliceSource::new(input, *pos);
        let r = self.match_args_from(&mut src, par, pool);
        *pos = src.pos;
        r
    }

    /// Match this macro's parameter text, reading `src` one token at a time.
    ///
    /// On success everything consumed has been taken from `src`, and the
    /// result holds the bound arguments in order.
    pub fn match_args_from<S: TokenSource + ?Sized>(
        &self,
        src: &mut S,
        par: crate::intern::Symbol,
        pool: &mut Vec<Vec<Token>>,
    ) -> Result<Vec<Vec<Token>>, MacroError> {
        let mut args = Vec::with_capacity(self.params.len());
        self.match_args_into(src, par, pool, &mut args)?;
        Ok(args)
    }

    /// As [`match_args_from`](Self::match_args_from), but binds into a
    /// caller-owned `args` buffer (cleared first) instead of returning a fresh
    /// `Vec`. A gullet expansion drains the bound arguments immediately, so it
    /// reuses one buffer across the millions of calls in an expl3 load rather
    /// than allocating the outer vector every time — the argument matcher was
    /// the single largest malloc caller in the profile.
    pub fn match_args_into<S: TokenSource + ?Sized>(
        &self,
        src: &mut S,
        par: crate::intern::Symbol,
        pool: &mut Vec<Vec<Token>>,
        args: &mut Vec<Vec<Token>>,
    ) -> Result<(), MacroError> {
        crate::counters::bump(&crate::counters::MATCH_ARGS);
        if self.params.is_empty() {
            crate::counters::bump(&crate::counters::ZERO_ARG);
        }
        args.clear();
        args.reserve(self.params.len());
        let mut i = 0;
        while i < self.params.len() {
            match &self.params[i] {
                Param::Literal(expected) => {
                    // Leading literals must match exactly.
                    match src.next() {
                        Some(ref t) if t == expected => {}
                        Some(t) => {
                            return Err(MacroError::NoMatch {
                                expected: expected.clone(),
                                found: Some(t),
                            });
                        }
                        None => return Err(MacroError::EndOfInput),
                    }
                    i += 1;
                }
                Param::BraceLiteral => {
                    // No preceding argument: simply require a `{` and leave it.
                    match src.next() {
                        Some(t) if is_begin_group(&t) => src.back(t),
                        Some(t) => {
                            return Err(MacroError::NoMatch {
                                expected: Token::Char('{', CatCode::BeginGroup),
                                found: Some(t),
                            });
                        }
                        None => return Err(MacroError::EndOfInput),
                    }
                    i += 1;
                }
                Param::Arg(..) => {
                    // Delimited when a literal follows, undelimited otherwise.
                    match self.params.get(i + 1) {
                        Some(Param::Literal(_)) => {
                            // The delimiter is the *whole* run of literals that
                            // follows, not just the first. Matching one token at
                            // a time commits to the first occurrence and cannot
                            // recover when the rest does not follow.
                            // `delims` is a short-lived scratch buffer drawn from
                            // the same free-list as the argument buffers, so a
                            // delimited match allocates nothing in steady state.
                            let mut delims = pool.pop().unwrap_or_default();
                            delims.clear();
                            let mut k = i + 1;
                            while let Some(Param::Literal(d)) = self.params.get(k) {
                                delims.push(d.clone());
                                k += 1;
                            }
                            let mut buf = pool.pop().unwrap_or_default();
                            let mut partial = pool.pop().unwrap_or_default();
                            let r = read_delimited(
                                src,
                                &delims,
                                self.long,
                                par,
                                &mut buf,
                                &mut partial,
                            );
                            partial.clear();
                            pool.push(partial);
                            delims.clear();
                            pool.push(delims);
                            r?;
                            args.push(buf);
                            i = k; // the delimiter is consumed with the argument
                        }
                        // `#n#{`: delimited by the left brace, which stays.
                        Some(Param::BraceLiteral) => {
                            let mut buf = pool.pop().unwrap_or_default();
                            read_to_brace(src, self.long, par, &mut buf)?;
                            args.push(buf);
                            i += 2;
                        }
                        _ => {
                            let mut buf = pool.pop().unwrap_or_default();
                            read_undelimited(src, self.long, par, &mut buf)?;
                            args.push(buf);
                            i += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Substitute `args` into the replacement text.
    pub fn expand(&self, args: &[Vec<Token>]) -> Vec<Token> {
        let mut out = Vec::new();
        self.expand_into(args, &mut out);
        out
    }

    /// Substitute `args` into the replacement text, appending to a caller-owned
    /// buffer. The caller can reuse `out` across macro calls -- one recycled
    /// buffer instead of a fresh heap allocation per expansion, which for the
    /// countless zero-argument expl3 functions is the *only* allocation a call
    /// would otherwise make. `out` is cleared first, then pre-reserved.
    pub fn expand_into(&self, args: &[Vec<Token>], out: &mut Vec<Token>) {
        out.clear();
        // Pre-size to the body length (plus the argument tokens, which is what
        // pushes it over). Growing one token at a time reallocated repeatedly on
        // every macro call -- and on expl3 nearly every command is one.
        let extra: usize = self
            .body
            .iter()
            .filter_map(|item| match item {
                BodyItem::Arg(n) => args.get(*n as usize - 1).map(|a| a.len()),
                _ => None,
            })
            .sum();
        out.reserve(self.body.len() + extra);
        for item in &self.body {
            match item {
                BodyItem::Token(t) => out.push(*t),
                BodyItem::Arg(n) => {
                    if let Some(a) = args.get(*n as usize - 1) {
                        out.extend_from_slice(a);
                    }
                }
            }
        }
    }

    /// How many tokens [`expand_into`](Self::expand_into) would produce for
    /// these arguments — the body length with each `#n` counted as its bound
    /// argument's length. Lets a caller reserve the exact destination and expand
    /// straight into it, with no intermediate buffer.
    pub fn expansion_len(&self, args: &[Vec<Token>]) -> usize {
        self.body
            .iter()
            .map(|item| match item {
                BodyItem::Token(_) => 1,
                BodyItem::Arg(n) => args.get(*n as usize - 1).map_or(0, |a| a.len()),
            })
            .sum()
    }

    /// Substitute `args` into the replacement text, writing into an exactly-sized
    /// slice (see [`expansion_len`](Self::expansion_len)). Same result as
    /// [`expand_into`](Self::expand_into) but with the destination provided by
    /// the caller — a slice carved out of the gullet's scan buffer — so the
    /// expansion is written once, in place, instead of built in a buffer and
    /// then copied to the front of the stream.
    pub fn expand_into_slice(&self, args: &[Vec<Token>], out: &mut [Token]) {
        let mut i = 0;
        for item in &self.body {
            match item {
                BodyItem::Token(t) => {
                    out[i] = *t;
                    i += 1;
                }
                BodyItem::Arg(n) => {
                    if let Some(a) = args.get(*n as usize - 1) {
                        out[i..i + a.len()].copy_from_slice(a);
                        i += a.len();
                    }
                }
            }
        }
        debug_assert_eq!(i, out.len());
    }

    /// Match and substitute in one step.
    pub fn apply(
        &self,
        input: &[Token],
        pos: &mut usize,
        par: crate::intern::Symbol,
        pool: &mut Vec<Vec<Token>>,
    ) -> Result<Vec<Token>, MacroError> {
        let args = self.match_args(input, pos, par, pool)?;
        Ok(self.expand(&args))
    }

    pub fn arity(&self) -> usize {
        self.params
            .iter()
            .filter(|p| matches!(p, Param::Arg(..)))
            .count()
    }

    /// How many upcoming tokens a call could need to match.
    ///
    /// Not the same as [`MacroDef::arity`]. A parameter text may be entirely
    /// delimiters -- plain.tex defines `\if@` with the parameter text `if` and
    /// no arguments at all -- and sizing the lookahead by arity alone hands
    /// such a macro nothing to match against, so the call fails on every
    /// attempt.
    pub fn lookahead(&self) -> usize {
        let delimiters: usize = self
            .params
            .iter()
            .map(|p| match p {
                Param::Arg(..) => 0,
                _ => 1,
            })
            .sum();
        // A macro that needs nothing must pull nothing: lookahead is
        // tokenized under today's catcodes and frozen, and plain.tex line 20
        // changes a catcode and uses it on the same line.
        64 * self.arity() + delimiters * 8
    }
}

/// Render tokens the way `\detokenize`/`\message` does, for tests and error
/// messages.
pub fn show(tokens: &[Token], interner: &Interner) -> String {
    let mut s = String::new();
    for t in tokens {
        match t.kind() {
            TokKind::Char(c, _) => s.push(c),
            TokKind::Cs(sym) => {
                let n = interner.resolve(sym);
                s.push('\\');
                s.push_str(n);
                if n.chars().count() > 1 || n.chars().next().is_some_and(|c| c.is_alphabetic()) {
                    s.push(' ');
                }
            }
        }
    }
    s
}

/// Checks the serialization of the trickiest closed slice of engine state: the
/// macro map, `BTreeMap<String, Rc<MacroDef>>`. It exercises `Rc` sharing (serde
/// `rc` feature) across `Token`/`CatCode`/`Param`/`BodyItem`/`MacroDef` and a
/// `postcard` round trip. A macro map that survives serialize→deserialize
/// byte-for-byte is the unit the checkpoint (see [`crate::checkpoint`]) restores.
#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::rc::Rc;

    #[test]
    fn macro_map_round_trips_through_postcard() {
        let mut map: BTreeMap<alloc::string::String, Rc<MacroDef>> = BTreeMap::new();
        let m = Rc::new(MacroDef {
            params: alloc::vec![
                Param::Arg(1, '#'),
                Param::Literal(Token::Char('-', CatCode::Other))
            ],
            body: alloc::vec![
                BodyItem::Token(Token::ControlSequence(crate::intern::Symbol(1))),
                BodyItem::Arg(1),
                BodyItem::Token(Token::Char('x', CatCode::Letter)),
            ],
            long: true,
            protected: false,
        });
        // Share one definition under two names to exercise Rc aliasing.
        map.insert("foo".into(), m.clone());
        map.insert("bar".into(), m);

        let bytes = postcard::to_allocvec(&map).expect("serialize");
        let back: BTreeMap<alloc::string::String, Rc<MacroDef>> =
            postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(
            map, back,
            "the macro map must survive a postcard round trip"
        );
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catcode::CatCodeTable;
    use crate::intern::{Interner, Symbol};
    use crate::token::Tokenizer;
    use core::cell::RefCell;

    // One interner shared by every helper, so a name tokenized by `toks`
    // resolves to the same `Symbol` wherever it is compared or detokenized.
    std::thread_local! {
        static INTERNER: RefCell<Interner> = RefCell::new(Interner::new());
    }

    fn toks(src: &str) -> Vec<Token> {
        let cc = CatCodeTable::plain();
        INTERNER.with(|i| crate::token::tokenize_all(src, &cc, i))
    }

    /// The interned `\par` symbol, as `apply`/`match_args` want it.
    fn par() -> Symbol {
        INTERNER.with(|i| i.borrow_mut().intern("par"))
    }

    /// `macros::show`, resolving names through the shared interner.
    fn sh(tokens: &[Token]) -> String {
        INTERNER.with(|i| show(tokens, &i.borrow()))
    }

    fn def(params: &str, body: &str, long: bool) -> MacroDef {
        MacroDef {
            params: parse_params(&toks(params)).expect("params parse"),
            body: parse_body(&toks(body), MAX_PARAMS).expect("body parse"),
            long,
            protected: false,
        }
    }

    #[test]
    fn undelimited_argument_takes_one_token() {
        let m = def("#1", "[#1]", false);
        let input = toks("ab");
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&input, &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[a]"
        );
        assert_eq!(pos, 1, "consumes exactly one token");
    }

    /// Exactly one level of braces is removed — the rule that decides what an
    /// enormous amount of real LaTeX means.
    #[test]
    fn outer_braces_are_stripped_once_only() {
        let m = def("#1", "[#1]", false);
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("{ab}"), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[ab]"
        );
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("{{ab}}"), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[{ab}]"
        );
    }

    #[test]
    fn two_arguments_bind_in_order() {
        let m = def("#1#2", "[#2#1]", false);
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("{x}{y}"), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[yx]"
        );
    }

    // The cases below were verified against TeX Live 2024, not merely read
    // from tex.web:
    //
    //   \\def\\upto#1-#2.{[#1|#2]}  then \\upto 3-7.   ->  [3|7]
    //   \\def\\strip#1{[#1]}        then \\strip{{ab}} ->  [{ab}]
    //   \\def\\strip#1{[#1]}        then \\strip{ab}   ->  [ab]
    #[test]
    fn delimited_parameters_scan_to_their_delimiter() {
        // \def\upto#1-#2.{...} matches `3-7.`
        let m = def("#1-#2.", "[#1|#2]", false);
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("3-7."), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[3|7]"
        );
    }

    #[test]
    fn delimited_argument_ignores_delimiters_inside_braces() {
        let m = def("#1.", "[#1]", false);
        let mut pos = 0;
        // The `.` inside braces must not terminate the argument.
        assert_eq!(
            sh(&m
                .apply(&toks("{a.b}c."), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[{a.b}c]"
        );
    }

    #[test]
    fn leading_literals_must_match() {
        let m = def("!#1", "[#1]", false);
        let mut pos = 0;
        assert!(
            m.apply(&toks("!x"), &mut pos, par(), &mut Vec::new())
                .is_ok()
        );
        let mut pos = 0;
        assert!(matches!(
            m.apply(&toks("?x"), &mut pos, par(), &mut Vec::new()),
            Err(MacroError::NoMatch { .. })
        ));
    }

    #[test]
    fn spaces_before_an_undelimited_argument_are_skipped() {
        let m = def("#1", "[#1]", false);
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("   x"), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[x]"
        );
    }

    /// `##` in a body is how a macro defines another macro that itself takes
    /// arguments. Verified against TeX Live: after
    /// `\def\outer#1{\def\inner##1{##1#1}}` and `\outer{Z}`,
    /// `\meaning\inner` is `macro:#1->#1Z` — the `##` collapsed to a single
    /// `#`, and the outer `#1` was substituted.
    #[test]
    fn double_hash_becomes_a_literal_hash() {
        let m = def("#1", "\\def\\inner##1{##1#1}", false);
        let mut pos = 0;
        let rendered = sh(&m
            .apply(&toks("Z"), &mut pos, par(), &mut Vec::new())
            .expect("applies"));
        assert!(
            rendered.contains("#1"),
            "## must collapse to a single #: {rendered}"
        );
        assert!(
            rendered.ends_with("Z}"),
            "the outer argument is substituted inside the inner body: {rendered}"
        );
    }

    /// A blank line inside a non-`\long` macro argument is TeX's runaway check.
    /// This is the single most common real-world runaway (measured at 37 of 48
    /// unclassified corpus logs).
    #[test]
    fn par_in_a_short_macro_argument_is_rejected() {
        let m = def("#1", "[#1]", false);
        let mut pos = 0;
        assert_eq!(
            m.apply(&toks("{a\n\nb}"), &mut pos, par(), &mut Vec::new()),
            Err(MacroError::ParInArgument)
        );

        let long = def("#1", "[#1]", true);
        let mut pos = 0;
        assert!(
            long.apply(&toks("{a\n\nb}"), &mut pos, par(), &mut Vec::new())
                .is_ok(),
            "\\long accepts \\par"
        );
    }

    #[test]
    fn running_out_of_input_is_an_error_not_a_panic() {
        let m = def("#1.", "[#1]", false);
        let mut pos = 0;
        assert_eq!(
            m.apply(&toks("no delimiter here"), &mut pos, par(), &mut Vec::new()),
            Err(MacroError::EndOfInput)
        );

        let m2 = def("#1", "[#1]", false);
        let mut pos = 0;
        assert_eq!(
            m2.apply(&toks("{unclosed"), &mut pos, par(), &mut Vec::new()),
            Err(MacroError::EndOfInput)
        );
    }

    #[test]
    fn parameters_must_be_numbered_consecutively() {
        assert!(parse_params(&toks("#1#2")).is_ok());
        assert!(parse_params(&toks("#1#3")).is_err(), "TeX rejects a gap");
        assert!(parse_params(&toks("#2")).is_err(), "must start at 1");
    }

    #[test]
    fn arity_counts_only_arguments() {
        assert_eq!(def("#1-#2.", "", false).arity(), 2);
        assert_eq!(def("", "", false).arity(), 0);
    }

    #[test]
    fn an_unused_argument_is_simply_dropped() {
        let m = def("#1#2", "[#1]", false);
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("{a}{b}"), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[a]"
        );
    }

    #[test]
    fn a_repeated_argument_is_substituted_each_time() {
        let m = def("#1", "[#1#1#1]", false);
        let mut pos = 0;
        assert_eq!(
            sh(&m
                .apply(&toks("{x}"), &mut pos, par(), &mut Vec::new())
                .expect("applies")),
            "[xxx]"
        );
    }

    #[test]
    fn never_panics_on_adversarial_definitions() {
        for p in ["#", "##", "#a", "#1#1", "#9"] {
            let _ = parse_params(&toks(p));
        }
        for b in ["#", "#a", "#1"] {
            let _ = parse_body(&toks(b), MAX_PARAMS);
        }
        let m = def("#1", "[#1]", false);
        for input in ["", "{", "}", "{{{", "\\par", "   "] {
            let mut pos = 0;
            let _ = m.apply(&toks(input), &mut pos, par(), &mut Vec::new());
        }
    }
}
