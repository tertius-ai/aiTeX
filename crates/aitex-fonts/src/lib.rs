//! aiTeX's format-neutral font boundary.
//!
//! Callers supply bytes and semantic requests; this crate parses, shapes,
//! enforces embedding policy, and returns bytes and aiTeX-owned data. File and
//! system-font discovery deliberately remain in the CLI, and the TeX engine
//! deliberately does not depend on this crate.
#![forbid(unsafe_code)]

mod classic;
mod model;
mod opentype;

pub use classic::{
    PreparedType1, prepare_type1_program, prepare_type1_subset, transformed_type1_name,
};
pub use model::{
    CLASSIC_TYPE1_PREPARATION_VERSION, Cid, ClassicEmbeddingPolicy, ClassicFaceIdentity, ClusterId,
    ColorGlyphBrush, ColorGlyphFormat, ColorGlyphLayer, ColorRenderMode, ColrV0Glyph,
    EmbeddingPermission, FaceIdentity, FaceIndex, FaceInfo, FontDigest, FontError, FontFormat,
    FontInstanceIdentity, FontTag, GlyphMapping, MathConstants, MissingGlyphPolicy,
    NormalizedVariation, PdfCode, PdfFaceMetrics, PdfGlyphVerticalMetric, PdfGlyphWidth,
    PdfVerticalMetrics, PositionedGlyph, PreparedSubset, RgbaColor, ShapeFeature, ShapeRequest,
    ShapedRun, SourceGlyphId, SubsetGlyphId, SubsetIdentity, SubsetRequest, TextCluster,
    TextDirection, VariationAxis, VariationSetting,
};
pub use opentype::{
    FontSubsetter, HARFRUST_SHAPER_VERSION, SKERA_SUBSETTER_VERSION, SkeraSubsetter, colr_v0_glyph,
    colr_v0_glyphs, font_instance_identity, inspect_face, math_constants, pdf_face_metrics,
    pdf_vertical_metrics, positioned_run, shape,
};
