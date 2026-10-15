use core::fmt;
use std::ops::Range;

/// A zero-based face in a standalone font or font collection.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FaceIndex(pub u32);

/// SHA-256 of the complete font-file bytes supplied by the host.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FontDigest(pub [u8; 32]);

impl FontDigest {
    /// Hash complete font bytes for a path-independent face identity.
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::Digest as _;
        Self(sha2::Sha256::digest(bytes).into())
    }

    /// Lowercase hexadecimal representation suitable for cache diagnostics.
    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        use fmt::Write as _;
        for byte in self.0 {
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

/// Classic-font choices that change the embedded outline program.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ClassicEmbeddingPolicy {
    Full,
    Subset,
}

/// Stable identity of one classic physical face before TeX encoding or size.
///
/// Bitwise transform values are intentional: two map entries may share an
/// outline only when the requested PostScript transformation is exactly the
/// same. Point size, TFM widths, encoding, and Unicode mapping belong to the
/// use layer and therefore do not appear here.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClassicFaceIdentity {
    pub digest: FontDigest,
    pub postscript_name: String,
    pub slant_bits: u64,
    pub extend_bits: u64,
    pub embedding: ClassicEmbeddingPolicy,
    pub preparation_version: u16,
}

impl ClassicFaceIdentity {
    pub fn type1(
        bytes: &[u8],
        postscript_name: impl Into<String>,
        slant: f64,
        extend: f64,
        embedding: ClassicEmbeddingPolicy,
    ) -> Self {
        Self {
            digest: FontDigest::of(bytes),
            postscript_name: postscript_name.into(),
            slant_bits: slant.to_bits(),
            extend_bits: extend.to_bits(),
            embedding,
            preparation_version: CLASSIC_TYPE1_PREPARATION_VERSION,
        }
    }
}

/// Increment when classic preparation or subsetting can change serialized
/// program bytes. It participates in face/cache identity.
pub const CLASSIC_TYPE1_PREPARATION_VERSION: u16 = 3;

impl fmt::Debug for FontDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("FontDigest")
            .field(&self.to_hex())
            .finish()
    }
}

/// Stable identity of one face, independent of its path or point size.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FaceIdentity {
    pub digest: FontDigest,
    pub index: FaceIndex,
}

/// A four-byte OpenType tag owned by aiTeX rather than a font-library API.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FontTag(pub [u8; 4]);

impl FontTag {
    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for FontTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match std::str::from_utf8(&self.0) {
            Ok(tag) => formatter.debug_tuple("FontTag").field(&tag).finish(),
            Err(_) => formatter.debug_tuple("FontTag").field(&self.0).finish(),
        }
    }
}

/// Outline technology presented by a parsed OpenType container.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FontFormat {
    TrueType,
    Cff,
    Cff2,
    Unsupported,
}

/// Color-glyph technology advertised by an OpenType face.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum ColorGlyphFormat {
    ColrV0,
    ColrV1,
    Svg,
    Sbix,
    Cbdt,
}

/// The kind of PDF rendering adapter required by a color-glyph technology.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ColorRenderMode {
    Vector,
    Svg,
    Bitmap,
}

impl ColorGlyphFormat {
    pub const fn render_mode(self) -> ColorRenderMode {
        match self {
            Self::ColrV0 | Self::ColrV1 => ColorRenderMode::Vector,
            Self::Svg => ColorRenderMode::Svg,
            Self::Sbix | Self::Cbdt => ColorRenderMode::Bitmap,
        }
    }
}

/// An sRGB color and alpha value resolved from one CPAL palette entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl RgbaColor {
    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

/// The solid brush attached to one COLRv0 layer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ColorGlyphBrush {
    /// Inherit the surrounding text color, with the given opacity.
    Foreground { alpha: u8 },
    /// Use a color resolved from the selected CPAL palette.
    Solid(RgbaColor),
}

/// One ordered outline and brush in a COLRv0 base glyph.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct ColorGlyphLayer {
    pub source_glyph: SourceGlyphId,
    pub brush: ColorGlyphBrush,
}

/// A COLRv0 base glyph resolved to ordered solid-color outline layers.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ColrV0Glyph {
    pub layers: Vec<ColorGlyphLayer>,
}

/// The embedding level encoded in OS/2 `fsType`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EmbeddingPermission {
    Installable,
    Editable,
    PreviewAndPrint,
    Restricted,
}

/// One variable-font design axis.
#[derive(Clone, Debug, PartialEq)]
pub struct VariationAxis {
    pub tag: FontTag,
    pub minimum: f32,
    pub default: f32,
    pub maximum: f32,
    pub hidden: bool,
}

/// Parsed properties that affect shaping, embedding, or cache identity.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct FaceInfo {
    pub identity: FaceIdentity,
    pub units_per_em: u16,
    pub glyph_count: u32,
    pub format: FontFormat,
    pub embedding: EmbeddingPermission,
    pub no_subsetting: bool,
    pub bitmap_embedding_only: bool,
    pub has_color_glyphs: bool,
    /// Exact technologies, in deterministic preference order.
    pub color_glyph_formats: Vec<ColorGlyphFormat>,
    pub has_math: bool,
    pub axes: Vec<VariationAxis>,
}

/// PDF-facing metrics for one concrete OpenType face instance.
///
/// Values are normalized to 1000 units per em, as required by PDF font
/// dictionaries. Glyph IDs remain source IDs until the subset map is applied.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfFaceMetrics {
    pub postscript_name: String,
    pub bounding_box: [i32; 4],
    pub ascent: i32,
    pub descent: i32,
    pub cap_height: i32,
    pub italic_angle: f32,
    pub glyph_widths: Vec<PdfGlyphWidth>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdfGlyphWidth {
    pub source: SourceGlyphId,
    pub width: i32,
}

/// Vertical PDF metrics for one source glyph, normalized to 1000 units/em.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PdfGlyphVerticalMetric {
    pub source: SourceGlyphId,
    /// Positive OpenType advance height; PDF serializes its negative as w1y.
    pub advance_height: i32,
    /// Position vector from horizontal origin to vertical origin.
    pub origin_x: i32,
    pub origin_y: i32,
}

/// Vertical metrics for a concrete face instance.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PdfVerticalMetrics {
    pub glyphs: Vec<PdfGlyphVerticalMetric>,
    /// True when the face had no `vhea`/`vmtx` and standard vertical metrics
    /// were synthesized from horizontal widths and the face ascent.
    pub synthesized: bool,
}

/// Initial OpenType MATH constants needed to connect a Unicode math-list
/// builder to the font boundary. Values remain in font units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MathConstants {
    pub units_per_em: u16,
    pub script_percent_scale_down: i16,
    pub script_script_percent_scale_down: i16,
    pub delimited_sub_formula_min_height: u16,
    pub display_operator_min_height: u16,
    pub axis_height: i16,
    pub fraction_rule_thickness: i16,
    pub radical_rule_thickness: i16,
}

/// Glyph identity in the source face before subsetting.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceGlyphId(pub u32);

/// Glyph identity in an emitted subset program.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubsetGlyphId(pub u32);

/// PDF character code. It is intentionally not interchangeable with a glyph.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PdfCode(pub u16);

/// PDF CID. It is intentionally not interchangeable with a glyph or code.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Cid(pub u16);

/// Index into [`ShapedRun::clusters`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClusterId(pub u32);

/// Direction of one already-itemized shaping run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// One OpenType feature and its half-open UTF-8 byte range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShapeFeature {
    pub tag: FontTag,
    pub value: u32,
    pub byte_range: Range<u32>,
}

impl ShapeFeature {
    /// Enable a feature over the complete shaping run.
    pub fn global(tag: FontTag, value: u32) -> Self {
        Self {
            tag,
            value,
            byte_range: 0..u32::MAX,
        }
    }
}

/// One user-space variable-font coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VariationSetting {
    pub tag: FontTag,
    pub value: f32,
}

/// One normalized F2Dot14 variation coordinate in axis order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NormalizedVariation {
    pub tag: FontTag,
    pub coordinate: i16,
}

/// A concrete face at one normalized variable-font location.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FontInstanceIdentity {
    pub face: FaceIdentity,
    pub variations: Vec<NormalizedVariation>,
}

/// Whether a missing character is an error or an explicit `.notdef` glyph.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MissingGlyphPolicy {
    #[default]
    Reject,
    PreserveNotdef,
}

/// A single-script, single-direction shaping request.
///
/// Bidi resolution, script itemization, and fallback happen above this API.
pub struct ShapeRequest<'a> {
    pub text: &'a str,
    pub face_index: FaceIndex,
    pub direction: TextDirection,
    /// ISO 15924 tag, such as `Latn`, `Arab`, or `Deva`.
    pub script: FontTag,
    /// BCP 47 language tag used for localized OpenType behavior.
    pub language: Option<&'a str>,
    pub features: &'a [ShapeFeature],
    pub variations: &'a [VariationSetting],
    pub missing_glyphs: MissingGlyphPolicy,
}

impl<'a> ShapeRequest<'a> {
    pub fn new(
        text: &'a str,
        face_index: FaceIndex,
        direction: TextDirection,
        script: FontTag,
    ) -> Self {
        Self {
            text,
            face_index,
            direction,
            script,
            language: None,
            features: &[],
            variations: &[],
            missing_glyphs: MissingGlyphPolicy::Reject,
        }
    }
}

/// Original semantic text carried separately from visual glyph order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextCluster {
    pub byte_range: Range<u32>,
    pub text: String,
}

/// One glyph after OpenType shaping, in font units.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionedGlyph {
    pub source_glyph: SourceGlyphId,
    pub cluster: ClusterId,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    pub unsafe_to_break: bool,
    pub unsafe_to_concat: bool,
    pub is_notdef: bool,
}

/// Positioned visual output plus logical Unicode clusters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShapedRun {
    pub instance: FontInstanceIdentity,
    pub implementation: &'static str,
    pub direction: TextDirection,
    pub script: FontTag,
    pub language: Option<String>,
    pub units_per_em: u16,
    pub glyphs: Vec<PositionedGlyph>,
    pub clusters: Vec<TextCluster>,
}

impl ShapedRun {
    /// Source glyphs needed to render the run, sorted and deduplicated.
    pub fn source_glyphs(&self) -> Vec<SourceGlyphId> {
        let mut glyphs = self
            .glyphs
            .iter()
            .map(|glyph| glyph.source_glyph)
            .collect::<Vec<_>>();
        glyphs.sort_unstable();
        glyphs.dedup();
        glyphs
    }

    /// Unicode scalars whose cmap entries should remain in a prepared subset.
    pub fn unicodes(&self) -> Vec<char> {
        let mut unicodes = self
            .clusters
            .iter()
            .flat_map(|cluster| cluster.text.chars())
            .collect::<Vec<_>>();
        unicodes.sort_unstable();
        unicodes.dedup();
        unicodes
    }
}

/// Inputs to a deterministic font subset operation.
pub struct SubsetRequest<'a> {
    pub face: FaceIdentity,
    pub glyphs: &'a [SourceGlyphId],
    pub unicodes: &'a [char],
    /// Concrete coordinates applied while producing a static subset.
    pub variations: &'a [VariationSetting],
}

impl<'a> SubsetRequest<'a> {
    pub fn new(face: FaceIdentity, glyphs: &'a [SourceGlyphId], unicodes: &'a [char]) -> Self {
        Self {
            face,
            glyphs,
            unicodes,
            variations: &[],
        }
    }

    pub fn for_run(run: &ShapedRun, glyphs: &'a [SourceGlyphId], unicodes: &'a [char]) -> Self {
        Self {
            face: run.instance.face,
            glyphs,
            unicodes,
            variations: &[],
        }
    }
}

/// Explicit source-to-subset glyph mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlyphMapping {
    pub source: SourceGlyphId,
    pub subset: SubsetGlyphId,
}

/// Bytes and identity returned by a subsetter implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedSubset {
    pub identity: SubsetIdentity,
    pub bytes: Vec<u8>,
    pub glyph_mapping: Vec<GlyphMapping>,
    pub was_subset: bool,
}

/// Complete deterministic identity for a prepared subset artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubsetIdentity {
    pub face: FaceIdentity,
    pub glyphs: Vec<SourceGlyphId>,
    pub unicodes: Vec<u32>,
    pub variations: Vec<NormalizedVariation>,
    pub embedding: EmbeddingPermission,
    pub policy: &'static str,
    pub implementation: &'static str,
}

/// Errors at the bytes-in/bytes-out font boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FontError {
    InvalidFont(String),
    InvalidScript(FontTag),
    InvalidLanguage(String),
    InvalidFeatureRange(Range<u32>),
    InvalidVariation(FontTag),
    TextTooLong,
    MissingGlyph {
        byte_offset: u32,
    },
    InvalidGlyph(SourceGlyphId),
    FaceMismatch {
        expected: FaceIdentity,
        actual: FaceIdentity,
    },
    UnsupportedOutline(FontFormat),
    EmbeddingRestricted,
    BitmapEmbeddingOnly,
    FullFontFromCollection,
    VariableInstanceUnsupported,
    ColorGlyphUnsupported(SourceGlyphId),
    ColorPaletteMissing(u16),
    ColorPaintUnsupported(SourceGlyphId),
    SubsetFailed(String),
}

impl fmt::Display for FontError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFont(error) => write!(formatter, "invalid OpenType font: {error}"),
            Self::InvalidScript(tag) => write!(formatter, "invalid ISO 15924 script tag {tag:?}"),
            Self::InvalidLanguage(language) => {
                write!(formatter, "invalid language tag {language:?}")
            }
            Self::InvalidFeatureRange(range) => {
                write!(
                    formatter,
                    "feature range {}..{} is not a UTF-8 range",
                    range.start, range.end
                )
            }
            Self::InvalidVariation(tag) => write!(formatter, "invalid value for variation {tag:?}"),
            Self::TextTooLong => {
                formatter.write_str("shaping input exceeds the 32-bit cluster space")
            }
            Self::MissingGlyph { byte_offset } => {
                write!(
                    formatter,
                    "font has no glyph for the cluster at UTF-8 byte {byte_offset}"
                )
            }
            Self::InvalidGlyph(glyph) => {
                write!(formatter, "source glyph {} is outside the font", glyph.0)
            }
            Self::FaceMismatch { expected, actual } => write!(
                formatter,
                "font bytes identify {actual:?}, but the subset request names {expected:?}"
            ),
            Self::UnsupportedOutline(format) => {
                write!(formatter, "unsupported outline format: {format:?}")
            }
            Self::EmbeddingRestricted => {
                formatter.write_str("font license prohibits document embedding")
            }
            Self::BitmapEmbeddingOnly => {
                formatter.write_str("font license permits bitmap embedding only")
            }
            Self::FullFontFromCollection => formatter.write_str(
                "font prohibits subsetting, but the selected face is inside a collection",
            ),
            Self::VariableInstanceUnsupported => formatter.write_str(
                "non-default variable-font instances require static outline instantiation",
            ),
            Self::ColorGlyphUnsupported(glyph) => write!(
                formatter,
                "source glyph {} requires unsupported color-font rendering",
                glyph.0
            ),
            Self::ColorPaletteMissing(palette) => {
                write!(formatter, "color font has no CPAL palette {palette}")
            }
            Self::ColorPaintUnsupported(glyph) => write!(
                formatter,
                "source glyph {} uses paint operations outside COLRv0 solid layers",
                glyph.0
            ),
            Self::SubsetFailed(error) => write!(formatter, "font subsetting failed: {error}"),
        }
    }
}

impl std::error::Error for FontError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_glyphs_and_unicodes_are_canonical() {
        let run = ShapedRun {
            instance: FontInstanceIdentity {
                face: FaceIdentity {
                    digest: FontDigest([0; 32]),
                    index: FaceIndex(0),
                },
                variations: Vec::new(),
            },
            implementation: "test-shaper-v1",
            direction: TextDirection::LeftToRight,
            script: FontTag(*b"Latn"),
            language: Some("en".into()),
            units_per_em: 1000,
            glyphs: vec![
                PositionedGlyph {
                    source_glyph: SourceGlyphId(7),
                    cluster: ClusterId(0),
                    x_advance: 1,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                    unsafe_to_break: false,
                    unsafe_to_concat: false,
                    is_notdef: false,
                },
                PositionedGlyph {
                    source_glyph: SourceGlyphId(3),
                    cluster: ClusterId(1),
                    x_advance: 1,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                    unsafe_to_break: false,
                    unsafe_to_concat: false,
                    is_notdef: false,
                },
                PositionedGlyph {
                    source_glyph: SourceGlyphId(7),
                    cluster: ClusterId(1),
                    x_advance: 1,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                    unsafe_to_break: false,
                    unsafe_to_concat: false,
                    is_notdef: false,
                },
            ],
            clusters: vec![
                TextCluster {
                    byte_range: 0..2,
                    text: "é".into(),
                },
                TextCluster {
                    byte_range: 2..3,
                    text: "a".into(),
                },
            ],
        };
        assert_eq!(
            run.source_glyphs(),
            vec![SourceGlyphId(3), SourceGlyphId(7)]
        );
        assert_eq!(run.unicodes(), vec!['a', 'é']);
    }
}
