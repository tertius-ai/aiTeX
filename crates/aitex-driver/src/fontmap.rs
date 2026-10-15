//! Parsers for the small PostScript data files that connect a TeX font name
//! to the program and encoding used in a PDF.
//!
//! TeX's DVI font definition contains only a TeX name such as `ec-lmr10`.
//! TeX Live's `pdftex.map` supplies the corresponding PostScript name,
//! `lmr10.pfb` program, and `lm-ec.enc` encoding.  Keeping this parser in the
//! driver makes that relationship explicit and independently testable.

/// One usable entry from a dvips/pdfTeX font map.
#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    pub tex_name: String,
    pub ps_name: String,
    pub encoding_file: Option<String>,
    pub font_file: Option<String>,
    pub slant: Option<f64>,
    pub extend: Option<f64>,
}

#[derive(Debug)]
enum MapToken {
    Bare(String),
    Quoted(String),
}

fn map_tokens(line: &str) -> Vec<MapToken> {
    let mut tokens = Vec::new();
    let bytes = line.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if at == bytes.len() || bytes[at] == b'%' {
            break;
        }
        if bytes[at] == b'"' {
            at += 1;
            let start = at;
            while at < bytes.len() && bytes[at] != b'"' {
                at += 1;
            }
            tokens.push(MapToken::Quoted(line[start..at].to_string()));
            at += usize::from(at < bytes.len());
        } else {
            let start = at;
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'%' {
                at += 1;
            }
            tokens.push(MapToken::Bare(line[start..at].to_string()));
            if bytes.get(at) == Some(&b'%') {
                break;
            }
        }
    }
    tokens
}

fn parse_map_line(line: &str) -> Option<MapEntry> {
    let tokens = map_tokens(line);
    let Some(MapToken::Bare(tex_name)) = tokens.first() else {
        return None;
    };
    let mut ps_name = None;
    let mut encoding_file = None;
    let mut font_file = None;
    let mut slant = None;
    let mut extend = None;
    for token in tokens.iter().skip(1) {
        match token {
            MapToken::Quoted(special) => {
                let words: Vec<&str> = special.split_whitespace().collect();
                for (index, word) in words.iter().enumerate() {
                    let value = index.checked_sub(1).and_then(|i| words[i].parse().ok());
                    match *word {
                        "SlantFont" => slant = value,
                        "ExtendFont" => extend = value,
                        _ => {}
                    }
                }
            }
            MapToken::Bare(word) if word.starts_with('<') => {
                let file = word.trim_start_matches('<').trim_start_matches('[');
                let lower = file.to_ascii_lowercase();
                if lower.ends_with(".enc") {
                    encoding_file = Some(file.to_string());
                } else if lower.ends_with(".pfb")
                    || lower.ends_with(".pfa")
                    || lower.ends_with(".ttf")
                    || lower.ends_with(".otf")
                {
                    font_file = Some(file.to_string());
                }
            }
            MapToken::Bare(word) if ps_name.is_none() => ps_name = Some(word.clone()),
            MapToken::Bare(_) => {}
        }
    }
    Some(MapEntry {
        tex_name: tex_name.clone(),
        ps_name: ps_name.unwrap_or_else(|| tex_name.clone()),
        encoding_file,
        font_file,
        slant,
        extend,
    })
}

/// Parse all non-comment entries in a dvips/pdfTeX map file.
pub fn parse_map(text: &str) -> Vec<MapEntry> {
    text.lines().filter_map(parse_map_line).collect()
}

/// Parse only requested TeX font names without first materializing the whole
/// installation map. A watch build normally needs fewer than ten of the
/// thousands of entries in `pdftex.map`.
pub fn parse_map_selected(
    text: &str,
    wanted: &std::collections::BTreeSet<String>,
) -> Vec<MapEntry> {
    text.lines()
        .filter(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|name| !name.starts_with('%') && wanted.contains(name))
        })
        .filter_map(parse_map_line)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PsToken<'a> {
    Name(&'a str),
    Word(&'a str),
    Open,
    Close,
    BraceOpen,
    BraceClose,
}

fn ps_tokens(text: &str) -> Vec<PsToken<'_>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at] == b'%' {
            while at < bytes.len() && !matches!(bytes[at], b'\n' | b'\r') {
                at += 1;
            }
            continue;
        }
        match bytes[at] {
            b'[' => {
                tokens.push(PsToken::Open);
                at += 1;
            }
            b']' => {
                tokens.push(PsToken::Close);
                at += 1;
            }
            b'{' => {
                tokens.push(PsToken::BraceOpen);
                at += 1;
            }
            b'}' => {
                tokens.push(PsToken::BraceClose);
                at += 1;
            }
            b'/' => {
                at += 1;
                let start = at;
                while at < bytes.len()
                    && !bytes[at].is_ascii_whitespace()
                    && !matches!(
                        bytes[at],
                        b'[' | b']' | b'{' | b'}' | b'%' | b'/' | b'(' | b')'
                    )
                {
                    at += 1;
                }
                if start < at {
                    tokens.push(PsToken::Name(&text[start..at]));
                }
            }
            b'(' => {
                // Literal strings cannot contribute glyph names. Skip balanced
                // strings so brackets inside comments/metadata do not look
                // like an encoding vector.
                let mut depth = 1usize;
                at += 1;
                while at < bytes.len() && depth > 0 {
                    match bytes[at] {
                        b'\\' => at = (at + 2).min(bytes.len()),
                        b'(' => {
                            depth += 1;
                            at += 1;
                        }
                        b')' => {
                            depth -= 1;
                            at += 1;
                        }
                        _ => at += 1,
                    }
                }
            }
            _ => {
                let start = at;
                while at < bytes.len()
                    && !bytes[at].is_ascii_whitespace()
                    && !matches!(
                        bytes[at],
                        b'[' | b']' | b'{' | b'}' | b'%' | b'/' | b'(' | b')'
                    )
                {
                    at += 1;
                }
                if start < at {
                    tokens.push(PsToken::Word(&text[start..at]));
                }
            }
        }
    }
    tokens
}

fn encoding_after_open(tokens: &[PsToken<'_>], open: usize) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut at = open + 1;
    while at < tokens.len() {
        match tokens[at] {
            PsToken::Name(name) => {
                names.push(name.to_string());
                at += 1;
            }
            PsToken::Word(count)
                if matches!(tokens.get(at + 1), Some(PsToken::BraceOpen))
                    && matches!(tokens.get(at + 2), Some(PsToken::Name(_)))
                    && matches!(tokens.get(at + 3), Some(PsToken::BraceClose))
                    && matches!(tokens.get(at + 4), Some(PsToken::Word("repeat"))) =>
            {
                let count = count.parse::<usize>().ok()?;
                let PsToken::Name(name) = tokens[at + 2] else {
                    unreachable!();
                };
                if names.len().checked_add(count)? > 256 {
                    return None;
                }
                names.extend(core::iter::repeat_n(name.to_string(), count));
                at += 5;
            }
            PsToken::Close => break,
            _ => at += 1,
        }
    }
    if names.is_empty() || names.len() > 256 {
        return None;
    }
    names.resize(256, ".notdef".to_string());
    Some(names)
}

/// Parse a PostScript encoding vector (`/EncodingName [ /A /B ... ] def`).
/// The returned vector is always indexed by an eight-bit character code.
pub fn parse_encoding(text: &str) -> Option<Vec<String>> {
    let tokens = ps_tokens(text);
    for (at, pair) in tokens.windows(2).enumerate() {
        let [PsToken::Name(_), PsToken::Open] = pair else {
            continue;
        };
        if let Some(encoding) = encoding_after_open(&tokens, at + 1) {
            return Some(encoding);
        }
    }
    None
}

/// Parse one font's vector from TeX Live's consolidated `dvips-all.enc`.
///
/// That file groups aliases as bare `FontName:` lines followed by one vector,
/// rather than wrapping every vector in `/EncodingName [ ... ] def`. Bitmap
/// fonts without a map entry (notably MnSymbol's size-specific PK fonts) still
/// need those names for a useful PDF Encoding and ToUnicode CMap.
pub fn parse_named_encoding(text: &str, font_name: &str) -> Option<Vec<String>> {
    let marker = format!("{font_name}:");
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim() != marker {
            continue;
        }
        let tokens = ps_tokens(text.get(offset..)?);
        let open = tokens
            .iter()
            .position(|token| matches!(token, PsToken::Open))?;
        return encoding_after_open(&tokens, open);
    }
    None
}

/// Decode the algorithmic Unicode forms in Adobe glyph names. A suffix such
/// as `.alt1` changes the drawing but not the character semantics.
pub fn unicode_from_glyph_name(glyph: &str) -> Option<String> {
    let base = glyph.split('.').next().unwrap_or(glyph);
    if let Some(hex) = base.strip_prefix("uni")
        && !hex.is_empty()
        && hex.len() % 4 == 0
        && hex.as_bytes().iter().all(u8::is_ascii_hexdigit)
    {
        return Some(hex.to_ascii_uppercase());
    }
    if let Some(hex) = base.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.as_bytes().iter().all(u8::is_ascii_hexdigit)
        && let Ok(scalar) = u32::from_str_radix(hex, 16)
        && let Some(character) = char::from_u32(scalar)
    {
        let mut units = [0u16; 2];
        return Some(
            character
                .encode_utf16(&mut units)
                .iter()
                .map(|unit| format!("{unit:04X}"))
                .collect(),
        );
    }
    // TeX math extension fonts name size variants by their visual role rather
    // than Adobe's `uniXXXX` convention.  `glyphtounicode.tex` intentionally
    // maps the *pieces* of constructed delimiters to private-use characters,
    // but omits the self-contained big/Big/bigg/Bigg glyphs.  Those are still
    // ordinary Unicode operators and delimiters; leaving them unmapped makes
    // a searchable formula contain U+FFFD for every `\left` delimiter.
    let semantic = match base {
        "parenleftbig" | "parenleftBig" | "parenleftbigg" | "parenleftBigg" => "0028",
        "parenrightbig" | "parenrightBig" | "parenrightbigg" | "parenrightBigg" => "0029",
        "bracketleftbig" | "bracketleftBig" | "bracketleftbigg" | "bracketleftBigg" => "005B",
        "bracketrightbig" | "bracketrightBig" | "bracketrightbigg" | "bracketrightBigg" => "005D",
        "floorleftbig" | "floorleftBig" | "floorleftbigg" | "floorleftBigg" => "230A",
        "floorrightbig" | "floorrightBig" | "floorrightbigg" | "floorrightBigg" => "230B",
        "ceilingleftbig" | "ceilingleftBig" | "ceilingleftbigg" | "ceilingleftBigg" => "2308",
        "ceilingrightbig" | "ceilingrightBig" | "ceilingrightbigg" | "ceilingrightBigg" => "2309",
        "braceleftbig" | "braceleftBig" | "braceleftbigg" | "braceleftBigg" => "007B",
        "bracerightbig" | "bracerightBig" | "bracerightbigg" | "bracerightBigg" => "007D",
        "angbracketleftbig" | "angbracketleftBig" | "angbracketleftbigg" | "angbracketleftBigg" => {
            "27E8"
        }
        "angbracketrightbig"
        | "angbracketrightBig"
        | "angbracketrightbigg"
        | "angbracketrightBigg" => "27E9",
        "slashbig" | "slashBig" | "slashbigg" | "slashBigg" => "002F",
        "backslashbig" | "backslashBig" | "backslashbigg" | "backslashBigg" => "005C",
        "unionsqtext" | "unionsqdisplay" => "2294",
        "contintegraltext" | "contintegraldisplay" => "222E",
        "circledottext" | "circledotdisplay" => "2299",
        "circleplustext" | "circleplusdisplay" => "2295",
        "circlemultiplytext" | "circlemultiplydisplay" => "2297",
        "summationtext" | "summationdisplay" => "2211",
        "producttext" | "productdisplay" => "220F",
        "integraltext" | "integraldisplay" => "222B",
        "uniontext" | "uniondisplay" => "222A",
        "intersectiontext" | "intersectiondisplay" => "2229",
        "unionmultitext" | "unionmultidisplay" => "228E",
        "logicalandtext" | "logicalanddisplay" => "2227",
        "logicalortext" | "logicalordisplay" => "2228",
        "coproducttext" | "coproductdisplay" => "2210",
        "hatwide" | "hatwider" | "hatwidest" => "0302",
        "tildewide" | "tildewider" | "tildewidest" => "0303",
        "radical" | "radicalbig" | "radicalBig" | "radicalbigg" | "radicalBigg" => "221A",
        _ => return None,
    };
    Some(semantic.to_string())
}

/// Parse the `dup <code> /<glyph> put` encoding in the clear-text portion of
/// a Type 1 program. This is the fallback for map entries without an `.enc`.
pub fn parse_type1_encoding(pfb: &[u8]) -> Option<Vec<String>> {
    let clear = if pfb.starts_with(&[0x80, 1]) && pfb.len() >= 6 {
        let length = u32::from_le_bytes([pfb[2], pfb[3], pfb[4], pfb[5]]) as usize;
        pfb.get(6..6usize.checked_add(length)?)?
    } else {
        pfb
    };
    let text = std::str::from_utf8(clear).ok()?;
    let tokens = ps_tokens(text);
    let uses_standard_encoding = tokens.windows(2).any(|pair| {
        matches!(
            pair,
            [PsToken::Name("Encoding"), PsToken::Word("StandardEncoding")]
        )
    });
    let mut encoding = if uses_standard_encoding {
        adobe_standard_encoding()
    } else {
        vec![".notdef".to_string(); 256]
    };
    let mut found = uses_standard_encoding;
    for window in tokens.windows(4) {
        let [
            PsToken::Word("dup"),
            PsToken::Word(code),
            PsToken::Name(name),
            PsToken::Word("put"),
        ] = window
        else {
            continue;
        };
        let Ok(code) = code.parse::<usize>() else {
            continue;
        };
        if code < encoding.len() {
            encoding[code] = (*name).to_string();
            found = true;
        }
    }
    found.then_some(encoding)
}

/// Adobe StandardEncoding, used directly by small Type 1 programs whose map
/// entry consequently has no external `.enc` file. Keep this compact by
/// generating the alphabetic runs and listing only the named punctuation,
/// digits, ligatures, and extended characters.
fn adobe_standard_encoding() -> Vec<String> {
    let mut encoding = vec![".notdef".to_string(); 256];
    for code in b'A'..=b'Z' {
        encoding[usize::from(code)] = char::from(code).to_string();
    }
    for code in b'a'..=b'z' {
        encoding[usize::from(code)] = char::from(code).to_string();
    }
    for (code, name) in [
        (32, "space"),
        (33, "exclam"),
        (34, "quotedbl"),
        (35, "numbersign"),
        (36, "dollar"),
        (37, "percent"),
        (38, "ampersand"),
        (39, "quoteright"),
        (40, "parenleft"),
        (41, "parenright"),
        (42, "asterisk"),
        (43, "plus"),
        (44, "comma"),
        (45, "hyphen"),
        (46, "period"),
        (47, "slash"),
        (48, "zero"),
        (49, "one"),
        (50, "two"),
        (51, "three"),
        (52, "four"),
        (53, "five"),
        (54, "six"),
        (55, "seven"),
        (56, "eight"),
        (57, "nine"),
        (58, "colon"),
        (59, "semicolon"),
        (60, "less"),
        (61, "equal"),
        (62, "greater"),
        (63, "question"),
        (64, "at"),
        (91, "bracketleft"),
        (92, "backslash"),
        (93, "bracketright"),
        (94, "asciicircum"),
        (95, "underscore"),
        (96, "quoteleft"),
        (123, "braceleft"),
        (124, "bar"),
        (125, "braceright"),
        (126, "asciitilde"),
        (161, "exclamdown"),
        (162, "cent"),
        (163, "sterling"),
        (164, "fraction"),
        (165, "yen"),
        (166, "florin"),
        (167, "section"),
        (168, "currency"),
        (169, "quotesingle"),
        (170, "quotedblleft"),
        (171, "guillemotleft"),
        (172, "guilsinglleft"),
        (173, "guilsinglright"),
        (174, "fi"),
        (175, "fl"),
        (177, "endash"),
        (178, "dagger"),
        (179, "daggerdbl"),
        (180, "periodcentered"),
        (182, "paragraph"),
        (183, "bullet"),
        (184, "quotesinglbase"),
        (185, "quotedblbase"),
        (186, "quotedblright"),
        (187, "guillemotright"),
        (188, "ellipsis"),
        (189, "perthousand"),
        (191, "questiondown"),
        (193, "grave"),
        (194, "acute"),
        (195, "circumflex"),
        (196, "tilde"),
        (197, "macron"),
        (198, "breve"),
        (199, "dotaccent"),
        (200, "dieresis"),
        (202, "ring"),
        (203, "cedilla"),
        (205, "hungarumlaut"),
        (206, "ogonek"),
        (207, "caron"),
        (208, "emdash"),
        (225, "AE"),
        (227, "ordfeminine"),
        (232, "Lslash"),
        (233, "Oslash"),
        (234, "OE"),
        (235, "ordmasculine"),
        (241, "ae"),
        (245, "dotlessi"),
        (248, "lslash"),
        (249, "oslash"),
        (250, "oe"),
        (251, "germandbls"),
    ] {
        encoding[code] = name.to_string();
    }
    encoding
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdftex_alias_encoding_and_transforms() {
        let map = r#"ec-lmr10 LMRoman10-Regular " enclmec ReEncodeFont " <lm-ec.enc <lmr10.pfb
phvro8r NimbusSanL-Regu " .167 SlantFont 1.2 ExtendFont " <8r.enc <uhvr8a.pfb % tail
ptmr8r Times-Roman " TeXBase1Encoding ReEncodeFont " <8r.enc"#;
        let entries = parse_map(map);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].tex_name, "ec-lmr10");
        assert_eq!(entries[0].ps_name, "LMRoman10-Regular");
        assert_eq!(entries[0].encoding_file.as_deref(), Some("lm-ec.enc"));
        assert_eq!(entries[0].font_file.as_deref(), Some("lmr10.pfb"));
        assert_eq!(entries[1].slant, Some(0.167));
        assert_eq!(entries[1].extend, Some(1.2));
        assert_eq!(entries[2].font_file, None);
        let wanted = ["phvro8r".to_string()].into_iter().collect();
        let selected = parse_map_selected(map, &wanted);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0], entries[1]);
    }

    #[test]
    fn parses_adjacent_bracket_encoding_vector() {
        let mut encoding =
            parse_encoding("% comment with [\n/enclmec[\n/.notdef /grave /Aacute\n] def\n")
                .expect("encoding");
        assert_eq!(encoding.len(), 256);
        assert_eq!(encoding.remove(0), ".notdef");
        assert_eq!(encoding.remove(0), "grave");
        assert_eq!(encoding.remove(0), "Aacute");
    }

    #[test]
    fn parses_named_consolidated_encoding_and_repeat() {
        let encoding = parse_named_encoding(
            "Other:\n[/X]\nMnSymbolE10:\nMnSymbolE9:\n[/bracketleft 2{/.notdef}repeat /radical]\n",
            "MnSymbolE9",
        )
        .expect("named encoding");
        assert_eq!(
            &encoding[..5],
            ["bracketleft", ".notdef", ".notdef", "radical", ".notdef"]
        );
        assert!(parse_named_encoding("Other:\n[/X]\n", "Missing").is_none());
    }

    #[test]
    fn decodes_algorithmic_unicode_glyph_names() {
        assert_eq!(
            unicode_from_glyph_name("uni230A.alt1").as_deref(),
            Some("230A")
        );
        assert_eq!(
            unicode_from_glyph_name("uni00410042").as_deref(),
            Some("00410042")
        );
        assert_eq!(
            unicode_from_glyph_name("u1D400").as_deref(),
            Some("D835DC00")
        );
        assert!(unicode_from_glyph_name("u110000").is_none());
        assert_eq!(unicode_from_glyph_name("radical").as_deref(), Some("221A"));
        assert_eq!(
            unicode_from_glyph_name("bracketleftBigg").as_deref(),
            Some("005B")
        );
        assert_eq!(
            unicode_from_glyph_name("summationdisplay").as_deref(),
            Some("2211")
        );
    }

    #[test]
    fn parses_type1_cleartext_encoding() {
        let clear = b"%!PS-AdobeFont\n/Encoding 256 array\ndup 0 /Gamma put\ndup 65 /A put\n";
        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        pfb.extend_from_slice(clear);
        let encoding = parse_type1_encoding(&pfb).expect("font encoding");
        assert_eq!(encoding[0], "Gamma");
        assert_eq!(encoding[65], "A");
    }

    #[test]
    fn parses_type1_encoding_after_a_cr_terminated_header_comment() {
        let clear = b"%!PS-AdobeFont-1.0: Example\r/Encoding 256 array\rdup 33 /arrowright put\r";
        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        pfb.extend_from_slice(clear);
        let encoding = parse_type1_encoding(&pfb).expect("font encoding");
        assert_eq!(encoding[33], "arrowright");
    }

    #[test]
    fn parses_type1_standard_encoding_without_an_external_vector() {
        let clear = b"%!PS-AdobeFont-1.0: Example\r/Encoding StandardEncoding def\r";
        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        pfb.extend_from_slice(clear);
        let encoding = parse_type1_encoding(&pfb).expect("standard encoding");
        assert_eq!(encoding[32], "space");
        assert_eq!(encoding[65], "A");
        assert_eq!(encoding[174], "fi");
        assert_eq!(encoding[251], "germandbls");
    }
}
