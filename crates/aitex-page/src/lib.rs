//! Final positioned page events shared by aiTeX's output backends.
//!
//! This crate deliberately knows nothing about OpenType parsing, shaping, PDF,
//! or filesystems.  Layout records exact scaled-point positions and stable
//! document-local resource IDs; the host resolves those IDs to font bytes.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::{string::String, vec::Vec};

/// A TeX scaled point: 65536 units per TeX point.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sp(pub i64);

/// A document-local font instance, independent of PDF object allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FontInstanceId(pub u32);

/// A glyph ID in the source face before font subsetting.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceGlyphId(pub u32);

/// Index into a shaped run's logical cluster table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClusterId(pub u32);

/// Visual writing direction of an already itemized and shaped run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// Portable semantic role for one text run.
///
/// Layout may leave runs as [`Span`](Self::Span). Higher layers can promote
/// them without importing PDF names or accessibility-library types into the
/// page contract.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum TextRole {
    #[default]
    Span,
    Paragraph,
    Heading(u8),
    Quote,
    Code,
    Link,
    Formula,
}

/// Original semantic text associated with one shaping cluster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextCluster {
    /// Half-open byte range in the original UTF-8 shaping input.
    pub byte_range: core::ops::Range<u32>,
    /// Exact original Unicode text for extraction and accessibility.
    pub text: String,
}

/// One shaped glyph in visual order, with final scaled-point metrics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionedGlyph {
    pub source_gid: SourceGlyphId,
    pub cluster: ClusterId,
    pub x_advance: Sp,
    pub y_advance: Sp,
    pub x_offset: Sp,
    pub y_offset: Sp,
    /// True only when shaping explicitly preserved glyph zero for a missing
    /// character. The PDF backend then keeps the semantic text in ActualText.
    pub is_notdef: bool,
}

/// One already-itemized and shaped glyph run.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct GlyphRun {
    pub font: FontInstanceId,
    /// Text size used by PDF's `Tf` operator.
    pub size: Sp,
    /// Baseline origin from TeX's page origin (one inch from top-left).
    pub origin_x: Sp,
    pub origin_y: Sp,
    pub direction: TextDirection,
    /// BCP 47 language inherited from shaping, when one was supplied.
    pub language: Option<String>,
    /// Semantic role used by optional tagged-output backends.
    pub role: TextRole,
    /// Glyphs in visual paint order.
    pub glyphs: Vec<PositionedGlyph>,
    /// Clusters in logical source order.
    pub clusters: Vec<TextCluster>,
}

impl GlyphRun {
    /// Construct a run with no language override and the neutral span role.
    ///
    /// Keeping construction behind this function lets the page contract gain
    /// optional metadata without breaking downstream struct literals.
    pub fn new(
        font: FontInstanceId,
        size: Sp,
        origin_x: Sp,
        origin_y: Sp,
        direction: TextDirection,
        glyphs: Vec<PositionedGlyph>,
        clusters: Vec<TextCluster>,
    ) -> Self {
        Self {
            font,
            size,
            origin_x,
            origin_y,
            direction,
            language: None,
            role: TextRole::Span,
            glyphs,
            clusters,
        }
    }

    pub fn with_language(mut self, language: Option<String>) -> Self {
        self.language = language;
        self
    }

    pub fn with_role(mut self, role: TextRole) -> Self {
        self.role = role;
        self
    }

    /// Original logical text, ordered by source byte position.
    pub fn logical_text(&self) -> String {
        let mut clusters = self.clusters.iter().collect::<Vec<_>>();
        clusters.sort_by_key(|cluster| cluster.byte_range.start);
        clusters
            .into_iter()
            .map(|cluster| cluster.text.as_str())
            .collect()
    }
}

/// A final page drawing event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Item {
    /// A byte-oriented TeX glyph. This preserves the current DVI oracle while
    /// the same shipout traversal also learns to emit shaped runs.
    ClassicGlyph {
        font: FontInstanceId,
        code: u32,
        origin_x: Sp,
        origin_y: Sp,
        advance: Sp,
    },
    GlyphRun(GlyphRun),
    /// Rule with a baseline-relative lower edge, matching DVI convention.
    Rule {
        x: Sp,
        baseline_y: Sp,
        width: Sp,
        height: Sp,
    },
    /// Backend-special bytes at a final position.
    Special {
        x: Sp,
        y: Sp,
        bytes: Vec<u8>,
    },
}

/// One page. Dimensions are PDF big points stored as thousandths to avoid
/// floating-point identity in caches (`612000` is US Letter width).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Page {
    pub width_millibp: u32,
    pub height_millibp: u32,
    /// TeX's `\count0` through `\count9` at shipout. PDF normally ignores
    /// these, while a DVI backend writes them in `bop`; keeping them here makes
    /// the page contract complete without making it DVI-shaped.
    pub counts: [i32; 10],
    pub items: Vec<Item>,
}

/// A fully laid-out document. Font instances are resolved separately by ID.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Document {
    pub pages: Vec<Page>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec};

    #[test]
    fn logical_text_uses_source_order_not_visual_order() {
        let run = GlyphRun::new(
            FontInstanceId(0),
            Sp(10 * 65_536),
            Sp(0),
            Sp(0),
            TextDirection::RightToLeft,
            vec![],
            vec![
                TextCluster {
                    byte_range: 2..4,
                    text: "ב".to_string(),
                },
                TextCluster {
                    byte_range: 0..2,
                    text: "א".to_string(),
                },
            ],
        )
        .with_language(Some("he".to_string()));
        assert_eq!(run.logical_text(), "אב");
    }
}
