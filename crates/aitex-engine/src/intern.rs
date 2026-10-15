//! Interned control-sequence names.
//!
//! Every control-sequence name (`\foo`, `\!`) and active-character key becomes a
//! small integer [`Symbol`]. That single change is what makes a [`Token`] `Copy`:
//! carrying a `Symbol(u32)` instead of a `String`, a token clones by `memcpy`, a
//! macro body's `Vec<Token>` clones without a per-element heap allocation, and an
//! eqtb lookup is an integer-keyed comparison rather than a `memcmp` down a tree
//! of strings.
//!
//! Storage mirrors `tex.web`'s string pool (§1106): every name's bytes live in
//! one shared `pool` buffer, and `ends[i]` records where name `i` finishes, so a
//! name costs no per-name heap allocation — just its bytes in the pool and one
//! `u32`. The name→symbol index is an open-addressing **hash table** (`ids`), not
//! a `BTreeMap`: interning and every `\cs` resolution then cost one hash and
//! typically one comparison, rather than `O(log n)` string comparisons down a
//! B-tree, which profiling put at a large share of a cold `latex.ltx` load.
//!
//! [`Token`]: crate::token::Token

use alloc::string::String;
use alloc::vec::Vec;

/// An interned control-sequence (or active-character) name: a stable index into
/// an [`Interner`].
///
/// `Copy`, so every token that carries one is `Copy` too. The index is stable
/// for the life of an interner (and across a checkpoint dump/restore, since the
/// interner's name pool is serialized and restored verbatim), so two `Symbol`s
/// are equal iff they name the same control sequence -- `\ifx` and every other
/// name comparison becomes a `u32` equality test.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Symbol(pub u32);

/// Empty-slot marker in the hash table. No name will ever be assigned symbol
/// `u32::MAX` (that would need four billion distinct control sequences).
const EMPTY: u32 = u32::MAX;

/// The two-way map between control-sequence names and [`Symbol`]s.
///
/// Names are stored `tex.web`-style: all bytes concatenated in `pool`, with
/// `ends[i]` the end offset of name `i` (its start is `ends[i-1]`, or 0). Only
/// `pool` and `ends` are serialized; the `ids` hash table is a rebuildable cache
/// reconstructed by [`rebuild_ids`](Interner::rebuild_ids) after a restore.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Interner {
    /// All name bytes, concatenated in interning order.
    pool: String,
    /// `ends[i]` = end offset of name `i` in `pool`. `ends.len()` is the number
    /// of distinct names, i.e. the next symbol id.
    ends: Vec<u32>,
    /// Open-addressing hash table: `ids[slot]` is a symbol id, or [`EMPTY`].
    /// Length is always a power of two (so `hash & (len-1)` indexes it) or zero.
    /// Rebuilt from `pool`/`ends`, never serialized.
    #[serde(skip)]
    ids: Vec<u32>,
}

/// FNV-1a over a name's bytes — cheap, dependency-free, and well spread for the
/// short ASCII-ish names control sequences use.
#[inline]
fn hash(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    /// An interner pre-sized for `cap` distinct names, so the first `cap`
    /// interns do not reallocate. Used to pre-size from a checkpoint's known
    /// count, or a cold-start estimate from the input size.
    pub fn with_capacity(cap: usize) -> Self {
        let mut s = Self {
            pool: String::with_capacity(cap * 8),
            ends: Vec::with_capacity(cap),
            ids: Vec::new(),
        };
        s.ids = alloc::vec![EMPTY; table_len_for(cap)];
        s
    }

    /// The byte range of name `i` in the pool.
    #[inline]
    fn span(&self, i: u32) -> (usize, usize) {
        let i = i as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        (start, self.ends[i] as usize)
    }

    /// The name symbol `i` stands for (unchecked — `i` must be in range).
    #[inline]
    fn name_of(&self, i: u32) -> &str {
        let (a, b) = self.span(i);
        &self.pool[a..b]
    }

    /// Find the id stored for `name` (already hashed to `h`), or `None`.
    #[inline]
    fn lookup(&self, name: &str, h: u64) -> Option<u32> {
        crate::counters::bump(&crate::counters::INTERN_LOOKUP);
        if self.ids.is_empty() {
            return None;
        }
        let mask = self.ids.len() - 1;
        let mut i = (h as usize) & mask;
        loop {
            let slot = self.ids[i];
            if slot == EMPTY {
                return None;
            }
            if self.name_of(slot) == name {
                return Some(slot);
            }
            i = (i + 1) & mask;
        }
    }

    /// Place a known-unique id into the table at its hash `h` (no comparison —
    /// the caller guarantees the name is not already present).
    #[inline]
    fn place(&mut self, id: u32, h: u64) {
        let mask = self.ids.len() - 1;
        let mut i = (h as usize) & mask;
        while self.ids[i] != EMPTY {
            i = (i + 1) & mask;
        }
        self.ids[i] = id;
    }

    /// Rebuild the hash table at a given power-of-two size, reinserting every
    /// name currently in the pool.
    fn rebuild_table_sized(&mut self, len: usize) {
        let mut table = alloc::vec![EMPTY; len];
        let mask = len - 1;
        for id in 0..self.ends.len() as u32 {
            let h = hash(self.name_of(id).as_bytes());
            let mut i = (h as usize) & mask;
            while table[i] != EMPTY {
                i = (i + 1) & mask;
            }
            table[i] = id;
        }
        self.ids = table;
    }

    /// Intern a name, returning its [`Symbol`] -- the existing one if the name
    /// has been seen, a freshly assigned one otherwise.
    pub fn intern(&mut self, name: &str) -> Symbol {
        let h = hash(name.as_bytes());
        if let Some(id) = self.lookup(name, h) {
            return Symbol(id);
        }
        let id = self.ends.len() as u32;
        self.pool.push_str(name);
        self.ends.push(self.pool.len() as u32);
        // Grow (and reinsert everything, including this new id) when the table is
        // absent or past a 0.7 load factor; otherwise just place the new id.
        let count = self.ends.len();
        if self.ids.is_empty() || count * 10 >= self.ids.len() * 7 {
            self.rebuild_table_sized((self.ids.len().max(8) * 2).next_power_of_two());
        } else {
            self.place(id, h);
        }
        Symbol(id)
    }

    /// The [`Symbol`] for a name that is already interned, or `None`. Does not
    /// allocate or assign -- for the read-only "have we ever seen this name"
    /// question, where interning an unknown one would be wrong.
    pub fn get(&self, name: &str) -> Option<Symbol> {
        self.lookup(name, hash(name.as_bytes())).map(Symbol)
    }

    /// The name a [`Symbol`] stands for.
    ///
    /// # Panics
    /// If the symbol did not come from this interner. Symbols are only minted by
    /// [`intern`](Self::intern), so in practice this is a program invariant, not
    /// a runtime condition.
    pub fn resolve(&self, sym: Symbol) -> &str {
        self.name_of(sym.0)
    }

    /// As [`resolve`](Self::resolve), but `None` instead of panicking for a
    /// symbol out of range -- for display paths that must never abort.
    pub fn try_resolve(&self, sym: Symbol) -> Option<&str> {
        if (sym.0 as usize) < self.ends.len() {
            Some(self.name_of(sym.0))
        } else {
            None
        }
    }

    /// Rebuild the `ids` index from the name pool. Call after deserializing,
    /// where only `pool`/`ends` were restored: without this, interning a name
    /// already in the pool would mint a *second* symbol for it and break symbol
    /// stability.
    pub fn rebuild_ids(&mut self) {
        self.rebuild_table_sized(table_len_for(self.ends.len()));
    }

    /// How many distinct names have been interned.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Approximate heap bytes held: name pool + end offsets + hash table.
    pub fn heap_bytes(&self) -> usize {
        self.pool.capacity()
            + self.ends.capacity() * core::mem::size_of::<u32>()
            + self.ids.capacity() * core::mem::size_of::<u32>()
    }
}

/// A power-of-two table size that holds `n` names below a 0.7 load factor.
fn table_len_for(n: usize) -> usize {
    ((n * 10 / 7) + 1).next_power_of_two().max(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_same_symbol() {
        let mut i = Interner::new();
        let a = i.intern("foo");
        let b = i.intern("bar");
        let a2 = i.intern("foo");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert_eq!(i.resolve(a), "foo");
        assert_eq!(i.resolve(b), "bar");
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn get_does_not_intern() {
        let mut i = Interner::new();
        assert_eq!(i.get("nope"), None);
        let s = i.intern("nope");
        assert_eq!(i.get("nope"), Some(s));
    }

    #[test]
    fn rebuild_after_restore_keeps_symbols_stable() {
        // Simulate a checkpoint round-trip: `pool`/`ends` survive, `ids` does not.
        let mut i = Interner::new();
        let foo = i.intern("foo");
        let bar = i.intern("bar");
        // Drop the index, as deserialization would leave it.
        i.ids.clear();
        i.rebuild_ids();
        // Existing names must resolve to their ORIGINAL symbols, and re-interning
        // must not mint new ones.
        assert_eq!(i.get("foo"), Some(foo));
        assert_eq!(i.intern("foo"), foo);
        assert_eq!(i.intern("bar"), bar);
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn empty_name_interns_like_any_other() {
        // The null control sequence `\` at end of input has the empty name.
        let mut i = Interner::new();
        let s = i.intern("");
        assert_eq!(i.resolve(s), "");
        assert_eq!(i.intern(""), s);
    }

    #[test]
    fn many_names_grow_and_resolve() {
        // Exercise several table growths and confirm every symbol round-trips.
        let mut i = Interner::new();
        let mut syms = alloc::vec::Vec::new();
        for n in 0..1000u32 {
            let name = alloc::format!("cs{n}");
            syms.push((i.intern(&name), name));
        }
        for (s, name) in &syms {
            assert_eq!(i.resolve(*s), name.as_str());
            assert_eq!(i.intern(name), *s);
        }
        assert_eq!(i.len(), 1000);
    }
}
