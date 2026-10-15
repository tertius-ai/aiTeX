#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SourceObjectId {
    number: u32,
    generation: u16,
}

struct SourceObject<'a> {
    id: SourceObjectId,
    body: &'a [u8],
}

struct NamedPdfObject {
    id: usize,
    initial: Vec<u8>,
    puts: Vec<Vec<u8>>,
    stream: Option<Vec<u8>>,
    complete: bool,
    position: Option<NamedObjectPosition>,
}

#[derive(Clone, Copy)]
struct NamedObjectPosition {
    x: f64,
    y: f64,
    page: usize,
}

struct PdfXObjectCapture {
    id: usize,
    bbox: [f64; 4],
    origin_h: i64,
    origin_v: i64,
    content: Vec<u8>,
    content_coords: Vec<(i64, i64)>,
    fonts: std::collections::BTreeSet<usize>,
    resources: Vec<Vec<u8>>,
    xobjects: Vec<(String, usize)>,
}

#[derive(Clone, Copy)]
struct PdfToken {
    start: usize,
    end: usize,
}

fn pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Tokenize PDF syntax while treating strings and names as indivisible. This
/// is deliberately smaller than a general PDF parser: normalized imported
/// pages need only dictionaries, arrays, names, numbers, and indirect refs.
fn pdf_tokens(bytes: &[u8]) -> Vec<PdfToken> {
    let mut tokens = Vec::new();
    let mut p = 0usize;
    while p < bytes.len() {
        if pdf_whitespace(bytes[p]) {
            p += 1;
            continue;
        }
        if bytes[p] == b'%' {
            p += 1;
            while p < bytes.len() && !matches!(bytes[p], b'\r' | b'\n') {
                p += 1;
            }
            continue;
        }
        let start = p;
        match bytes[p] {
            b'(' => {
                p += 1;
                let mut depth = 1usize;
                while p < bytes.len() && depth > 0 {
                    match bytes[p] {
                        b'\\' => {
                            p += 1;
                            if p < bytes.len() {
                                if bytes[p] == b'\r' && bytes.get(p + 1) == Some(&b'\n') {
                                    p += 2;
                                } else {
                                    p += 1;
                                }
                            }
                        }
                        b'(' => {
                            depth += 1;
                            p += 1;
                        }
                        b')' => {
                            depth -= 1;
                            p += 1;
                        }
                        _ => p += 1,
                    }
                }
            }
            b'<' if bytes.get(p + 1) == Some(&b'<') => p += 2,
            b'>' if bytes.get(p + 1) == Some(&b'>') => p += 2,
            b'<' => {
                p += 1;
                while p < bytes.len() && bytes[p] != b'>' {
                    p += 1;
                }
                p = p.saturating_add(1).min(bytes.len());
            }
            b'/' => {
                p += 1;
                while p < bytes.len() && !pdf_whitespace(bytes[p]) && !pdf_delimiter(bytes[p]) {
                    p += 1;
                }
            }
            b'[' | b']' | b'{' | b'}' => p += 1,
            _ => {
                p += 1;
                while p < bytes.len() && !pdf_whitespace(bytes[p]) && !pdf_delimiter(bytes[p]) {
                    p += 1;
                }
            }
        }
        tokens.push(PdfToken { start, end: p });
    }
    tokens
}

fn token_is(bytes: &[u8], token: PdfToken, value: &[u8]) -> bool {
    bytes.get(token.start..token.end) == Some(value)
}

fn token_u32(bytes: &[u8], token: PdfToken) -> Option<u32> {
    std::str::from_utf8(bytes.get(token.start..token.end)?)
        .ok()?
        .parse()
        .ok()
}

fn token_u16(bytes: &[u8], token: PdfToken) -> Option<u16> {
    std::str::from_utf8(bytes.get(token.start..token.end)?)
        .ok()?
        .parse()
        .ok()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty())
        .then(|| {
            haystack
                .windows(needle.len())
                .rposition(|window| window == needle)
        })
        .flatten()
}

fn parse_normalized_pdf(bytes: &[u8]) -> Option<Vec<SourceObject<'_>>> {
    if !bytes.starts_with(b"%PDF-") {
        return None;
    }
    let marker = rfind_bytes(bytes, b"startxref")?;
    let start_tokens = pdf_tokens(bytes.get(marker + b"startxref".len()..)?);
    let xref = token_u32(
        bytes.get(marker + b"startxref".len()..)?,
        *start_tokens.first()?,
    )? as usize;
    let xref_bytes = bytes.get(xref..)?;
    let tokens = pdf_tokens(xref_bytes);
    if !tokens
        .first()
        .is_some_and(|token| token_is(xref_bytes, *token, b"xref"))
    {
        return None; // xref streams are intentionally normalized by the CLI
    }
    let mut entries = Vec::<(SourceObjectId, usize)>::new();
    let mut p = 1usize;
    while p < tokens.len() && !token_is(xref_bytes, tokens[p], b"trailer") {
        let first = token_u32(xref_bytes, *tokens.get(p)?)?;
        let count = token_u32(xref_bytes, *tokens.get(p + 1)?)?;
        p += 2;
        for index in 0..count {
            let offset = token_u32(xref_bytes, *tokens.get(p)?)? as usize;
            let generation = token_u32(xref_bytes, *tokens.get(p + 1)?)?;
            let live = token_is(xref_bytes, *tokens.get(p + 2)?, b"n");
            p += 3;
            if live && offset > 0 {
                entries.push((
                    SourceObjectId {
                        number: first.checked_add(index)?,
                        generation: generation.try_into().ok()?,
                    },
                    offset,
                ));
            }
        }
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by_key(|entry| entry.1);
    let mut objects = Vec::with_capacity(entries.len());
    for (index, (id, offset)) in entries.iter().copied().enumerate() {
        let end = entries.get(index + 1).map(|entry| entry.1).unwrap_or(xref);
        let chunk = bytes.get(offset..end)?;
        let header = pdf_tokens(chunk.get(..chunk.len().min(256))?);
        if token_u32(chunk, *header.first()?)? != id.number
            || token_u16(chunk, *header.get(1)?)? != id.generation
            || !token_is(chunk, *header.get(2)?, b"obj")
        {
            return None;
        }
        let body_start = header[2].end;
        let body_end = rfind_bytes(chunk, b"endobj")?;
        if body_end < body_start {
            return None;
        }
        objects.push(SourceObject {
            id,
            body: chunk.get(body_start..body_end)?,
        });
    }
    Some(objects)
}

fn pdf_value_span(bytes: &[u8], key: &[u8]) -> Option<(usize, usize)> {
    let tokens = pdf_tokens(bytes);
    let key = [b"/".as_slice(), key].concat();
    // Indirect objects begin with one outer dictionary.  A key with the same
    // spelling inside a nested dictionary is not the object's key: page
    // dictionaries commonly contain `/Group << /Type /Group ... >>` before
    // their own `/Type /Page`.  Taking the first textual match made such a
    // perfectly ordinary page invisible to the Form importer.
    let mut stack = Vec::<&[u8]>::new();
    let mut at = None;
    for (index, token) in tokens.iter().enumerate() {
        if token_is(bytes, *token, b"<<") {
            stack.push(b">>");
        } else if token_is(bytes, *token, b"[") {
            stack.push(b"]");
        } else if stack
            .last()
            .is_some_and(|close| token_is(bytes, *token, close))
        {
            stack.pop();
        } else if stack.as_slice() == [b">>".as_slice()] && token_is(bytes, *token, &key) {
            at = Some(index);
            break;
        }
    }
    let at = at?;
    let value = *tokens.get(at + 1)?;
    if token_is(bytes, value, b"<<") || token_is(bytes, value, b"[") {
        let mut stack = vec![if token_is(bytes, value, b"<<") {
            b">>".as_slice()
        } else {
            b"]".as_slice()
        }];
        for token in tokens.iter().skip(at + 2) {
            if token_is(bytes, *token, b"<<") {
                stack.push(b">>");
            } else if token_is(bytes, *token, b"[") {
                stack.push(b"]");
            } else if stack
                .last()
                .is_some_and(|close| token_is(bytes, *token, close))
            {
                stack.pop();
                if stack.is_empty() {
                    return Some((value.start, token.end));
                }
            }
        }
        return None;
    }
    if token_u32(bytes, value).is_some()
        && token_u16(bytes, *tokens.get(at + 2)?).is_some()
        && token_is(bytes, *tokens.get(at + 3)?, b"R")
    {
        return Some((value.start, tokens[at + 3].end));
    }
    Some((value.start, value.end))
}

fn pdf_references(bytes: &[u8]) -> Vec<SourceObjectId> {
    let tokens = pdf_tokens(bytes);
    tokens
        .windows(3)
        .filter_map(|triple| {
            token_is(bytes, triple[2], b"R").then_some(SourceObjectId {
                number: token_u32(bytes, triple[0])?,
                generation: token_u16(bytes, triple[1])?,
            })
        })
        .collect()
}

fn content_stream_ids(
    objects: &[SourceObject<'_>],
    value: &[u8],
    depth: usize,
    out: &mut Vec<SourceObjectId>,
) -> Option<()> {
    if depth > 32 {
        return None;
    }
    for id in pdf_references(value) {
        let object = objects.iter().find(|object| object.id == id)?;
        if stream_range(object.body).is_some() {
            out.push(id);
            continue;
        }
        let tokens = pdf_tokens(object.body);
        if !tokens
            .first()
            .is_some_and(|token| token_is(object.body, *token, b"["))
            || !tokens
                .last()
                .is_some_and(|token| token_is(object.body, *token, b"]"))
        {
            return None;
        }
        content_stream_ids(objects, object.body, depth + 1, out)?;
    }
    Some(())
}

/// Return the number of content streams used by the first normalized page.
///
/// A page may name an inline array or, as emitted by Apache FOP, an indirect
/// object whose value is an array. The CLI uses this before Form import so it
/// can decompress multiple streams before they are concatenated.
pub fn first_page_content_stream_count(bytes: &[u8]) -> Option<usize> {
    let objects = parse_normalized_pdf(bytes)?;
    let page = page_objects(&objects).next()?;
    let (start, end) = pdf_value_span(&page.body, b"Contents")?;
    let mut ids = Vec::new();
    content_stream_ids(&objects, page.body.get(start..end)?, 0, &mut ids)?;
    Some(ids.len())
}

fn stream_range(body: &[u8]) -> Option<(usize, usize)> {
    let mut start = None;
    for (needle, adjustment) in [(b"stream\r\n".as_slice(), 8usize), (b"stream\n", 7usize)] {
        if let Some(at) = find_bytes(body, needle) {
            let data = at + adjustment;
            if start.is_none_or(|old| data < old) {
                start = Some(data);
            }
        }
    }
    let start = start?;
    let end = rfind_bytes(body, b"endstream")?;
    (end >= start).then_some((start, end))
}

fn remap_refs(
    bytes: &[u8],
    mapping: &std::collections::BTreeMap<SourceObjectId, usize>,
) -> Vec<u8> {
    let tokens = pdf_tokens(bytes);
    let mut replacements = Vec::<(usize, usize, usize)>::new();
    for triple in tokens.windows(3) {
        let Some(number) = token_u32(bytes, triple[0]) else {
            continue;
        };
        let Some(generation) = token_u16(bytes, triple[1]) else {
            continue;
        };
        if !token_is(bytes, triple[2], b"R") {
            continue;
        }
        if let Some(output) = mapping.get(&SourceObjectId { number, generation }) {
            replacements.push((triple[0].start, triple[2].end, *output));
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut copied = 0usize;
    for (start, end, output) in replacements {
        if start < copied {
            continue;
        }
        out.extend_from_slice(&bytes[copied..start]);
        out.extend_from_slice(format!("{output} 0 R").as_bytes());
        copied = end;
    }
    out.extend_from_slice(&bytes[copied..]);
    out
}

fn remap_object(
    body: &[u8],
    mapping: &std::collections::BTreeMap<SourceObjectId, usize>,
) -> Vec<u8> {
    let Some((stream_start, stream_end)) = stream_range(body) else {
        return remap_refs(body, mapping);
    };
    let mut out = remap_refs(&body[..stream_start], mapping);
    // A compressed image stream is arbitrary binary and may coincidentally
    // contain `12 0 R`; references exist only in its preceding dictionary.
    out.extend_from_slice(&body[stream_start..stream_end]);
    out.extend_from_slice(&remap_refs(&body[stream_end..], mapping));
    out
}

fn object_references(body: &[u8]) -> Vec<SourceObjectId> {
    let Some((stream_start, stream_end)) = stream_range(body) else {
        return pdf_references(body);
    };
    let mut references = pdf_references(&body[..stream_start]);
    references.extend(pdf_references(&body[stream_end..]));
    references
}

fn dictionary_inner(bytes: &[u8]) -> Option<&[u8]> {
    let start = find_bytes(bytes, b"<<")?.checked_add(2)?;
    let end = rfind_bytes(bytes, b">>")?;
    (start <= end).then(|| &bytes[start..end])
}

fn replace_named_refs_at(
    bytes: &[u8],
    names: &std::collections::BTreeMap<String, usize>,
    position: Option<NamedObjectPosition>,
) -> Vec<u8> {
    let tokens = pdf_tokens(bytes);
    let mut out = Vec::with_capacity(bytes.len());
    let mut copied = 0usize;
    for token in tokens {
        let Some(raw) = bytes.get(token.start..token.end) else {
            continue;
        };
        let replacement = match (raw, position) {
            (b"@xpos", Some(position)) => Some(pdf_number(position.x, 4)),
            (b"@ypos", Some(position)) => Some(pdf_number(position.y, 4)),
            (b"@thispage", Some(position)) => Some(format!("{} 0 R", position.page)),
            _ => None,
        };
        let replacement = if replacement.is_some() {
            replacement
        } else if let Some(name) = raw.strip_prefix(b"@")
            && let Ok(name) = std::str::from_utf8(name)
            && let Some(id) = names.get(name)
        {
            Some(format!("{id} 0 R"))
        } else {
            None
        };
        let Some(replacement) = replacement else {
            continue;
        };
        out.extend_from_slice(&bytes[copied..token.start]);
        out.extend_from_slice(replacement.as_bytes());
        copied = token.end;
    }
    out.extend_from_slice(&bytes[copied..]);
    out
}

fn replace_named_refs(bytes: &[u8], names: &std::collections::BTreeMap<String, usize>) -> Vec<u8> {
    replace_named_refs_at(bytes, names, None)
}

/// Remap the logical object numbers exposed through pdfTeX's `\pdflastobj`
/// to the deterministic object ids allocated by this writer. PDF strings and
/// names are indivisible tokens here, so text such as `(1 0 R)` is untouched.
fn replace_aitex_object_refs(
    bytes: &[u8],
    names: &std::collections::BTreeMap<String, usize>,
) -> Vec<u8> {
    let tokens = pdf_tokens(bytes);
    let mut out = Vec::with_capacity(bytes.len());
    let mut copied = 0usize;
    let mut index = 0usize;
    while index + 2 < tokens.len() {
        let number = token_u32(bytes, tokens[index]);
        let generation_zero = token_is(bytes, tokens[index + 1], b"0");
        let reference = token_is(bytes, tokens[index + 2], b"R");
        let id = number.and_then(|number| names.get(&format!("aitexobj{number}")));
        if generation_zero && reference && let Some(id) = id {
            out.extend_from_slice(&bytes[copied..tokens[index].start]);
            out.extend_from_slice(format!("{id} 0 R").as_bytes());
            copied = tokens[index + 2].end;
            index += 3;
        } else {
            index += 1;
        }
    }
    out.extend_from_slice(&bytes[copied..]);
    out
}

fn merged_named_object(
    object: &NamedPdfObject,
    names: &std::collections::BTreeMap<String, usize>,
) -> Vec<u8> {
    if object.complete {
        replace_aitex_object_refs(
            &replace_named_refs_at(&object.initial, names, object.position),
            names,
        )
    } else if let Some(initial) = dictionary_inner(&object.initial) {
        let mut merged = b"<< ".to_vec();
        merged.extend_from_slice(initial);
        for put in &object.puts {
            if let Some(inner) = dictionary_inner(put) {
                merged.push(b' ');
                merged.extend_from_slice(inner);
            }
        }
        merged.extend_from_slice(b" >>");
        replace_aitex_object_refs(
            &replace_named_refs_at(&merged, names, object.position),
            names,
        )
    } else {
        replace_aitex_object_refs(
            &replace_named_refs_at(&object.initial, names, object.position),
            names,
        )
    }
}

/// Parse dvipdfmx's complete `pdf: object @name ...` form.
///
/// Unlike `pdf:obj`, this spelling may carry a complete stream object in one
/// special. Work on bytes so binary stream payloads survive unchanged.
fn pdf_dvipdfmx_object_special(bytes: &[u8]) -> Option<(&str, &[u8])> {
    let rest = bytes
        .trim_ascii()
        .strip_prefix(b"pdf:")?
        .trim_ascii_start()
        .strip_prefix(b"object")?;
    if !rest.first().is_some_and(|byte| pdf_whitespace(*byte)) {
        return None;
    }
    let rest = rest.trim_ascii_start();
    let name_end = rest.iter().position(|byte| pdf_whitespace(*byte))?;
    let name = rest.get(..name_end)?.strip_prefix(b"@")?;
    let name = std::str::from_utf8(name).ok()?;
    let body = rest.get(name_end..)?.trim_ascii_start();
    (!name.is_empty() && !body.is_empty()).then_some((name, body))
}

/// Parse dvipdfmx's `pdf:stream` and `pdf:fstream` named-object specials.
/// The parenthesized payload is data for `stream` and a host filename for
/// `fstream`; the trailing dictionary supplies stream attributes.
fn pdf_dvipdfmx_stream_special(text: &str) -> Option<(bool, &str, Vec<u8>, Vec<u8>)> {
    let rest = text.trim().strip_prefix("pdf:")?.trim_start();
    let (file, rest) = if let Some(rest) = rest.strip_prefix("fstream") {
        (true, rest)
    } else if let Some(rest) = rest.strip_prefix("stream") {
        (false, rest)
    } else {
        return None;
    };
    let rest = rest.trim_start();
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = rest.get(..name_end)?.strip_prefix('@')?;
    if name.is_empty() {
        return None;
    }
    let payload = special_string(rest, &format!("@{name}"))?;
    let dictionary = dictionary_inner(rest.as_bytes())
        .map(|inner| {
            let mut dictionary = b"<<".to_vec();
            dictionary.extend_from_slice(inner);
            dictionary.extend_from_slice(b">>");
            dictionary
        })
        .unwrap_or_else(|| b"<<>>".to_vec());
    Some((file, name, dictionary, payload))
}

/// Parse `aitex:pdfobj{stream,file} @name <dict-byte-count>\n<dict><data>`.
fn pdf_object_stream_special<'a>(
    text: &'a str,
    prefix: &str,
) -> Option<(&'a str, &'a str, &'a str)> {
    let rest = text.strip_prefix(prefix)?.trim_start();
    let newline = rest.find('\n')?;
    let mut header = rest[..newline].split_whitespace();
    let name = header.next()?.strip_prefix('@')?;
    let dictionary_len = header.next()?.parse::<usize>().ok()?;
    if header.next().is_some() {
        return None;
    }
    let payload = rest.get(newline + 1..)?;
    let dictionary = payload.get(..dictionary_len)?;
    let data = payload.get(dictionary_len..)?;
    Some((name, dictionary, data))
}

fn pdf_named_special<'a>(text: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let text = text.trim();
    let rest = if let Some(prefix) = prefix.strip_prefix("pdf:") {
        text.strip_prefix("pdf:")?
            .trim_start()
            .strip_prefix(prefix)?
            .trim_start()
    } else {
        text.strip_prefix(prefix)?.trim_start()
    };
    let split = rest
        .find(|character: char| {
            character.is_whitespace() || matches!(character, '<' | '>' | '[' | ']' | '(' | ')')
        })
        .unwrap_or(rest.len());
    let name = rest.get(..split)?.strip_prefix('@')?;
    (!name.is_empty()).then(|| (name, rest.get(split..).unwrap_or_default().trim_start()))
}

fn pdf_command_special<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let rest = text
        .trim()
        .strip_prefix("pdf:")?
        .trim_start()
        .strip_prefix(command)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim_start())
}

fn pdf_dvipdfmx_annotation_special(text: &str) -> Option<(&str, f64, f64, f64, &str)> {
    let (name, fields) = pdf_named_special(text, "pdf:ann")?;
    let dictionary_at = fields.find("<<")?;
    let dimensions = fields.get(..dictionary_at)?.split_whitespace().collect::<Vec<_>>();
    let at = |key: &str| dimensions.iter().position(|field| *field == key);
    let width = at("width")
        .and_then(|index| dimensions.get(index + 1))
        .and_then(|value| pdf_dimension(value))?;
    let height = at("height")
        .and_then(|index| dimensions.get(index + 1))
        .and_then(|value| pdf_dimension(value))?;
    let depth = at("depth")
        .and_then(|index| dimensions.get(index + 1))
        .and_then(|value| pdf_dimension(value))
        .unwrap_or(0.0);
    Some((name, width, height, depth, fields.get(dictionary_at..)?))
}

/// Parse dvipdfm(x)'s positioned, graphics-state-isolated PDF literal.
///
/// Both `pdf:content` and `pdf: content` occur in TeX Live packages.  Do not
/// accept longer command names which merely begin with `content`.
fn pdf_content_special(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix("pdf:")?.trim_start();
    let literal = rest.strip_prefix("content")?;
    (literal.is_empty() || literal.starts_with(char::is_whitespace))
        .then(|| literal.trim_start())
}

/// Parse aiTeX's private binding between a pdfTeX-visible logical object and
/// the one-based absolute page number that object denotes.
fn pdf_page_ref_special(text: &str) -> Option<(&str, usize)> {
    let (name, page) = pdf_named_special(text, "aitex:pdfpageref")?;
    let page = page.parse::<usize>().ok()?;
    (page > 0).then_some((name, page))
}

fn pdf_xobject_begin(text: &str) -> Option<(&str, [f64; 4])> {
    let (name, fields) = pdf_named_special(text, "pdf:bxobj")?;
    let words = fields.split_whitespace().collect::<Vec<_>>();
    let at = |key: &str| words.iter().position(|word| *word == key);
    if let Some(index) = at("bbox") {
        let coordinate = |offset: usize| {
            words
                .get(index + offset)
                .and_then(|value| pdf_dimension(value))
        };
        return Some((
            name,
            [coordinate(1)?, coordinate(2)?, coordinate(3)?, coordinate(4)?],
        ));
    }
    let width = at("width")
        .and_then(|index| words.get(index + 1))
        .and_then(|value| pdf_dimension(value))?;
    let height = at("height")
        .and_then(|index| words.get(index + 1))
        .and_then(|value| pdf_dimension(value))?;
    let depth = at("depth")
        .and_then(|index| words.get(index + 1))
        .and_then(|value| pdf_dimension(value))
        .unwrap_or(0.0);
    Some((name, [0.0, -depth, width, height]))
}

fn pdf_resource_fragments(
    fragments: &[Vec<u8>],
    names: &std::collections::BTreeMap<String, usize>,
) -> Vec<u8> {
    let mut out = Vec::new();
    for fragment in fragments {
        if let Some(inner) = dictionary_inner(fragment) {
            out.push(b' ');
            out.extend_from_slice(&replace_named_refs(inner, names));
        }
    }
    out
}

fn pdf_dictionary_fragments(
    fragments: &[Vec<u8>],
    names: &std::collections::BTreeMap<String, usize>,
    position: Option<NamedObjectPosition>,
) -> Vec<u8> {
    let mut out = Vec::new();
    for fragment in fragments {
        if let Some(inner) = dictionary_inner(fragment) {
            out.push(b' ');
            out.extend_from_slice(&replace_named_refs_at(inner, names, position));
        }
    }
    out
}

fn write_captured_xobject(
    output: &mut Objects,
    mut capture: PdfXObjectCapture,
    extra: &[u8],
    names: &std::collections::BTreeMap<String, usize>,
) {
    for _ in capture.content_coords.drain(..) {
        capture.content.extend_from_slice(b"Q\n");
    }
    let xobjects = capture
        .xobjects
        .iter()
        .map(|(name, id)| format!("/{} {id} 0 R", pdf_name(name)))
        .collect::<Vec<_>>()
        .join(" ");
    let fonts = capture
        .fonts
        .iter()
        .map(|id| format!("/F{id} {id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    let resources = pdf_resource_fragments(&capture.resources, names);
    let extra = dictionary_inner(extra).unwrap_or_default();
    let mut object = format!(
        "<< /Type /XObject /Subtype /Form /FormType 1 \
         /BBox [{:.8} {:.8} {:.8} {:.8}] /Resources << /Font << {fonts} >> /XObject << {xobjects} >>",
        capture.bbox[0], capture.bbox[1], capture.bbox[2], capture.bbox[3]
    )
    .into_bytes();
    object.extend_from_slice(&resources);
    object.extend_from_slice(b" >> ");
    object.extend_from_slice(&replace_named_refs(extra, names));
    output.write_stream(capture.id, &object, &capture.content);
}

fn inherited_page_value<'a>(
    objects: &'a [SourceObject<'a>],
    page: SourceObjectId,
    key: &[u8],
) -> Option<&'a [u8]> {
    let mut current = page;
    for _ in 0..32 {
        let object = objects.iter().find(|object| object.id == current)?;
        if let Some((start, end)) = pdf_value_span(&object.body, key) {
            return object.body.get(start..end);
        }
        let (start, end) = pdf_value_span(&object.body, b"Parent")?;
        current = *pdf_references(object.body.get(start..end)?).first()?;
    }
    None
}

/// Follow a value that is stored in a standalone indirect object.
///
/// Page attributes are commonly inline arrays or numbers, but PDF permits
/// values such as `/CropBox 92 0 R`.  Keep this deliberately narrow: only a
/// complete three-token indirect reference is followed, so arrays and
/// dictionaries containing references retain their original syntax.
fn resolved_pdf_value<'a>(
    objects: &'a [SourceObject<'a>],
    mut value: &'a [u8],
) -> Option<&'a [u8]> {
    for _ in 0..32 {
        let tokens = pdf_tokens(value);
        if tokens.len() != 3 || !token_is(value, tokens[2], b"R") {
            return Some(value);
        }
        let id = SourceObjectId {
            number: token_u32(value, tokens[0])?,
            generation: token_u16(value, tokens[1])?,
        };
        value = objects.iter().find(|object| object.id == id)?.body;
    }
    None
}

fn page_objects<'objects, 'bytes>(
    objects: &'objects [SourceObject<'bytes>],
) -> impl Iterator<Item = &'objects SourceObject<'bytes>> {
    objects.iter().filter(|object| {
        pdf_value_span(&object.body, b"Type")
            .is_some_and(|(start, end)| object.body.get(start..end) == Some(b"/Page".as_slice()))
    })
}

fn pdf_box(bytes: &[u8]) -> Option<[f64; 4]> {
    let tokens = pdf_tokens(bytes);
    let values = tokens
        .iter()
        .filter_map(|token| {
            std::str::from_utf8(bytes.get(token.start..token.end)?)
                .ok()?
                .parse::<f64>()
                .ok()
        })
        .collect::<Vec<_>>();
    let [x1, y1, x2, y2] = values.as_slice() else {
        return None;
    };
    if ![x1, y1, x2, y2].iter().all(|value| value.is_finite())
        || x1 == x2
        || y1 == y2
    {
        return None;
    }
    // A PDF rectangle is two opposite corners, not an ordered lower-left and
    // upper-right pair. Producers in the wild emit reversed coordinates (for
    // example a CropBox whose y coordinates run top-to-bottom); pdfTeX and
    // Poppler normalize them. Rejecting such a box made the graphics route
    // fall back to a much larger MediaBox and changed document pagination.
    Some([x1.min(*x2), y1.min(*y2), x1.max(*x2), y1.max(*y2)])
}

/// Return the first page's raw crop box when a classic-xref PDF can be
/// inspected directly. The CLI combines this with the inherited page rotation
/// before exposing natural dimensions to a graphics driver.
pub fn first_page_bbox(bytes: &[u8]) -> Option<[f64; 4]> {
    let objects = parse_normalized_pdf(bytes)?;
    first_page_bbox_from(&objects)
}

fn first_page_bbox_from(objects: &[SourceObject<'_>]) -> Option<[f64; 4]> {
    let page = page_objects(&objects).next()?;
    for key in [b"CropBox".as_slice(), b"MediaBox".as_slice()] {
        if let Some(value) = inherited_page_value(&objects, page.id, key)
            && let Some(value) = resolved_pdf_value(&objects, value)
            && let Some(bbox) = pdf_box(value)
        {
            return Some(bbox);
        }
    }
    None
}

/// Return the first page's inherited `/Rotate`, normalized to one of PDF's
/// four right-angle page rotations.  A missing entry means zero rotation.
pub fn first_page_rotation(bytes: &[u8]) -> Option<i16> {
    let objects = parse_normalized_pdf(bytes)?;
    first_page_rotation_from(&objects)
}

fn first_page_rotation_from(objects: &[SourceObject<'_>]) -> Option<i16> {
    let page = page_objects(&objects).next()?;
    let Some(value) = inherited_page_value(&objects, page.id, b"Rotate") else {
        return Some(0);
    };
    let value = resolved_pdf_value(&objects, value)?;
    let token = pdf_tokens(value).into_iter().next()?;
    let angle = std::str::from_utf8(value.get(token.start..token.end)?)
        .ok()?
        .parse::<i32>()
        .ok()?
        .rem_euclid(360);
    matches!(angle, 0 | 90 | 180 | 270).then_some(angle as i16)
}

/// The box seen by a page consumer after applying `/Rotate`.
///
/// `extractbb` retains the selected box's lower-left coordinate and swaps its
/// extents for quarter turns.  The synthetic-XBB path must do the same: using
/// the raw portrait crop box for a landscape page makes graphicx believe a
/// short, wide figure is hundreds of points tall.
pub fn first_page_effective_bbox(bytes: &[u8]) -> Option<[f64; 4]> {
    let objects = parse_normalized_pdf(bytes)?;
    first_page_effective_bbox_from(&objects)
}

fn first_page_effective_bbox_from(objects: &[SourceObject<'_>]) -> Option<[f64; 4]> {
    let [llx, lly, urx, ury] = first_page_bbox_from(objects)?;
    match first_page_rotation_from(objects)? {
        90 | 270 => Some([llx, lly, llx + ury - lly, lly + urx - llx]),
        0 | 180 => Some([llx, lly, urx, ury]),
        _ => None,
    }
}

/// Return the first page box whose extents match pdfTeX's natural image size.
///
/// A PDF crop box is allowed to extend beyond its media box, but page
/// consumers display their intersection. pdfTeX applies that clipping before
/// assigning the dimensions of `\pdfrefximage`; an XBB containing the raw
/// crop box would therefore make `graphicx` reserve too much space. Keep the
/// resulting lower-left coordinate because an ordinary nonzero crop origin is
/// still part of the bounding box consumed by the graphics package.
pub fn first_page_pdftex_bbox(bytes: &[u8]) -> Option<[f64; 4]> {
    let objects = parse_normalized_pdf(bytes)?;
    first_page_pdftex_bbox_from(&objects)
}

fn first_page_pdftex_bbox_from(objects: &[SourceObject<'_>]) -> Option<[f64; 4]> {
    let page = page_objects(&objects).next()?;
    let media = inherited_page_value(&objects, page.id, b"MediaBox")
        .and_then(|value| resolved_pdf_value(&objects, value))
        .and_then(pdf_box)?;
    let crop = inherited_page_value(&objects, page.id, b"CropBox")
        .and_then(|value| resolved_pdf_value(&objects, value))
        .and_then(pdf_box)
        .unwrap_or(media);
    let [media_llx, media_lly, media_urx, media_ury] = media;
    let [crop_llx, crop_lly, crop_urx, crop_ury] = crop;
    let llx = media_llx.max(crop_llx);
    let lly = media_lly.max(crop_lly);
    let urx = media_urx.min(crop_urx);
    let ury = media_ury.min(crop_ury);
    if urx <= llx || ury <= lly {
        return None;
    }
    match first_page_rotation_from(objects)? {
        90 | 270 => Some([llx, lly, llx + ury - lly, lly + urx - llx]),
        0 | 180 => Some([llx, lly, urx, ury]),
        _ => None,
    }
}

/// Return the number of page objects when a classic-xref PDF can be inspected
/// directly. Besides selecting pages for import, TeX graphics drivers consume
/// this value from an `.xbb` sidecar before any page is embedded.
pub fn pdf_page_count(bytes: &[u8]) -> Option<usize> {
    let objects = parse_normalized_pdf(bytes)?;
    let count = page_objects(&objects).count();
    (count > 0).then_some(count)
}

/// Geometry discovered while proving that a source PDF can be embedded
/// directly. Keeping these answers together lets host-side graphics probing
/// parse a figure once instead of separately asking about its page count,
/// crop box, rotation, and importability.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectPdfPageInfo {
    pub bbox: [f64; 4],
    pub page_count: usize,
}

/// Inspect and validate an original PDF for direct Form import in one parse.
///
/// This deliberately retains the importer's full graph walk and stream
/// checks. A classic xref and plausible page dictionary alone are not enough:
/// incremental PDFs can refer to resource objects that are absent from their
/// newest table and must still go through host normalization.
pub fn direct_pdf_page_info(bytes: &[u8], page: u32) -> Option<DirectPdfPageInfo> {
    if page.max(1) != 1 {
        return None;
    }
    let objects = parse_normalized_pdf(bytes)?;
    let page_count = page_objects(&objects).count();
    if page_count != 1 {
        return None;
    }
    let bbox = first_page_pdftex_bbox_from(&objects)?;
    import_pdf_form_from(&mut Objects::new(), bbox, &objects)?;
    Some(DirectPdfPageInfo { bbox, page_count })
}

/// Whether the original PDF bytes are already in the exact form accepted by
/// the vector importer. This deliberately admits only a requested first page
/// from a single-page, classic-xref file. Multi-page selection, object streams,
/// incremental xrefs, and unusual content arrays continue through MuPDF's
/// normalizer rather than relying on file-order guesses.
pub fn directly_importable_single_page_pdf(bytes: &[u8], page: u32) -> bool {
    direct_pdf_page_info(bytes, page).is_some()
}

/// Import one normalized source-PDF page as a Form XObject. All source
/// objects are copied with remapped object numbers; keeping the resource graph
/// intact is both safer and simpler than recognizing every font/image/filter
/// object that a PDF producer may emit.
fn import_pdf_form(output: &mut Objects, bytes: &[u8], bbox: [f64; 4]) -> Option<usize> {
    let objects = parse_normalized_pdf(bytes)?;
    import_pdf_form_from(output, bbox, &objects)
}

fn import_pdf_form_from(
    output: &mut Objects,
    bbox: [f64; 4],
    objects: &[SourceObject<'_>],
) -> Option<usize> {
    let page = page_objects(&objects).next()?;
    let resources = inherited_page_value(&objects, page.id, b"Resources")
        .unwrap_or(b"<< >>")
        .to_vec();
    // A page transparency group becomes the Form XObject's transparency
    // group when the page is embedded.  Omitting it changes isolated-group
    // compositing (and, in particular, CMYK blending) even when every object
    // and resource referenced by the page content was copied correctly.
    let group = pdf_value_span(&page.body, b"Group")
        .and_then(|(start, end)| page.body.get(start..end))
        .map(Vec::from);
    let (contents_start, contents_end) = pdf_value_span(&page.body, b"Contents")?;
    let mut content_ids = Vec::new();
    content_stream_ids(
        &objects,
        page.body.get(contents_start..contents_end)?,
        0,
        &mut content_ids,
    )?;
    // Preserve an existing encoded page-content stream directly. Most figure
    // PDFs have exactly one Flate stream; decoding it in a child process made
    // both the normalized intermediate and our final Form tens of megabytes
    // larger. Multiple unfiltered streams can still be concatenated safely.
    let mut content = Vec::new();
    let mut stream_attributes = Vec::new();
    for content_id in &content_ids {
        let object = objects.iter().find(|object| object.id == *content_id)?;
        if let Some((start, end)) = pdf_value_span(&object.body, b"Filter") {
            if content_ids.len() != 1 {
                return None;
            }
            stream_attributes.extend_from_slice(b" /Filter ");
            stream_attributes.extend_from_slice(object.body.get(start..end)?);
            if let Some((start, end)) = pdf_value_span(&object.body, b"DecodeParms") {
                stream_attributes.extend_from_slice(b" /DecodeParms ");
                stream_attributes.extend_from_slice(object.body.get(start..end)?);
            }
        }
        let (start, end) = stream_range(&object.body)?;
        content.push(object.body.get(start..end)?);
    }

    // A page's Catalog/Pages/Page/Contents objects are scaffolding, not part
    // of an embedded Form. Copy only the transitive graph reachable from the
    // Form resources and transparency group. Besides producing the right PDF
    // object graph, this avoids retaining a second copy of large decompressed
    // plotting streams that are written once below as the Form's content.
    let mut pending = pdf_references(&resources);
    if let Some(group) = &group {
        pending.extend(pdf_references(group));
    }
    pending.extend(pdf_references(&stream_attributes));
    let mut reachable = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let object = objects.iter().find(|object| object.id == id)?;
        pending.extend(object_references(&object.body));
    }
    let mut mapping = std::collections::BTreeMap::new();
    for id in &reachable {
        mapping.insert(*id, output.reserve());
    }
    for source_id in &reachable {
        let object = objects.iter().find(|object| object.id == *source_id)?;
        let id = *mapping.get(source_id)?;
        output.write_owned(id, remap_object(&object.body, &mapping));
    }
    let resources = remap_refs(&resources, &mapping);
    let group = group.map(|group| remap_refs(&group, &mapping));
    let stream_attributes = remap_refs(&stream_attributes, &mapping);
    let form_id = output.reserve();
    let [llx, lly, urx, ury] = bbox;
    // A page dictionary's `/Rotate` is viewer metadata; it does not transform
    // the page content stream when that stream is reused as a Form XObject.
    // Reproduce the page transform in the Form matrix. `bbox` is the effective
    // box advertised by extractbb, while a Form's own `/BBox` must remain in
    // the unrotated content coordinate system because its `/Matrix` transforms
    // both the content and that clipping box.
    let effective_width = urx - llx;
    let effective_height = ury - lly;
    let (form_bbox, matrix) = match first_page_rotation_from(objects)? {
        0 => (bbox, None),
        90 => {
            let source_urx = llx + effective_height;
            let source_ury = lly + effective_width;
            (
                [llx, lly, source_urx, source_ury],
                Some([0.0, -1.0, 1.0, 0.0, llx - lly, lly + source_urx]),
            )
        }
        180 => (
            bbox,
            Some([-1.0, 0.0, 0.0, -1.0, llx + urx, lly + ury]),
        ),
        270 => {
            let source_urx = llx + effective_height;
            let source_ury = lly + effective_width;
            (
                [llx, lly, source_urx, source_ury],
                Some([0.0, 1.0, -1.0, 0.0, llx + source_ury, lly - llx]),
            )
        }
        _ => return None,
    };
    let [form_llx, form_lly, form_urx, form_ury] = form_bbox;
    let mut form = format!(
        "<< /Type /XObject /Subtype /Form /FormType 1 \
         /BBox [{form_llx} {form_lly} {form_urx} {form_ury}]"
    )
    .into_bytes();
    if let Some([a, b, c, d, e, f]) = matrix {
        form.extend_from_slice(format!(" /Matrix [{a} {b} {c} {d} {e} {f}]").as_bytes());
    }
    form.extend_from_slice(b" /Resources ");
    form.extend_from_slice(&resources);
    if let Some(group) = group {
        form.extend_from_slice(b" /Group ");
        form.extend_from_slice(&group);
    }
    if stream_attributes.is_empty() {
        // An unfiltered synthesized stream can carry an explicit state
        // boundary in addition to the isolation guaranteed by Form `Do`.
        output.write_form_stream(form_id, &form, &content);
    } else {
        output.write_encoded_form_stream(form_id, &form, &stream_attributes, content[0]);
    }
    Some(form_id)
}
