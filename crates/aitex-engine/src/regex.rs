//! POSIX extended regular expressions, because `\pdfmatch` is one.
//!
//! pdfTeX gets its matcher from the C library — `regcomp`/`regexec` with
//! `REG_EXTENDED` — and this engine has no C library, so here it is. The
//! contract below is not read off the POSIX standard but measured against
//! `pdftex -ini`, case by case; the tests at the bottom are those measurements.
//!
//! # What POSIX means here, and why it is not Perl
//!
//! POSIX matching is **leftmost-longest**: among all matches starting at the
//! earliest possible position, the longest wins. Perl-family engines are
//! leftmost-*first* — they take whichever alternative is written first. The
//! difference is not academic:
//!
//! ```text
//! \pdfmatch{(a|ab)}{ab}     pdfTeX: 1, \pdflastmatch0 = 0->ab
//!                           a Perl engine would say 0->a
//! ```
//!
//! So the matcher explores every path and keeps the one that ends furthest
//! right, rather than returning at the first `Match`.
//!
//! # Where this deliberately stops short
//!
//! POSIX also specifies which *subexpression* captures win when several paths
//! give the same overall match, and that rule is famously intricate. This
//! engine prunes on `(pc, pos)`: once a program counter has been tried at a
//! string position, trying it again cannot reach further right, so the pruning
//! is exact for the overall match and for termination — `(a*)*` cannot spin —
//! but it can settle on a different, equally-long capture split than a strict
//! POSIX implementation would. Every pattern TeX Live actually writes is
//! anchored or unambiguous enough that the question does not arise; the tests
//! cover the ones hycolor and microtype use.

use alloc::vec::Vec;

/// One byte range in a bracket expression, inclusive.
type Range = (u8, u8);

#[derive(Debug, Clone)]
struct Class {
    negated: bool,
    ranges: Vec<Range>,
}

impl Class {
    fn matches(&self, b: u8, icase: bool) -> bool {
        let hit = |c: u8| self.ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
        let mut m = hit(b);
        if icase && !m {
            m = hit(b.to_ascii_lowercase()) || hit(b.to_ascii_uppercase());
        }
        m != self.negated
    }
}

#[derive(Debug, Clone, Copy)]
enum Inst {
    Char(u8),
    Any,
    Class(u16),
    /// Try `0` first, then `1`. Both are explored — see the module docs.
    Split(u32, u32),
    Jmp(u32),
    /// Record the current position in capture slot `n` (`2i` opens group `i`,
    /// `2i+1` closes it).
    Save(u16),
    /// `^` and `$`. pdfTeX does not pass `REG_NEWLINE`, so these are the ends of
    /// the whole string, not of a line.
    Bol,
    Eol,
    Match,
}

/// A compiled pattern.
pub struct Regex {
    prog: Vec<Inst>,
    classes: Vec<Class>,
    /// Capture groups, not counting group 0.
    pub groups: usize,
    icase: bool,
}

/// Where a pattern matched, and where each of its groups did.
pub struct Match {
    pub start: usize,
    pub end: usize,
    /// `groups[i]` is group `i+1`; `None` when the group took no part in the
    /// match, which pdfTeX reports as `-1->`.
    pub groups: Vec<Option<(usize, usize)>>,
}

// ---- the parser -------------------------------------------------------------
//
// ERE, by the grammar POSIX gives it:
//
//     alt    := concat ('|' concat)*
//     concat := repeat*
//     repeat := atom ('*' | '+' | '?' | '{n[,[m]]}')*
//     atom   := '(' alt ')' | '[' bracket ']' | '.' | '^' | '$' | '\' c | byte

/// The syntax tree. Compiled straight after parsing and then dropped.
enum Node {
    Empty,
    Byte(u8),
    Any,
    Class(u16),
    Bol,
    Eol,
    Group(u16, alloc::boxed::Box<Node>),
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: alloc::boxed::Box<Node>,
        min: u32,
        max: Option<u32>,
    },
}

struct Parser<'a> {
    p: &'a [u8],
    i: usize,
    classes: Vec<Class>,
    ngroups: u16,
    /// Open `(`s. A `)` with none is a literal — measured:
    /// `\pdfmatch{a)b}{a)b}` is `1` at `0->a)b`, and `\pdfmatch{)}{x)y}` is
    /// `1` at `1->)`.
    depth: u32,
}

/// `[:alpha:]` and friends, as ranges. POSIX names them; a bracket expression
/// may hold several plus literal ranges, so each contributes ranges rather than
/// a predicate.
fn posix_class(name: &[u8]) -> Option<Vec<Range>> {
    Some(match name {
        b"alpha" => alloc::vec![(b'A', b'Z'), (b'a', b'z')],
        b"digit" => alloc::vec![(b'0', b'9')],
        b"alnum" => alloc::vec![(b'0', b'9'), (b'A', b'Z'), (b'a', b'z')],
        b"upper" => alloc::vec![(b'A', b'Z')],
        b"lower" => alloc::vec![(b'a', b'z')],
        b"space" => alloc::vec![(b'\t', b'\r'), (b' ', b' ')],
        b"blank" => alloc::vec![(b'\t', b'\t'), (b' ', b' ')],
        b"punct" => alloc::vec![(b'!', b'/'), (b':', b'@'), (b'[', b'`'), (b'{', b'~')],
        b"print" => alloc::vec![(b' ', b'~')],
        b"graph" => alloc::vec![(b'!', b'~')],
        b"cntrl" => alloc::vec![(0, 31), (127, 127)],
        b"xdigit" => alloc::vec![(b'0', b'9'), (b'A', b'F'), (b'a', b'f')],
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.p.get(self.i).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> Option<Node> {
        let mut branches = alloc::vec![self.concat()?];
        while self.eat(b'|') {
            branches.push(self.concat()?);
        }
        Some(if branches.len() == 1 {
            branches.pop()?
        } else {
            Node::Alt(branches)
        })
    }

    fn concat(&mut self) -> Option<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == b'|' || (c == b')' && self.depth > 0) {
                break;
            }
            items.push(self.repeat()?);
        }
        Some(match items.len() {
            0 => Node::Empty,
            1 => items.pop()?,
            _ => Node::Cat(items),
        })
    }

    fn repeat(&mut self) -> Option<Node> {
        let mut node = self.atom()?;
        loop {
            let (min, max) = match self.peek() {
                Some(b'*') => {
                    self.i += 1;
                    (0, None)
                }
                Some(b'+') => {
                    self.i += 1;
                    (1, None)
                }
                Some(b'?') => {
                    self.i += 1;
                    (0, Some(1))
                }
                // A `{` after an atom must be a bound. glibc — which is what
                // pdfTeX's `regcomp` is — rejects the rest rather than falling
                // back to a literal brace: `\pdfmatch{a{b}}{a{b}}` and
                // `\pdfmatch{a{}}{a}` are both `-1`, as is `\pdfmatch{a{3,1}}`.
                Some(b'{') => self.bound()?,
                _ => break,
            };
            node = Node::Repeat {
                node: alloc::boxed::Box::new(node),
                min,
                max,
            };
        }
        Some(node)
    }

    /// `{n}`, `{n,}` or `{n,m}`. Returns `None` — without consuming — when what
    /// follows `{` is not a bound.
    fn bound(&mut self) -> Option<(u32, Option<u32>)> {
        let save = self.i;
        self.i += 1; // the `{`
        let mut n: u32 = 0;
        let mut any = false;
        while let Some(c) = self.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            n = n.checked_mul(10)?.checked_add(u32::from(c - b'0'))?;
            any = true;
            self.i += 1;
        }
        if !any {
            self.i = save;
            return None;
        }
        let max = if self.eat(b',') {
            let mut m: u32 = 0;
            let mut got = false;
            while let Some(c) = self.peek() {
                if !c.is_ascii_digit() {
                    break;
                }
                m = m.checked_mul(10)?.checked_add(u32::from(c - b'0'))?;
                got = true;
                self.i += 1;
            }
            if got { Some(m) } else { None }
        } else {
            Some(n)
        };
        if !self.eat(b'}') {
            self.i = save;
            return None;
        }
        // A bound that counts down is not a pattern.
        if let Some(m) = max {
            if m < n {
                return None;
            }
        }
        Some((n, max))
    }

    fn atom(&mut self) -> Option<Node> {
        match self.bump()? {
            b'(' => {
                self.ngroups += 1;
                let idx = self.ngroups;
                self.depth += 1;
                let inner = self.alt()?;
                if !self.eat(b')') {
                    return None;
                }
                self.depth -= 1;
                Some(Node::Group(idx, alloc::boxed::Box::new(inner)))
            }
            // Only reachable at depth 0 — `concat` stops at a `)` inside a
            // group — so this is the unmatched one, and it is a literal.
            b')' => Some(Node::Byte(b')')),
            b'[' => {
                let c = self.bracket()?;
                self.classes.push(c);
                Some(Node::Class((self.classes.len() - 1) as u16))
            }
            b'.' => Some(Node::Any),
            b'^' => Some(Node::Bol),
            b'$' => Some(Node::Eol),
            b'\\' => Some(Node::Byte(self.bump()?)),
            // A quantifier with nothing to quantify is a malformed pattern, and
            // pdfTeX answers -1 rather than guessing.
            b'*' | b'+' | b'?' => None,
            c => Some(Node::Byte(c)),
        }
    }

    fn bracket(&mut self) -> Option<Class> {
        let negated = self.eat(b'^');
        let mut ranges = Vec::new();
        // A `]` first is a literal `]`, not the end of the expression.
        let mut first = true;
        loop {
            let c = self.peek()?;
            if c == b']' && !first {
                self.i += 1;
                return Some(Class { negated, ranges });
            }
            first = false;
            self.i += 1;
            // `[:name:]`
            if c == b'[' && self.peek() == Some(b':') {
                let start = self.i + 1;
                let mut end = start;
                while end < self.p.len() && self.p[end] != b':' {
                    end += 1;
                }
                if end + 1 < self.p.len() && self.p[end + 1] == b']' {
                    ranges.extend(posix_class(&self.p[start..end])?);
                    self.i = end + 2;
                    continue;
                }
            }
            let lo = if c == b'\\' { self.bump()? } else { c };
            // `a-z`, but a `-` last in the expression is a literal `-`.
            if self.peek() == Some(b'-') && self.p.get(self.i + 1).is_some_and(|&n| n != b']') {
                self.i += 1;
                let mut hi = self.bump()?;
                if hi == b'\\' {
                    hi = self.bump()?;
                }
                if hi < lo {
                    return None;
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
    }
}

// ---- the compiler -----------------------------------------------------------

/// Repeating a subexpression means copying its program, so a bound is capped.
/// `a{1,60000}` is a legal pattern and a denial of service; pdfTeX's matcher has
/// the same problem and the same answer — refuse to compile it.
const MAX_REPEAT: u32 = 1000;

struct Compiler {
    prog: Vec<Inst>,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> usize {
        self.prog.push(i);
        self.prog.len() - 1
    }

    fn node(&mut self, n: &Node) -> Option<()> {
        match n {
            Node::Empty => {}
            Node::Byte(b) => {
                self.emit(Inst::Char(*b));
            }
            Node::Any => {
                self.emit(Inst::Any);
            }
            Node::Class(i) => {
                self.emit(Inst::Class(*i));
            }
            Node::Bol => {
                self.emit(Inst::Bol);
            }
            Node::Eol => {
                self.emit(Inst::Eol);
            }
            Node::Group(i, inner) => {
                self.emit(Inst::Save(i * 2));
                self.node(inner)?;
                self.emit(Inst::Save(i * 2 + 1));
            }
            Node::Cat(items) => {
                for it in items {
                    self.node(it)?;
                }
            }
            Node::Alt(branches) => {
                // Each branch but the last is guarded by a `Split`, and each
                // jumps to the end when it succeeds.
                let mut jumps = Vec::new();
                for (k, b) in branches.iter().enumerate() {
                    if k + 1 == branches.len() {
                        self.node(b)?;
                    } else {
                        let sp = self.emit(Inst::Split(0, 0));
                        let here = self.prog.len() as u32;
                        self.node(b)?;
                        jumps.push(self.emit(Inst::Jmp(0)));
                        let next = self.prog.len() as u32;
                        self.prog[sp] = Inst::Split(here, next);
                    }
                }
                let end = self.prog.len() as u32;
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node, min, max } => {
                if *min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
                    return None;
                }
                for _ in 0..*min {
                    self.node(node)?;
                }
                match max {
                    None => {
                        // `x*` after the mandatory copies.
                        let sp = self.emit(Inst::Split(0, 0));
                        let body = self.prog.len() as u32;
                        self.node(node)?;
                        self.emit(Inst::Jmp(sp as u32));
                        let out = self.prog.len() as u32;
                        self.prog[sp] = Inst::Split(body, out);
                    }
                    Some(m) => {
                        let mut splits = Vec::new();
                        for _ in *min..*m {
                            let sp = self.emit(Inst::Split(0, 0));
                            let body = self.prog.len() as u32;
                            self.node(node)?;
                            splits.push((sp, body));
                        }
                        let out = self.prog.len() as u32;
                        for (sp, body) in splits {
                            self.prog[sp] = Inst::Split(body, out);
                        }
                    }
                }
            }
        }
        Some(())
    }
}

/// Compile a POSIX ERE. `None` is pdfTeX's `-1`: the pattern is not one.
pub fn compile(pattern: &[u8], icase: bool) -> Option<Regex> {
    let mut p = Parser {
        p: pattern,
        i: 0,
        classes: Vec::new(),
        ngroups: 0,
        depth: 0,
    };
    let tree = p.alt()?;
    if p.i != pattern.len() {
        return None;
    }
    let mut c = Compiler { prog: Vec::new() };
    c.node(&tree)?;
    c.emit(Inst::Match);
    Some(Regex {
        prog: c.prog,
        classes: p.classes,
        groups: p.ngroups as usize,
        icase,
    })
}

impl Regex {
    /// The leftmost-longest match, POSIX-style. See the module docs for what
    /// "longest" costs and where the capture rule stops short.
    pub fn find(&self, text: &[u8]) -> Option<Match> {
        let slots = 2 * (self.groups + 1);
        for start in 0..=text.len() {
            let mut caps = alloc::vec![usize::MAX; slots];
            let mut best: Option<(usize, Vec<usize>)> = None;
            // `(pc, pos)` seen on this attempt. From a given instruction at a
            // given position the reachable ends are the same however you got
            // there, so revisiting can only repeat work — and pruning is what
            // makes `(a*)*` terminate rather than descend forever.
            let mut seen = alloc::vec![false; self.prog.len() * (text.len() + 1)];
            self.run(text, 0, start, start, &mut caps, &mut best, &mut seen);
            if let Some((end, caps)) = best {
                let groups = (1..=self.groups)
                    .map(|i| {
                        let (a, b) = (caps[2 * i], caps[2 * i + 1]);
                        (a != usize::MAX && b != usize::MAX).then_some((a, b))
                    })
                    .collect();
                return Some(Match { start, end, groups });
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        text: &[u8],
        pc: usize,
        pos: usize,
        start: usize,
        caps: &mut Vec<usize>,
        best: &mut Option<(usize, Vec<usize>)>,
        seen: &mut Vec<bool>,
    ) {
        let key = pc * (text.len() + 1) + pos;
        if seen[key] {
            return;
        }
        seen[key] = true;
        match self.prog[pc] {
            Inst::Char(b) => {
                if let Some(&c) = text.get(pos) {
                    let hit = if self.icase {
                        c.eq_ignore_ascii_case(&b)
                    } else {
                        c == b
                    };
                    if hit {
                        self.run(text, pc + 1, pos + 1, start, caps, best, seen);
                    }
                }
            }
            Inst::Any => {
                if pos < text.len() {
                    self.run(text, pc + 1, pos + 1, start, caps, best, seen);
                }
            }
            Inst::Class(i) => {
                if let Some(&c) = text.get(pos) {
                    if self.classes[i as usize].matches(c, self.icase) {
                        self.run(text, pc + 1, pos + 1, start, caps, best, seen);
                    }
                }
            }
            Inst::Split(a, b) => {
                self.run(text, a as usize, pos, start, caps, best, seen);
                self.run(text, b as usize, pos, start, caps, best, seen);
            }
            Inst::Jmp(a) => self.run(text, a as usize, pos, start, caps, best, seen),
            Inst::Save(n) => {
                let old = caps[n as usize];
                caps[n as usize] = pos;
                self.run(text, pc + 1, pos, start, caps, best, seen);
                caps[n as usize] = old;
            }
            Inst::Bol => {
                if pos == 0 {
                    self.run(text, pc + 1, pos, start, caps, best, seen);
                }
            }
            Inst::Eol => {
                if pos == text.len() {
                    self.run(text, pc + 1, pos, start, caps, best, seen);
                }
            }
            Inst::Match => {
                // Longest wins, not first — this is the whole of POSIX
                // semantics, and the reason the search does not return here.
                if best.as_ref().is_none_or(|(e, _)| pos > *e) {
                    let mut c = caps.clone();
                    c[0] = start;
                    c[1] = pos;
                    *best = Some((pos, c));
                }
            }
        }
        // The mark is left standing: within one attempt this instruction at this
        // position has been fully explored.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of these was measured against `pdftex -ini -etex` before it was
    /// written down. The comment on each is what pdfTeX printed.
    fn m(pat: &str, text: &str) -> Option<Match> {
        compile(pat.as_bytes(), false)?.find(text.as_bytes())
    }
    fn span(pat: &str, text: &str) -> Option<(usize, alloc::string::String)> {
        let r = m(pat, text)?;
        Some((
            r.start,
            alloc::string::String::from_utf8_lossy(&text.as_bytes()[r.start..r.end]).into_owned(),
        ))
    }

    /// `\pdfmatch{(a|ab)}{ab}` → `0->ab`. A leftmost-*first* engine says `a`,
    /// and that one character is the whole difference between POSIX and Perl.
    #[test]
    fn alternation_takes_the_longest_not_the_first() {
        assert_eq!(span("(a|ab)", "ab"), Some((0, "ab".into())));
    }

    /// microtype's integer test: `\pdfmatch{^-*[0-9]+ *$}{-42  }` → 1.
    #[test]
    fn microtypes_integer_pattern() {
        assert!(m("^-*[0-9]+ *$", "-42  ").is_some());
        assert!(m("^-*[0-9]+ *$", "-42x").is_none());
        assert!(m("^-*[0-9]+ *$", "").is_none());
    }

    /// microtype's number test, with the nested optional group.
    /// `\pdflastmatch1` = `0->12.5`, `2` = `2->.5`, `3` = `-1->`.
    #[test]
    fn microtypes_number_pattern_captures() {
        let r = m("^([0-9]+([.,][0-9]+)?|[.,][0-9]+)", "12.5pt").expect("matches");
        assert_eq!(r.groups[0], Some((0, 4)));
        assert_eq!(r.groups[1], Some((2, 4)));
    }

    /// hycolor's, which is three of these separated by spaces and anchored.
    #[test]
    fn hycolors_rgb_pattern() {
        let num = r"(0*1\.0*|0*1|0+\.?[0-9]*|\.[0-9]+)";
        let pat = alloc::format!("^{num} {num} {num}$");
        let r = compile(pat.as_bytes(), false)
            .expect("compiles")
            .find(b"1 0.5 .25")
            .expect("matches");
        assert_eq!(r.start, 0);
        assert_eq!(r.end, 9);
        assert_eq!(r.groups[0], Some((0, 1)));
        assert_eq!(r.groups[1], Some((2, 5)));
        assert_eq!(r.groups[2], Some((6, 9)));
        assert!(
            compile(pat.as_bytes(), false)
                .unwrap()
                .find(b"1 2 3")
                .is_none(),
            "2 is not a legal rgb component"
        );
    }

    /// `\pdfmatch{a{2,3}}{aaaa}` → `0->aaa`: greedy, and it stops at the bound.
    #[test]
    fn a_bound_is_greedy_up_to_its_maximum() {
        assert_eq!(span("a{2,3}", "aaaa"), Some((0, "aaa".into())));
        assert_eq!(span("a{2}", "aaaa"), Some((0, "aa".into())));
        assert!(m("a{5,}", "aaaa").is_none());
    }

    /// `\pdfmatch{[^a-c]+}{abcXYZabc}` → `3->XYZ`.
    #[test]
    fn a_negated_class_finds_the_leftmost_run() {
        assert_eq!(span("[^a-c]+", "abcXYZabc"), Some((3, "XYZ".into())));
    }

    /// `\pdfmatch{[[:digit:]]+}{ab123cd}` → `2->123`.
    #[test]
    fn posix_named_classes() {
        assert_eq!(span("[[:digit:]]+", "ab123cd"), Some((2, "123".into())));
        assert_eq!(span("[[:alpha:]]+", "12abc34"), Some((2, "abc".into())));
    }

    /// `\pdfmatch{x*}{yyy}` → `1`, `\pdflastmatch0` → `0->`. An empty match at
    /// the start *is* a match, and a matcher that reported "no" here would send
    /// microtype down the wrong branch.
    #[test]
    fn an_empty_match_at_the_start_counts() {
        assert_eq!(span("x*", "yyy"), Some((0, "".into())));
        assert_eq!(span("", "abc"), Some((0, "".into())));
        assert_eq!(span("^$", ""), Some((0, "".into())));
    }

    /// `\pdfmatch{(x)?y}{y}` → group 1 is `-1->`: it took no part.
    #[test]
    fn a_group_that_did_not_participate_is_none() {
        let r = m("(x)?y", "y").expect("matches");
        assert_eq!(r.groups[0], None);
    }

    /// `\pdfmatch{abc}{xxABCxx}` → 0, and with `icase` → 1 at `2->ABC`.
    #[test]
    fn icase_folds_both_ways() {
        assert!(m("abc", "xxABCxx").is_none());
        let r = compile(b"abc", true)
            .unwrap()
            .find(b"xxABCxx")
            .expect("icase");
        assert_eq!((r.start, r.end), (2, 5));
        let r = compile(b"[a-c]+", true)
            .unwrap()
            .find(b"xxABCxx")
            .expect("icase");
        assert_eq!((r.start, r.end), (2, 5));
    }

    /// `\pdfmatch{[}{x}` → `-1`. A pattern that will not compile is not a
    /// pattern that matches nothing.
    #[test]
    fn a_malformed_pattern_does_not_compile() {
        assert!(compile(b"[", false).is_none());
        assert!(compile(b"(a", false).is_none());
        assert!(compile(b"*a", false).is_none());
        assert!(compile(b"+a", false).is_none());
        assert!(compile(b"?a", false).is_none());
        assert!(compile(b"[z-a]", false).is_none());
        // A `{` after an atom must be a bound, and glibc will not fall back to
        // a literal brace: all three of these are `-1` in pdfTeX.
        assert!(compile(b"a{3,1}", false).is_none());
        assert!(compile(b"a{}", false).is_none());
        assert!(compile(b"a{b}", false).is_none());
    }

    /// An *unmatched* `)` is not an error, it is a literal:
    /// `\pdfmatch{a)b}{a)b}` → `0->a)b`, `\pdfmatch{)}{x)y}` → `1->)`.
    /// A bare `)` reads as an error in most regex flavours; glibc's ERE does
    /// not, and pdfTeX is glibc.
    #[test]
    fn an_unmatched_close_paren_is_a_literal() {
        assert_eq!(span("a)b", "a)b"), Some((0, "a)b".into())));
        assert_eq!(span(")", "x)y"), Some((1, ")".into())));
        assert_eq!(span("a)", "a"), None);
    }

    /// `\pdfmatch{(|a)}{a}` → `0->a`: an empty alternative is legal, and the
    /// longest still wins over it.
    #[test]
    fn an_empty_alternative_is_legal_and_loses_to_a_longer_one() {
        assert_eq!(span("(|a)", "a"), Some((0, "a".into())));
    }

    /// `^` and `$` are anchors wherever they appear, so `a^b` cannot match the
    /// text `a^b` — pdfTeX answers 0, not 1.
    #[test]
    fn anchors_are_anchors_in_the_middle_too() {
        assert!(m("a^b", "a^b").is_none());
        assert!(m("a$b", "a$b").is_none());
        assert_eq!(span("^", "abc"), Some((0, "".into())));
    }

    /// `\pdfmatch{a\|b}{a|b}` → `0->a|b`: a backslash makes the next byte a
    /// literal, whatever it is.
    #[test]
    fn a_backslash_escapes_a_metacharacter() {
        assert_eq!(span(r"a\|b", "a|b"), Some((0, "a|b".into())));
        assert_eq!(span(r"a\.c", "abc"), None);
        assert_eq!(span(r"a\.c", "a.c"), Some((0, "a.c".into())));
    }

    /// `(a*)*` against anything must terminate. The `(pc, pos)` pruning is what
    /// makes that true, and it is the one place where a plain backtracker hangs.
    #[test]
    fn an_empty_body_repetition_terminates() {
        assert_eq!(span("(a*)*", "aaa"), Some((0, "aaa".into())));
        assert_eq!(span("(a*)*b", "aaab"), Some((0, "aaab".into())));
    }

    /// `]` first in a bracket expression is a literal, and `-` last is too.
    #[test]
    fn brackets_special_positions() {
        assert_eq!(span("[]]", "x]y"), Some((1, "]".into())));
        assert_eq!(span("[a-]+", "-a-"), Some((0, "-a-".into())));
    }
}
