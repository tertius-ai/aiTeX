//! Liang's hyphenation algorithm — `tex.web` §919–§966.
//!
//! A pattern is a short letter sequence carrying a digit at each position,
//! like `.ach4` or `as1p`. A `.` matches a word boundary. To hyphenate a word,
//! every pattern that occurs anywhere in `.word.` contributes its digits to the
//! inter-letter positions it covers, and the *largest* value wins at each
//! position. An odd value means a break is allowed there.
//!
//! The rule that odd means yes and even means no is what lets a later pattern
//! *forbid* what an earlier one allowed, which is how a few thousand patterns
//! encode a language's exceptions without listing words.
//!
//! Two things about the shape here are deliberate:
//!
//! - Patterns are stored keyed by their letters, values separately. TeX packs
//!   them into a linked trie because it was fitting the whole thing into 1980s
//!   memory; the packing is not part of the algorithm, and reproducing it would
//!   make the interesting part unreadable.
//! - Exceptions from `\hyphenation{...}` are checked first and completely
//!   override the patterns, as TeX does. They are how a document fixes a word
//!   the patterns get wrong.

use crate::node::Node;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// The patterns and exceptions of every language.
///
/// `tex.web` keeps one trie with the language as the first character of every
/// key — §963's `hc[0]:=cur_lang` — so patterns loaded under one `\language`
/// are invisible under another. §1034 is what `cur_lang` means:
///
/// ```text
/// if language<=0 then cur_lang:=0
/// else if language>255 then cur_lang:=0
/// else cur_lang:=language
/// ```
///
/// We kept one set for all of them, so patterns loaded under `\language5`
/// hyphenated a paragraph set under `\language0`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Hyphenator {
    /// Pattern letters (including any `.`) to the value at each position.
    /// `values[i]` sits *before* letter `i`, and there is one more value than
    /// there are letters.
    patterns: BTreeMap<(u8, String), Vec<u8>>,
    /// Words `\hyphenation` gave explicitly, to their break positions.
    exceptions: BTreeMap<(u8, String), Vec<usize>>,
    /// The longest pattern, so matching need not try every length.
    max_len: usize,
}

impl Hyphenator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty() && self.exceptions.is_empty()
    }

    /// Number of words stored by `\hyphenation`, for the semantic part of
    /// §1327's format inventory. Unlike the packed trie's allocation figures,
    /// this count means the same thing in both representations.
    pub(crate) fn exception_count(&self) -> usize {
        self.exceptions.len()
    }

    /// Add one pattern in TeX's notation, e.g. `.ach4` or `a1bc3d`.
    ///
    /// Returns true when a pattern for the same letters was already there.
    /// `tex.web` §963 reports that and keeps the *new* values:
    ///
    /// ```text
    /// if trie_o[q]<>min_quarterword then
    ///   begin print_err("Duplicate pattern");
    ///   help1("(See Appendix H.)"); error;
    ///   end;
    /// trie_o[q]:=v;
    /// ```
    ///
    /// The overwrite was already what a map insert does; the report was not.
    /// It matters more than it looks: the caller must fold each pattern
    /// character through `\lccode` first (§962), or `bb1` and `0B2B0` are two
    /// keys where TeX has one — and the second, all-even, is what decides.
    pub fn add_pattern(&mut self, lang: u8, pattern: &str) -> bool {
        let mut letters = String::new();
        // One more value than letters: a digit may precede the first letter.
        let mut values: Vec<u8> = Vec::new();
        values.push(0);
        for c in pattern.chars() {
            if let Some(d) = c.to_digit(10) {
                // This digit belongs at the position just reached.
                let last = values.len() - 1;
                values[last] = d as u8;
            } else {
                letters.push(c);
                values.push(0);
            }
        }
        self.add_pattern_parts(lang, letters, values)
    }

    /// Store a pattern that the engine has already scanned according to §962.
    ///
    /// This is separate from [`Self::add_pattern`] because TeX's scanner gives
    /// consecutive digits a surprising meaning: after one digit supplies a
    /// hyphen level, the next is treated as a letter and folded through
    /// `\lccode`. A string parser cannot distinguish that digit-as-letter from
    /// ordinary pattern notation after the fact.
    pub(crate) fn add_pattern_parts(&mut self, lang: u8, letters: String, values: Vec<u8>) -> bool {
        if letters.is_empty() {
            return false;
        }
        self.max_len = self.max_len.max(letters.chars().count());
        // §961 computes the trie op from the values and leaves it at
        // `min_quarterword` — no op at all — when every one of them is zero:
        //
        //     l:=k; v:=min_quarterword;
        //     loop@+begin if hyf[l]<>0 then v:=new_trie_op(k-l,hyf[l],v);
        //       if l>0 then decr(l)@+else goto done1;
        //       end;
        //
        // and §963's duplicate test is `trie_o[q]<>min_quarterword`. So a
        // pattern whose values are all zero is not a duplicate of anything and
        // nothing is a duplicate of it — `bb`, `bb1`, `0B2B0` is *one*
        // complaint, not two. The assignment is unconditional either way, so an
        // all-zero pattern clears whatever was there.
        let had_op = self
            .patterns
            .insert((lang, letters), values.clone())
            .is_some_and(|old| old.iter().any(|&v| v != 0));
        had_op && values.iter().any(|&v| v != 0)
    }

    /// Add an exception in TeX's notation, e.g. `man-u-script`.
    pub fn add_exception(&mut self, lang: u8, word: &str) {
        let mut letters = String::new();
        let mut breaks = Vec::new();
        for c in word.chars() {
            if c == '-' {
                breaks.push(letters.chars().count());
            } else {
                letters.push(c.to_lowercase().next().unwrap_or(c));
            }
        }
        if !letters.is_empty() {
            self.exceptions.insert((lang, letters), breaks);
        }
    }

    /// Positions in `word` where a hyphen may be inserted.
    ///
    /// A position `k` means "between the k-th and (k+1)-th letter", so `k` runs
    /// from 1 to `len-1`. `left` and `right` are `\lefthyphenmin` and
    /// `\righthyphenmin`: TeX refuses to leave fewer than that many letters on
    /// either side, which is why `a-bout` and `walk-s` never happen.
    pub fn hyphenate(&self, lang: u8, word: &str, left: usize, right: usize) -> Vec<usize> {
        let lower: String = word.chars().flat_map(|c| c.to_lowercase()).collect();
        let n = lower.chars().count();
        // A word shorter than the two minimums together has nowhere legal to
        // break, so there is nothing to compute.
        if n < left + right || n == 0 {
            return Vec::new();
        }

        if let Some(breaks) = self.exceptions.get(&(lang, lower.clone())) {
            return breaks
                .iter()
                .copied()
                .filter(|k| *k >= left && n - *k >= right)
                .collect();
        }
        if self.patterns.is_empty() {
            return Vec::new();
        }

        // `.word.` — the dots are what let a pattern anchor to a boundary.
        let padded: Vec<char> = core::iter::once('.')
            .chain(lower.chars())
            .chain(core::iter::once('.'))
            .collect();
        // One value per gap in the padded word.
        let mut values = alloc::vec![0u8; padded.len() + 1];

        for start in 0..padded.len() {
            let remaining = padded.len() - start;
            for len in 1..=self.max_len.min(remaining) {
                let slice: String = padded[start..start + len].iter().collect();
                let Some(pv) = self.patterns.get(&(lang, slice)) else {
                    continue;
                };
                // Largest value wins at each position.
                for (i, v) in pv.iter().enumerate() {
                    let at = start + i;
                    if at < values.len() && *v > values[at] {
                        values[at] = *v;
                    }
                }
            }
        }

        // `values[i]` sits before padded letter `i`; padded letter 1 is the
        // word's first letter, so the gap after word letter `k` is
        // `values[k + 1]`.
        (1..n)
            .filter(|k| k >= &left && n - k >= right)
            .filter(|k| values.get(k + 1).is_some_and(|v| v % 2 == 1))
            .collect()
    }
}

/// Insert discretionaries into a horizontal list wherever the patterns allow a
/// break.
///
/// Shared by both engines: this is a function of a node list and a
/// [`Hyphenator`], with nothing of either engine's state in it. Returns the new
/// list and how many discretionaries went in.
#[allow(clippy::too_many_arguments)]
pub fn hyphenate_list(
    h: &Hyphenator,
    lang: u8,
    list: &[Node],
    left: usize,
    right: usize,
    hyphen_char: &dyn Fn(usize) -> i64,
    fonts: &[crate::tfm::TfmFont],
    uchyph: bool,
    lccode: &dyn Fn(u32) -> u32,
) -> (Vec<Node>, usize, Vec<(usize, usize, u32)>) {
    let _p = crate::profile::Phase::enter(crate::profile::P_HYPHENATE);
    let mut inserted = 0usize;
    let mut missing_hyphens = Vec::new();
    if h.is_empty() {
        return (list.to_vec(), 0, missing_hyphens);
    }
    let mut out: Vec<Node> = Vec::with_capacity(list.len());
    // A run of nodes that make up one word. Kerns count as *inside* a word:
    // cmr10 puts one between many letter pairs, and treating one as a boundary
    // splits "hyphenation" into fragments too short to hyphenate — which is why
    // nothing was ever hyphenated.
    let mut run: Vec<Node> = Vec::new();

    /// The letters a character stands for: itself, or — if it is a ligature —
    /// the letters it was made from, recursively.
    ///
    /// **Bounded**, because a `.tfm` is untrusted input and nothing in the
    /// format prevents a ligature program from being *cyclic*: a character
    /// whose ligature decomposition contains itself decomposes forever. TRIP's
    /// font is built that way on purpose, and it overflowed the stack and
    /// aborted the process — the engine must fail a document, never the run.
    ///
    /// A real chain is two or three deep (`ffi` is `ff` + `i`, and `ff` is `f`
    /// + `f`), so the limit costs nothing that exists. At the limit the
    /// character stands for itself, which is what a font with no ligature
    /// program would have said.
    ///
    /// The bound has to be on the **output**, not just the depth: decomposition
    /// branches in two, so a depth of 16 is 65,536 letters from one character
    /// even though the recursion terminates. That is what happened when only
    /// the depth was capped — the crash became a hang, with the whole run
    /// inside this function building a word millions of letters long.
    const MAX_LIG_DEPTH: u32 = 8;
    const MAX_LIG_LETTERS: usize = 32;
    const MAX_HYPHEN_WORD: usize = 63;
    fn letters_of(
        code: u32,
        font: usize,
        fonts: &[crate::tfm::TfmFont],
        out: &mut Vec<u32>,
        depth: u32,
        limit: usize,
    ) {
        // `limit` is where this character's letters must stop. Checked first,
        // so a cyclic font costs a bounded amount of work per character rather
        // than an unbounded amount of memory.
        match fonts.get(font).and_then(|f| f.unligature(code)) {
            // A component equal to the character itself is a cycle of length
            // one and the common case; the depth and output caps catch the rest.
            Some((a, b))
                if depth < MAX_LIG_DEPTH && out.len() < limit && a != code && b != code =>
            {
                letters_of(a, font, fonts, out, depth + 1, limit);
                letters_of(b, font, fonts, out, depth + 1, limit);
            }
            _ => out.push(code),
        }
    }

    fn represented_letter_count(node: &Node, fonts: &[crate::tfm::TfmFont]) -> usize {
        let Node::Char { code, font, lig } = node else {
            return 0;
        };
        if let Some(raw) = lig {
            return raw
                .iter()
                .filter(|code| **code != crate::node::LIG_BOUNDARY)
                .count();
        }
        let mut letters = Vec::new();
        letters_of(*code, *font, fonts, &mut letters, 0, MAX_LIG_LETTERS);
        letters.len()
    }

    /// Turn metric-layer shaping output back into horizontal-list nodes.
    fn shaped_nodes(shaped: Vec<crate::tfm::ShapedItem>, chars: &[u32], font: usize) -> Vec<Node> {
        let mut out = Vec::with_capacity(shaped.len());
        for item in shaped {
            match item {
                crate::tfm::ShapedItem::Glyph {
                    code,
                    prefix,
                    source,
                    ligature,
                    left_hit,
                    right_hit,
                    ..
                } => {
                    let lig = if ligature {
                        let mut original = Vec::with_capacity(
                            prefix.len()
                                + source.len()
                                + usize::from(left_hit)
                                + usize::from(right_hit),
                        );
                        if left_hit {
                            original.push(crate::node::LIG_BOUNDARY);
                        }
                        original.extend(prefix);
                        for i in source {
                            if let Some(&code) = chars.get(i) {
                                original.push(code);
                            }
                        }
                        if right_hit {
                            original.push(crate::node::LIG_BOUNDARY);
                        }
                        Some(original.into_boxed_slice())
                    } else {
                        None
                    };
                    out.push(Node::Char { code, font, lig });
                }
                crate::tfm::ShapedItem::Kern { width, .. } => {
                    out.push(Node::Kern(width, crate::node::kern_sub::NORMAL));
                }
            }
        }
        out
    }

    /// Reconstitute a complete branch by repeatedly taking §902 cut prefixes.
    fn reconstitute_all(
        chars: &[u32],
        font: usize,
        f: &crate::tfm::TfmFont,
        bchar: Option<u32>,
        init: Option<crate::tfm::ReconstituteInit<'_>>,
    ) -> Vec<Node> {
        let breaks = alloc::vec![false; chars.len()];
        let mut out = Vec::new();
        let mut j = 0usize;
        while j < chars.len() {
            let step = f.reconstitute(
                chars,
                j,
                bchar,
                None,
                &breaks,
                (j == 0).then_some(init).flatten(),
            );
            let next = step.last.saturating_add(1).max(j + 1);
            out.extend(shaped_nodes(step.items, chars, font));
            j = next;
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn flush(
        h: &Hyphenator,
        run: &mut Vec<Node>,
        out: &mut Vec<Node>,
        lang: u8,
        left: usize,
        right: usize,
        hyphen_char: &dyn Fn(usize) -> i64,
        inserted: &mut usize,
        missing_hyphens: &mut Vec<(usize, usize, u32)>,
        fonts: &[crate::tfm::TfmFont],
        uchyph: bool,
        lccode: &dyn Fn(u32) -> u32,
        after_glue: bool,
        rest: &[Node],
    ) {
        if run.is_empty() {
            return;
        }
        // §902 `@<Check that the nodes following hb permit hyphenation…@>`:
        //
        //     loop@+ begin if not(is_char_node(s)) then
        //       case type(s) of
        //       ligature_node: do_nothing;
        //       kern_node: if subtype(s)<>normal then goto done4;
        //       whatsit_node,glue_node,penalty_node,ins_node,adjust_node,mark_node:
        //         goto done4;
        //       othercases goto done1
        //       endcases;
        //     s:=link(s);
        //     end;
        //
        // `done4` carries on with the hyphenation and `done1` abandons it. So a
        // word followed by a *discretionary* is not hyphenated at all — which
        // is why plain's `inter\-word` keeps its `inter` whole. Nor is one
        // followed by a box, a rule or a math node. We asked nothing about what
        // came after, and hyphenated `in-ter` where TeX leaves it alone.
        // The loop *walks*: a character or a ligature after the word is
        // stepped over — a comma, a full stop — and so is an ordinary kern.
        // What stops the walk favourably is glue or one of the node kinds that
        // cannot affect the letters; what abandons is a discretionary, a box, a
        // rule or a math node.
        let mut may = true;
        for n in rest {
            match n {
                Node::Char { .. } => continue,
                Node::Kern(_, sub) if *sub == crate::node::kern_sub::NORMAL => continue,
                Node::Kern(..)
                | Node::Glue(_)
                | Node::Leaders { .. }
                | Node::Penalty(_)
                | Node::Language { .. }
                | Node::Whatsit(_)
                | Node::WriteOut(_)
                | Node::Ins(_)
                | Node::Adjust { .. }
                | Node::Mark { .. } => break,
                _ => {
                    may = false;
                    break;
                }
            }
        }
        if !may {
            out.append(run);
            return;
        }
        // §894: only a word that follows glue is offered to `hyphenate`.
        if !after_glue {
            out.append(run);
            return;
        }
        let font = run
            .iter()
            .find_map(|n| match n {
                Node::Char { font, .. } => Some(*font),
                _ => None,
            })
            .unwrap_or(0);
        // §896 `done2`, and it is the *word's* font that decides:
        //
        //     hyf_char:=hyphen_char[hf];
        //     if hyf_char<0 then goto done1;
        //     if hyf_char>255 then goto done1;
        let hc = hyphen_char(font);
        if !(0..=255).contains(&hc) {
            out.append(run);
            return;
        }
        let hyphen_char = hc as u32;
        // Find the last actual letter. Nodes after it can only be normal TFM
        // kerns: §897 advances `hb` across them too, so a successful
        // hyphenation replaces them together with the letters. They are split
        // here only so the no-hyphens exits below can restore the original run
        // byte for byte.
        let last_letter = run.iter().rposition(|n| matches!(n, Node::Char { .. }));
        let tail: Vec<Node> = match last_letter {
            Some(i) => run.split_off(i + 1),
            None => {
                out.append(run);
                return;
            }
        };
        // The word as *letters*: a ligature covers more than one.
        let mut text = alloc::string::String::new();
        let mut codes: Vec<u32> = Vec::new();
        for n in run.iter() {
            if let Node::Char { code, font, lig } = n {
                if let Some(original) = lig {
                    // §897 reads `lig_ptr`, which is precisely the consumed
                    // raw input recorded on the node. Generated retain-both
                    // ligatures have an empty list and contribute no letters;
                    // boundary subtype markers are diagnostic metadata only.
                    codes.extend(
                        original
                            .iter()
                            .copied()
                            .filter(|c| *c != crate::node::LIG_BOUNDARY),
                    );
                } else {
                    let limit = codes.len() + MAX_LIG_LETTERS;
                    letters_of(*code, *font, fonts, &mut codes, 0, limit);
                }
            }
        }
        // §896 decides whether the word may be hyphenated at all, from its
        // *first* letter:
        //
        //     if lc_code(c)=0 then goto done1;
        //     if (lc_code(c)=c)or(uc_hyph>0) then goto done2 else goto done1;
        //
        // A character with no `\lccode` is not a letter and stops the word; one
        // whose `\lccode` is not itself is a capital, and a word beginning with
        // a capital is hyphenated only when `\uchyph` is positive. We used
        // Unicode's own lowercasing and never asked, so `\uchyph=0` hyphenated
        // anyway — trip.tex sets it to zero at line 200 for exactly this.
        match codes.first() {
            Some(&c0) => {
                let lc = lccode(c0);
                if lc == 0 || (lc != c0 && !uchyph) {
                    out.append(run);
                    out.extend(tail);
                    return;
                }
            }
            None => {
                out.append(run);
                out.extend(tail);
                return;
            }
        }
        // The pattern text is the word's `\lccode` image (§892's `hu`/`hc`),
        // not its Unicode lowercase: `\lccode` is what a document sets to say
        // which characters are letters and what each one folds to.
        for &c in &codes {
            let lc = lccode(c);
            if let Some(c) = char::from_u32(if lc == 0 { c } else { lc }) {
                text.push(c);
            }
        }
        let breaks = h.hyphenate(lang, &text, left, right);
        if breaks.is_empty() {
            out.append(run);
            out.extend(tail);
            return;
        }
        // §913 rebuilds one cut prefix at a time. Candidate hyphens are tested
        // while that cursor is live; once one changes the translation, the
        // major and minor branches advance independently until their input
        // positions synchronize again (§§914–918).
        let Some(f) = fonts.get(font) else {
            out.append(run);
            out.extend(tail);
            return;
        };
        // §§581, 915 ask `new_character` for the candidate hyphen each time a
        // discretionary is built. A missing glyph is warned about and omitted
        // from the pre-break branch; it is not a zero-metric character. The
        // rest of that branch must still be reconstituted because ligatures
        // before the break may differ from the unbroken word.
        let hyphen_exists = f.has_char(hyphen_char);
        let word_left_boundary = run.first().is_some_and(|n| match n {
            Node::Char { lig: Some(raw), .. } => {
                raw.first().copied() == Some(crate::node::LIG_BOUNDARY)
            }
            _ => false,
        });
        let font_boundary = run.last().is_some_and(|n| match n {
            Node::Char { lig: Some(raw), .. } => {
                raw.last().copied() == Some(crate::node::LIG_BOUNDARY)
            }
            _ => false,
        }) || tail.last().is_some_and(
            |n| matches!(n, Node::Kern(_, sub) if *sub == crate::node::kern_sub::NORMAL),
        );

        // §903 saves the character or ligature node `ha` immediately before
        // the first letter when it belongs to the word's font.  Its displayed
        // character goes in `hu[0]`, while the original character list and
        // left-boundary subtype seed `reconstitute`'s initial translation.
        // The node itself is removed: the reconstructed major branch replaces
        // it together with the word.  This is how punctuation can propagate
        // through an arbitrarily long retained-operand ligature program.
        let mut init_raw = Vec::new();
        let mut init_lig = false;
        let mut init_left_hit = false;
        let mut initial_code = None;
        if let Some(Node::Char {
            code,
            font: context_font,
            lig,
        }) = out.last()
        {
            if *context_font == font {
                initial_code = Some(*code);
                if let Some(raw) = lig {
                    init_lig = true;
                    init_left_hit = raw.first().copied() == Some(crate::node::LIG_BOUNDARY);
                    init_raw.extend(
                        raw.iter()
                            .copied()
                            .filter(|c| *c != crate::node::LIG_BOUNDARY),
                    );
                    // A left-boundary ligature with no character provenance is
                    // reconstructed from the boundary program itself (§903's
                    // `init_list=null` special case).
                    if init_raw.is_empty() && init_left_hit {
                        initial_code = Some(crate::node::LIG_BOUNDARY);
                        init_lig = false;
                        init_left_hit = false;
                    }
                } else {
                    init_raw.push(*code);
                }
                out.pop();
            }
        }
        if initial_code.is_none() && word_left_boundary {
            initial_code = Some(crate::node::LIG_BOUNDARY);
        }

        let mut hu = Vec::with_capacity(codes.len() + usize::from(initial_code.is_some()));
        if let Some(code) = initial_code {
            hu.push(code);
        }
        hu.extend_from_slice(&codes);
        let offset = hu.len() - codes.len();
        let mut hyf = alloc::vec![false; hu.len()];
        for i in breaks {
            if i > 0 && i < codes.len() {
                hyf[offset + i - 1] = true;
            }
        }
        let font_bchar = f.boundary_char();
        let make_init = || {
            (!init_raw.is_empty() || init_lig).then_some(crate::tfm::ReconstituteInit {
                raw: &init_raw,
                ligature: init_lig,
                left_hit: init_left_hit,
            })
        };
        // §897's `hyf_bchar` is not merely the font's implicit boundary.
        // Before testing `\lccode`, TeX saves the character that stopped the
        // word. Thus a same-font full stop is supplied to `reconstitute` as a
        // lookahead even though it is not among the nodes being replaced. This
        // recreates a TFM kern such as Latin Modern bold's `y.` after an
        // intervening `\unskip` split the original character run. A normal
        // kern or a right-hit ligature at the end instead supplies the font's
        // own boundary character.
        let stopped_char = rest.first().and_then(|n| match n {
            Node::Char {
                code,
                font: next_font,
                lig,
            } if *next_font == font => lig
                .as_ref()
                .and_then(|raw| {
                    raw.iter()
                        .copied()
                        .find(|c| *c != crate::node::LIG_BOUNDARY)
                })
                .or(Some(*code)),
            _ => None,
        });
        let bchar = stopped_char.or_else(|| font_boundary.then(|| f.boundary_char()).flatten());
        let mut j = 0usize;
        while j < hu.len() {
            let mut l = j;
            let step = f.reconstitute(
                &hu,
                j,
                bchar,
                Some(hyphen_char),
                &hyf,
                (j == 0).then(make_init).flatten(),
            );
            j = step.last.saturating_add(1).max(j + 1);
            let mut major = shaped_nodes(step.items, &hu, font);
            let mut passed = step.hyphen_passed;
            if passed.is_none() {
                out.append(&mut major);
                if j > 0 && hyf.get(j - 1).copied().unwrap_or(false) {
                    l = j;
                    passed = Some(j - 1);
                }
            }

            while let Some(i) = passed {
                hyf[i] = false;

                // §915 temporarily replaces hu[i+1] by the hyphen and
                // reconstitutes hu[l..i+1] with the font boundary after it.
                let mut pre_chars = if l <= i {
                    hu[l..=i].to_vec()
                } else {
                    Vec::new()
                };
                if hyphen_exists {
                    pre_chars.push(hyphen_char);
                } else {
                    missing_hyphens.push((out.len(), font, hyphen_char));
                }
                let pre = reconstitute_all(
                    &pre_chars,
                    font,
                    f,
                    font_bchar,
                    (l == 0).then(make_init).flatten(),
                );

                // §916 starts the new line with the implicit left boundary,
                // then advances its branch until it catches the major cursor.
                let mut minor_l = i + 1;
                let mut post = Vec::new();
                let mut first_post_chars = None;
                if f.left_boundary_start().is_some() {
                    minor_l = minor_l.saturating_sub(1);
                    let mut chars = hu.clone();
                    chars[minor_l] = crate::node::LIG_BOUNDARY;
                    first_post_chars = Some(chars);
                }
                // §916 alternates the two branches until their input cursors
                // are equal. A post-break cut prefix can run past the major
                // cursor; advancing the major branch can then run past the
                // post-break cursor in turn, so this is deliberately one
                // joint loop rather than one pass over each side. TRIP's
                // `BBBBBB` does exactly that: stopping after the first pair of
                // loops omitted the final `B` from the post-break branch.
                while minor_l != j {
                    if minor_l < j {
                        let post_chars = first_post_chars.take().unwrap_or_else(|| hu.clone());
                        let post_step =
                            f.reconstitute(&post_chars, minor_l, bchar, None, &hyf, None);
                        minor_l = post_step.last.saturating_add(1).max(minor_l + 1);
                        post.extend(shaped_nodes(post_step.items, &post_chars, font));
                    } else if j < hu.len() {
                        let major_step = f.reconstitute(&hu, j, bchar, None, &hyf, None);
                        j = major_step.last.saturating_add(1).max(j + 1);
                        major.extend(shaped_nodes(major_step.items, &hu, font));
                    } else {
                        break;
                    }
                }

                let replace = major.len();
                // §918's node format has a seven-bit replacement count. TeX
                // discards the discretionary when synchronization expanded a
                // pathological ligature branch beyond 127 nodes; TRIP makes a
                // 130-node branch specifically to verify this guard.
                if replace <= 127 {
                    *inserted += 1;
                    out.push(Node::Disc(alloc::boxed::Box::new(crate::node::Disc {
                        pre,
                        post,
                        replace,
                    })));
                }
                out.append(&mut major);

                // §914 may place a second independent discretionary exactly
                // where both branches synchronized.
                if j > 0 && hyf.get(j - 1).copied().unwrap_or(false) {
                    l = j;
                    passed = Some(j - 1);
                    major = Vec::new();
                } else {
                    passed = None;
                }
            }
        }
        // A successful reconstruction already regenerated any trailing normal
        // kern that belonged to `ha..hb`; appending `tail` would duplicate it.
        run.clear();
    }

    // `tex.web` §894's header, which is the whole precondition:
    //
    //     When the following code is activated, the |line_break| procedure is
    //     in its second pass, and |cur_p| points to a glue node.
    //
    // §866 reaches it only for a glue node, so a word not preceded by glue is
    // never hyphenated — the first word of a paragraph most of all, since what
    // precedes it is the `\parindent` box or nothing. §896 then skips normal
    // kerns and whatsits on its way to the first letter, so those do not break
    // the chain.
    //
    // We hyphenated every run of letters, which put discretionaries in the
    // first word of every paragraph TeX leaves whole.
    let mut after_glue = false;
    // §897 stores at most 63 characters in `hu`: `if hn=63 then goto done3`.
    // The remainder is stepped over after the glue's single hyphenation
    // attempt has been spent. Without that bound a long token whose punctuation
    // has a nonzero `\lccode` acquires breakpoints TeX never considers.
    let mut run_letters = 0usize;
    // §866 disables `auto_breaking` at a math-on node and restores it at the
    // matching math-off node. §894 can call the hyphenator only from a glue
    // breakpoint reached while auto breaking is enabled, so math material is
    // never a word candidate. In particular, `url` typesets addresses in math
    // mode with zero-width math glue between punctuation; treating that glue
    // as an ordinary interword space hyphenated `modelcontextproto-col.io`,
    // where TeX keeps the URL intact.
    let mut auto_breaking = true;
    let mut language = lang;
    let mut left_min = left;
    let mut right_min = right;
    for (idx, n) in list.iter().cloned().enumerate() {
        if !auto_breaking {
            if let Node::Math { after, .. } = &n {
                auto_breaking = *after;
            }
            after_glue = false;
            out.push(n);
            continue;
        }
        let in_word = match &n {
            Node::Char { code, font, lig } => {
                // §897 fixes `hf` from the first letter and stops the word as
                // soon as a character in another font is encountered.  This is
                // semantically significant even when the character codes have
                // nonzero `\lccode`s in both fonts: reconstitution happens in
                // `hf`, so admitting a cross-font character would rebuild it in
                // the wrong font.  A common real-world instance is `\{word`:
                // the brace glyph in CMSY happens to have code `f`, followed by
                // letters in the text font.
                // A run can only start with a character; normal kerns are
                // admitted only after that.  Reading the first node is both
                // sufficient and important here: scanning the accumulated run
                // for every next character turns long unbroken input into an
                // accidental quadratic-time path.
                if matches!(run.first(), Some(Node::Char { font: run_font, .. }) if run_font != font)
                {
                    false
                } else {
                    // A generated retained-operand ligature can own no input
                    // letters at all. §896 skips such a node while looking for a
                    // word; §897 keeps it inside a word already being collected so
                    // the eventual replacement still covers the exact node run.
                    // Judging its generated character by `\lccode` split TRIP's
                    // boundary-built `11` after its first node, before the second
                    // raw letter was ever offered to the patterns.
                    let generated = lig
                        .as_ref()
                        .is_some_and(|raw| raw.iter().all(|c| *c == crate::node::LIG_BOUNDARY));
                    if generated {
                        !run.is_empty()
                    } else if !run.is_empty() {
                        // §897 has already found the first letter and now copies
                        // a ligature's complete `lig_ptr` into `hu`. Encountering
                        // one nonletter jumps to `done3` *before* committing any
                        // of that ligature, so the whole node terminates and stays
                        // outside the word. This differs deliberately from §896
                        // below, where only the first source character is tested
                        // while TeX is still looking for a word. TRIP ends
                        // `AABBBBCACAC/` with a `C/` ligature: admitting it made
                        // the pattern word end in `/` and suppressed the `C-A`
                        // discretionary immediately before it.
                        lig.as_ref().map_or_else(
                            || {
                                lccode(*code) != 0
                                    || char::from_u32(*code).is_some_and(|c| c.is_alphabetic())
                            },
                            |raw| {
                                raw.iter()
                                    .copied()
                                    .filter(|c| *c != crate::node::LIG_BOUNDARY)
                                    .all(|c| lccode(c) != 0)
                            },
                        )
                    } else {
                        // §896 looks *through* a ligature at `lig_ptr(s)` — the first of
                        // the characters it stands for — and judges that. `ffi` begins
                        // with `f` and is a word character; the open-quote ligature
                        // begins with a backtick, whose `\lccode` is zero, and so is
                        // not. Judging the ligature by its own code instead made
                        // ```` ``Theorem ```` one word that no pattern matches.
                        let first = lig
                            .as_ref()
                            .and_then(|o| {
                                o.iter().copied().find(|c| *c != crate::node::LIG_BOUNDARY)
                            })
                            .or_else(|| {
                                fonts
                                    .get(*font)
                                    .and_then(|f| f.unligature(*code))
                                    .map(|p| p.0)
                            })
                            .unwrap_or(*code);
                        lccode(first) != 0
                            || char::from_u32(first).is_some_and(|c| c.is_alphabetic())
                    }
                }
            }
            // A kern only continues a word if one is already going; a kern
            // between words belongs to neither.
            //
            // And only a *normal* kern. §897 takes one into the word by its
            // subtype, and anything else ends the word outright:
            //
            // ```text
            // else if (type(s)=kern_node)and(subtype(s)=normal) then
            //   begin hb:=s;
            //   ...
            //   end
            // else goto done3;
            // ```
            //
            // §1125's `\accent` puts a kern of subtype `acc_kern` on each side of
            // its character, so an accented word ends at the accent — and since
            // §894 allows one attempt per glue node, the rest of the word is never
            // offered either:
            //
            // ```text
            // ref    XX Gr^^?unbaum YY        XX co^^?operation YY
            // ours   XX Gr^^?un-baum YY       XX co^^?-op-er-a-tion YY
            // ```
            //
            // TeX finds `Gr`, two letters it cannot hyphenate, and stops.
            Node::Kern(_, sub) => !run.is_empty() && *sub == crate::node::kern_sub::NORMAL,
            _ => false,
        };
        let represented = represented_letter_count(&n, fonts);
        let in_word = in_word && run_letters.saturating_add(represented) <= MAX_HYPHEN_WORD;
        if in_word {
            run_letters += represented;
            run.push(n);
        } else {
            // Whether this flush hands §897 an actual word, which is what spends
            // the glue's one hyphenation attempt.
            let had_word = !run.is_empty();
            flush(
                h,
                &mut run,
                &mut out,
                language,
                left_min,
                right_min,
                hyphen_char,
                &mut inserted,
                &mut missing_hyphens,
                fonts,
                uchyph,
                lccode,
                after_glue,
                &list[idx..],
            );
            run_letters = 0;
            // §896's `continue` arms: a normal kern or a whatsit between the
            // glue and the word is stepped over, not counted as a boundary.
            //
            // So is a *character* whose `\lccode` is zero. §896 tests
            //
            //     if lc_code(c)<>0 then
            //       if (lc_code(c)=c)or(uc_hyph>0) then goto done2
            //       else goto done1;
            //     continue: prev_s:=s; s:=link(prev_s);
            //
            // and a `(` or a quote falls off the end of that `if` into
            // `continue` — it is stepped over and the search for the first
            // letter goes on. Treating it as a boundary instead meant **no word
            // preceded by punctuation was ever hyphenated**: `(Theorem` and
            // ``` ``Theorem ``` came out whole where TeX sets `The-o-rem`, which
            // is one of the two divergences left in the closest corpus paper.
            //
            // But the stepping-over is only for as long as §896 is still
            // *looking*. §894 makes one attempt per glue node, and once §897 has
            // collected a word the attempt is spent — there is no loop back to
            // §896 for whatever follows. So a zero-`\lccode` character keeps the
            // search alive ahead of the word and ends it behind:
            //
            //     ref    XX (semi)stable YY
            //     ours   XX (semi)sta-ble YY
            //
            // TeX finds `semi`, cannot hyphenate four letters, and stops. We
            // stepped over the `)` as though still searching and hyphenated
            // `stable` too. `(knowledge)` must still come out `(knowl-edge)`,
            // which is the same rule read the other way.
            match &n {
                Node::Glue(_) => after_glue = true,
                Node::Math { after, .. } => {
                    auto_breaking = *after;
                    after_glue = false;
                }
                Node::Language {
                    language: new_language,
                    left,
                    right,
                } => {
                    language = *new_language;
                    left_min = usize::from(*left);
                    right_min = usize::from(*right);
                    if had_word {
                        after_glue = false;
                    }
                }
                _ if had_word => after_glue = false,
                Node::Kern(_, sub) if *sub == crate::node::kern_sub::NORMAL => {}
                Node::Whatsit(_) | Node::WriteOut(_) => {}
                Node::Char { lig: Some(raw), .. }
                    if raw.iter().all(|c| *c == crate::node::LIG_BOUNDARY) => {}
                // §896 judges a ligature by the first character in
                // `lig_ptr`, not by the glyph the ligature displays.  TRIP's
                // boundary-built `7A` displays as A (whose lccode is nonzero)
                // but begins with 7 (whose lccode is zero), so it is
                // punctuation to step over while looking for the word after
                // this glue.  The word begins only after the following
                // language whatsit.  Testing `code` here spent the glue's one
                // hyphenation attempt on that punctuation ligature even though
                // the word-recognition branch above had classified it
                // correctly.
                Node::Char { lig: Some(raw), .. }
                    if raw
                        .iter()
                        .copied()
                        .find(|c| *c != crate::node::LIG_BOUNDARY)
                        .is_some_and(|c| lccode(c) == 0) => {}
                Node::Char { code, .. } if lccode(*code) == 0 => {}
                _ => after_glue = false,
            }
            out.push(n);
        }
    }
    flush(
        h,
        &mut run,
        &mut out,
        language,
        left_min,
        right_min,
        hyphen_char,
        &mut inserted,
        &mut missing_hyphens,
        fonts,
        uchyph,
        lccode,
        after_glue,
        &[],
    );
    (out, inserted, missing_hyphens)
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    /// The patterns Liang's paper and `tex.web` use as the worked example.
    fn liang() -> Hyphenator {
        let mut h = Hyphenator::new();
        for p in [
            "hy3ph", "he2n", "hena4", "hen5at", ".mis1", "mis1s", "s1si", "si4on", "n2at", "na4t",
            "1tio", "2io2n", "o2n",
        ] {
            h.add_pattern(0, p);
        }
        h
    }

    #[test]
    fn a_pattern_parses_into_letters_and_values() {
        let mut h = Hyphenator::new();
        h.add_pattern(0, "a1bc3d");
        // Letters `abcd`, with 1 after `a` and 3 after `c`.
        assert_eq!(
            h.patterns.get(&(0u8, alloc::string::String::from("abcd"))),
            Some(&alloc::vec![0, 1, 0, 3, 0])
        );

        // A digit may lead, which is how a pattern forbids a break before its
        // first letter.
        h.add_pattern(0, "2ion");
        assert_eq!(
            h.patterns.get(&(0u8, alloc::string::String::from("ion"))),
            Some(&alloc::vec![2, 0, 0, 0])
        );
    }

    /// `hyphenation` breaks as `hy-phen-ation` — the example `tex.web` §923
    /// works through by hand.
    #[test]
    fn the_worked_example_from_tex_web() {
        let h = liang();
        assert_eq!(h.hyphenate(0, "hyphenation", 1, 1), alloc::vec![2, 6]);
    }

    /// `\lefthyphenmin` and `\righthyphenmin` keep short fragments off the
    /// line ends. plain TeX uses 2 and 3.
    #[test]
    fn the_minimums_suppress_short_fragments() {
        let h = liang();
        // With plain's 2/3, the break at 2 survives and the one at 6 needs
        // `11 - 6 = 5 >= 3`, so both do.
        assert_eq!(h.hyphenate(0, "hyphenation", 2, 3), alloc::vec![2, 6]);
        // Demanding five letters on the left kills the first.
        assert_eq!(h.hyphenate(0, "hyphenation", 5, 3), alloc::vec![6]);
        // And demanding six on the right kills the second.
        assert_eq!(h.hyphenate(0, "hyphenation", 2, 6), alloc::vec![2]);
    }

    #[test]
    fn a_word_too_short_to_break_is_left_alone() {
        let h = liang();
        assert!(h.hyphenate(0, "on", 2, 3).is_empty());
        assert!(h.hyphenate(0, "", 2, 3).is_empty());
        assert!(h.hyphenate(0, "a", 1, 1).is_empty());
    }

    /// An even value forbids a break an odd one would have allowed. This is
    /// the whole reason patterns compose.
    #[test]
    fn an_even_value_vetoes_an_odd_one() {
        let mut h = Hyphenator::new();
        h.add_pattern(0, "a1b");
        assert_eq!(h.hyphenate(0, "cabc", 1, 1), alloc::vec![2]);
        // A longer, more specific pattern with an even value at the same gap
        // wins, because the larger value is kept.
        h.add_pattern(0, "ca2bc");
        assert!(h.hyphenate(0, "cabc", 1, 1).is_empty());
    }

    /// `\hyphenation{...}` overrides the patterns completely.
    #[test]
    fn an_exception_replaces_the_patterns() {
        let mut h = liang();
        h.add_exception(0, "hyphen-ation");
        assert_eq!(h.hyphenate(0, "hyphenation", 1, 1), alloc::vec![6]);
    }

    #[test]
    fn case_is_ignored() {
        let h = liang();
        assert_eq!(
            h.hyphenate(0, "Hyphenation", 1, 1),
            h.hyphenate(0, "hyphenation", 1, 1)
        );
    }

    #[test]
    fn never_panics_on_hostile_words() {
        let h = liang();
        for w in ["", ".", "..", "-", "----", "é", "日本語", "a1b2c3"] {
            let _ = h.hyphenate(0, w, 2, 3);
        }
        let mut h2 = Hyphenator::new();
        for p in ["", ".", "1", "123", "..a..", "9z9"] {
            h2.add_pattern(0, p);
        }
        let _ = h2.hyphenate(0, "test", 1, 1);
    }

    #[test]
    fn math_mode_material_is_not_hyphenated() {
        let mut h = Hyphenator::new();
        h.add_exception(0, "mod-el");
        let word = || {
            "model"
                .chars()
                .map(|c| Node::Char {
                    code: c as u32,
                    font: 0,
                    lig: None,
                })
                .collect::<Vec<_>>()
        };
        let font = crate::tfm::TfmFont::empty();
        let lc = |c: u32| c;

        let mut ordinary = alloc::vec![Node::Glue(crate::node::Glue::default())];
        ordinary.extend(word());
        ordinary.push(Node::Glue(crate::node::Glue::default()));
        let (_, ordinary_inserted, _) = hyphenate_list(
            &h,
            0,
            &ordinary,
            1,
            1,
            &|_| 45,
            core::slice::from_ref(&font),
            true,
            &lc,
        );
        assert_eq!(ordinary_inserted, 1);

        let mut math = alloc::vec![
            Node::Glue(crate::node::Glue::default()),
            Node::Math {
                after: false,
                width: crate::scaled::Scaled::ZERO,
            },
            // `url` places zero-width math glue around punctuation. It must not
            // re-enable paragraph hyphenation while math mode is still open.
            Node::Glue(crate::node::Glue::default()),
        ];
        math.extend(word());
        math.push(Node::Math {
            after: true,
            width: crate::scaled::Scaled::ZERO,
        });
        math.push(Node::Glue(crate::node::Glue::default()));
        let (_, math_inserted, _) = hyphenate_list(
            &h,
            0,
            &math,
            1,
            1,
            &|_| 45,
            core::slice::from_ref(&font),
            true,
            &lc,
        );
        assert_eq!(math_inserted, 0);
    }

    #[test]
    fn a_font_change_terminates_the_hyphenation_word() {
        let mut h = Hyphenator::new();
        // If the symbol-font `f` and the text were incorrectly collected as
        // one word, this exception would force a discretionary and the whole
        // replacement would be reconstituted in font 0.
        h.add_exception(0, "fca-pabilities");
        let mut input = alloc::vec![
            Node::Glue(crate::node::Glue::default()),
            Node::Char {
                code: u32::from(b'f'),
                font: 0,
                lig: None,
            },
        ];
        input.extend("capabilities".bytes().map(|code| Node::Char {
            code: u32::from(code),
            font: 1,
            lig: None,
        }));
        input.push(Node::Glue(crate::node::Glue::default()));

        let fonts = [crate::tfm::TfmFont::empty(), crate::tfm::TfmFont::empty()];
        let (output, inserted, _) =
            hyphenate_list(&h, 0, &input, 1, 1, &|_| 45, &fonts, true, &|c| c);

        assert_eq!(inserted, 0);
        assert_eq!(output, input);
    }

    #[test]
    fn punctuation_after_a_word_spends_the_glues_only_hyphenation_attempt() {
        let mut h = Hyphenator::new();
        h.add_exception(0, "pro-mote");
        let mut input = alloc::vec![Node::Glue(crate::node::Glue::default())];
        input.extend("close/promote".bytes().map(|code| Node::Char {
            code: u32::from(code),
            font: 0,
            lig: None,
        }));
        input.push(Node::Glue(crate::node::Glue::default()));

        let font = crate::tfm::TfmFont::empty();
        let lc = |c: u32| {
            if u32::from(b'a') <= c && c <= u32::from(b'z') {
                c
            } else {
                0
            }
        };
        let (output, inserted, _) = hyphenate_list(
            &h,
            0,
            &input,
            1,
            1,
            &|_| i64::from(b'-'),
            core::slice::from_ref(&font),
            true,
            &lc,
        );

        assert_eq!(inserted, 0);
        assert_eq!(output, input);
    }

    #[test]
    fn hyphenation_word_is_capped_at_sixty_three_letters() {
        let text = "ask/respond/aggregate/dispatch/program/cycle/return/close/promote/branch/consume/task/program";
        assert!(text.len() > 63);

        let mut h = Hyphenator::new();
        h.add_pattern(0, "pro1mote");
        let mut input = alloc::vec![Node::Glue(crate::node::Glue::default())];
        input.extend(text.bytes().map(|code| Node::Char {
            code: u32::from(code),
            font: 0,
            lig: None,
        }));
        input.push(Node::Glue(crate::node::Glue::default()));

        let font = crate::tfm::TfmFont::empty();
        let lc = |c: u32| {
            if c == u32::from(b'/') || (u32::from(b'a') <= c && c <= u32::from(b'z')) {
                c
            } else {
                0
            }
        };
        let (output, inserted, _) = hyphenate_list(
            &h,
            0,
            &input,
            2,
            3,
            &|_| i64::from(b'-'),
            core::slice::from_ref(&font),
            true,
            &lc,
        );

        assert_eq!(inserted, 0, "the only pattern match lies beyond hu[63]");
        assert_eq!(output, input);
    }

    #[test]
    fn reconstitution_uses_the_nonletter_that_stopped_the_word() {
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
        let font = crate::tfm::TfmFont::parse(&bytes, Some(crate::scaled::Scaled(717_619)))
            .expect("ec-lmbx10 parses");
        let Some(crate::tfm::LigKern::Kern(y_dot)) =
            font.lig_kern(u32::from(b'y'), u32::from(b'.'))
        else {
            panic!("ec-lmbx10 must kern y.");
        };

        // The `\unskip` in amsart's run-in-heading macro ends the original
        // character run before its generated full stop, so this deliberately
        // starts without the font kern. TeX's second-pass reconstitution uses
        // the full stop as `hyf_bchar` and puts the kern back.
        let mut input = alloc::vec![Node::Glue(crate::node::Glue::default())];
        input.extend("optimality".bytes().map(|code| Node::Char {
            code: u32::from(code),
            font: 0,
            lig: None,
        }));
        input.push(Node::Char {
            code: u32::from(b'.'),
            font: 0,
            lig: None,
        });
        input.push(Node::Glue(crate::node::Glue::default()));

        let mut h = Hyphenator::new();
        h.add_exception(0, "opti-mality");
        let lc = |c: u32| {
            if u32::from(b'a') <= c && c <= u32::from(b'z') {
                c
            } else {
                0
            }
        };
        let (output, inserted, _) = hyphenate_list(
            &h,
            0,
            &input,
            2,
            3,
            &|_| i64::from(b'-'),
            core::slice::from_ref(&font),
            true,
            &lc,
        );
        assert_eq!(inserted, 1);
        assert_eq!(
            output
                .iter()
                .filter(|node| matches!(
                    node,
                    Node::Char { code, font: 0, .. } if *code == u32::from(b'.')
                ))
                .count(),
            1,
            "the nonletter boundary character must be reconstituted exactly once"
        );
        assert!(output.windows(3).any(|w| matches!(
            w,
            [
                Node::Char { code, font: 0, .. },
                Node::Kern(width, crate::node::kern_sub::NORMAL),
                Node::Char { code: dot, font: 0, .. }
            ] if *code == u32::from(b'y') && *width == y_dot && *dot == u32::from(b'.')
        )));
    }
}
