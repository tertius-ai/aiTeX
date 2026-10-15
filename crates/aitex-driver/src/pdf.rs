//! Writing a PDF, by hand.
//!
//! A PDF is a sequence of numbered objects, a cross-reference table of their
//! byte offsets, and a trailer pointing at the table. The serializer stays
//! deliberately small. Font preparation crosses the `aitex-fonts` boundary,
//! and a pure-Rust zlib encoder handles ordinary PDF streams.
//!
//! The classic DVI path embeds Type 1 programs exactly as TeX Live ships them
//! (`cmr10.pfb`), with Type 3 as its bitmap fallback. The native positioned
//! path embeds already-shaped static TrueType or CFF OpenType faces as Type 0
//! CID fonts. The two routes deliberately share serialization machinery while
//! retaining separate font and text policies.

use crate::dvi;

pub use aitex_fonts::{
    ClassicEmbeddingPolicy, ClassicFaceIdentity, PreparedType1, prepare_type1_program,
    prepare_type1_subset, transformed_type1_name,
};

/// Whether an EPS program stays within the deterministic graphics-operator
/// subset translated by the PDF writer. Hosts use this to avoid eagerly
/// launching a renderer for a vector program the driver will preserve.
pub fn directly_embeddable_eps(bytes: &[u8]) -> bool {
    eps_content(bytes).1
}

/// Bytes and face selection for one document-local positioned font instance.
#[derive(Clone)]
#[non_exhaustive]
pub struct OpenTypeFontSource {
    pub bytes: std::sync::Arc<[u8]>,
    pub face_index: aitex_fonts::FaceIndex,
    pub variations: std::sync::Arc<[aitex_fonts::VariationSetting]>,
    /// Zero-based CPAL palette selected for COLRv0 glyphs.
    pub palette_index: u16,
    /// Optional content-addressed artifact supplied by a persistent host cache.
    /// The driver validates its complete identity before using it.
    pub prepared_subset: Option<std::sync::Arc<aitex_fonts::PreparedSubset>>,
}

/// Optional semantic policy for the standalone positioned PDF writer.
///
/// Tagged output is deliberately opt-in while the project is building out
/// artifacts, figures, annotations, metadata, and independent PDF/UA
/// validation. Enabling it creates a real structure tree and parent tree; it
/// does not claim conformance to a publication profile.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct PositionedPdfSemantics<'a> {
    pub tag_text: bool,
    pub document_language: Option<&'a str>,
}

impl<'a> PositionedPdfSemantics<'a> {
    pub const fn tagged(document_language: &'a str) -> Self {
        Self {
            tag_text: true,
            document_language: Some(document_language),
        }
    }
}

impl OpenTypeFontSource {
    pub fn new(bytes: impl Into<std::sync::Arc<[u8]>>, face_index: u32) -> Self {
        Self {
            bytes: bytes.into(),
            face_index: aitex_fonts::FaceIndex(face_index),
            variations: std::sync::Arc::from([]),
            palette_index: 0,
            prepared_subset: None,
        }
    }

    pub fn with_variations(
        mut self,
        variations: impl Into<std::sync::Arc<[aitex_fonts::VariationSetting]>>,
    ) -> Self {
        self.variations = variations.into();
        self
    }

    pub fn with_palette(mut self, palette_index: u16) -> Self {
        self.palette_index = palette_index;
        self
    }

    pub fn with_prepared_subset(
        mut self,
        subset: impl Into<std::sync::Arc<aitex_fonts::PreparedSubset>>,
    ) -> Self {
        self.prepared_subset = Some(subset.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PositionedPdfError {
    MissingFont(aitex_page::FontInstanceId),
    InvalidCluster {
        font: aitex_page::FontInstanceId,
        cluster: aitex_page::ClusterId,
    },
    GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId),
    UnsupportedDirection(aitex_page::TextDirection),
    UnsupportedColorAlpha {
        glyph: aitex_page::SourceGlyphId,
        alpha: u8,
    },
    UnsupportedColorBrush(aitex_page::SourceGlyphId),
    InvalidLanguage(String),
    UnsupportedClassicGlyph,
    UnsupportedSpecial,
    HybridPageCount {
        dvi: usize,
        positioned: usize,
    },
    Font(aitex_fonts::FontError),
}

impl std::fmt::Display for PositionedPdfError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFont(font) => {
                write!(formatter, "positioned font {} was not resolved", font.0)
            }
            Self::InvalidCluster { font, cluster } => write!(
                formatter,
                "positioned font {} references missing cluster {}",
                font.0, cluster.0
            ),
            Self::GlyphOutsidePdfCodeSpace(glyph) => write!(
                formatter,
                "source glyph {} is outside one PDF two-byte code space",
                glyph.0
            ),
            Self::UnsupportedDirection(direction) => {
                write!(
                    formatter,
                    "positioned PDF direction {direction:?} is not implemented"
                )
            }
            Self::UnsupportedColorAlpha { glyph, alpha } => write!(
                formatter,
                "color glyph layer {} uses unsupported alpha {alpha}",
                glyph.0
            ),
            Self::UnsupportedColorBrush(glyph) => write!(
                formatter,
                "color glyph layer {} uses an unsupported brush",
                glyph.0
            ),
            Self::InvalidLanguage(language) => {
                write!(formatter, "invalid BCP 47 language tag {language:?}")
            }
            Self::UnsupportedClassicGlyph => formatter.write_str(
                "classic page glyphs require the DVI adapter and cannot be silently reinterpreted",
            ),
            Self::UnsupportedSpecial => formatter.write_str(
                "positioned PDF specials are not implemented; no special was silently dropped",
            ),
            Self::HybridPageCount { dvi, positioned } => write!(
                formatter,
                "hybrid PDF page streams disagree: DVI has {dvi} pages, positioned output has {positioned}"
            ),
            Self::Font(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PositionedPdfError {}

impl From<aitex_fonts::FontError> for PositionedPdfError {
    fn from(error: aitex_fonts::FontError) -> Self {
        Self::Font(error)
    }
}

/// PDF serialization and execution choices.
///
/// Compact streams are the production default. Keeping an explicit plain mode
/// makes the handwritten object graph readable in tests and developer probes.
/// `compression_workers` is a scheduling budget and never affects serialized
/// bytes, so callers may reduce it when they parallelize whole documents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PdfOptions {
    pub compress_streams: bool,
    pub compression_workers: usize,
    /// Pack eligible non-stream objects and emit a PDF 1.5 xref stream.
    pub compact_objects: bool,
    /// Remove unused classic Type 1 glyph programs when their structure can
    /// be proven safe. The complete font remains the automatic fallback.
    pub subset_type1: bool,
}

const STREAM_COMPRESSION_LEVEL: u32 = 6;
pub const DEFAULT_PDF_COMPRESSION_WORKERS: usize = 4;

impl PdfOptions {
    pub const fn uncompressed() -> Self {
        Self {
            compress_streams: false,
            compression_workers: 1,
            compact_objects: false,
            subset_type1: true,
        }
    }

    pub const fn with_compression(mut self, enabled: bool) -> Self {
        self.compress_streams = enabled;
        self
    }

    pub const fn with_compression_workers(mut self, workers: usize) -> Self {
        self.compression_workers = workers;
        self
    }

    pub const fn with_compact_objects(mut self, enabled: bool) -> Self {
        self.compact_objects = enabled;
        self
    }

    pub const fn with_type1_subsetting(mut self, enabled: bool) -> Self {
        self.subset_type1 = enabled;
        self
    }

    /// Stable identity for caches which retain serialized PDF objects.
    pub const fn stream_format_tag(self) -> &'static str {
        match (
            self.compress_streams,
            self.compact_objects,
            self.subset_type1,
        ) {
            (true, true, true) => {
                "pdf15-objstm-xrefstm-flate6-type1-union-subset-v3-pageops-v2-images-v2"
            }
            (true, true, false) => "pdf15-objstm-xrefstm-flate6-type1-full-v1-pageops-v2-images-v2",
            (true, false, true) => "classic-xref-flate6-type1-union-subset-v3-pageops-v2-images-v2",
            (true, false, false) => "classic-xref-flate6-type1-full-v1-pageops-v2-images-v2",
            (false, true, true) => {
                "pdf15-objstm-xrefstm-plain-type1-union-subset-v3-pageops-v2-images-v2"
            }
            (false, true, false) => "pdf15-objstm-xrefstm-plain-type1-full-v1-pageops-v2-images-v2",
            (false, false, true) => "classic-xref-plain-type1-union-subset-v3-pageops-v2-images-v2",
            (false, false, false) => "classic-xref-plain-type1-full-v1-pageops-v2-images-v2",
        }
    }
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            compress_streams: true,
            compression_workers: DEFAULT_PDF_COMPRESSION_WORKERS,
            compact_objects: true,
            subset_type1: true,
        }
    }
}

/// One scaled TeX point in PDF points.  TeX uses 72.27 points per inch while
/// PDF uses 72; treating them as the same unit enlarges every coordinate,
/// glyph, and rule by 0.375% and misaligns positioned PDF literals.
fn pt(sp: i64) -> f64 {
    sp as f64 / 65536.0 * 72.0 / 72.27
}

/// A compact, deterministic PDF real number with a caller-selected error
/// bound. PDF accepts ordinary decimal numbers; trailing zeroes only enlarge
/// content streams and make repeated operands less compressible.
fn pdf_number(value: f64, precision: usize) -> String {
    let value = if value.is_finite() { value } else { 0.0 };
    let mut rendered = format!("{value:.precision$}");
    if rendered.contains('.') {
        while rendered.ends_with('0') {
            rendered.pop();
        }
        if rendered.ends_with('.') {
            rendered.pop();
        }
    }
    if rendered == "-0" {
        rendered.replace_range(.., "0");
    }
    rendered
}

/// Spell a PDF number and retain the numeric value represented by those
/// decimal places. Hot page assembly uses the value for residual and relative
/// positioning; deriving it arithmetically avoids reparsing every generated
/// operand while preserving the same ties-to-even rounding as Rust's fixed
/// decimal formatter.
fn encoded_pdf_number(value: f64, precision: usize) -> (String, f64) {
    (
        pdf_number(value, precision),
        encoded_pdf_value(value, precision),
    )
}

fn encoded_pdf_value(value: f64, precision: usize) -> f64 {
    let value = if value.is_finite() { value } else { 0.0 };
    let factor = match precision {
        0 => 1.0,
        1 => 10.0,
        2 => 100.0,
        3 => 1_000.0,
        4 => 10_000.0,
        5 => 100_000.0,
        6 => 1_000_000.0,
        7 => 10_000_000.0,
        8 => 100_000_000.0,
        9 => 1_000_000_000.0,
        10 => 10_000_000_000.0,
        _ => 10f64.powi(precision.try_into().unwrap_or(i32::MAX)),
    };
    let scaled = value * factor;
    let encoded = if scaled.is_finite() {
        scaled.round_ties_even() / factor
    } else {
        value
    };
    encoded
}

/// What the caller must tell us about a font that the DVI file does not say:
/// where its program is and how wide its characters are. The DVI records only
/// a name and a size.
#[derive(Clone)]
pub struct FontSource {
    /// PostScript `/BaseFont` name. This often differs from the TeX/DVI name:
    /// `ec-lmr10`, for example, maps to `LMRoman10-Regular`.
    pub postscript_name: String,
    /// The `.pfb` bytes, or `None` to reference the font without embedding it.
    pub pfb: Option<std::sync::Arc<[u8]>>,
    /// A content-addressed, already unwrapped/transformed Type 1 program.
    /// Persistent callers can retain this across documents and avoid repeating
    /// PFB parsing and `SlantFont`/`ExtendFont` rewriting. When present it takes
    /// precedence over `pfb`; one-shot callers may continue to supply `pfb`.
    pub prepared_type1: Option<PreparedType1>,
    /// A packed bitmap font used when no scalable program exists. The PDF
    /// writer embeds it as Type 3; at most one of `pfb` and `pk` should be set.
    pub pk: Option<std::sync::Arc<[u8]>>,
    /// Glyph name at each eight-bit character code. An explicit PDF Encoding
    /// built from this vector makes reencoded TeX fonts render as intended.
    pub encoding: Option<std::sync::Arc<[String]>>,
    /// UTF-16BE hex at each character code, derived from the encoding and
    /// LaTeX's `\pdfglyphtounicode` table. This drives copy/paste and search.
    pub to_unicode: std::sync::Arc<[Option<String>]>,
    /// dvips map `SlantFont` shear (zero for an untransformed font).
    pub slant: f64,
    /// dvips map `ExtendFont` horizontal scale (one when untransformed).
    pub extend: f64,
    /// Character widths in scaled points, indexed by code, for 0..=255.
    pub widths: std::sync::Arc<[i64]>,
    /// `\fontdimen`-independent bounding box, in 1000ths of the em. Only the
    /// font descriptor needs it and no renderer checks it closely.
    pub bbox: [i32; 4],
}

/// A physical Type 1 face, independent of TeX encoding, TFM instance, and
/// point size. Compatible uses share this value and receive one union subset.
#[derive(Clone)]
pub struct ClassicFontFace {
    pub identity: ClassicFaceIdentity,
    pub postscript_name: String,
    pub pfb: std::sync::Arc<[u8]>,
    pub slant: f64,
    pub extend: f64,
    pub bbox: [i32; 4],
}

impl ClassicFontFace {
    pub fn type1(
        pfb: impl Into<std::sync::Arc<[u8]>>,
        postscript_name: impl Into<String>,
        slant: f64,
        extend: f64,
        embedding: ClassicEmbeddingPolicy,
        bbox: [i32; 4],
    ) -> Self {
        let pfb = pfb.into();
        let postscript_name = postscript_name.into();
        let identity =
            ClassicFaceIdentity::type1(&pfb, postscript_name.clone(), slant, extend, embedding);
        Self {
            identity,
            postscript_name,
            pfb,
            slant,
            extend,
            bbox,
        }
    }
}

/// Device-specific bitmap material or one shared scalable face.
#[derive(Clone)]
pub enum ClassicFontProgram {
    Type1(std::sync::Arc<ClassicFontFace>),
    Prepared {
        face: std::sync::Arc<ClassicFontFace>,
        prepared: PreparedType1,
    },
    Pk {
        bytes: std::sync::Arc<[u8]>,
        postscript_name: String,
        bbox: [i32; 4],
    },
    /// An eight-bit TeX use of an embedded TrueType face selected by a
    /// pdfTeX/dvips map. The DVI's encoding and TFM widths remain on
    /// `ClassicFontUse`; this variant carries the scalable sfnt program.
    TrueType {
        bytes: std::sync::Arc<[u8]>,
        postscript_name: String,
        slant: f64,
        extend: f64,
        bbox: [i32; 4],
    },
    /// Compatibility route for callers intentionally relying on a resident
    /// PDF font. The compiler itself never selects this route.
    Resident {
        postscript_name: String,
        slant: f64,
        extend: f64,
        bbox: [i32; 4],
    },
}

/// One TeX/DVI use of a classic face. Encoding, extraction, widths, and size
/// remain here because they affect observable text semantics.
#[derive(Clone)]
pub struct ClassicFontUse {
    pub program: ClassicFontProgram,
    pub encoding: Option<std::sync::Arc<[String]>>,
    pub to_unicode: std::sync::Arc<[Option<String>]>,
    pub widths: std::sync::Arc<[i64]>,
}

impl FontSource {
    pub fn into_classic_use(self, embedding: ClassicEmbeddingPolicy) -> ClassicFontUse {
        let source = self;
        let program = if let Some(pk) = source.pk {
            ClassicFontProgram::Pk {
                bytes: pk,
                postscript_name: source.postscript_name,
                bbox: source.bbox,
            }
        } else if let Some(pfb) = source.pfb {
            ClassicFontProgram::Type1(std::sync::Arc::new(ClassicFontFace::type1(
                pfb,
                source.postscript_name,
                source.slant,
                source.extend,
                embedding,
                source.bbox,
            )))
        } else if let Some(prepared) = source.prepared_type1 {
            // An older caller may only retain the prepared bytes. Treat those
            // immutable bytes as the physical program: without the source PFB
            // it would be unsafe to enlarge an existing subset closure.
            let face = std::sync::Arc::new(ClassicFontFace::type1(
                std::sync::Arc::<[u8]>::from(prepared.program()),
                source.postscript_name,
                source.slant,
                source.extend,
                ClassicEmbeddingPolicy::Full,
                source.bbox,
            ));
            ClassicFontProgram::Prepared { face, prepared }
        } else {
            ClassicFontProgram::Resident {
                postscript_name: source.postscript_name,
                slant: source.slant,
                extend: source.extend,
                bbox: source.bbox,
            }
        };
        ClassicFontUse {
            program,
            encoding: source.encoding,
            to_unicode: source.to_unicode,
            widths: source.widths,
        }
    }
}

impl From<FontSource> for ClassicFontUse {
    fn from(source: FontSource) -> Self {
        source.into_classic_use(ClassicEmbeddingPolicy::Subset)
    }
}

/// Bytes for a file named by a graphics special, plus an optional raster made
/// by a format-aware renderer. The driver uses its vector EPS translator when
/// possible and uses the raster for PDF/PNG/JPEG assets or procedure-heavy EPS.
#[derive(Clone)]
pub struct AssetSource {
    pub bytes: std::sync::Arc<[u8]>,
    /// A selected PDF page normalized to a classic cross-reference table with
    /// directly reusable compressed streams. The writer imports it as a Form
    /// XObject, preserving vector paths, fonts, transparency, and images.
    pub pdf: Option<std::sync::Arc<[u8]>>,
    /// An RGB PNG (preferred) or binary PPM (`P6`) at any resolution. PNG's
    /// compressed image stream is embedded directly; PPM remains accepted for
    /// callers and tests that already provide it.
    pub raster: Option<std::sync::Arc<[u8]>>,
    /// The full raster's natural bounding box in the coordinate system used
    /// by the `pdf:image bbox` special. PNG/JPEG pixels are not necessarily
    /// PDF points: a 144-dpi 1184-pixel image is about 592bp wide. Keeping the
    /// physical box beside the bytes lets a cropped special scale and offset
    /// the Image XObject without accidentally treating pixels as points.
    pub raster_bbox: Option<[f64; 4]>,
}

// These are same-module fragments: the split is deliberately behavior-neutral,
// while keeping the stateful writer pass free of visibility and call changes.
include!("pdf/import.rs");
include!("pdf/objects.rs");
include!("pdf/positioned.rs");
include!("pdf/specials.rs");
include!("pdf/raster.rs");
include!("pdf/document.rs");
include!("pdf/writer.rs");
