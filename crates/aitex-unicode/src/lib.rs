//! Host-side Unicode text services for aiTeX.
//!
//! This is intentionally above `aitex-engine`: ICU4X, `unicode-bidi`,
//! `fontdb`, and HarfRust never enter the serializable `no_std` interpreter.
//! The service returns aiTeX-owned native nodes and retains the font bytes
//! needed later by the positioned PDF resource writer.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aitex_engine::fast::NativeTextRequest;
use aitex_engine::node::{Glue, NativeCluster, NativeDirection, NativeGlyph, NativeRun, Node};
use aitex_engine::scaled::Scaled;
use aitex_fonts::{
    FaceIndex, FontError, FontTag, MissingGlyphPolicy, NormalizedVariation, ShapeFeature,
    ShapeRequest, TextDirection, VariationSetting,
};
use fontdb::{Database, Family, ID, Query, Source};
use icu_properties::props::Script;
use icu_properties::script::ScriptWithExtensions;
use icu_segmenter::LineSegmenter;
use unicode_bidi::BidiInfo;

/// Bytes and collection index for one document-local native font ID.
#[derive(Clone)]
pub struct ResolvedFont {
    pub bytes: Arc<[u8]>,
    pub face_index: u32,
    pub postscript_name: String,
    /// User-space coordinates for this concrete variable-font instance.
    pub variations: Arc<[VariationSetting]>,
    /// Present for ambient fonts, so completed-output cache manifests can
    /// invalidate when the selected program changes on disk.
    pub source_path: Option<PathBuf>,
}

/// Stateful per-document Unicode service.
///
/// `fontdb` is only an index. Candidate ordering, coverage checks, and
/// fallback policy are aiTeX's and remain deterministic for a fixed database.
pub struct UnicodeHost {
    database: Database,
    registered: HashMap<(ID, Vec<NormalizedVariation>), u32>,
    fonts: Vec<ResolvedFont>,
    next_bidi_span: u32,
    discover_system_fonts: bool,
    system_fonts_loaded: bool,
}

impl UnicodeHost {
    /// Make the platform's normal font locations available for this job.
    /// Discovery is deferred until the first run that project fonts cannot
    /// satisfy, avoiding a system-wide scan for reproducible bundled-font jobs.
    pub fn system() -> Self {
        Self::system_with_dirs(std::iter::empty::<&Path>())
    }

    /// Add explicit directories before the platform database. This is useful
    /// for applications with bundled fonts and for deterministic test jobs;
    /// fallback ordering remains aiTeX-owned after indexing.
    pub fn system_with_dirs<P, I>(directories: I) -> Self
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = P>,
    {
        let mut database = Database::new();
        for directory in directories {
            for path in project_font_files(directory.as_ref()) {
                let _ = database.load_font_file(path);
            }
        }
        let mut host = Self::from_database(database);
        host.discover_system_fonts = true;
        host.system_fonts_loaded = false;
        host
    }

    /// Construct from a caller-prepared database (used by locked fixtures and
    /// embedders that do not want ambient system-font discovery).
    pub fn from_database(database: Database) -> Self {
        Self {
            database,
            registered: HashMap::new(),
            fonts: Vec::new(),
            next_bidi_span: 0,
            discover_system_fonts: false,
            system_fonts_loaded: true,
        }
    }

    pub fn font(&self, id: u32) -> Option<&ResolvedFont> {
        self.fonts.get(id as usize)
    }

    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    pub fn dependency_paths(&self) -> impl Iterator<Item = &Path> {
        self.fonts
            .iter()
            .filter_map(|font| font.source_path.as_deref())
    }

    /// Itemize and shape an inline Unicode span into ordinary TeX layout
    /// nodes. ICU4X supplies UAX #14 opportunities, `unicode-bidi` supplies
    /// resolved embedding levels, and HarfRust shapes each script/font run.
    /// Runs stay in logical order until the TeX line breaker applies UAX #9's
    /// line-local visual reordering to the actual chosen line boundaries.
    pub fn shape(&mut self, request: &NativeTextRequest) -> Result<Vec<Node>, String> {
        if request.text.contains(['\n', '\r']) {
            return Err(String::from(
                "mandatory line separators are not supported inside \\aitextext; use TeX paragraph breaks",
            ));
        }
        if request.text.is_empty() {
            return Ok(Vec::new());
        }

        let features = parse_features(request.features.as_deref())?;
        let variations = parse_variations(request.variations.as_deref())?;
        let direction = parse_direction(request.direction.as_deref())?;
        if !matches!(direction, RequestedDirection::Auto) {
            let (font_direction, bidi_level) = match direction {
                RequestedDirection::LeftToRight => (TextDirection::LeftToRight, 0),
                RequestedDirection::RightToLeft => (TextDirection::RightToLeft, 1),
                RequestedDirection::TopToBottom => (TextDirection::TopToBottom, 0),
                RequestedDirection::BottomToTop => (TextDirection::BottomToTop, 0),
                RequestedDirection::Auto => unreachable!(),
            };
            let bidi_span = self.next_bidi_span;
            self.next_bidi_span = self.next_bidi_span.wrapping_add(1);
            return self.shape_directional_run(
                &request.text,
                request,
                font_direction,
                bidi_level,
                bidi_span,
                &features,
                &variations,
            );
        }

        let bidi = BidiInfo::new(&request.text, None);
        let Some(paragraph) = bidi.paragraphs.first() else {
            return Ok(Vec::new());
        };
        let bidi_span = self.next_bidi_span;
        self.next_bidi_span = self.next_bidi_span.wrapping_add(1);
        let mut output = Vec::new();
        for (range, level) in logical_bidi_runs(&request.text, &bidi, paragraph.range.clone()) {
            let text = &request.text[range.clone()];
            let direction = if level % 2 == 1 {
                TextDirection::RightToLeft
            } else {
                TextDirection::LeftToRight
            };
            output.extend(self.shape_directional_run(
                text,
                request,
                direction,
                level,
                bidi_span,
                &features,
                &variations,
            )?);
        }
        Ok(output)
    }

    fn shape_directional_run(
        &mut self,
        text: &str,
        request: &NativeTextRequest,
        direction: TextDirection,
        bidi_level: u8,
        bidi_span: u32,
        features: &[ShapeFeature],
        variations: &[VariationSetting],
    ) -> Result<Vec<Node>, String> {
        let scripts = itemize_scripts(text);
        let mut output = Vec::new();
        for (range, script) in scripts {
            let part = &text[range];
            output.extend(self.shape_script_run(
                part, request, direction, script, bidi_level, bidi_span, features, variations,
            )?);
        }
        Ok(output)
    }

    fn shape_script_run(
        &mut self,
        text: &str,
        request: &NativeTextRequest,
        direction: TextDirection,
        script: Script,
        bidi_level: u8,
        bidi_span: u32,
        features: &[ShapeFeature],
        variations: &[VariationSetting],
    ) -> Result<Vec<Node>, String> {
        if matches!(
            direction,
            TextDirection::TopToBottom | TextDirection::BottomToTop
        ) {
            // UAX #14 boundaries describe horizontal line opportunities. A
            // vertical run is one positioned column; splitting at every CJK
            // boundary would turn its characters back into adjacent inline
            // boxes before the PDF writer ever sees their y advances.
            return self
                .shape_one(
                    text, request, direction, script, bidi_level, bidi_span, features, variations,
                )
                .map(|node| vec![node]);
        }
        let segmenter = LineSegmenter::new_auto(Default::default());
        let breaks = segmenter.segment_str(text).collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut start = 0usize;
        for end in breaks.into_iter().filter(|end| *end > 0) {
            if end <= start || end > text.len() {
                continue;
            }
            let chunk = &text[start..end];
            let content_end = chunk
                .char_indices()
                .rev()
                .find(|(_, character)| !character.is_whitespace())
                .map_or(0, |(at, character)| at + character.len_utf8());
            let content = &chunk[..content_end];
            if !content.is_empty() {
                output.push(self.shape_one(
                    content, request, direction, script, bidi_level, bidi_span, features,
                    variations,
                )?);
            }
            let whitespace = &chunk[content_end..];
            if !whitespace.is_empty() {
                let count = whitespace.chars().count().max(1) as i32;
                let em = request.size.0;
                output.push(Node::Glue(Glue {
                    width: Scaled(em.saturating_mul(count) / 3),
                    stretch: Scaled(em.saturating_mul(count) / 6),
                    shrink: Scaled(em.saturating_mul(count) / 9),
                    ..Glue::default()
                }));
            } else if end < text.len() {
                // UAX #14 supplied a non-space break (CJK and similar text).
                // Knuth--Plass already understands a zero penalty exactly.
                output.push(Node::Penalty(0));
            }
            start = end;
        }
        if start < text.len() {
            output.push(self.shape_one(
                &text[start..],
                request,
                direction,
                script,
                bidi_level,
                bidi_span,
                features,
                variations,
            )?);
        }
        Ok(output)
    }

    fn shape_one(
        &mut self,
        text: &str,
        request: &NativeTextRequest,
        direction: TextDirection,
        script: Script,
        bidi_level: u8,
        bidi_span: u32,
        features: &[ShapeFeature],
        variations: &[VariationSetting],
    ) -> Result<Node, String> {
        let tag = script_tag(script);
        let candidates = self.candidates(
            request.family.as_deref(),
            script,
            self.discover_system_fonts && !self.system_fonts_loaded,
        )?;
        let mut last_error = None;
        for candidate in candidates {
            let Some((bytes, face_index, postscript_name, source_path)) = self.font_data(candidate)
            else {
                continue;
            };
            let mut shape_request = ShapeRequest::new(text, FaceIndex(face_index), direction, tag);
            let language = request.language.as_deref().map(normalize_language);
            shape_request.language = language.as_deref();
            shape_request.features = features;
            shape_request.variations = variations;
            shape_request.missing_glyphs = MissingGlyphPolicy::Reject;
            match aitex_fonts::shape(&bytes, &shape_request) {
                Ok(shaped) => {
                    let glyphs = shaped.source_glyphs();
                    let metrics = aitex_fonts::pdf_face_metrics(
                        &bytes,
                        FaceIndex(face_index),
                        variations,
                        &glyphs,
                    )
                    .map_err(|error| error.to_string())?;
                    let font = self.register(
                        candidate,
                        bytes,
                        face_index,
                        postscript_name,
                        source_path,
                        variations,
                        &shaped.instance.variations,
                    );
                    let page_run = aitex_fonts::positioned_run(
                        &shaped,
                        aitex_page::FontInstanceId(font),
                        aitex_page::Sp(i64::from(request.size.0)),
                        aitex_page::Sp(0),
                        aitex_page::Sp(0),
                    );
                    let width = if matches!(
                        direction,
                        TextDirection::TopToBottom | TextDirection::BottomToTop
                    ) {
                        i64::from(request.size.0)
                    } else {
                        page_run
                            .glyphs
                            .iter()
                            .fold(0i64, |sum, glyph| sum.saturating_add(glyph.x_advance.0))
                    };
                    let scale = |value: i32| -> Scaled {
                        let scaled =
                            i64::from(value).saturating_mul(i64::from(request.size.0)) / 1000;
                        Scaled(scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
                    };
                    let glyphs = page_run
                        .glyphs
                        .into_iter()
                        .map(|glyph| NativeGlyph {
                            source_gid: glyph.source_gid.0,
                            cluster: glyph.cluster.0,
                            x_advance: Scaled(glyph.x_advance.0 as i32),
                            y_advance: Scaled(glyph.y_advance.0 as i32),
                            x_offset: Scaled(glyph.x_offset.0 as i32),
                            y_offset: Scaled(glyph.y_offset.0 as i32),
                            is_notdef: glyph.is_notdef,
                        })
                        .collect();
                    let clusters = page_run
                        .clusters
                        .into_iter()
                        .map(|cluster| NativeCluster {
                            byte_start: cluster.byte_range.start,
                            byte_end: cluster.byte_range.end,
                            text: cluster.text,
                        })
                        .collect();
                    let direction = match direction {
                        TextDirection::LeftToRight => NativeDirection::LeftToRight,
                        TextDirection::RightToLeft => NativeDirection::RightToLeft,
                        TextDirection::TopToBottom => NativeDirection::TopToBottom,
                        TextDirection::BottomToTop => NativeDirection::BottomToTop,
                    };
                    let native = NativeRun::new(
                        font,
                        request.size,
                        direction,
                        Scaled(width.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32),
                        scale(metrics.ascent.max(0)),
                        scale(metrics.descent.saturating_neg().max(0)),
                        glyphs,
                        clusters,
                    )
                    .with_language(request.language.clone())
                    .with_bidi(bidi_level, bidi_span);
                    return Ok(Node::NativeRun(Box::new(native)));
                }
                Err(FontError::MissingGlyph { .. }) => continue,
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        if self.discover_system_fonts && !self.system_fonts_loaded {
            self.database.load_system_fonts();
            self.system_fonts_loaded = true;
            return self.shape_one(
                text, request, direction, script, bidi_level, bidi_span, features, variations,
            );
        }
        Err(last_error.unwrap_or_else(|| {
            format!(
                "no embeddable system font covers {text:?} (script {})",
                tag_string(tag)
            )
        }))
    }

    fn candidates(
        &self,
        requested: Option<&str>,
        script: Script,
        defer_missing_requested: bool,
    ) -> Result<Vec<ID>, String> {
        let mut output = Vec::new();
        let mut seen = HashSet::new();
        let mut add_family = |name: &str| -> bool {
            let families = [Family::Name(name)];
            if let Some(id) = self.database.query(&Query {
                families: &families,
                ..Query::default()
            }) && seen.insert(id)
            {
                output.push(id);
                return true;
            }
            false
        };
        let mut requested_match = false;
        if let Some(requested) = requested {
            for family in requested
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                requested_match |= add_family(family);
            }
            if !requested.trim().is_empty() && !requested_match && defer_missing_requested {
                return Ok(Vec::new());
            }
            if !requested.trim().is_empty() && !requested_match {
                return Err(format!(
                    "requested font family was not found: {requested:?}"
                ));
            }
        }
        for family in fallback_families(script) {
            let _ = add_family(family);
        }
        let generic = [Family::Serif];
        if let Some(id) = self.database.query(&Query {
            families: &generic,
            ..Query::default()
        }) && seen.insert(id)
        {
            output.push(id);
        }
        let mut remaining = self.database.faces().collect::<Vec<_>>();
        remaining.sort_by(|left, right| {
            left.post_script_name
                .cmp(&right.post_script_name)
                .then_with(|| left.index.cmp(&right.index))
        });
        for face in remaining {
            if seen.insert(face.id) {
                output.push(face.id);
            }
        }
        Ok(output)
    }

    fn font_data(&self, id: ID) -> Option<(Arc<[u8]>, u32, String, Option<PathBuf>)> {
        let face = self.database.face(id)?;
        let name = face.post_script_name.clone();
        let source_path = match &face.source {
            Source::File(path) | Source::SharedFile(path, _) => Some(path.clone()),
            Source::Binary(_) => None,
        };
        self.database.with_face_data(id, |bytes, index| {
            (Arc::<[u8]>::from(bytes), index, name, source_path)
        })
    }

    fn register(
        &mut self,
        id: ID,
        bytes: Arc<[u8]>,
        face_index: u32,
        postscript_name: String,
        source_path: Option<PathBuf>,
        variations: &[VariationSetting],
        normalized_variations: &[NormalizedVariation],
    ) -> u32 {
        let key = (id, normalized_variations.to_vec());
        if let Some(existing) = self.registered.get(&key) {
            return *existing;
        }
        let font = self.fonts.len() as u32;
        self.fonts.push(ResolvedFont {
            bytes,
            face_index,
            postscript_name,
            variations: variations.to_vec().into(),
            source_path,
        });
        self.registered.insert(key, font);
        font
    }
}

fn project_font_files(directory: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, seen: &mut HashSet<PathBuf>, output: &mut Vec<PathBuf>) {
        let Ok(canonical) = directory.canonicalize() else {
            return;
        };
        if !seen.insert(canonical.clone()) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(canonical) else {
            return;
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                visit(&path, seen, output);
            } else if path.is_file()
                && matches!(
                    path.extension()
                        .and_then(|extension| extension.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref(),
                    Some("ttf" | "ttc" | "otf" | "otc")
                )
            {
                output.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(directory, &mut HashSet::new(), &mut files);
    files
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestedDirection {
    Auto,
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

fn setting_tag(value: &str, kind: &str) -> Result<FontTag, String> {
    let bytes = value.as_bytes();
    if bytes.len() != 4 || !bytes.iter().all(u8::is_ascii_alphanumeric) {
        return Err(format!(
            "invalid OpenType {kind} tag {value:?}; expected four ASCII letters or digits"
        ));
    }
    Ok(FontTag::new([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn parse_features(value: Option<&str>) -> Result<Vec<ShapeFeature>, String> {
    let mut features = Vec::new();
    for token in value
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        if let Some((name, choice)) = token.split_once('=')
            && name.len() != 4
        {
            let choice = choice.trim();
            let settings: &[(&str, u32)] = match (name.trim(), choice) {
                ("Ligatures", "Common") => &[("liga", 1), ("clig", 1)],
                ("Ligatures", "NoCommon") => &[("liga", 0), ("clig", 0)],
                ("Kerning", "On") => &[("kern", 1)],
                ("Kerning", "Off") => &[("kern", 0)],
                ("Numbers", "OldStyle") => &[("onum", 1)],
                ("Numbers", "Lining") => &[("lnum", 1)],
                ("Numbers", "Proportional") => &[("pnum", 1)],
                ("Numbers", "Monospaced") => &[("tnum", 1)],
                ("Letters", "SmallCaps") => &[("smcp", 1)],
                _ => {
                    return Err(format!(
                        "unsupported fontspec feature {token:?}; use OpenType tags such as +liga or ss01=1"
                    ));
                }
            };
            for (tag, value) in settings {
                features.push(ShapeFeature::global(setting_tag(tag, "feature")?, *value));
            }
            continue;
        }
        let (tag, value) = if let Some(tag) = token.strip_prefix('+') {
            (tag, 1)
        } else if let Some(tag) = token.strip_prefix('-') {
            (tag, 0)
        } else if let Some((tag, value)) = token.split_once('=') {
            let value = value.parse::<u32>().map_err(|_| {
                format!("invalid OpenType feature value in {token:?}; expected an unsigned integer")
            })?;
            (tag.trim(), value)
        } else {
            (token, 1)
        };
        features.push(ShapeFeature::global(
            setting_tag(tag.trim(), "feature")?,
            value,
        ));
    }
    Ok(features)
}

fn parse_variations(value: Option<&str>) -> Result<Vec<VariationSetting>, String> {
    let mut variations = Vec::new();
    for token in value
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let Some((tag, value)) = token.split_once('=') else {
            return Err(format!(
                "invalid variable-font coordinate {token:?}; expected tag=value"
            ));
        };
        let tag = setting_tag(tag.trim(), "variation-axis")?;
        let value = value
            .trim()
            .parse::<f32>()
            .map_err(|_| format!("invalid variable-font coordinate value in {token:?}"))?;
        if !value.is_finite() {
            return Err(format!(
                "invalid variable-font coordinate value in {token:?}; expected a finite number"
            ));
        }
        variations.push(VariationSetting { tag, value });
    }
    Ok(variations)
}

fn parse_direction(value: Option<&str>) -> Result<RequestedDirection, String> {
    match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(RequestedDirection::Auto),
        "ltr" | "left-to-right" => Ok(RequestedDirection::LeftToRight),
        "rtl" | "right-to-left" => Ok(RequestedDirection::RightToLeft),
        "ttb" | "top-to-bottom" => Ok(RequestedDirection::TopToBottom),
        "btt" | "bottom-to-top" => Ok(RequestedDirection::BottomToTop),
        other => Err(format!(
            "unsupported text direction {other:?}; expected auto, ltr, rtl, ttb, or btt"
        )),
    }
}

/// Validate the public selector syntax before a document starts. Keeping this
/// parser beside shaping ensures CLI/config values and TeX primitives accept
/// exactly the same feature, variation, and direction language.
pub fn validate_text_settings(
    features: Option<&str>,
    variations: Option<&str>,
    direction: Option<&str>,
) -> Result<(), String> {
    parse_features(features)?;
    parse_variations(variations)?;
    parse_direction(direction)?;
    Ok(())
}

fn logical_bidi_runs(
    text: &str,
    bidi: &BidiInfo<'_>,
    range: std::ops::Range<usize>,
) -> Vec<(std::ops::Range<usize>, u8)> {
    if range.start >= range.end {
        return Vec::new();
    }
    let mut runs = Vec::new();
    let mut start = range.start;
    let mut level = bidi.levels[start].number();
    for (relative, _) in text[range.clone()].char_indices().skip(1) {
        let at = range.start + relative;
        let next_level = bidi.levels[at].number();
        if next_level != level {
            runs.push((start..at, level));
            start = at;
            level = next_level;
        }
    }
    runs.push((start..range.end, level));
    runs
}

fn itemize_scripts(text: &str) -> Vec<(std::ops::Range<usize>, Script)> {
    let properties = ScriptWithExtensions::new();
    let chars = text.char_indices().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }
    let raw = chars
        .iter()
        .map(|(_, character)| properties.get_script_val(*character))
        .collect::<Vec<_>>();
    let mut resolved = raw.clone();
    let mut previous = None;
    for script in &mut resolved {
        if matches!(
            *script,
            Script::Common | Script::Inherited | Script::Unknown
        ) {
            if let Some(value) = previous {
                *script = value;
            }
        } else {
            previous = Some(*script);
        }
    }
    let mut next = None;
    for script in resolved.iter_mut().rev() {
        if matches!(
            *script,
            Script::Common | Script::Inherited | Script::Unknown
        ) {
            *script = next.unwrap_or(Script::Latin);
        } else {
            next = Some(*script);
        }
    }
    let mut runs = Vec::new();
    let mut start = 0usize;
    let mut script = resolved[0];
    for index in 1..chars.len() {
        if resolved[index] != script {
            runs.push((start..chars[index].0, script));
            start = chars[index].0;
            script = resolved[index];
        }
    }
    runs.push((start..text.len(), script));
    runs
}

fn script_tag(script: Script) -> FontTag {
    FontTag::new(match script {
        Script::Arabic => *b"Arab",
        Script::Hebrew => *b"Hebr",
        Script::Devanagari => *b"Deva",
        Script::Bengali => *b"Beng",
        Script::Gurmukhi => *b"Guru",
        Script::Gujarati => *b"Gujr",
        Script::Oriya => *b"Orya",
        Script::Tamil => *b"Taml",
        Script::Telugu => *b"Telu",
        Script::Kannada => *b"Knda",
        Script::Malayalam => *b"Mlym",
        Script::Sinhala => *b"Sinh",
        Script::Thai => *b"Thai",
        Script::Lao => *b"Laoo",
        Script::Han => *b"Hani",
        Script::Hiragana => *b"Hira",
        Script::Katakana => *b"Kana",
        Script::Hangul => *b"Hang",
        Script::Cyrillic => *b"Cyrl",
        Script::Greek => *b"Grek",
        _ => *b"Latn",
    })
}

fn fallback_families(script: Script) -> &'static [&'static str] {
    match script {
        Script::Arabic => &["Amiri", "Noto Naskh Arabic", "Noto Sans Arabic"],
        Script::Hebrew => &["Noto Serif Hebrew", "Noto Sans Hebrew"],
        Script::Devanagari => &["Noto Serif Devanagari", "Noto Sans Devanagari"],
        Script::Bengali => &["Noto Serif Bengali", "Noto Sans Bengali"],
        Script::Gurmukhi => &["Noto Serif Gurmukhi", "Noto Sans Gurmukhi"],
        Script::Gujarati => &["Noto Serif Gujarati", "Noto Sans Gujarati"],
        Script::Oriya => &["Noto Serif Oriya", "Noto Sans Oriya"],
        Script::Tamil => &["Noto Serif Tamil", "Noto Sans Tamil"],
        Script::Telugu => &["Noto Serif Telugu", "Noto Sans Telugu"],
        Script::Kannada => &["Noto Serif Kannada", "Noto Sans Kannada"],
        Script::Malayalam => &["Noto Serif Malayalam", "Noto Sans Malayalam"],
        Script::Sinhala => &["Noto Serif Sinhala", "Noto Sans Sinhala"],
        Script::Thai => &["Noto Serif Thai", "Noto Sans Thai", "Thonburi"],
        Script::Lao => &["Noto Serif Lao", "Noto Sans Lao"],
        Script::Han | Script::Hiragana | Script::Katakana => &[
            "Noto Serif CJK SC",
            "Noto Sans CJK SC",
            "Hiragino Mincho ProN",
            "PingFang SC",
        ],
        Script::Hangul => &[
            "Noto Serif CJK KR",
            "Noto Sans CJK KR",
            "Apple SD Gothic Neo",
        ],
        _ => &["Noto Serif", "Noto Sans", "Times New Roman"],
    }
}

fn tag_string(tag: FontTag) -> String {
    String::from_utf8_lossy(&tag.0).into_owned()
}

fn normalize_language(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "english" | "american" | "british" => String::from("en"),
        "arabic" => String::from("ar"),
        "hebrew" => String::from("he"),
        "hindi" => String::from("hi"),
        "thai" => String::from("th"),
        "lao" => String::from("lo"),
        "chinese" | "simplifiedchinese" => String::from("zh-Hans"),
        "traditionalchinese" => String::from("zh-Hant"),
        "japanese" => String::from("ja"),
        "korean" => String::from("ko"),
        _ => language.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_database() -> Database {
        let mut database = Database::new();
        for relative in [
            "NotoSans-Latin-Test.ttf",
            "Amiri-Arabic-Test.ttf",
            "NotoSansHebrew-Test.ttf",
            "NotoSansDevanagari-Test.ttf",
            "NotoSansThai-Test.ttf",
            "NotoSansLao-Test.ttf",
            "NotoSansCJKsc-Test.otf",
            "Junicode-Variable-Test.ttf",
        ] {
            let path = format!(
                "{}/../aitex-fonts/tests/fonts/{relative}",
                env!("CARGO_MANIFEST_DIR")
            );
            database.load_font_data(
                std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}")),
            );
        }
        database
    }

    #[test]
    fn complete_project_font_coverage_never_indexes_ambient_fonts() {
        let directory = format!("{}/../aitex-fonts/tests/fonts", env!("CARGO_MANIFEST_DIR"));
        let mut host = UnicodeHost::system_with_dirs([directory]);
        assert!(!host.system_fonts_loaded);
        let request = NativeTextRequest {
            text: String::from("office affinity AVATAR café"),
            family: Some(String::from("Noto Sans")),
            language: Some(String::from("en")),
            features: Some(String::from("+liga,+kern")),
            variations: None,
            direction: None,
            size: Scaled::from_points(10),
        };
        let nodes = host.shape(&request).expect("shape from project font");
        assert!(!nodes.is_empty());
        assert!(!host.system_fonts_loaded);
    }

    #[test]
    fn itemizes_scripts_and_preserves_common_punctuation() {
        let runs = itemize_scripts("Latin, العربية");
        assert_eq!(runs.len(), 2);
        assert_eq!(script_tag(runs[0].1).0, *b"Latn");
        assert_eq!(script_tag(runs[1].1).0, *b"Arab");
        assert_eq!(&"Latin, العربية"[runs[0].0.clone()], "Latin, ");
    }

    #[test]
    fn shapes_ltr_rtl_and_indic_with_locked_fonts() {
        let mut host = UnicodeHost::from_database(fixture_database());
        let request = NativeTextRequest {
            text: String::from("office السَّلَامُ नमस्ते"),
            family: None,
            language: None,
            features: None,
            variations: None,
            direction: None,
            size: Scaled::from_points(10),
        };
        let nodes = host.shape(&request).expect("shape multilingual text");
        let runs = nodes
            .iter()
            .filter_map(|node| match node {
                Node::NativeRun(run) => Some(run),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(runs.len() >= 3, "{nodes:?}");
        assert!(
            runs.iter()
                .any(|run| run.direction == NativeDirection::RightToLeft)
        );
        assert!(runs.iter().all(|run| run.width.0 > 0));
        assert!(host.font_count() >= 3);
    }

    #[test]
    fn mixed_bidi_runs_stay_logical_until_line_breaking() {
        let mut host = UnicodeHost::from_database(fixture_database());
        let request = NativeTextRequest {
            text: String::from("שלום office עולם"),
            family: None,
            language: Some(String::from("he")),
            features: None,
            variations: None,
            direction: None,
            size: Scaled::from_points(10),
        };
        let nodes = host.shape(&request).expect("shape mixed bidi text");
        let runs = nodes
            .iter()
            .filter_map(|node| match node {
                Node::NativeRun(run) => Some(run.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            runs.iter()
                .flat_map(|run| &run.clusters)
                .map(|cluster| cluster.text.as_str())
                .collect::<String>(),
            "שלוםofficeעולם"
        );
        assert_eq!(
            runs.iter().map(|run| run.bidi_level).collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
        assert!(runs.iter().all(|run| run.bidi_span == runs[0].bidi_span));
    }

    #[test]
    fn uax14_breaks_cjk_without_inventing_hyphenation() {
        let scripts = itemize_scripts("日本語");
        assert_eq!(scripts.len(), 1);
        let breaks = LineSegmenter::new_auto(Default::default())
            .segment_str("日本語")
            .collect::<Vec<_>>();
        assert_eq!(breaks, vec![0, 3, 6, 9]);
    }

    #[test]
    fn shapes_locked_cjk_and_southeast_asian_runs() {
        let mut host = UnicodeHost::from_database(fixture_database());
        for (text, language) in [
            ("日本語中文测试漢字", "zh"),
            ("ภาษาไทย ยินดีต้อนรับ", "th"),
            ("ສະບາຍດີ ພາສາລາວ", "lo"),
        ] {
            let nodes = host
                .shape(&NativeTextRequest {
                    text: text.to_string(),
                    family: None,
                    language: Some(language.to_string()),
                    features: None,
                    variations: None,
                    direction: None,
                    size: Scaled::from_points(10),
                })
                .expect("shape locked multilingual fixture");
            assert!(nodes.iter().any(|node| matches!(node, Node::NativeRun(_))));
        }
    }

    #[test]
    fn validates_features_variations_and_directions_precisely() {
        validate_text_settings(
            Some("+liga,-kern,ss01=2,Ligatures=Common,Numbers=OldStyle"),
            Some("wght=650,wdth=90"),
            Some("ttb"),
        )
        .expect("supported selectors");
        assert!(
            validate_text_settings(Some("Ligatures=TeX"), None, None)
                .unwrap_err()
                .contains("unsupported fontspec feature")
        );
        assert!(
            validate_text_settings(None, Some("weight=heavy"), None)
                .unwrap_err()
                .contains("expected four ASCII")
        );
        assert!(
            validate_text_settings(None, None, Some("sideways"))
                .unwrap_err()
                .contains("unsupported text direction")
        );
    }

    #[test]
    fn requested_fonts_variations_and_vertical_runs_are_explicit() {
        let mut host = UnicodeHost::from_database(fixture_database());
        let variable = host
            .shape(&NativeTextRequest {
                text: String::from("office"),
                family: Some(String::from("Junicode VF")),
                language: Some(String::from("en")),
                features: Some(String::from("+liga")),
                variations: Some(String::from("wght=650")),
                direction: Some(String::from("ltr")),
                size: Scaled::from_points(10),
            })
            .expect("shape variable-font instance");
        assert!(
            variable
                .iter()
                .any(|node| matches!(node, Node::NativeRun(_)))
        );
        assert_eq!(host.font(0).expect("registered font").variations.len(), 1);

        let vertical = host
            .shape(&NativeTextRequest {
                text: String::from("日本語"),
                family: Some(String::from("Noto Sans CJK SC")),
                language: Some(String::from("ja")),
                features: None,
                variations: None,
                direction: Some(String::from("ttb")),
                size: Scaled::from_points(10),
            })
            .expect("shape vertical CJK");
        let runs = vertical
            .iter()
            .filter_map(|node| match node {
                Node::NativeRun(run) => Some(run.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(runs.len(), 1, "one vertical column must remain one run");
        assert_eq!(runs[0].direction, NativeDirection::TopToBottom);
        assert!(runs[0].glyphs.iter().all(|glyph| glyph.x_advance.0 == 0));
        assert!(runs[0].glyphs.iter().all(|glyph| glyph.y_advance.0 > 0));

        let error = host
            .shape(&NativeTextRequest {
                text: String::from("hello"),
                family: Some(String::from("Definitely Missing Family")),
                language: None,
                features: None,
                variations: None,
                direction: None,
                size: Scaled::from_points(10),
            })
            .unwrap_err();
        assert!(
            error.contains("requested font family was not found"),
            "{error}"
        );
    }
}
