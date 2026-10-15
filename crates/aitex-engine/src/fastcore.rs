//! The tight monolithic engine core — the path to TeX-parity speed (see
//! `docs/ARCHITECTURE.md` §8).
//!
//! Prototypes proved the 3.7x gap to TeX is the *abstraction tax* of the layered
//! gullet (~10 ns/token: `Level` enum dispatch, `TokenSource` indirection, an
//! `Option` per token, bounds checks, per-call heap alloc), not the data structure
//! — and that a single tight loop over *contiguous* packed representations runs the
//! macro cycle at ~1.2 ns/token, under TeX's linked-mem[] cost. This module is that
//! loop, built for real. It is `unsafe` on the hot path by design; every index is
//! guarded by an invariant established immediately before it.
//!
//! Stage 1–2 of the staged plan: the contiguous body arena and the tight `get_next`
//! over a frame stack with `out_param` argument substitution. `expand`/`match_args`/
//! `scan_int` (stages 3–4) build on this.

use crate::macros::BodyItem;
use crate::token::Token;
use alloc::rc::Rc;
use alloc::vec::Vec;

/// The monolithic `get_x_token` machine — the C-in-Rust gullet core (validated at
/// 2.71x over the layered path; see `cinrust_match_bench`). One tight loop over the
/// input stack: read a token in place (`out_param` for `#n`), and when it is an
/// expandable macro, scan its arguments and push its body as a new frame — no
/// `TokenSource` trait, no `Level` enum, no `Option`-per-token, no `Vec<Vec<Token>>`
/// per call, `get_unchecked` throughout. Bodies are read straight from the shared
/// `Rc<MacroDef>` store (already contiguous — no arena needed). This is the seed that
/// grows into the full gullet (conditionals, `\the`, scanning) and then replaces the
/// layered hot path.
#[derive(Default)]
pub struct Pump {
    /// Macro definitions, indexed by symbol id (like `MacroTable`). Frames hold a
    /// `def` index, keeping the body alive and contiguous.
    defs: Vec<Option<Rc<crate::macros::MacroDef>>>,
    frames: Vec<PFrame>,
    /// Contiguous argument-token storage (TeX's `param_stack`), reused across calls.
    params: Vec<Token>,
    pranges: Vec<(u32, u32)>,
}

/// One input-stack level, flattened — no enum. Reads `defs[def].body[pos..end]`, with
/// `out_param` substituting `params[acur..aend]` for a `#n`.
#[derive(Clone, Copy)]
struct PFrame {
    def: u32,
    pos: u32,
    end: u32,
    pbase: u32,
    pn: u32,
    acur: u32,
    aend: u32,
}

impl Pump {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a macro body under a symbol id.
    pub fn define(&mut self, sym: u32, def: Rc<crate::macros::MacroDef>) {
        let i = sym as usize;
        if self.defs.len() <= i {
            self.defs.resize(i + 1, None);
        }
        self.defs[i] = Some(def);
    }

    /// Push a macro's body as the current frame (its args already in `params`,
    /// `[pbase, pbase+pn)` of `pranges`). `bstart..bend` index into its body.
    #[inline]
    fn push_body(&mut self, def: u32, bstart: u32, bend: u32, pbase: u32, pn: u32) {
        self.frames.push(PFrame {
            def,
            pos: bstart,
            end: bend,
            pbase,
            pn,
            acur: 0,
            aend: 0,
        });
    }

    /// Start expanding macro `sym` at top level with `nargs` arguments read from the
    /// following input (for the entry point / tests).
    pub fn begin(&mut self, sym: u32) {
        if let Some(Some(def)) = self.defs.get(sym as usize) {
            let end = def.body.len() as u32;
            self.push_body(sym, 0, end, self.pranges.len() as u32, 0);
        }
    }

    /// Raw next token (no expansion) — `get_next`. Advances in place; `out_param`
    /// substitutes an argument; pops exhausted frames.
    #[inline]
    #[allow(unsafe_code)]
    fn get_next(&mut self) -> Option<Token> {
        loop {
            let n = self.frames.len();
            if n == 0 {
                return None;
            }
            // SAFETY: n > 0.
            let f = unsafe { self.frames.get_unchecked_mut(n - 1) };
            if f.acur < f.aend {
                let i = f.acur as usize;
                f.acur += 1;
                // SAFETY: acur < aend, set from a valid pranges/params region.
                return Some(unsafe { *self.params.get_unchecked(i) });
            }
            if f.pos >= f.end {
                self.pop_frame();
                continue;
            }
            let def = f.def as usize;
            let pos = f.pos as usize;
            f.pos += 1;
            let pbase = f.pbase;
            // SAFETY: `def` was validated when the frame was pushed and `defs` is not
            // mutated during reading; `pos < end <= body.len()`.
            let item = unsafe {
                *self
                    .defs
                    .get_unchecked(def)
                    .as_ref()
                    .unwrap_unchecked()
                    .body
                    .get_unchecked(pos)
            };
            match item {
                BodyItem::Token(t) => return Some(t),
                BodyItem::Arg(k) => {
                    let pi = pbase as usize + (k as usize).saturating_sub(1);
                    // SAFETY: well-formed body: k <= pn, so pi < pranges.len().
                    let (s, l) = unsafe { *self.pranges.get_unchecked(pi) };
                    let f = unsafe { self.frames.get_unchecked_mut(n - 1) };
                    f.acur = s;
                    f.aend = s + l;
                }
            }
        }
    }

    #[inline]
    fn pop_frame(&mut self) {
        if let Some(f) = self.frames.pop() {
            if f.pn > 0 {
                let base = self.pranges[f.pbase as usize].0 as usize;
                self.params.truncate(base);
            }
            self.pranges.truncate(f.pbase as usize);
        }
    }

    /// `get_x_token`: the next *non-expandable* token. Expands macros in place —
    /// scans their arguments monolithically into the param stack, then pushes the
    /// body. A macro is recognised by `is_cs()` + a registered `def`.
    #[inline]
    #[allow(unsafe_code)]
    pub fn get_x_token(&mut self) -> Option<Token> {
        loop {
            let t = self.get_next()?;
            let sym = match t.as_cs() {
                Some(s) => s.0,
                None => return Some(t),
            };
            let di = sym as usize;
            // Read the two lengths BY VALUE — no `Rc::clone` (a refcount bump per
            // call is exactly the per-token tax we are removing). The body itself is
            // read from `self.defs` by index in `get_next`, so no handle is held.
            let (nparams, body_len) = match self.defs.get(di) {
                Some(Some(d)) => (d.params.len(), d.body.len() as u32),
                _ => return Some(t), // not a (fast-path) macro
            };
            // Monolithic match_args: undelimited args (single token or brace group),
            // read via get_next straight into the reused param stack.
            let pbase = self.pranges.len() as u32;
            for _ in 0..nparams {
                let start = self.params.len() as u32;
                // skip leading spaces
                let first = loop {
                    match self.get_next() {
                        Some(x) if x.is_space() => continue,
                        Some(x) => break x,
                        None => break Token::Char('\0', crate::catcode::CatCode::Ignored),
                    }
                };
                if first.cat() == Some(crate::catcode::CatCode::BeginGroup) {
                    let mut depth = 1u32;
                    while let Some(x) = self.get_next() {
                        match x.cat() {
                            Some(crate::catcode::CatCode::BeginGroup) => {
                                depth += 1;
                                self.params.push(x);
                            }
                            Some(crate::catcode::CatCode::EndGroup) => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                self.params.push(x);
                            }
                            _ => self.params.push(x),
                        }
                    }
                } else {
                    self.params.push(first);
                }
                self.pranges.push((start, self.params.len() as u32 - start));
            }
            self.push_body(sym, 0, body_len, pbase, nparams as u32);
        }
    }
}

/// Every macro body concatenated into one contiguous arena. A macro's replacement
/// text is a `(start, len)` range — no `Rc`, no per-body allocation, cache-dense, and
/// read by raw index in [`FastCore::get_next`].
#[derive(Debug, Default, Clone)]
pub struct BodyArena {
    items: Vec<BodyItem>,
}

impl BodyArena {
    /// Append a body, returning its range.
    pub fn add(&mut self, body: &[BodyItem]) -> (u32, u32) {
        let start = self.items.len() as u32;
        self.items.extend_from_slice(body);
        (start, body.len() as u32)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// One level of the tight input stack: a body range being read, plus the `out_param`
/// cursor for an argument currently being substituted. TeX's `in_state_record` for a
/// token list, flattened — no enum, no boxing.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// Body cursor into the arena: `[pos, end)`.
    pos: u32,
    end: u32,
    /// This call's bound arguments occupy `pranges[pbase .. pbase + pn]`.
    pbase: u32,
    pn: u32,
    /// `out_param`: the argument range currently being read is `params[acur, aend)`.
    /// `acur == aend` means no argument is being substituted (read the body).
    acur: u32,
    aend: u32,
}

/// The tight token pump. Reads macro bodies in place off the contiguous arena,
/// substituting arguments from a contiguous param stack — the hot loop with the
/// abstraction removed.
#[derive(Debug, Default)]
pub struct FastCore {
    arena: BodyArena,
    frames: Vec<Frame>,
    /// Contiguous argument-token storage (TeX's `param_stack`), bump-allocated per
    /// call and reset when the call's frame pops.
    params: Vec<Token>,
    /// Per bound-argument `(start, len)` into `params`.
    pranges: Vec<(u32, u32)>,
}

impl FastCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arena_mut(&mut self) -> &mut BodyArena {
        &mut self.arena
    }

    /// Begin reading a macro call: bind `args` into the param stack and push a body
    /// frame that reads `(bstart, blen)` in place. `#n` in the body will substitute
    /// `args[n-1]` via `out_param` — no `Vec<Vec<Token>>`, no copy of the body.
    pub fn call(&mut self, bstart: u32, blen: u32, args: &[&[Token]]) {
        let pbase = self.pranges.len() as u32;
        for a in args {
            let start = self.params.len() as u32;
            self.params.extend_from_slice(a);
            self.pranges.push((start, a.len() as u32));
        }
        self.frames.push(Frame {
            pos: bstart,
            end: bstart + blen,
            pbase,
            pn: args.len() as u32,
            acur: 0,
            aend: 0,
        });
    }

    /// The next token of the expansion, advancing in place — TeX §389 `get_next` for
    /// a token-list level, made monolithic. The one hot function; see the module doc
    /// for why it is `unsafe`.
    #[inline]
    #[allow(unsafe_code)]
    pub fn get_next(&mut self) -> Option<Token> {
        loop {
            let fi = self.frames.len();
            if fi == 0 {
                return None;
            }
            // SAFETY: `fi > 0` checked; `fi - 1` is the top frame.
            let f = unsafe { self.frames.get_unchecked_mut(fi - 1) };
            // out_param: an argument is being substituted.
            if f.acur < f.aend {
                let idx = f.acur as usize;
                f.acur += 1;
                // SAFETY: `acur < aend`, and `[acur, aend)` was set from a valid
                // `pranges` entry pointing inside `params`.
                return Some(unsafe { *self.params.get_unchecked(idx) });
            }
            if f.pos >= f.end {
                // Frame exhausted: pop it and reclaim its param-stack region.
                self.pop_frame();
                continue;
            }
            let pos = f.pos as usize;
            f.pos += 1;
            // SAFETY: `f.pos < f.end <= arena.len()` (ranges come from `arena.add`).
            let item = unsafe { *self.arena.items.get_unchecked(pos) };
            match item {
                BodyItem::Token(t) => return Some(t),
                BodyItem::Arg(k) => {
                    // #k substitutes the (k-1)-th bound argument of this frame.
                    let pi = f.pbase as usize + (k as usize).saturating_sub(1);
                    // SAFETY: a well-formed body only references `#k` for
                    // `k <= pn`, so `pi < pbase + pn <= pranges.len()`.
                    let (s, l) = unsafe { *self.pranges.get_unchecked(pi) };
                    f.acur = s;
                    f.aend = s + l;
                }
            }
        }
    }

    /// Pop the top frame and truncate the param stack back to its base, so argument
    /// storage is reused (bump/reset — zero steady-state allocation).
    #[inline]
    fn pop_frame(&mut self) {
        if let Some(f) = self.frames.pop() {
            // Frames pop LIFO, so this call's args are the tail of both stacks.
            // Read the params truncation point BEFORE removing the pranges entries.
            if f.pn > 0 {
                let base = self.pranges[f.pbase as usize].0 as usize;
                self.params.truncate(base);
            }
            self.pranges.truncate(f.pbase as usize);
        }
    }

    pub fn is_idle(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catcode::CatCode;
    use crate::macros::MacroFrame;
    use alloc::rc::Rc;
    use alloc::vec;

    fn tok(k: u32) -> Token {
        Token::Char(
            char::from_u32('a' as u32 + (k % 26)).unwrap(),
            CatCode::Letter,
        )
    }

    fn cs(k: u32) -> Token {
        Token::ControlSequence(crate::intern::Symbol(k))
    }

    #[test]
    fn pump_expands_nested_macros() {
        use crate::macros::{MacroDef, Param};
        let bg = Token::Char('{', CatCode::BeginGroup);
        let eg = Token::Char('}', CatCode::EndGroup);
        let mut p = Pump::new();
        // \m1#1 -> p #1 q
        p.define(
            1,
            Rc::new(MacroDef {
                params: vec![Param::Arg(1, '#')],
                body: vec![
                    BodyItem::Token(tok(15)),
                    BodyItem::Arg(1),
                    BodyItem::Token(tok(16)),
                ],
                long: true,
                protected: false,
            }),
        );
        // \m2#1#2 -> #1 #2
        p.define(
            2,
            Rc::new(MacroDef {
                params: vec![Param::Arg(1, '#'), Param::Arg(2, '#')],
                body: vec![BodyItem::Arg(1), BodyItem::Arg(2)],
                long: true,
                protected: false,
            }),
        );
        // \root -> a \m1{x} b \m2{y}{z} c
        p.define(
            0,
            Rc::new(MacroDef {
                params: vec![],
                body: vec![
                    BodyItem::Token(tok(0)), // a
                    BodyItem::Token(cs(1)),  // \m1
                    BodyItem::Token(bg),
                    BodyItem::Token(tok(23)),
                    BodyItem::Token(eg),     // {x}
                    BodyItem::Token(tok(1)), // b
                    BodyItem::Token(cs(2)),  // \m2
                    BodyItem::Token(bg),
                    BodyItem::Token(tok(24)),
                    BodyItem::Token(eg), // {y}
                    BodyItem::Token(bg),
                    BodyItem::Token(tok(25)),
                    BodyItem::Token(eg),     // {z}
                    BodyItem::Token(tok(2)), // c
                ],
                long: true,
                protected: false,
            }),
        );
        p.begin(0);
        let mut got = vec![];
        while let Some(t) = p.get_x_token() {
            got.push(t);
        }
        // a  (p x q)  b  (y z)  c
        assert_eq!(
            got,
            vec![
                tok(0),
                tok(15),
                tok(23),
                tok(16),
                tok(1),
                tok(24),
                tok(25),
                tok(2)
            ],
        );
    }

    /// Throughput of the integrated monolithic pump on nested macros-calling-macros
    /// — the real `get_x_token` cost.
    /// `cargo test --release -p aitex-engine pump_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn pump_bench() {
        use crate::macros::{MacroDef, Param};
        use std::hint::black_box;
        use std::time::Instant;

        let bg = Token::Char('{', CatCode::BeginGroup);
        let eg = Token::Char('}', CatCode::EndGroup);
        let mut p = Pump::new();
        // leaf macro \l#1#2 -> 16 literal tokens + #1 + #2
        let mut lbody: Vec<BodyItem> = (0..16).map(|k| BodyItem::Token(tok(k))).collect();
        lbody.insert(6, BodyItem::Arg(1));
        lbody.insert(13, BodyItem::Arg(2));
        p.define(
            1,
            Rc::new(MacroDef {
                params: vec![Param::Arg(1, '#'), Param::Arg(2, '#')],
                body: lbody,
                long: true,
                protected: false,
            }),
        );
        // driver \d -> 8 copies of  \l{aaaa}{bbbb}
        let mut dbody: Vec<BodyItem> = vec![];
        for _ in 0..8 {
            dbody.push(BodyItem::Token(cs(1)));
            dbody.push(BodyItem::Token(bg));
            for _ in 0..4 {
                dbody.push(BodyItem::Token(tok(0)));
            }
            dbody.push(BodyItem::Token(eg));
            dbody.push(BodyItem::Token(bg));
            for _ in 0..4 {
                dbody.push(BodyItem::Token(tok(1)));
            }
            dbody.push(BodyItem::Token(eg));
        }
        p.define(
            0,
            Rc::new(MacroDef {
                params: vec![],
                body: dbody,
                long: true,
                protected: false,
            }),
        );

        const REPS: usize = 3_000_000;
        let t = Instant::now();
        let mut acc = 0u64;
        let mut done = 0usize;
        for _ in 0..REPS {
            p.begin(0);
            while let Some(tk) = p.get_x_token() {
                acc = acc.wrapping_add(black_box(tk).is_cs() as u64);
                done += 1;
            }
        }
        let el = t.elapsed();
        black_box(acc);
        std::println!(
            "(B) monolithic pump get_x_token       : {:5.2} ns/tok",
            el.as_nanos() as f64 / done as f64,
        );

        // ---- (A) layered pump: same algorithm, MacroFrame + Vec<Vec<Token>> args ----
        let defs: Vec<Option<Rc<MacroDef>>> = {
            let mut v: Vec<Option<Rc<MacroDef>>> = vec![None; 2];
            v[1] = p.defs[1].clone();
            v
        };
        let root = p.defs[0].clone().unwrap();
        let mut stack: Vec<MacroFrame> = Vec::new();
        let mut pool: Vec<Vec<Token>> = Vec::new();
        let bg2 = bg;
        let t = Instant::now();
        let mut acc2 = 0u64;
        let mut done = 0usize;
        // layered get_next over the MacroFrame stack.
        for _ in 0..REPS {
            stack.push(MacroFrame::new(root.clone(), vec![]));
            'outer: loop {
                // get_next
                let tk = loop {
                    match stack.last_mut() {
                        None => break 'outer,
                        Some(fr) => match fr.next() {
                            Some(x) => break x,
                            None => {
                                stack.pop();
                            }
                        },
                    }
                };
                // is it a macro?
                let sym = match tk.as_cs() {
                    Some(s) => s.0 as usize,
                    None => {
                        acc2 = acc2.wrapping_add(black_box(tk).is_cs() as u64);
                        done += 1;
                        continue;
                    }
                };
                let def = match defs.get(sym) {
                    Some(Some(d)) => d.clone(),
                    _ => {
                        acc2 = acc2.wrapping_add(black_box(tk).is_cs() as u64);
                        done += 1;
                        continue;
                    }
                };
                // match undelimited args into Vec<Vec<Token>>
                let mut args: Vec<Vec<Token>> = Vec::with_capacity(def.params.len());
                for _ in 0..def.params.len() {
                    let mut a = pool.pop().unwrap_or_default();
                    a.clear();
                    let first = loop {
                        match lget(&mut stack) {
                            Some(x) if x.is_space() => continue,
                            Some(x) => break x,
                            None => break Token::Char('\0', CatCode::Ignored),
                        }
                    };
                    if first.cat() == Some(CatCode::BeginGroup) {
                        let mut depth = 1u32;
                        while let Some(x) = lget(&mut stack) {
                            match x.cat() {
                                Some(CatCode::BeginGroup) => {
                                    depth += 1;
                                    a.push(x);
                                }
                                Some(CatCode::EndGroup) => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    a.push(x);
                                }
                                _ => a.push(x),
                            }
                        }
                    } else {
                        a.push(first);
                    }
                    args.push(a);
                }
                let _ = bg2;
                stack.push(MacroFrame::new(def, args));
            }
        }
        let el2 = t.elapsed();
        black_box(acc2);
        std::println!(
            "(A) layered pump (MacroFrame+Vec args): {:5.2} ns/tok",
            el2.as_nanos() as f64 / done as f64,
        );
        std::println!(
            "  speedup = {:.2}x",
            el2.as_nanos() as f64 / el.as_nanos() as f64
        );
    }

    // layered get_next helper for the comparison pump above.
    #[allow(dead_code)]
    fn lget(stack: &mut Vec<crate::macros::MacroFrame>) -> Option<Token> {
        loop {
            match stack.last_mut() {
                None => return None,
                Some(fr) => match fr.next() {
                    Some(t) => return Some(t),
                    None => {
                        stack.pop();
                    }
                },
            }
        }
    }

    #[test]
    fn out_param_substitution_is_correct() {
        // body: t0 t1 #1 t2 #2  ; args: #1=[x,y] #2=[z]
        let mut c = FastCore::new();
        let (s, l) = c.arena_mut().add(&[
            BodyItem::Token(tok(0)),
            BodyItem::Token(tok(1)),
            BodyItem::Arg(1),
            BodyItem::Token(tok(2)),
            BodyItem::Arg(2),
        ]);
        let a1 = [tok(23), tok(24)];
        let a2 = [tok(25)];
        c.call(s, l, &[&a1, &a2]);
        let mut got = vec![];
        while let Some(t) = c.get_next() {
            got.push(t);
        }
        assert_eq!(
            got,
            vec![tok(0), tok(1), tok(23), tok(24), tok(2), tok(25)],
            "body with #1=[x,y] #2=[z] substituted in order"
        );
        assert!(c.is_idle());
    }

    /// THE decisive test for the C-in-Rust rewrite: the FULL macro-call cycle
    /// (scan 2 brace-group arguments from an input stream + read the body with
    /// substitution), done two ways over the SAME `Token`/`BodyItem` types:
    ///   (A) layered — real `match_args_into` over a `SliceSource` (trait dispatch),
    ///       then `MacroFrame::next` (`Rc<Vec>` + `Option`), with `Vec<Vec<Token>>`
    ///       arguments;
    ///   (B) C-in-Rust — one monolithic `unsafe` function over flat slices, a reused
    ///       param stack, `get_unchecked`, no trait / `Option` / per-arg alloc.
    /// This isolates the *abstraction tax* (the ~10 ns/token the whole session
    /// localized to distributed per-token work, of which arg-matching is the biggest
    /// piece) from the data representation, which is held identical.
    /// Run: `cargo test --release -p aitex-engine cinrust_match_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn cinrust_match_bench() {
        use crate::intern::Symbol;
        use crate::macros::{MacroDef, Param};
        use crate::token::TokKind;
        use std::hint::black_box;
        use std::time::Instant;

        const CALLS: usize = 8_000_000;
        let par = Symbol(u32::MAX); // \par sentinel; args here never contain it

        let bg = Token::Char('{', CatCode::BeginGroup);
        let eg = Token::Char('}', CatCode::EndGroup);
        // Input for one call: two brace groups of 4 letters each — expl3 `:nn` shape.
        let input: Vec<Token> = vec![
            bg,
            tok(0),
            tok(1),
            tok(2),
            tok(3),
            eg,
            bg,
            tok(4),
            tok(5),
            tok(6),
            tok(7),
            eg,
        ];
        // Body: 20 items, #1 at 6, #2 at 13 (same shape as the other bench).
        let body: Vec<BodyItem> = (0..20)
            .map(|k| match k {
                6 => BodyItem::Arg(1),
                13 => BodyItem::Arg(2),
                _ => BodyItem::Token(tok(k as u32)),
            })
            .collect();
        let def = Rc::new(MacroDef {
            params: vec![Param::Arg(1, '#'), Param::Arg(2, '#')],
            body: body.clone(),
            long: true,
            protected: false,
        });

        // ---- (A) layered: real match_args_into + MacroFrame ----
        let mut pool: Vec<Vec<Token>> = Vec::new();
        let mut args: Vec<Vec<Token>> = Vec::new();
        let t = Instant::now();
        let mut acca = 0u64;
        let mut done = 0usize;
        while done < CALLS {
            let mut src = crate::macros::SliceSource::new(&input, 0);
            def.match_args_into(&mut src, par, &mut pool, &mut args)
                .unwrap();
            let mut fr = MacroFrame::new(def.clone(), core::mem::take(&mut args));
            while let Some(tk) = fr.next() {
                acca = acca.wrapping_add(black_box(tk).is_cs() as u64);
                done += 1;
            }
            args = fr.take_args();
            for a in args.drain(..) {
                if pool.len() < 16 {
                    pool.push(a);
                }
            }
        }
        let at = t.elapsed();
        std::println!(
            "(A) layered  match_args_into + MacroFrame : {:5.2} ns/tok",
            at.as_nanos() as f64 / done as f64
        );

        // ---- (B) C-in-Rust: monolithic, flat, unchecked, reused param stack ----
        let mut pstack: Vec<Token> = Vec::with_capacity(32);
        let mut pranges: Vec<(u32, u32)> = Vec::with_capacity(8);
        let t = Instant::now();
        let mut accb = 0u64;
        let mut done = 0usize;
        let inp = input.as_slice();
        let bod = body.as_slice();
        while done < CALLS {
            unsafe {
                pstack.clear();
                pranges.clear();
                // scan 2 undelimited (brace-group) args
                let mut ip = 0usize;
                for _ in 0..2 {
                    while ip < inp.len() && inp.get_unchecked(ip).is_space() {
                        ip += 1;
                    }
                    let start = pstack.len() as u32;
                    let first = *inp.get_unchecked(ip);
                    ip += 1;
                    if matches!(first.kind(), TokKind::Char(_, CatCode::BeginGroup)) {
                        let mut depth = 1u32;
                        while ip < inp.len() {
                            let tk = *inp.get_unchecked(ip);
                            ip += 1;
                            match tk.cat() {
                                Some(CatCode::BeginGroup) => {
                                    depth += 1;
                                    pstack.push(tk);
                                }
                                Some(CatCode::EndGroup) => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    pstack.push(tk);
                                }
                                _ => pstack.push(tk),
                            }
                        }
                    } else {
                        pstack.push(first);
                    }
                    pranges.push((start, pstack.len() as u32 - start));
                }
                // read body with out_param substitution
                for &item in bod {
                    match item {
                        BodyItem::Token(tk) => {
                            accb = accb.wrapping_add(black_box(tk).is_cs() as u64);
                            done += 1;
                        }
                        BodyItem::Arg(k) => {
                            let (s, l) = *pranges.get_unchecked((k as usize) - 1);
                            let mut i = s;
                            while i < s + l {
                                let tk = *pstack.get_unchecked(i as usize);
                                accb = accb.wrapping_add(black_box(tk).is_cs() as u64);
                                done += 1;
                                i += 1;
                            }
                        }
                    }
                }
            }
        }
        let bt = t.elapsed();
        std::println!(
            "(B) C-in-Rust monolithic (flat, unchecked): {:5.2} ns/tok",
            bt.as_nanos() as f64 / done as f64
        );
        std::println!(
            "  speedup = {:.2}x  (>= ~3x => the C-in-Rust rewrite closes the gap; ~1x => it does not)",
            at.as_nanos() as f64 / bt.as_nanos() as f64
        );
        black_box((acca, accb));
    }

    /// In-tree per-token benchmark: the tight `get_next` vs the layered
    /// `MacroFrame::next`, same logical work (contiguous vs Rc<Vec>+Option).
    /// `cargo test --release -p aitex-engine fastcore_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn fastcore_bench() {
        use crate::macros::MacroDef;
        use std::hint::black_box;
        use std::time::Instant;

        const NMACROS: usize = 40_000;
        const BLEN: usize = 20;
        const CALLS: usize = 20_000_000;

        // shared body shape: positions 6 and 13 are #1 / #2
        let body: Vec<BodyItem> = (0..BLEN)
            .map(|k| match k {
                6 => BodyItem::Arg(1),
                13 => BodyItem::Arg(2),
                _ => BodyItem::Token(tok(k as u32)),
            })
            .collect();
        let a1 = [tok(1), tok(2), tok(3), tok(4)];
        let a2 = [tok(5), tok(6), tok(7), tok(8)];

        // ---- tight core ----
        let mut c = FastCore::new();
        let mut ranges = vec![];
        for _ in 0..NMACROS {
            ranges.push(c.arena_mut().add(&body));
        }
        let mut s: u64 = 0x1234;
        let mut rng = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let t = Instant::now();
        let mut acc = 0u64;
        let mut done = 0usize;
        while done < CALLS {
            let (bs, bl) = ranges[(rng() as usize) % NMACROS];
            c.call(bs, bl, &[&a1, &a2]);
            while let Some(tk) = c.get_next() {
                acc = acc.wrapping_add(black_box(tk).is_cs() as u64);
                done += 1;
            }
        }
        let ft = t.elapsed();
        std::println!(
            "fastcore get_next (contiguous, unsafe): {:5.2} ns/tok",
            ft.as_nanos() as f64 / done as f64
        );

        // ---- layered MacroFrame (Rc<Vec> + Option) ----
        let def = Rc::new(MacroDef {
            params: vec![],
            body: body.clone(),
            long: false,
            protected: false,
        });
        let t = Instant::now();
        let mut acc2 = 0u64;
        let mut done = 0usize;
        while done < CALLS {
            let mut fr = MacroFrame::new(def.clone(), vec![a1.to_vec(), a2.to_vec()]);
            while let Some(tk) = fr.next() {
                acc2 = acc2.wrapping_add(black_box(tk).is_cs() as u64);
                done += 1;
            }
        }
        let mt = t.elapsed();
        std::println!(
            "MacroFrame::next (Rc<Vec>+Option)      : {:5.2} ns/tok",
            mt.as_nanos() as f64 / done as f64
        );
        std::println!(
            "  speedup = {:.2}x",
            mt.as_nanos() as f64 / ft.as_nanos() as f64
        );
        black_box((acc, acc2));
        let _ = MacroFrame::new(def, vec![]);
    }
}
