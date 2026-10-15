use sha2::Digest as _;
use std::collections::{BTreeMap, BTreeSet};

/// A `.pfb` is the Type 1 program wrapped in segment headers: `0x80`, a type
/// byte, then a four-byte little-endian length. PDF wants the program without
/// them, plus the length of each of its three parts.
fn unwrap_pfb(pfb: &[u8]) -> Option<(Vec<u8>, usize, usize, usize)> {
    let mut out = Vec::with_capacity(pfb.len());
    let mut lens = [0usize; 3];
    let mut i = 0;
    let mut seg = 0;
    while i + 6 <= pfb.len() {
        if pfb[i] != 0x80 {
            return None;
        }
        let kind = pfb[i + 1];
        if kind == 3 {
            break;
        }
        let n = u32::from_le_bytes([pfb[i + 2], pfb[i + 3], pfb[i + 4], pfb[i + 5]]) as usize;
        let start = i + 6;
        let end = start.checked_add(n)?;
        out.extend_from_slice(pfb.get(start..end)?);
        let part = match (seg, kind) {
            (0, _) => 0,
            (_, 1) => 2,
            _ => 1,
        };
        lens[part] += n;
        seg += 1;
        i = end;
    }
    if out.is_empty() {
        return None;
    }
    Some((out, lens[0], lens[1], lens[2]))
}

/// PostScript name used when classic slant or extension is embedded in a font.
pub fn transformed_type1_name(base: &str, slant: f64, extend: f64) -> String {
    let mut name = base.to_string();
    if (extend - 1.0).abs() > f64::EPSILON {
        name.push_str(&format!("-Extend_{}", (extend * 1000.0).round() as i64));
    }
    if slant.abs() > f64::EPSILON {
        name.push_str(&format!("-Slant_{}", (slant * 1000.0).round() as i64));
    }
    name
}

fn pdf_name(name: &str) -> String {
    let mut out = String::new();
    for &byte in name.as_bytes() {
        if (b'!'..=b'~').contains(&byte)
            && !matches!(
                byte,
                b'#' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
            )
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("#{byte:02X}"));
        }
    }
    out
}

/// An immutable Type 1 program in PDF's native unwrapped representation.
#[derive(Clone)]
pub struct PreparedType1 {
    program: std::sync::Arc<[u8]>,
    length1: usize,
    length2: usize,
    length3: usize,
    embedded_transform: bool,
    embedded_name: String,
    subset: bool,
    requested_glyphs: Option<std::sync::Arc<[String]>>,
}

impl PreparedType1 {
    pub fn retained_bytes(&self) -> usize {
        self.program.len()
    }

    pub fn program(&self) -> &[u8] {
        &self.program
    }

    pub fn lengths(&self) -> [usize; 3] {
        [self.length1, self.length2, self.length3]
    }

    pub fn embedded_transform(&self) -> bool {
        self.embedded_transform
    }

    /// PostScript name written into both the program and the PDF font objects.
    pub fn embedded_name(&self) -> &str {
        &self.embedded_name
    }

    /// Whether unused glyph programs were removed.
    pub fn is_subset(&self) -> bool {
        self.subset
    }

    /// Whether this immutable artifact safely contains a requested union
    /// closure. Complete-program fallbacks cover every request.
    pub fn covers(&self, glyphs: &BTreeSet<String>) -> bool {
        !self.subset
            || self
                .requested_glyphs
                .as_deref()
                .is_some_and(|requested| glyphs.iter().all(|glyph| requested.contains(glyph)))
    }
}

/// Unwrap a PFB and apply the classic dvips map transform.
pub fn prepare_type1_program(
    pfb: &[u8],
    postscript_name: &str,
    slant: f64,
    extend: f64,
) -> Option<PreparedType1> {
    let slant = if slant.is_finite() { slant } else { 0.0 };
    let extend = if extend.is_finite() && extend > 0.0 {
        extend
    } else {
        1.0
    };
    let transformed_name = transformed_type1_name(postscript_name, slant, extend);
    let transform_requested = slant.abs() > f64::EPSILON || (extend - 1.0).abs() > f64::EPSILON;
    let (mut program, mut length1, length2, length3) = unwrap_pfb(pfb)?;
    let mut embedded_transform = false;
    if transform_requested
        && let Some((transformed, transformed_length1)) =
            transform_type1_program(program.clone(), length1, &transformed_name, slant, extend)
    {
        program = transformed;
        length1 = transformed_length1;
        embedded_transform = true;
    }
    Some(PreparedType1 {
        program: program.into(),
        length1,
        length2,
        length3,
        embedded_transform,
        embedded_name: if embedded_transform {
            transformed_name
        } else {
            postscript_name.to_string()
        },
        subset: false,
        requested_glyphs: None,
    })
}

/// Prepare a Type 1 program and remove glyph programs outside `glyph_names`.
///
/// The subsetter is intentionally conservative. It retains all reachable
/// Type 1 subroutines and all other private-dictionary machinery, so hinting
/// and executable font behavior remain untouched. It also retains `.notdef`
/// and the base and accent glyphs named by any statically readable `seac`
/// composite. If glyph or subroutine reachability cannot be proved, it keeps
/// the affected complete table rather than risking a damaged subset.
pub fn prepare_type1_subset(
    pfb: &[u8],
    postscript_name: &str,
    slant: f64,
    extend: f64,
    glyph_names: &BTreeSet<String>,
) -> Option<PreparedType1> {
    let prepared = prepare_type1_program(pfb, postscript_name, slant, extend)?;
    let Some((program, length2, retained, total)) = subset_type1_program(
        prepared.program().to_vec(),
        prepared.length1,
        prepared.length2,
        prepared.length3,
        glyph_names,
    ) else {
        return Some(prepared);
    };
    if retained == total {
        return Some(prepared);
    }

    let subset_name = format!(
        "{}+{}",
        type1_subset_tag(pfb, prepared.embedded_name(), glyph_names),
        prepared.embedded_name()
    );
    let Some((program, length1)) = rename_type1_program(program, prepared.length1, &subset_name)
    else {
        return Some(prepared);
    };
    Some(PreparedType1 {
        program: program.into(),
        length1,
        length2,
        length3: prepared.length3,
        embedded_transform: prepared.embedded_transform,
        embedded_name: subset_name,
        subset: true,
        requested_glyphs: Some(glyph_names.iter().cloned().collect::<Vec<_>>().into()),
    })
}

fn type1_subset_tag(pfb: &[u8], embedded_name: &str, glyph_names: &BTreeSet<String>) -> String {
    let mut hash = sha2::Sha256::new();
    hash.update(b"aitex-type1-subset-v2\0");
    hash.update((pfb.len() as u64).to_be_bytes());
    hash.update(pfb);
    hash.update((embedded_name.len() as u64).to_be_bytes());
    hash.update(embedded_name.as_bytes());
    for name in glyph_names {
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
    }
    hash.finalize()[..6]
        .iter()
        .map(|byte| char::from(b'A' + byte % 26))
        .collect()
}

const EEXEC_KEY: u16 = 55_665;
const CHARSTRING_KEY: u16 = 4_330;
const C1: u16 = 52_845;
const C2: u16 = 22_719;

fn decrypt(ciphertext: &[u8], key: u16) -> Vec<u8> {
    let mut state = key;
    ciphertext
        .iter()
        .map(|&cipher| {
            let plain = cipher ^ (state >> 8) as u8;
            state = u16::from(cipher)
                .wrapping_add(state)
                .wrapping_mul(C1)
                .wrapping_add(C2);
            plain
        })
        .collect()
}

fn encrypt(plaintext: &[u8], key: u16) -> Vec<u8> {
    let mut state = key;
    plaintext
        .iter()
        .map(|&plain| {
            let cipher = plain ^ (state >> 8) as u8;
            state = u16::from(cipher)
                .wrapping_add(state)
                .wrapping_mul(C1)
                .wrapping_add(C2);
            cipher
        })
        .collect()
}

#[derive(Clone)]
struct CharStringEntry {
    name: String,
    start: usize,
    end: usize,
    cipher_start: usize,
    cipher_end: usize,
}

struct CharStrings {
    count_start: usize,
    count_end: usize,
    entries_start: usize,
    entries_end: usize,
    entries: Vec<CharStringEntry>,
}

#[derive(Clone)]
struct SubrEntry {
    index: usize,
    start: usize,
    end: usize,
    cipher_start: usize,
    cipher_end: usize,
}

struct Subrs {
    entries_start: usize,
    entries_end: usize,
    entries: Vec<SubrEntry>,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(|byte| is_space(*byte)) {
        at += 1;
    }
    at
}

fn token_end(bytes: &[u8], mut at: usize) -> usize {
    while bytes
        .get(at)
        .is_some_and(|byte| !is_space(*byte) && !matches!(*byte, b'/' | b'[' | b']'))
    {
        at += 1;
    }
    at
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn parse_usize(bytes: &[u8], start: usize, end: usize) -> Option<usize> {
    std::str::from_utf8(bytes.get(start..end)?)
        .ok()?
        .parse()
        .ok()
}

fn parse_charstrings(private: &[u8]) -> Option<CharStrings> {
    let marker = find_bytes(private, b"/CharStrings", 0)?;
    let count_start = skip_space(private, marker + b"/CharStrings".len());
    let count_end = token_end(private, count_start);
    let declared = parse_usize(private, count_start, count_end)?;
    let begin = find_bytes(private, b"begin", count_end)?;
    let entries_start = skip_space(private, begin + b"begin".len());
    let mut at = entries_start;
    let mut entries = Vec::with_capacity(declared);
    loop {
        at = skip_space(private, at);
        if private.get(at..at + 3) == Some(b"end")
            && private.get(at + 3).is_none_or(|byte| is_space(*byte))
        {
            break;
        }
        if private.get(at) != Some(&b'/') {
            return None;
        }
        let start = at;
        let name_start = at + 1;
        let name_end = token_end(private, name_start);
        if name_end == name_start {
            return None;
        }
        let name = std::str::from_utf8(private.get(name_start..name_end)?)
            .ok()?
            .to_string();
        let length_start = skip_space(private, name_end);
        let length_end = token_end(private, length_start);
        let length = parse_usize(private, length_start, length_end)?;
        let reader_start = skip_space(private, length_end);
        let reader_end = token_end(private, reader_start);
        if !matches!(private.get(reader_start..reader_end), Some(b"RD" | b"-|")) {
            return None;
        }
        if !private.get(reader_end).is_some_and(|byte| is_space(*byte)) {
            return None;
        }
        // In a binary PFB, RD/-| consumes exactly the one lexical delimiter
        // before reading the declared number of binary bytes.
        let cipher_start = reader_end + 1;
        let cipher_end = cipher_start.checked_add(length)?;
        private.get(cipher_start..cipher_end)?;
        let terminator_start = skip_space(private, cipher_end);
        let terminator_end = token_end(private, terminator_start);
        if !matches!(
            private.get(terminator_start..terminator_end),
            Some(b"ND" | b"|-")
        ) {
            return None;
        }
        let end = skip_space(private, terminator_end);
        entries.push(CharStringEntry {
            name,
            start,
            end,
            cipher_start,
            cipher_end,
        });
        at = end;
    }
    if entries.len() != declared {
        return None;
    }
    Some(CharStrings {
        count_start,
        count_end,
        entries_start,
        entries_end: at,
        entries,
    })
}

fn parse_subrs(private: &[u8]) -> Option<Subrs> {
    let marker = find_bytes(private, b"/Subrs", 0)?;
    let count_start = skip_space(private, marker + b"/Subrs".len());
    let count_end = token_end(private, count_start);
    let declared = parse_usize(private, count_start, count_end)?;
    let array = find_bytes(private, b"array", count_end)?;
    let entries_start = skip_space(private, array + b"array".len());
    let mut at = entries_start;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    while private.get(at..at + 3) == Some(b"dup")
        && private.get(at + 3).is_some_and(|byte| is_space(*byte))
    {
        let start = at;
        let index_start = skip_space(private, at + 3);
        let index_end = token_end(private, index_start);
        let index = parse_usize(private, index_start, index_end)?;
        if index >= declared || !seen.insert(index) {
            return None;
        }
        let length_start = skip_space(private, index_end);
        let length_end = token_end(private, length_start);
        let length = parse_usize(private, length_start, length_end)?;
        let reader_start = skip_space(private, length_end);
        let reader_end = token_end(private, reader_start);
        if !matches!(private.get(reader_start..reader_end), Some(b"RD" | b"-|"))
            || !private.get(reader_end).is_some_and(|byte| is_space(*byte))
        {
            return None;
        }
        let cipher_start = reader_end + 1;
        let cipher_end = cipher_start.checked_add(length)?;
        private.get(cipher_start..cipher_end)?;
        let terminator_start = skip_space(private, cipher_end);
        let terminator_end = token_end(private, terminator_start);
        if !matches!(
            private.get(terminator_start..terminator_end),
            Some(b"NP" | b"|")
        ) {
            return None;
        }
        let end = skip_space(private, terminator_end);
        entries.push(SubrEntry {
            index,
            start,
            end,
            cipher_start,
            cipher_end,
        });
        at = end;
    }
    Some(Subrs {
        entries_start,
        entries_end: at,
        entries,
    })
}

fn type1_len_iv(private: &[u8]) -> Option<isize> {
    let marker = find_bytes(private, b"/lenIV", 0)?;
    let start = skip_space(private, marker + b"/lenIV".len());
    let end = token_end(private, start);
    std::str::from_utf8(private.get(start..end)?)
        .ok()?
        .parse()
        .ok()
}

fn charstring_number(bytes: &[u8], at: usize) -> Option<(i32, usize)> {
    let first = *bytes.get(at)?;
    match first {
        32..=246 => Some((i32::from(first) - 139, at + 1)),
        247..=250 => Some((
            (i32::from(first) - 247) * 256 + i32::from(*bytes.get(at + 1)?) + 108,
            at + 2,
        )),
        251..=254 => Some((
            -(i32::from(first) - 251) * 256 - i32::from(*bytes.get(at + 1)?) - 108,
            at + 2,
        )),
        255 => Some((
            i32::from_be_bytes(bytes.get(at + 1..at + 5)?.try_into().ok()?),
            at + 5,
        )),
        _ => None,
    }
}

fn standard_encoding_name(code: i32) -> Option<&'static str> {
    match code {
        32 => Some("space"),
        65..=90 | 97..=122 => Some(match u8::try_from(code).ok()? {
            b'A' => "A",
            b'B' => "B",
            b'C' => "C",
            b'D' => "D",
            b'E' => "E",
            b'F' => "F",
            b'G' => "G",
            b'H' => "H",
            b'I' => "I",
            b'J' => "J",
            b'K' => "K",
            b'L' => "L",
            b'M' => "M",
            b'N' => "N",
            b'O' => "O",
            b'P' => "P",
            b'Q' => "Q",
            b'R' => "R",
            b'S' => "S",
            b'T' => "T",
            b'U' => "U",
            b'V' => "V",
            b'W' => "W",
            b'X' => "X",
            b'Y' => "Y",
            b'Z' => "Z",
            b'a' => "a",
            b'b' => "b",
            b'c' => "c",
            b'd' => "d",
            b'e' => "e",
            b'f' => "f",
            b'g' => "g",
            b'h' => "h",
            b'i' => "i",
            b'j' => "j",
            b'k' => "k",
            b'l' => "l",
            b'm' => "m",
            b'n' => "n",
            b'o' => "o",
            b'p' => "p",
            b'q' => "q",
            b'r' => "r",
            b's' => "s",
            b't' => "t",
            b'u' => "u",
            b'v' => "v",
            b'w' => "w",
            b'x' => "x",
            b'y' => "y",
            b'z' => "z",
            _ => return None,
        }),
        193 => Some("grave"),
        194 => Some("acute"),
        195 => Some("circumflex"),
        196 => Some("tilde"),
        197 => Some("macron"),
        198 => Some("breve"),
        199 => Some("dotaccent"),
        200 => Some("dieresis"),
        202 => Some("ring"),
        203 => Some("cedilla"),
        205 => Some("hungarumlaut"),
        206 => Some("ogonek"),
        207 => Some("caron"),
        245 => Some("dotlessi"),
        _ => None,
    }
}

fn seac_dependencies(ciphertext: &[u8], len_iv: isize) -> Option<Vec<&'static str>> {
    let decrypted = if len_iv < 0 {
        ciphertext.to_vec()
    } else {
        let decrypted = decrypt(ciphertext, CHARSTRING_KEY);
        decrypted.get(usize::try_from(len_iv).ok()?..)?.to_vec()
    };
    let mut stack = Vec::<i32>::new();
    let mut dependencies = Vec::new();
    let mut at = 0;
    while at < decrypted.len() {
        if decrypted[at] >= 32 {
            let (number, next) = charstring_number(&decrypted, at)?;
            stack.push(number);
            at = next;
            continue;
        }
        if decrypted[at] == 12 {
            let escaped = *decrypted.get(at + 1)?;
            if escaped == 6 {
                let base = *stack.get(stack.len().checked_sub(2)?)?;
                let accent = *stack.last()?;
                dependencies.push(standard_encoding_name(base)?);
                dependencies.push(standard_encoding_name(accent)?);
            }
            at += 2;
        } else {
            at += 1;
        }
        stack.clear();
    }
    Some(dependencies)
}

#[derive(Clone, Copy)]
enum CharStringValue {
    Known(i32),
    Unknown,
}

impl CharStringValue {
    fn known(self) -> Option<i32> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }
}

fn charstring_plaintext(ciphertext: &[u8], len_iv: isize) -> Option<Vec<u8>> {
    if len_iv < 0 {
        return Some(ciphertext.to_vec());
    }
    let decrypted = decrypt(ciphertext, CHARSTRING_KEY);
    Some(decrypted.get(usize::try_from(len_iv).ok()?..)?.to_vec())
}

struct SubrClosure<'a> {
    private: &'a [u8],
    len_iv: isize,
    by_index: BTreeMap<usize, &'a SubrEntry>,
    retained: BTreeSet<usize>,
    active: BTreeSet<usize>,
    steps: usize,
    step_limit: usize,
}

impl SubrClosure<'_> {
    fn scan(
        &mut self,
        bytes: &[u8],
        stack: &mut Vec<CharStringValue>,
        other_results: &mut Vec<CharStringValue>,
    ) -> Option<()> {
        let mut at = 0;
        while at < bytes.len() {
            self.steps = self.steps.checked_add(1)?;
            if self.steps > self.step_limit {
                return None;
            }
            if bytes[at] >= 32 {
                let (number, next) = charstring_number(bytes, at)?;
                stack.push(CharStringValue::Known(number));
                at = next;
                continue;
            }
            match bytes[at] {
                10 => {
                    let index = usize::try_from(stack.pop()?.known()?).ok()?;
                    let entry = *self.by_index.get(&index)?;
                    self.retained.insert(index);
                    if !self.active.insert(index) {
                        return None;
                    }
                    let subr = charstring_plaintext(
                        self.private.get(entry.cipher_start..entry.cipher_end)?,
                        self.len_iv,
                    )?;
                    self.scan(&subr, stack, other_results)?;
                    self.active.remove(&index);
                }
                11 | 14 => return Some(()),
                12 => {
                    at += 1;
                    match *bytes.get(at)? {
                        12 => {
                            let divisor = stack.pop()?;
                            let dividend = stack.pop()?;
                            let quotient = match (dividend.known(), divisor.known()) {
                                (Some(dividend), Some(divisor))
                                    if divisor != 0 && dividend % divisor == 0 =>
                                {
                                    CharStringValue::Known(dividend / divisor)
                                }
                                _ => CharStringValue::Unknown,
                            };
                            stack.push(quotient);
                        }
                        16 => {
                            let other = stack.pop()?.known()?;
                            let count = usize::try_from(stack.pop()?.known()?).ok()?;
                            if count > stack.len() {
                                return None;
                            }
                            let arguments = stack.split_off(stack.len() - count);
                            other_results.clear();
                            match (other, count) {
                                // Adobe's standard hint-replacement OtherSubr
                                // passes its argument back through `pop`. This
                                // is how the conventional Subr 4 dispatches a
                                // statically supplied subroutine number.
                                (3, 1) => other_results.push(arguments[0]),
                                // Standard flex returns the final point.
                                (0, 3) => {
                                    other_results.push(CharStringValue::Unknown);
                                    other_results.push(CharStringValue::Unknown);
                                }
                                // Standard flex setup and point collection do
                                // not return operands to the charstring stack.
                                (1 | 2, 0) => {}
                                // Unknown PostScript OtherSubrs may return
                                // values. Preserve uncertainty so a later
                                // dynamic callsubr makes pruning decline.
                                _ => {
                                    other_results.push(CharStringValue::Unknown);
                                    other_results.push(CharStringValue::Unknown);
                                }
                            }
                        }
                        17 => stack.push(other_results.pop().unwrap_or(CharStringValue::Unknown)),
                        _ => stack.clear(),
                    }
                }
                _ => stack.clear(),
            }
            at += 1;
        }
        Some(())
    }
}

fn retained_subrs(
    private: &[u8],
    subrs: &Subrs,
    charstrings: &CharStrings,
    retained_glyphs: &BTreeSet<String>,
    len_iv: isize,
) -> Option<BTreeSet<usize>> {
    let by_index = subrs
        .entries
        .iter()
        .map(|entry| (entry.index, entry))
        .collect::<BTreeMap<_, _>>();
    let mut closure = SubrClosure {
        private,
        len_iv,
        by_index,
        // The first five entries are conventional Adobe flex helpers. Keeping
        // them costs only a few bytes and avoids depending on whether a viewer
        // initializes that machinery eagerly.
        retained: subrs
            .entries
            .iter()
            .filter_map(|entry| (entry.index < 5).then_some(entry.index))
            .collect(),
        active: BTreeSet::new(),
        steps: 0,
        step_limit: private.len().saturating_mul(4).max(1024),
    };
    for entry in charstrings
        .entries
        .iter()
        .filter(|entry| retained_glyphs.contains(&entry.name))
    {
        let bytes =
            charstring_plaintext(private.get(entry.cipher_start..entry.cipher_end)?, len_iv)?;
        closure.scan(&bytes, &mut Vec::new(), &mut Vec::new())?;
    }
    Some(closure.retained)
}

fn subset_type1_program(
    mut program: Vec<u8>,
    length1: usize,
    length2: usize,
    length3: usize,
    glyph_names: &BTreeSet<String>,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    if length1.checked_add(length2)?.checked_add(length3)? > program.len() || length2 < 4 {
        return None;
    }
    let encrypted = program.get(length1..length1 + length2)?;
    let decrypted = decrypt(encrypted, EEXEC_KEY);
    let private = decrypted.get(4..)?;
    let table = parse_charstrings(private)?;
    let len_iv = type1_len_iv(private).unwrap_or(4);
    let by_name = table
        .entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut retained = glyph_names
        .iter()
        .filter(|name| by_name.contains_key(name.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    retained.insert(".notdef".to_string());
    if !by_name.contains_key(".notdef") {
        return None;
    }
    // Scan every glyph, not merely selected glyphs. This conservatively keeps
    // the small set of components used anywhere in the font and means an
    // unusual shared subroutine cannot hide a needed seac dependency.
    for entry in &table.entries {
        for dependency in
            seac_dependencies(private.get(entry.cipher_start..entry.cipher_end)?, len_iv)?
        {
            if by_name.contains_key(dependency) {
                retained.insert(dependency.to_string());
            }
        }
    }
    let total = table.entries.len();
    if retained.len() >= total {
        return Some((program, length2, total, total));
    }

    let pruned_subrs = parse_subrs(private).and_then(|subrs| {
        if subrs.entries_end > table.count_start {
            return None;
        }
        let retained_subrs = retained_subrs(private, &subrs, &table, &retained, len_iv)?;
        (retained_subrs.len() < subrs.entries.len()).then_some((subrs, retained_subrs))
    });

    let mut subset = Vec::with_capacity(private.len());
    if let Some((subrs, retained_subrs)) = &pruned_subrs {
        subset.extend_from_slice(private.get(..subrs.entries_start)?);
        for entry in &subrs.entries {
            if retained_subrs.contains(&entry.index) {
                subset.extend_from_slice(private.get(entry.start..entry.end)?);
            }
        }
        subset.extend_from_slice(private.get(subrs.entries_end..table.count_start)?);
    } else {
        subset.extend_from_slice(private.get(..table.count_start)?);
    }
    subset.extend_from_slice(retained.len().to_string().as_bytes());
    subset.extend_from_slice(private.get(table.count_end..table.entries_start)?);
    subset.push(b'\n');
    for entry in &table.entries {
        if retained.contains(&entry.name) {
            subset.extend_from_slice(private.get(entry.start..entry.end)?);
        }
    }
    subset.extend_from_slice(private.get(table.entries_end..)?);

    let mut plaintext = Vec::with_capacity(subset.len() + 4);
    // The four eexec random bytes do not carry semantics. Fixed bytes make the
    // subset stable across machines and cache lifetimes.
    plaintext.extend_from_slice(&[0; 4]);
    plaintext.extend_from_slice(&subset);
    let encrypted_subset = encrypt(&plaintext, EEXEC_KEY);
    let new_length2 = encrypted_subset.len();
    program.splice(length1..length1 + length2, encrypted_subset);
    Some((program, new_length2, retained.len(), total))
}

fn replace_postscript_line(text: &str, key: &str, replacement: &str) -> Option<String> {
    let at = text.find(key)?;
    let start = text[..at]
        .rfind(|character| matches!(character, '\n' | '\r'))
        .map_or(0, |line| line + 1);
    let line_end = text[at..]
        .find(|character| matches!(character, '\n' | '\r'))
        .map(|line| at + line);
    let end = line_end.map_or(text.len(), |line| {
        if text.as_bytes().get(line..line + 2) == Some(b"\r\n") {
            line + 2
        } else {
            line + 1
        }
    });
    let mut out = String::with_capacity(text.len() + replacement.len());
    out.push_str(&text[..start]);
    out.push_str(replacement);
    out.push_str(line_end.map_or("\n", |line| &text[line..end]));
    out.push_str(&text[end..]);
    Some(out)
}

fn transform_type1_program(
    mut program: Vec<u8>,
    length1: usize,
    font_name: &str,
    slant: f64,
    extend: f64,
) -> Option<(Vec<u8>, usize)> {
    let clear = std::str::from_utf8(program.get(..length1)?).ok()?;
    let clear = replace_postscript_line(
        clear,
        "/FontMatrix",
        &format!(
            "/FontMatrix [{:.8} 0 {:.8} 0.001 0 0] readonly def",
            0.001 * extend,
            0.001 * slant
        ),
    )?;
    let clear = replace_postscript_line(
        &clear,
        "/FontName",
        &format!("/FontName /{} def", pdf_name(font_name)),
    )?;
    let new_length1 = clear.len();
    program.splice(..length1, clear.into_bytes());
    Some((program, new_length1))
}

fn rename_type1_program(
    mut program: Vec<u8>,
    length1: usize,
    font_name: &str,
) -> Option<(Vec<u8>, usize)> {
    let clear = std::str::from_utf8(program.get(..length1)?).ok()?;
    let clear = replace_postscript_line(
        clear,
        "/FontName",
        &format!("/FontName /{} def", pdf_name(font_name)),
    )?;
    let new_length1 = clear.len();
    program.splice(..length1, clear.into_bytes());
    Some((program, new_length1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypted_charstring(body: &[u8]) -> Vec<u8> {
        let mut plaintext = vec![0; 4];
        plaintext.extend_from_slice(body);
        encrypt(&plaintext, CHARSTRING_KEY)
    }

    fn test_pfb_with_b(b_charstring: &[u8]) -> Vec<u8> {
        let clear = b"%!PS-AdobeFont-1.0: Example 1.0\n/FontName /Example def\n/FontMatrix [0.001 0 0 0.001 0 0] def\ncurrentfile eexec\n";
        let mut private = b"/lenIV 4 def\n/Subrs 8 array\n".to_vec();
        for (index, body) in [
            (0, vec![11]),
            (1, vec![11]),
            (2, vec![11]),
            (3, vec![11]),
            // Conventional dynamic dispatcher: the caller supplies the real
            // subroutine number below index 4 on the operand stack.
            (4, vec![140, 142, 12, 16, 12, 17, 10, 11]),
            // Retained transitively through the dispatcher, then calls 7.
            (5, vec![146, 10, 11]),
            // Deliberately unreachable.
            (6, vec![11]),
            (7, vec![11]),
        ] {
            let encrypted = encrypted_charstring(&body);
            private.extend_from_slice(format!("dup {index} {} RD ", encrypted.len()).as_bytes());
            private.extend_from_slice(&encrypted);
            private.extend_from_slice(b" NP\n");
        }
        private.extend_from_slice(b"/CharStrings 6 dict dup begin\n");
        for (name, body) in [
            (".notdef", vec![14]),
            ("A", vec![14]),
            ("B", b_charstring.to_vec()),
            ("C", vec![14]),
            ("acute", vec![14]),
            // An unselected composite still conservatively protects its base
            // and accent component programs.
            (
                "Aacute",
                vec![139, 139, 13, 139, 139, 139, 204, 247, 86, 12, 6],
            ),
        ] {
            let encrypted = encrypted_charstring(&body);
            private.extend_from_slice(format!("/{name} {} RD ", encrypted.len()).as_bytes());
            private.extend_from_slice(&encrypted);
            private.extend_from_slice(b" ND\n");
        }
        private.extend_from_slice(b"end\nend\nreadonly put\nnoaccess put\n");
        let mut eexec = vec![0; 4];
        eexec.extend_from_slice(&private);
        let eexec = encrypt(&eexec, EEXEC_KEY);
        let trailer = b"\n0000000000000000\ncleartomark\n";

        let mut pfb = Vec::new();
        for (kind, bytes) in [(1u8, clear.as_slice()), (2, eexec.as_slice()), (1, trailer)] {
            pfb.extend_from_slice(&[0x80, kind]);
            pfb.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            pfb.extend_from_slice(bytes);
        }
        pfb.extend_from_slice(&[0x80, 3]);
        pfb
    }

    fn test_pfb() -> Vec<u8> {
        // 5 4 callsubr: Subr 4 dispatches to Subr 5.
        test_pfb_with_b(&[144, 143, 10, 14])
    }

    #[test]
    fn unwraps_all_three_type1_parts() {
        let mut pfb = vec![0x80, 1, 4, 0, 0, 0];
        pfb.extend_from_slice(b"HEAD");
        pfb.extend_from_slice(&[0x80, 2, 3, 0, 0, 0]);
        pfb.extend_from_slice(b"BIN");
        pfb.extend_from_slice(&[0x80, 1, 2, 0, 0, 0]);
        pfb.extend_from_slice(b"ZZ");
        pfb.extend_from_slice(&[0x80, 3]);
        let (program, length1, length2, length3) = unwrap_pfb(&pfb).expect("unwraps");
        assert_eq!(program, b"HEADBINZZ");
        assert_eq!((length1, length2, length3), (4, 3, 2));
    }

    #[test]
    fn transforms_font_matrix_and_name() {
        let clear =
            b"%!PS-AdobeFont\n/FontName /Example def\n/FontMatrix [0.001 0 0 0.001 0 0] def\n";
        let (program, length) = transform_type1_program(
            clear.to_vec(),
            clear.len(),
            "Example-Extend_820-Slant_167",
            0.167,
            0.82,
        )
        .expect("transform");
        let clear = std::str::from_utf8(&program[..length]).expect("ASCII");
        assert!(clear.contains("/FontName /Example-Extend_820-Slant_167 def"));
        assert!(clear.contains("/FontMatrix [0.00082000 0 0.00016700 0.001 0 0] readonly def"));
    }

    #[test]
    fn replaces_a_postscript_line_without_discarding_cr_terminated_headers() {
        let clear = concat!(
            "%!PS-AdobeFont-1.0: Example\r",
            "/FontName /Example def\r",
            "/Encoding StandardEncoding def\r",
        );
        let renamed = replace_postscript_line(clear, "/FontName", "/FontName /Subset def")
            .expect("font name line");
        assert_eq!(
            renamed,
            concat!(
                "%!PS-AdobeFont-1.0: Example\r",
                "/FontName /Subset def\r",
                "/Encoding StandardEncoding def\r",
            )
        );
    }

    #[test]
    fn subsets_type1_deterministically_and_keeps_composite_dependencies() {
        let pfb = test_pfb();
        let wanted = BTreeSet::from(["B".to_string()]);
        let first = prepare_type1_subset(&pfb, "Example", 0.0, 1.0, &wanted).expect("prepare");
        let second = prepare_type1_subset(&pfb, "Example", 0.0, 1.0, &wanted).expect("prepare");
        assert!(first.is_subset());
        assert_eq!(first.program(), second.program());
        assert_eq!(first.embedded_name(), second.embedded_name());
        assert_eq!(first.embedded_name().find('+'), Some(6));

        let [length1, length2, _] = first.lengths();
        let decrypted = decrypt(&first.program()[length1..length1 + length2], EEXEC_KEY);
        let table = parse_charstrings(&decrypted[4..]).expect("subset CharStrings");
        let names = table
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(names, BTreeSet::from([".notdef", "A", "B", "acute"]));
        assert!(!names.contains("Aacute"));
        assert!(!names.contains("C"));
        let subrs = parse_subrs(&decrypted[4..]).expect("subset Subrs");
        assert_eq!(
            subrs
                .entries
                .iter()
                .map(|entry| entry.index)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1, 2, 3, 4, 5, 7])
        );
    }

    #[test]
    fn malformed_type1_subsetting_falls_back_to_complete_program() {
        let (program, length1, length2, length3) = unwrap_pfb(&test_pfb()).expect("test PFB");
        let mut decrypted = decrypt(&program[length1..length1 + length2], EEXEC_KEY);
        let marker = find_bytes(&decrypted, b"/CharStrings", 0).expect("marker");
        decrypted[marker + 1] = b'X';
        let encrypted = encrypt(&decrypted, EEXEC_KEY);
        let mut pfb = Vec::new();
        for (kind, bytes) in [
            (1u8, &program[..length1]),
            (2, encrypted.as_slice()),
            (1, &program[length1 + length2..length1 + length2 + length3]),
        ] {
            pfb.extend_from_slice(&[0x80, kind]);
            pfb.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            pfb.extend_from_slice(bytes);
        }
        pfb.extend_from_slice(&[0x80, 3]);
        let subset = prepare_type1_subset(
            &pfb,
            "Example",
            0.0,
            1.0,
            &BTreeSet::from(["B".to_string()]),
        )
        .expect("complete fallback");
        let complete = prepare_type1_program(&pfb, "Example", 0.0, 1.0).expect("complete");
        assert!(!subset.is_subset());
        assert_eq!(subset.program(), complete.program());
    }

    #[test]
    fn dynamic_subroutine_reachability_keeps_the_complete_subrs_table() {
        // An arbitrary OtherSubr is allowed to return a value through `pop`.
        // If that value chooses callsubr dynamically, static pruning must
        // decline even though ordinary CharStrings subsetting remains safe.
        let pfb = test_pfb_with_b(&[139, 140, 238, 12, 16, 12, 17, 10, 14]);
        let subset = prepare_type1_subset(
            &pfb,
            "Example",
            0.0,
            1.0,
            &BTreeSet::from(["B".to_string()]),
        )
        .expect("glyph-only subset");
        assert!(subset.is_subset());
        let [length1, length2, _] = subset.lengths();
        let decrypted = decrypt(&subset.program()[length1..length1 + length2], EEXEC_KEY);
        let subrs = parse_subrs(&decrypted[4..]).expect("complete Subrs");
        assert_eq!(
            subrs
                .entries
                .iter()
                .map(|entry| entry.index)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1, 2, 3, 4, 5, 6, 7])
        );
    }
}
