/// Read one PostScript/PDF literal string following `key`.  Hyperref's dvips
/// backend emits its URI, destination, bookmark and metadata fields in this
/// form, including octal escapes and UTF-16BE strings.
fn special_string(text: &str, key: &str) -> Option<Vec<u8>> {
    for (at, _) in text.match_indices(key) {
        let mut p = at + key.len();
        let b = text.as_bytes();
        while b.get(p).is_some_and(u8::is_ascii_whitespace) {
            p += 1;
        }
        if b.get(p) != Some(&b'(') {
            continue;
        }
        p += 1;
        let mut depth = 1usize;
        let mut out = Vec::new();
        while let Some(&c) = b.get(p) {
            p += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(&next) = b.get(p) else {
                        break;
                    };
                    if next.is_ascii_digit() && next < b'8' {
                        let mut value = 0u8;
                        let mut n = 0;
                        while n < 3 {
                            let Some(&d) = b.get(p) else { break };
                            if !(b'0'..=b'7').contains(&d) {
                                break;
                            }
                            value = value.wrapping_mul(8).wrapping_add(d - b'0');
                            p += 1;
                            n += 1;
                        }
                        out.push(value);
                    } else {
                        p += 1;
                        match next {
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            b'\n' => {}
                            b'\r' => {
                                if b.get(p) == Some(&b'\n') {
                                    p += 1;
                                }
                            }
                            _ => out.push(next),
                        }
                    }
                }
                b'^' if b.get(p) == Some(&b'^')
                    && b.get(p + 1).is_some_and(u8::is_ascii_hexdigit)
                    && b.get(p + 2).is_some_and(u8::is_ascii_hexdigit) =>
                {
                    let hex = |value: u8| match value {
                        b'0'..=b'9' => value - b'0',
                        b'a'..=b'f' => value - b'a' + 10,
                        b'A'..=b'F' => value - b'A' + 10,
                        _ => 0,
                    };
                    out.push(hex(b[p + 1]) * 16 + hex(b[p + 2]));
                    p += 3;
                }
                _ => out.push(c),
            }
        }
    }
    None
}

fn special_integer(text: &str, key: &str) -> Option<i32> {
    for (at, _) in text.match_indices(key) {
        let rest = text.get(at + key.len()..)?.trim_start();
        let token = rest
            .split(|c: char| c.is_ascii_whitespace() || matches!(c, '/' | ']' | '>'))
            .next()?;
        if let Ok(value) = token.parse() {
            return Some(value);
        }
    }
    None
}

/// Read a flat array of finite PDF numbers following `key`, normalizing it on
/// the way out. Hyperref's direct-pdf backend uses this for annotation borders
/// and colors (`/Border[0 0 1]`, `/C[1 0 0]`). Parsing numbers instead of
/// copying arbitrary source text keeps the generated object dictionary valid.
fn special_number_array(text: &str, key: &str) -> Option<String> {
    for (at, _) in text.match_indices(key) {
        let mut p = at + key.len();
        let bytes = text.as_bytes();
        while bytes.get(p).is_some_and(u8::is_ascii_whitespace) {
            p += 1;
        }
        if bytes.get(p) != Some(&b'[') {
            continue;
        }
        let end = text.get(p + 1..)?.find(']')? + p + 1;
        let values = text.get(p + 1..end)?;
        let tokens = values.split_ascii_whitespace().collect::<Vec<_>>();
        if tokens.is_empty()
            || tokens
                .iter()
                .any(|token| token.parse::<f64>().ok().is_none_or(|value| !value.is_finite()))
        {
            continue;
        }
        return Some(format!("[{}]", tokens.join(" ")));
    }
    None
}

/// Read a PDF name following `key`, including its leading slash.
fn special_name(text: &str, key: &str) -> Option<String> {
    for (at, _) in text.match_indices(key) {
        let mut p = at + key.len();
        let bytes = text.as_bytes();
        while bytes.get(p).is_some_and(u8::is_ascii_whitespace) {
            p += 1;
        }
        if bytes.get(p) != Some(&b'/') {
            continue;
        }
        let start = p;
        p += 1;
        while bytes
            .get(p)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            p += 1;
        }
        if p > start + 1 {
            return text.get(start..p).map(String::from);
        }
    }
    None
}

fn decimal(text: &str) -> Option<f64> {
    text.trim().parse().ok()
}

/// Convert a TeX dimension from a dvips special to PDF points (`bp`).
fn pdf_dimension(text: &str) -> Option<f64> {
    let text = text.trim();
    for (unit, scale) in [
        ("truept", 72.0 / 72.27),
        ("pt", 72.0 / 72.27),
        ("bp", 1.0),
        ("in", 72.0),
        ("cm", 72.0 / 2.54),
        ("mm", 72.0 / 25.4),
    ] {
        if let Some(n) = text.strip_suffix(unit).and_then(decimal) {
            return Some(n * scale);
        }
    }
    decimal(text)
}

fn special_page_size(text: &str) -> Option<(f64, f64)> {
    let rest = text.trim().strip_prefix("papersize=")?;
    let (w, h) = rest.split_once(',')?;
    Some((pdf_dimension(w)?, pdf_dimension(h)?))
}

fn private_page_size(text: &str) -> Option<(f64, f64)> {
    let rest = text.trim().strip_prefix("aitex:page-size ")?;
    let (w, h) = rest.split_once(',')?;
    Some((pdf_dimension(w)?, pdf_dimension(h)?))
}

fn private_latex_page_size(text: &str) -> Option<(f64, f64)> {
    let rest = text.trim().strip_prefix("aitex:latex-page-size ")?;
    let (w, h) = rest.split_once(',')?;
    Some((pdf_dimension(w)?, pdf_dimension(h)?))
}

fn private_page_origin(text: &str) -> Option<(f64, f64)> {
    let rest = text.trim().strip_prefix("aitex:page-origin ")?;
    let (x, y) = rest.split_once(',')?;
    Some((pdf_dimension(x)?, pdf_dimension(y)?))
}

fn private_eps_size(text: &str) -> Option<(f64, f64)> {
    let rest = text.trim().strip_prefix("aitex:eps-size ")?;
    let (width, height) = rest.split_once(',')?;
    Some((pdf_dimension(width)?, pdf_dimension(height)?))
}

fn private_pdftex_graphic_scale(text: &str) -> Option<(f64, f64)> {
    let rest = text
        .trim()
        .strip_prefix("aitex:pdftex-graphic-scale ")?;
    let (scale_x, scale_y) = rest.split_once(',')?;
    let scale_x = decimal(scale_x)?;
    let scale_y = decimal(scale_y)?;
    (scale_x.is_finite() && scale_y.is_finite()).then_some((scale_x, scale_y))
}

fn special_page_rotation(text: &str) -> Option<i16> {
    text.trim()
        .strip_prefix("aitex:page-rotate ")?
        .trim()
        .parse()
        .ok()
}

fn special_destination_view(text: &str) -> DestinationView {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.contains("/FitBH") {
        DestinationView::FitBH
    } else if compact.contains("/FitBV") {
        DestinationView::FitBV
    } else if compact.contains("/FitH") {
        DestinationView::FitH
    } else if compact.contains("/FitV") {
        DestinationView::FitV
    } else if compact.contains("/FitB") {
        DestinationView::FitB
    } else if compact.contains("/Fit") {
        DestinationView::Fit
    } else {
        DestinationView::Xyz
    }
}

fn special_rotation_start(text: &str) -> Option<f64> {
    let rest = text
        .trim()
        .strip_prefix("ps: gsave currentpoint currentpoint translate ")?;
    let mut words = rest.split_whitespace();
    let angle: f64 = words.next()?.parse().ok()?;
    // dvips works in a coordinate system whose vertical axis is opposite
    // PDF's. Its `N neg rotate` therefore becomes a positive PDF rotation.
    match (words.next(), words.next()) {
        (Some("neg"), Some("rotate")) => Some(angle),
        (Some("rotate"), _) => Some(-angle),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Color(f64, f64, f64);

impl Color {
    const BLACK: Self = Self(0.0, 0.0, 0.0);

    fn cmyk(cyan: f64, magenta: f64, yellow: f64, black: f64) -> Self {
        Self(
            1.0 - (cyan + black).min(1.0),
            1.0 - (magenta + black).min(1.0),
            1.0 - (yellow + black).min(1.0),
        )
    }

    fn operators(self) -> String {
        let red = pdf_number(self.0, 6);
        let green = pdf_number(self.1, 6);
        let blue = pdf_number(self.2, 6);
        format!(
            "{red} {green} {blue} rg {red} {green} {blue} RG\n"
        )
    }
}

fn special_color(text: &str) -> Option<Color> {
    let mut words = text.split_whitespace();
    if words.next()? != "color" || words.next()? != "push" {
        return None;
    }
    let payload: Vec<&str> = words.collect();
    // dvipdfmx.def accepts ready-to-emit PDF operators in addition to its
    // named color models. xcolor uses this form for rules and backgrounds,
    // for example `color push 1 g 1 G` for white. Parse the nonstroking value
    // and apply it to both fill and stroke, as the model paths below do.
    if let [gray, "g", ..] = payload.as_slice()
        && let Some(gray) = decimal(gray)
    {
        return Some(Color(gray, gray, gray));
    }
    if let [red, green, blue, "rg", ..] = payload.as_slice()
        && let (Some(red), Some(green), Some(blue)) =
            (decimal(red), decimal(green), decimal(blue))
    {
        return Some(Color(red, green, blue));
    }
    if let [cyan, magenta, yellow, black, "k", ..] = payload.as_slice()
        && let (Some(cyan), Some(magenta), Some(yellow), Some(black)) = (
            decimal(cyan),
            decimal(magenta),
            decimal(yellow),
            decimal(black),
        )
    {
        return Some(Color::cmyk(cyan, magenta, yellow, black));
    }
    let model = *payload.first()?;
    let numbers: Vec<f64> = payload[1..].iter().filter_map(|word| decimal(word)).collect();
    match (model, numbers.as_slice()) {
        ("rgb", [r, g, b, ..]) => Some(Color(*r, *g, *b)),
        ("gray", [g, ..]) => Some(Color(*g, *g, *g)),
        ("cmyk", [c, m, y, k, ..]) => Some(Color::cmyk(*c, *m, *y, *k)),
        (_, _) => match model.to_ascii_lowercase().as_str() {
            // The standard `dvipsnames` palette. pdfTeX/xcolor carries these
            // names into DVI specials instead of expanding them to numbers,
            // so a direct PDF driver must implement the same fixed palette.
            "greenyellow" => Some(Color::cmyk(0.15, 0.0, 0.69, 0.0)),
            "yellow" => Some(Color::cmyk(0.0, 0.0, 1.0, 0.0)),
            "goldenrod" => Some(Color::cmyk(0.0, 0.10, 0.84, 0.0)),
            "dandelion" => Some(Color::cmyk(0.0, 0.29, 0.84, 0.0)),
            "apricot" => Some(Color::cmyk(0.0, 0.32, 0.52, 0.0)),
            "peach" => Some(Color::cmyk(0.0, 0.50, 0.70, 0.0)),
            "melon" => Some(Color::cmyk(0.0, 0.46, 0.50, 0.0)),
            "yelloworange" => Some(Color::cmyk(0.0, 0.42, 1.0, 0.0)),
            "orange" => Some(Color::cmyk(0.0, 0.61, 0.87, 0.0)),
            "burntorange" => Some(Color::cmyk(0.0, 0.51, 1.0, 0.0)),
            "bittersweet" => Some(Color::cmyk(0.0, 0.75, 1.0, 0.24)),
            "redorange" => Some(Color::cmyk(0.0, 0.77, 0.87, 0.0)),
            "mahogany" => Some(Color::cmyk(0.0, 0.85, 0.87, 0.35)),
            "maroon" => Some(Color::cmyk(0.0, 0.87, 0.68, 0.32)),
            "brickred" => Some(Color::cmyk(0.0, 0.89, 0.94, 0.28)),
            "red" => Some(Color::cmyk(0.0, 1.0, 1.0, 0.0)),
            "orangered" => Some(Color::cmyk(0.0, 1.0, 0.50, 0.0)),
            "rubinered" => Some(Color::cmyk(0.0, 1.0, 0.13, 0.0)),
            "wildstrawberry" => Some(Color::cmyk(0.0, 0.96, 0.39, 0.0)),
            "salmon" => Some(Color::cmyk(0.0, 0.53, 0.38, 0.0)),
            "carnationpink" => Some(Color::cmyk(0.0, 0.63, 0.0, 0.0)),
            "magenta" => Some(Color::cmyk(0.0, 1.0, 0.0, 0.0)),
            "violetred" => Some(Color::cmyk(0.0, 0.81, 0.0, 0.0)),
            "rhodamine" => Some(Color::cmyk(0.0, 0.82, 0.0, 0.0)),
            "mulberry" => Some(Color::cmyk(0.34, 0.90, 0.0, 0.02)),
            "redviolet" => Some(Color::cmyk(0.07, 0.90, 0.0, 0.34)),
            "fuchsia" => Some(Color::cmyk(0.47, 0.91, 0.0, 0.08)),
            "lavender" => Some(Color::cmyk(0.0, 0.48, 0.0, 0.0)),
            "thistle" => Some(Color::cmyk(0.12, 0.59, 0.0, 0.0)),
            "orchid" => Some(Color::cmyk(0.32, 0.64, 0.0, 0.0)),
            "darkorchid" => Some(Color::cmyk(0.40, 0.80, 0.20, 0.0)),
            "purple" => Some(Color::cmyk(0.45, 0.86, 0.0, 0.0)),
            "plum" => Some(Color::cmyk(0.50, 1.0, 0.0, 0.0)),
            "violet" => Some(Color::cmyk(0.79, 0.88, 0.0, 0.0)),
            "royalpurple" => Some(Color::cmyk(0.75, 0.90, 0.0, 0.0)),
            "blueviolet" => Some(Color::cmyk(0.86, 0.91, 0.0, 0.04)),
            "periwinkle" => Some(Color::cmyk(0.57, 0.55, 0.0, 0.0)),
            "cadetblue" => Some(Color::cmyk(0.62, 0.57, 0.23, 0.0)),
            "cornflowerblue" => Some(Color::cmyk(0.65, 0.13, 0.0, 0.0)),
            "midnightblue" => Some(Color::cmyk(0.98, 0.13, 0.0, 0.43)),
            "navyblue" => Some(Color::cmyk(0.94, 0.54, 0.0, 0.0)),
            "royalblue" => Some(Color::cmyk(1.0, 0.50, 0.0, 0.0)),
            "blue" => Some(Color::cmyk(1.0, 1.0, 0.0, 0.0)),
            "cerulean" => Some(Color::cmyk(0.94, 0.11, 0.0, 0.0)),
            "cyan" => Some(Color::cmyk(1.0, 0.0, 0.0, 0.0)),
            "processblue" => Some(Color::cmyk(0.96, 0.0, 0.0, 0.0)),
            "skyblue" => Some(Color::cmyk(0.62, 0.0, 0.12, 0.0)),
            "turquoise" => Some(Color::cmyk(0.85, 0.0, 0.20, 0.0)),
            "tealblue" => Some(Color::cmyk(0.86, 0.0, 0.34, 0.02)),
            "aquamarine" => Some(Color::cmyk(0.82, 0.0, 0.30, 0.0)),
            "bluegreen" => Some(Color::cmyk(0.85, 0.0, 0.33, 0.0)),
            "emerald" => Some(Color::cmyk(1.0, 0.0, 0.50, 0.0)),
            "junglegreen" => Some(Color::cmyk(0.99, 0.0, 0.52, 0.0)),
            "seagreen" => Some(Color::cmyk(0.69, 0.0, 0.50, 0.0)),
            "green" => Some(Color::cmyk(1.0, 0.0, 1.0, 0.0)),
            "forestgreen" => Some(Color::cmyk(0.91, 0.0, 0.88, 0.12)),
            "pinegreen" => Some(Color::cmyk(0.92, 0.0, 0.59, 0.25)),
            "limegreen" => Some(Color::cmyk(0.50, 0.0, 1.0, 0.0)),
            "yellowgreen" => Some(Color::cmyk(0.44, 0.0, 0.74, 0.0)),
            "springgreen" => Some(Color::cmyk(0.26, 0.0, 0.76, 0.0)),
            "olivegreen" => Some(Color::cmyk(0.64, 0.0, 0.95, 0.40)),
            "rawsienna" => Some(Color::cmyk(0.0, 0.72, 1.0, 0.45)),
            "sepia" => Some(Color::cmyk(0.0, 0.83, 1.0, 0.70)),
            "brown" => Some(Color::cmyk(0.0, 0.81, 1.0, 0.60)),
            "tan" => Some(Color::cmyk(0.14, 0.42, 0.56, 0.0)),
            "gray" => Some(Color::cmyk(0.0, 0.0, 0.0, 0.50)),
            "black" => Some(Color::BLACK),
            "white" => Some(Color(1.0, 1.0, 1.0)),
            _ => None,
        },
    }
}

/// The dvips color/pagecolor drivers describe a page background with the same
/// color payload used by `color push`, but under a `background` prefix.  The
/// PostScript-shaped reset (`background "newpath clip`) deliberately remains
/// unparsed: a PDF page starts white and needs no paint for that form.
fn special_background(text: &str) -> Option<Color> {
    let payload = text.trim().strip_prefix("background ")?;
    let mut color = String::from("color push ");
    color.push_str(payload);
    special_color(&color)
}

#[derive(Debug)]
struct EpsSpecial {
    name: String,
    llx: f64,
    lly: f64,
    urx: f64,
    ury: f64,
    width: Option<f64>,
    height: Option<f64>,
    clip: bool,
}

#[derive(Debug)]
struct ImageSpecial {
    name: String,
    page: u32,
    /// Explicit viewport from a `bbox` clause. PGF's image inclusion path
    /// legitimately omits it and relies on the raster's natural box.
    bbox: Option<[f64; 4]>,
    width: Option<f64>,
    height: Option<f64>,
    clip: bool,
}

/// The image form emitted by `dvipdfmx.def`:
///
/// `pdf:image bbox 0 0 640 480 clip 0 width 120.0pt (figure.png)`
///
/// PGF also emits the shorter
/// `pdf:image width 120pt height 80pt (figure.png)`. In that form dvipdfmx
/// obtains the viewport from the image itself, so `bbox` is optional here too.
///
/// PDF pages use `pdf:epdf` but carry the same placement fields. Optional
/// page/pagebox/decode clauses may sit between them, so locate named fields
/// instead of depending on one fixed word count.
fn special_dvipdfmx_image(text: &str) -> Option<ImageSpecial> {
    let rest = text
        .trim()
        .strip_prefix("pdf:image ")
        .or_else(|| text.trim().strip_prefix("pdf:epdf "))?;
    let mut open = None;
    let mut escaped_prefix = false;
    for (index, ch) in rest.char_indices() {
        if escaped_prefix {
            escaped_prefix = false;
        } else if ch == '\\' {
            escaped_prefix = true;
        } else if ch == '(' {
            open = Some(index);
            break;
        }
    }
    let open = open?;
    let tail = &rest[open + 1..];
    let mut escaped = false;
    let mut depth = 1usize;
    let mut name = String::new();
    let mut closed = false;
    for ch in tail.chars() {
        if escaped {
            name.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '(' {
            depth += 1;
            name.push(ch);
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                closed = true;
                break;
            }
            name.push(ch);
        } else {
            name.push(ch);
        }
    }
    if !closed || name.is_empty() {
        return None;
    }
    let words: Vec<&str> = rest[..open].split_whitespace().collect();
    let at = |field: &str| words.iter().position(|word| *word == field);
    let bbox = if let Some(bbox) = at("bbox") {
        Some([
            decimal(words.get(bbox + 1)?)?,
            decimal(words.get(bbox + 2)?)?,
            decimal(words.get(bbox + 3)?)?,
            decimal(words.get(bbox + 4)?)?,
        ])
    } else {
        None
    };
    let width = at("width")
        .and_then(|i| words.get(i + 1))
        .and_then(|s| pdf_dimension(s));
    let height = at("height")
        .and_then(|i| words.get(i + 1))
        .and_then(|s| pdf_dimension(s));
    let page = at("page")
        .and_then(|i| words.get(i + 1))
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|page| *page > 0)
        .unwrap_or(1);
    let clip = at("clip").is_some_and(|i| {
        words
            .get(i + 1)
            .and_then(|value| value.parse::<i32>().ok())
            .map_or(true, |value| value != 0)
    });
    Some(ImageSpecial {
        name,
        page,
        bbox,
        width,
        height,
        clip,
    })
}

fn special_eps(text: &str) -> Option<EpsSpecial> {
    let rest = text.trim().strip_prefix("PSfile=")?;
    let (name, fields) = if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        (quoted[..end].to_string(), &quoted[end + 1..])
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (rest[..end].to_string(), &rest[end..])
    };
    let mut values = std::collections::BTreeMap::new();
    let mut clip = false;
    for field in fields.split_whitespace() {
        if field == "clip" {
            clip = true;
            continue;
        }
        if let Some((key, value)) = field.split_once('=') {
            if let Some(value) = decimal(value) {
                values.insert(key, value);
            }
        }
    }
    Some(EpsSpecial {
        name,
        llx: *values.get("llx").unwrap_or(&0.0),
        lly: *values.get("lly").unwrap_or(&0.0),
        urx: *values.get("urx")?,
        ury: *values.get("ury")?,
        // dvips records requested dimensions in tenths of a bp.
        width: values.get("rwi").map(|n| n.abs() / 10.0),
        height: values.get("rhi").map(|n| n.abs() / 10.0),
        clip,
    })
}

fn pop_numbers(stack: &mut Vec<f64>, n: usize) -> Option<Vec<f64>> {
    if stack.len() < n {
        stack.clear();
        return None;
    }
    Some(stack.split_off(stack.len() - n))
}

/// Translate the drawing subset common to EPS figures into an uncompressed
/// PDF form stream.  The path and color operators are deliberately the same in
/// PostScript and PDF; affine transforms are written as `cm` matrices.
fn eps_content(bytes: &[u8]) -> (Vec<u8>, bool) {
    let source = String::from_utf8_lossy(bytes);
    let mut stack = Vec::<f64>::new();
    let mut out = String::from("q\n");
    let mut supported = true;
    for line in source.lines() {
        let code = line.split('%').next().unwrap_or("");
        for word in code.split_whitespace() {
            if let Some(n) = decimal(word) {
                stack.push(n);
                continue;
            }
            let op = match word {
                "newpath" => Some("n\n".to_string()),
                "closepath" => Some("h\n".to_string()),
                "fill" | "eofill" => Some("f\n".to_string()),
                "stroke" => Some("S\n".to_string()),
                "clip" => Some("W n\n".to_string()),
                "eoclip" => Some("W* n\n".to_string()),
                "gsave" => Some("q\n".to_string()),
                "grestore" => Some("Q\n".to_string()),
                "moveto" => pop_numbers(&mut stack, 2).map(|v| format!("{} {} m\n", v[0], v[1])),
                "lineto" => pop_numbers(&mut stack, 2).map(|v| format!("{} {} l\n", v[0], v[1])),
                "curveto" => pop_numbers(&mut stack, 6)
                    .map(|v| format!("{} {} {} {} {} {} c\n", v[0], v[1], v[2], v[3], v[4], v[5])),
                "setlinewidth" => pop_numbers(&mut stack, 1).map(|v| format!("{} w\n", v[0])),
                "setgray" => pop_numbers(&mut stack, 1).map(|v| format!("{} g {} G\n", v[0], v[0])),
                "setrgbcolor" => pop_numbers(&mut stack, 3).map(|v| {
                    format!(
                        "{} {} {} rg {} {} {} RG\n",
                        v[0], v[1], v[2], v[0], v[1], v[2]
                    )
                }),
                "setcmykcolor" => pop_numbers(&mut stack, 4).map(|v| {
                    format!(
                        "{} {} {} {} k {} {} {} {} K\n",
                        v[0], v[1], v[2], v[3], v[0], v[1], v[2], v[3]
                    )
                }),
                "translate" => {
                    pop_numbers(&mut stack, 2).map(|v| format!("1 0 0 1 {} {} cm\n", v[0], v[1]))
                }
                "scale" => {
                    pop_numbers(&mut stack, 2).map(|v| format!("{} 0 0 {} 0 0 cm\n", v[0], v[1]))
                }
                "rotate" => pop_numbers(&mut stack, 1).map(|v| {
                    let a = v[0].to_radians();
                    format!(
                        "{:.8} {:.8} {:.8} {:.8} 0 0 cm\n",
                        a.cos(),
                        a.sin(),
                        -a.sin(),
                        a.cos()
                    )
                }),
                "rectfill" => pop_numbers(&mut stack, 4)
                    .map(|v| format!("{} {} {} {} re f\n", v[0], v[1], v[2], v[3])),
                "rectstroke" => pop_numbers(&mut stack, 4)
                    .map(|v| format!("{} {} {} {} re S\n", v[0], v[1], v[2], v[3])),
                "showpage" => None,
                _ => {
                    // Unknown PostScript can contain procedure bodies whose
                    // operands must not leak into a later supported operator.
                    stack.clear();
                    supported = false;
                    None
                }
            };
            if let Some(op) = op {
                out.push_str(&op);
            }
        }
    }
    out.push_str("Q\n");
    (out.into_bytes(), supported)
}
