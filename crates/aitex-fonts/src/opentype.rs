use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use harfrust::{
    BufferFlags, Direction, Feature, FontRef, Language, Script, ShapeOptions, ShaperData,
    ShaperInstance, UnicodeBuffer, Variation,
};
use kurbo::{BezPath, CubicBez, Point};
use read_fonts::{TableProvider, collections::IntSet, types::NameId};
use skrifa::MetadataProvider;
use skrifa::color::{
    Brush as SkrifaBrush, ColorGlyphFormat as SkrifaColorGlyphFormat, ColorPainter, CompositeMode,
    Transform,
};
use skrifa::{
    instance::{LocationRef, Size},
    outline::{DrawSettings, OutlinePen},
    string::StringId,
};
use write_fonts::{
    FontBuilder,
    tables::glyf::{Bbox, GlyfLocaBuilder, Glyph, SimpleGlyph},
};

use crate::{
    ClusterId, ColorGlyphBrush, ColorGlyphFormat, ColorGlyphLayer, ColrV0Glyph,
    EmbeddingPermission, FaceIdentity, FaceIndex, FaceInfo, FontDigest, FontError, FontFormat,
    FontInstanceIdentity, FontTag, GlyphMapping, MathConstants, MissingGlyphPolicy,
    NormalizedVariation, PdfFaceMetrics, PdfGlyphVerticalMetric, PdfGlyphWidth, PdfVerticalMetrics,
    PositionedGlyph, PreparedSubset, RgbaColor, ShapeRequest, ShapedRun, SourceGlyphId,
    SubsetGlyphId, SubsetIdentity, SubsetRequest, TextCluster, TextDirection, VariationAxis,
    VariationSetting,
};

/// Part of every shaped-run cache identity.
pub const HARFRUST_SHAPER_VERSION: &str = "harfrust-0.13.3-v1";

/// Part of every prepared-subset cache identity.
pub const SKERA_SUBSETTER_VERSION: &str = "skera-0.6.0-retain-gids-static-instance-v2";

fn contain_subsetter_panic<T>(action: impl FnOnce() -> T) -> Result<T, FontError> {
    catch_unwind(AssertUnwindSafe(action))
        .map_err(|_| FontError::SubsetFailed("subsetter rejected malformed font data".to_owned()))
}

fn font_ref(bytes: &[u8], index: FaceIndex) -> Result<FontRef<'_>, FontError> {
    FontRef::from_index(bytes, index.0).map_err(|error| FontError::InvalidFont(error.to_string()))
}

fn tag(tag: FontTag) -> read_fonts::types::Tag {
    read_fonts::types::Tag::new(&tag.0)
}

fn own_tag(tag: read_fonts::types::Tag) -> FontTag {
    FontTag(tag.to_be_bytes())
}

fn outline_format(font: &FontRef<'_>) -> FontFormat {
    use read_fonts::types::Tag;
    if font.table_data(Tag::new(b"CFF2")).is_some() {
        FontFormat::Cff2
    } else if font.table_data(Tag::new(b"CFF ")).is_some() {
        FontFormat::Cff
    } else if font.table_data(Tag::new(b"glyf")).is_some() {
        FontFormat::TrueType
    } else {
        FontFormat::Unsupported
    }
}

fn validate_skera_metric_variations(font: &FontRef<'_>) -> Result<(), FontError> {
    use read_fonts::types::Tag;

    if font.table_data(Tag::new(b"HVAR")).is_some() {
        let hvar = font
            .hvar()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?;
        let store = hvar
            .item_variation_store()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?;
        if store.item_variation_data_count() == 0 {
            return Err(FontError::InvalidFont(
                "HVAR item variation store contains no variation data".to_owned(),
            ));
        }
    }
    if font.table_data(Tag::new(b"VVAR")).is_some() {
        let vvar = font
            .vvar()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?;
        let store = vvar
            .item_variation_store()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?;
        if store.item_variation_data_count() == 0 {
            return Err(FontError::InvalidFont(
                "VVAR item variation store contains no variation data".to_owned(),
            ));
        }
    }
    Ok(())
}

fn embedding_permissions(fs_type: u16) -> (EmbeddingPermission, bool, bool) {
    let permission = if fs_type & 0x0002 != 0 {
        EmbeddingPermission::Restricted
    } else if fs_type & 0x0004 != 0 {
        EmbeddingPermission::PreviewAndPrint
    } else if fs_type & 0x0008 != 0 {
        EmbeddingPermission::Editable
    } else {
        EmbeddingPermission::Installable
    };
    (permission, fs_type & 0x0100 != 0, fs_type & 0x0200 != 0)
}

fn color_glyph_formats(font: &FontRef<'_>) -> Vec<ColorGlyphFormat> {
    use read_fonts::types::Tag;

    let mut formats = Vec::new();
    if let Some(colr) = font.table_data(Tag::new(b"COLR")) {
        let version = colr
            .as_bytes()
            .get(..2)
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
            .unwrap_or(0);
        formats.push(if version == 0 {
            ColorGlyphFormat::ColrV0
        } else {
            ColorGlyphFormat::ColrV1
        });
    }
    for (table, format) in [
        (*b"SVG ", ColorGlyphFormat::Svg),
        (*b"sbix", ColorGlyphFormat::Sbix),
        (*b"CBDT", ColorGlyphFormat::Cbdt),
    ] {
        if font.table_data(Tag::new(&table)).is_some() {
            formats.push(format);
        }
    }
    formats
}

/// Parse the face metadata that affects shaping, PDF embedding, and caching.
pub fn inspect_face(bytes: &[u8], index: FaceIndex) -> Result<FaceInfo, FontError> {
    let font = font_ref(bytes, index)?;
    let units_per_em = font
        .head()
        .map_err(|error| FontError::InvalidFont(error.to_string()))?
        .units_per_em();
    let glyph_count = u32::from(
        font.maxp()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?
            .num_glyphs(),
    );
    let os2_tag = read_fonts::types::Tag::new(b"OS/2");
    let (embedding, no_subsetting, bitmap_embedding_only) = if font.table_data(os2_tag).is_some() {
        let table = font
            .os2()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?;
        embedding_permissions(table.fs_type())
    } else {
        (EmbeddingPermission::Installable, false, false)
    };
    let axes = font
        .axes()
        .iter()
        .map(|axis| VariationAxis {
            tag: own_tag(axis.tag()),
            minimum: axis.min_value(),
            default: axis.default_value(),
            maximum: axis.max_value(),
            hidden: axis.is_hidden(),
        })
        .collect();
    let color_glyph_formats = color_glyph_formats(&font);
    let has_math = font
        .table_data(read_fonts::types::Tag::new(b"MATH"))
        .is_some();

    Ok(FaceInfo {
        identity: FaceIdentity {
            digest: FontDigest::of(bytes),
            index,
        },
        units_per_em,
        glyph_count,
        format: outline_format(&font),
        embedding,
        no_subsetting,
        bitmap_embedding_only,
        has_color_glyphs: !color_glyph_formats.is_empty(),
        color_glyph_formats,
        has_math,
        axes,
    })
}

struct ColrV0Collector<'a> {
    base: SourceGlyphId,
    palette: &'a [RgbaColor],
    layers: Vec<ColorGlyphLayer>,
    unsupported: bool,
}

impl ColrV0Collector<'_> {
    fn reject(&mut self) {
        self.unsupported = true;
    }
}

impl ColorPainter for ColrV0Collector<'_> {
    fn push_transform(&mut self, _transform: Transform) {
        self.reject();
    }

    fn pop_transform(&mut self) {
        self.reject();
    }

    fn push_clip_glyph(&mut self, _glyph_id: read_fonts::types::GlyphId) {
        self.reject();
    }

    fn push_clip_box(&mut self, _clip_box: read_fonts::types::BoundingBox<f32>) {
        self.reject();
    }

    fn pop_clip(&mut self) {
        self.reject();
    }

    fn fill(&mut self, _brush: SkrifaBrush<'_>) {
        self.reject();
    }

    fn fill_glyph(
        &mut self,
        glyph_id: read_fonts::types::GlyphId,
        brush_transform: Option<Transform>,
        brush: SkrifaBrush<'_>,
    ) {
        if brush_transform.is_some() {
            self.reject();
            return;
        }
        let SkrifaBrush::Solid {
            palette_index,
            alpha,
        } = brush
        else {
            self.reject();
            return;
        };
        let alpha_byte = |value: f32| {
            if value.is_finite() {
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            } else {
                0
            }
        };
        let brush = if palette_index == 0xffff {
            ColorGlyphBrush::Foreground {
                alpha: alpha_byte(alpha),
            }
        } else {
            let Some(mut color) = self.palette.get(usize::from(palette_index)).copied() else {
                self.reject();
                return;
            };
            color.alpha = alpha_byte(f32::from(color.alpha) / 255.0 * alpha);
            ColorGlyphBrush::Solid(color)
        };
        self.layers.push(ColorGlyphLayer {
            source_glyph: SourceGlyphId(glyph_id.to_u32()),
            brush,
        });
    }

    fn push_layer(&mut self, _composite_mode: CompositeMode) {
        self.reject();
    }

    fn pop_layer(&mut self) {
        self.reject();
    }
}

/// Resolve one COLRv0 glyph into ordered outline layers and CPAL colors.
///
/// `Ok(None)` means the glyph has no COLRv0 representation. The returned
/// model is owned by aiTeX and remains independent of Fontations APIs.
pub fn colr_v0_glyph(
    bytes: &[u8],
    index: FaceIndex,
    variations: &[VariationSetting],
    glyph: SourceGlyphId,
    palette_index: u16,
) -> Result<Option<ColrV0Glyph>, FontError> {
    Ok(
        colr_v0_glyphs(bytes, index, variations, &[glyph], palette_index)?
            .pop()
            .map(|(_, glyph)| glyph),
    )
}

/// Resolve the COLRv0 glyphs in one face using a single parse and paint setup.
///
/// Glyphs without a COLRv0 representation are omitted. This batch form keeps
/// the ordinary monochrome PDF path to one cheap table check per font rather
/// than reparsing or hashing the font once per used glyph.
pub fn colr_v0_glyphs(
    bytes: &[u8],
    index: FaceIndex,
    variations: &[VariationSetting],
    glyphs: &[SourceGlyphId],
    palette_index: u16,
) -> Result<Vec<(SourceGlyphId, ColrV0Glyph)>, FontError> {
    let font = font_ref(bytes, index)?;
    let glyph_count = u32::from(
        font.maxp()
            .map_err(|error| FontError::InvalidFont(error.to_string()))?
            .num_glyphs(),
    );
    if let Some(&glyph) = glyphs.iter().find(|glyph| glyph.0 >= glyph_count) {
        return Err(FontError::InvalidGlyph(glyph));
    }
    if !color_glyph_formats(&font).contains(&ColorGlyphFormat::ColrV0) {
        return Ok(Vec::new());
    }
    let color_glyphs = font.color_glyphs();
    if !glyphs.iter().any(|glyph| {
        color_glyphs
            .get_with_format(
                read_fonts::types::GlyphId::new(glyph.0),
                SkrifaColorGlyphFormat::ColrV0,
            )
            .is_some()
    }) {
        return Ok(Vec::new());
    }
    let palettes = font.color_palettes();
    let palette = palettes
        .get(palette_index)
        .ok_or(FontError::ColorPaletteMissing(palette_index))?;
    let colors = palette
        .colors()
        .iter()
        .map(|color| RgbaColor {
            red: color.red,
            green: color.green,
            blue: color.blue,
            alpha: color.alpha,
        })
        .collect::<Vec<_>>();
    let (instance, _) = shaper_instance(&font, variations)?;
    let mut resolved = Vec::new();
    for &glyph in glyphs {
        let Some(color_glyph) = color_glyphs.get_with_format(
            read_fonts::types::GlyphId::new(glyph.0),
            SkrifaColorGlyphFormat::ColrV0,
        ) else {
            continue;
        };
        let mut collector = ColrV0Collector {
            base: glyph,
            palette: &colors,
            layers: Vec::new(),
            unsupported: false,
        };
        color_glyph
            .paint(LocationRef::new(instance.coords()), &mut collector)
            .map_err(|error| FontError::InvalidFont(error.to_string()))?;
        if collector.unsupported || collector.layers.is_empty() {
            return Err(FontError::ColorPaintUnsupported(collector.base));
        }
        resolved.push((
            glyph,
            ColrV0Glyph {
                layers: collector.layers,
            },
        ));
    }
    Ok(resolved)
}

fn per_em(value: f32, units_per_em: u16) -> i32 {
    if units_per_em == 0 || !value.is_finite() {
        return 0;
    }
    (value * 1000.0 / f32::from(units_per_em)).round() as i32
}

/// Collect metrics required by a PDF CIDFont dictionary.
pub fn pdf_face_metrics(
    bytes: &[u8],
    index: FaceIndex,
    variations: &[VariationSetting],
    glyphs: &[SourceGlyphId],
) -> Result<PdfFaceMetrics, FontError> {
    let info = inspect_face(bytes, index)?;
    let font = font_ref(bytes, index)?;
    let (instance, _) = shaper_instance(&font, variations)?;
    let color_glyphs = font.color_glyphs();
    let non_colr_color_tables = [b"SVG ", b"sbix", b"CBDT"].into_iter().any(|table| {
        font.table_data(read_fonts::types::Tag::new(table))
            .is_some()
    });
    if let Some(&glyph) = glyphs.iter().find(|glyph| {
        let glyph_id = read_fonts::types::GlyphId::new(glyph.0);
        let has_color = color_glyphs.get(glyph_id).is_some();
        let has_colr_v0 = color_glyphs
            .get_with_format(glyph_id, SkrifaColorGlyphFormat::ColrV0)
            .is_some();
        non_colr_color_tables || (has_color && !has_colr_v0)
    }) {
        return Err(FontError::ColorGlyphUnsupported(glyph));
    }

    let metrics = font.metrics(Size::unscaled(), instance.coords());
    let glyph_metrics = font.glyph_metrics(Size::unscaled(), instance.coords());
    let mut source_glyphs = glyphs.to_vec();
    source_glyphs.sort_unstable();
    source_glyphs.dedup();
    let mut glyph_widths = Vec::with_capacity(source_glyphs.len());
    for source in source_glyphs {
        let width = glyph_metrics
            .advance_width(read_fonts::types::GlyphId::new(source.0))
            .ok_or(FontError::InvalidGlyph(source))?;
        glyph_widths.push(PdfGlyphWidth {
            source,
            width: per_em(width, info.units_per_em),
        });
    }

    let fallback_name = format!("aiTeX-{}", &info.identity.digest.to_hex()[..12]);
    let postscript_name = font
        .localized_strings(StringId::from(6))
        .english_or_first()
        .map(|name| name.to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback_name);
    let bounds = metrics.bounds.unwrap_or_default();
    Ok(PdfFaceMetrics {
        postscript_name,
        bounding_box: [
            per_em(bounds.x_min, info.units_per_em),
            per_em(bounds.y_min, info.units_per_em),
            per_em(bounds.x_max, info.units_per_em),
            per_em(bounds.y_max, info.units_per_em),
        ],
        ascent: per_em(metrics.ascent, info.units_per_em),
        descent: per_em(metrics.descent, info.units_per_em),
        cap_height: per_em(
            metrics.cap_height.unwrap_or(metrics.ascent),
            info.units_per_em,
        ),
        italic_angle: metrics.italic_angle,
        glyph_widths,
    })
}

fn variation_delta(
    value: Result<read_fonts::types::Fixed, read_fonts::ReadError>,
) -> Result<i32, FontError> {
    value
        .map(|value| value.to_f64().round() as i32)
        .map_err(|error| FontError::InvalidFont(error.to_string()))
}

/// Collect PDF vertical metrics for one concrete OpenType face instance.
///
/// `vhea`/`vmtx`, `VORG`, and `VVAR` are honored when present. Faces without
/// vertical tables receive the standard 1000-unit advance with a horizontal
/// half-width origin so a vertical layout experiment never needs to guess in
/// the PDF writer.
pub fn pdf_vertical_metrics(
    bytes: &[u8],
    index: FaceIndex,
    variations: &[VariationSetting],
    glyphs: &[SourceGlyphId],
) -> Result<PdfVerticalMetrics, FontError> {
    use read_fonts::types::{GlyphId, Tag};

    let info = inspect_face(bytes, index)?;
    let font = font_ref(bytes, index)?;
    let (instance, _) = shaper_instance(&font, variations)?;
    let coords = instance.coords();
    let glyph_metrics = font.glyph_metrics(Size::unscaled(), coords);
    let face_metrics = font.metrics(Size::unscaled(), coords);
    let vmtx = if font.table_data(Tag::new(b"vmtx")).is_some() {
        Some(
            font.vmtx()
                .map_err(|error| FontError::InvalidFont(error.to_string()))?,
        )
    } else {
        None
    };
    let vvar = if font.table_data(Tag::new(b"VVAR")).is_some() {
        Some(
            font.vvar()
                .map_err(|error| FontError::InvalidFont(error.to_string()))?,
        )
    } else {
        None
    };
    let vorg = if font.table_data(Tag::new(b"VORG")).is_some() {
        Some(
            font.vorg()
                .map_err(|error| FontError::InvalidFont(error.to_string()))?,
        )
    } else {
        None
    };
    let synthesized = vmtx.is_none();
    let mut source_glyphs = glyphs.to_vec();
    source_glyphs.sort_unstable();
    source_glyphs.dedup();
    let mut output = Vec::with_capacity(source_glyphs.len());
    for source in source_glyphs {
        if source.0 >= info.glyph_count {
            return Err(FontError::InvalidGlyph(source));
        }
        let gid = GlyphId::new(source.0);
        let width = glyph_metrics
            .advance_width(gid)
            .ok_or(FontError::InvalidGlyph(source))?;
        let (advance_height, origin_y) = if let Some(vmtx) = &vmtx {
            let advance_delta = if let Some(vvar) = &vvar {
                variation_delta(vvar.advance_height_delta(gid, coords))?
            } else {
                0
            };
            let origin_delta = if let Some(vvar) = &vvar {
                variation_delta(vvar.v_org_delta(gid, coords))?
            } else {
                0
            };
            let advance = i32::from(vmtx.advance(gid).ok_or_else(|| {
                FontError::InvalidFont(format!("vmtx has no advance for glyph {}", source.0))
            })?)
            .saturating_add(advance_delta);
            let origin = if let Some(vorg) = &vorg {
                i32::from(vorg.vertical_origin_y(gid)).saturating_add(origin_delta)
            } else {
                let top = glyph_metrics.bounds(gid).map_or(0.0, |bounds| bounds.y_max);
                let bearing_delta = if let Some(vvar) = &vvar {
                    variation_delta(vvar.tsb_delta(gid, coords))?
                } else {
                    0
                };
                (top.round() as i32)
                    .saturating_add(i32::from(vmtx.side_bearing(gid).ok_or_else(|| {
                        FontError::InvalidFont(format!(
                            "vmtx has no side bearing for glyph {}",
                            source.0
                        ))
                    })?))
                    .saturating_add(bearing_delta)
            };
            (advance, origin)
        } else {
            (
                i32::from(info.units_per_em),
                face_metrics.ascent.round() as i32,
            )
        };
        output.push(PdfGlyphVerticalMetric {
            source,
            advance_height: per_em(advance_height as f32, info.units_per_em),
            origin_x: per_em(width / 2.0, info.units_per_em),
            origin_y: per_em(origin_y as f32, info.units_per_em),
        });
    }
    Ok(PdfVerticalMetrics {
        glyphs: output,
        synthesized,
    })
}

fn table_i16(table: &[u8], offset: usize) -> Option<i16> {
    let bytes = table.get(offset..offset + 2)?;
    Some(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn table_u16(table: &[u8], offset: usize) -> Option<u16> {
    let bytes = table.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn parse_math_constants(table: &[u8], units_per_em: u16) -> Option<MathConstants> {
    if table_u16(table, 0)? != 1 {
        return None;
    }
    let constants = usize::from(table_u16(table, 4)?);
    let value_record = |index: usize| table_i16(table, constants + 8 + index * 4);
    Some(MathConstants {
        units_per_em,
        script_percent_scale_down: table_i16(table, constants)?,
        script_script_percent_scale_down: table_i16(table, constants + 2)?,
        delimited_sub_formula_min_height: table_u16(table, constants + 4)?,
        display_operator_min_height: table_u16(table, constants + 6)?,
        axis_height: value_record(1)?,
        fraction_rule_thickness: value_record(34)?,
        radical_rule_thickness: value_record(47)?,
    })
}

/// Read the initial OpenType MATH contract used by a future Unicode math-list
/// builder. A face without MATH returns `Ok(None)`; a malformed MATH table is
/// rejected instead of silently acquiring classic TeX constants.
pub fn math_constants(bytes: &[u8], index: FaceIndex) -> Result<Option<MathConstants>, FontError> {
    let font = font_ref(bytes, index)?;
    let Some(table) = font.table_data(read_fonts::types::Tag::new(b"MATH")) else {
        return Ok(None);
    };
    let units_per_em = font
        .head()
        .map_err(|error| FontError::InvalidFont(error.to_string()))?
        .units_per_em();
    parse_math_constants(table.as_bytes(), units_per_em)
        .map(Some)
        .ok_or_else(|| FontError::InvalidFont("malformed or unsupported MATH table".into()))
}

/// Collect an instantiated outline into a TrueType-compatible quadratic path.
/// CFF/CFF2 cubics are approximated to a quarter font unit, well below one
/// device pixel at ordinary PDF text sizes.
struct QuadraticPen {
    path: BezPath,
    current: Point,
    contour_start: Point,
    has_current: bool,
}

impl Default for QuadraticPen {
    fn default() -> Self {
        Self {
            path: BezPath::new(),
            current: Point::ZERO,
            contour_start: Point::ZERO,
            has_current: false,
        }
    }
}

impl OutlinePen for QuadraticPen {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = Point::new(f64::from(x), f64::from(y));
        self.path.move_to(point);
        self.current = point;
        self.contour_start = point;
        self.has_current = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = Point::new(f64::from(x), f64::from(y));
        self.path.line_to(point);
        self.current = point;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let control = Point::new(f64::from(cx0), f64::from(cy0));
        let point = Point::new(f64::from(x), f64::from(y));
        self.path.quad_to(control, point);
        self.current = point;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let end = Point::new(f64::from(x), f64::from(y));
        let cubic = CubicBez::new(
            self.current,
            Point::new(f64::from(cx0), f64::from(cy0)),
            Point::new(f64::from(cx1), f64::from(cy1)),
            end,
        );
        for (_, _, quadratic) in cubic.to_quads(0.25) {
            self.path.quad_to(quadratic.p1, quadratic.p2);
        }
        self.current = end;
    }

    fn close(&mut self) {
        if self.has_current {
            self.path.close_path();
            self.current = self.contour_start;
        }
    }
}

fn be_u16(value: f32) -> u16 {
    value.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

fn be_i16(value: f32) -> i16 {
    value
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn replace_i16(bytes: &mut [u8], offset: usize, value: i16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn replace_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn static_instance_table_is_safe(tag: read_fonts::types::Tag) -> bool {
    ![
        *b"CFF ", *b"CFF2", *b"glyf", *b"loca", *b"head", *b"hhea", *b"hmtx", *b"maxp", *b"fvar",
        *b"avar", *b"gvar", *b"cvar", *b"HVAR", *b"VVAR", *b"MVAR", *b"STAT", *b"GDEF", *b"GPOS",
        *b"GSUB", *b"fpgm", *b"prep", *b"cvt ", *b"COLR", *b"CPAL", *b"SVG ", *b"sbix", *b"CBDT",
        *b"CBLC", *b"BASE", *b"VORG", *b"DSIG",
    ]
    .contains(&tag.to_be_bytes())
}

/// Materialize exactly the outlines and horizontal metrics consumed by PDF.
/// Source GIDs are retained so the page layer's already-shaped glyph stream
/// remains valid. The result has no variation tables and always uses `glyf`,
/// giving TrueType-variable and CFF2 sources the same embedding contract.
fn instantiate_static_subset(
    source: &FontRef<'_>,
    subset_bytes: &[u8],
    coords: &[read_fonts::types::F2Dot14],
    requested_glyphs: &[SourceGlyphId],
) -> Result<Vec<u8>, FontError> {
    let subset = FontRef::new(subset_bytes)
        .map_err(|error| FontError::SubsetFailed(format!("invalid intermediate: {error}")))?;
    let max_gid = requested_glyphs
        .iter()
        .map(|glyph| glyph.0)
        .max()
        .unwrap_or(0);
    let glyph_count = u16::try_from(max_gid.saturating_add(1))
        .map_err(|_| FontError::InvalidGlyph(SourceGlyphId(max_gid)))?;
    let retained = requested_glyphs
        .iter()
        .map(|glyph| glyph.0)
        .chain(std::iter::once(0))
        .collect::<BTreeSet<_>>();
    let outlines = source.outline_glyphs();
    let glyph_metrics = source.glyph_metrics(Size::unscaled(), coords);
    let mut glyf_loca = GlyfLocaBuilder::new();
    let mut hmtx = Vec::with_capacity(usize::from(glyph_count) * 4);
    let mut font_bbox: Option<Bbox> = None;
    let mut advance_max = 0u16;
    let mut min_lsb = i16::MAX;
    let mut min_rsb = i16::MAX;
    let mut max_extent = i16::MIN;
    let mut max_points = 0u16;
    let mut max_contours = 0u16;

    for raw_gid in 0..u32::from(glyph_count) {
        let gid = read_fonts::types::GlyphId::new(raw_gid);
        let glyph = if retained.contains(&raw_gid) {
            if let Some(outline) = outlines.get(gid) {
                let mut pen = QuadraticPen::default();
                outline
                    .draw(DrawSettings::unhinted(Size::unscaled(), coords), &mut pen)
                    .map_err(|error| {
                        FontError::SubsetFailed(format!(
                            "cannot instantiate glyph {raw_gid}: {error}"
                        ))
                    })?;
                if pen.path.elements().is_empty() {
                    Glyph::Empty
                } else {
                    Glyph::Simple(SimpleGlyph::from_bezpath(&pen.path).map_err(|error| {
                        FontError::SubsetFailed(format!(
                            "cannot encode instantiated glyph {raw_gid}: {error:?}"
                        ))
                    })?)
                }
            } else {
                Glyph::Empty
            }
        } else {
            Glyph::Empty
        };
        let bbox = glyph.bbox();
        if let Glyph::Simple(simple) = &glyph {
            max_contours =
                max_contours.max(simple.contours.len().min(usize::from(u16::MAX)) as u16);
            let points = simple
                .contours
                .iter()
                .map(|contour| contour.len())
                .sum::<usize>();
            max_points = max_points.max(points.min(usize::from(u16::MAX)) as u16);
        }
        if let Some(bbox) = bbox {
            font_bbox = Some(font_bbox.map_or(bbox, |current| current.union(bbox)));
        }
        glyf_loca.add_glyph(&glyph).map_err(|error| {
            FontError::SubsetFailed(format!("cannot serialize glyph {raw_gid}: {error}"))
        })?;

        let advance = glyph_metrics.advance_width(gid).map(be_u16).unwrap_or(0);
        let lsb = glyph_metrics
            .left_side_bearing(gid)
            .map(be_i16)
            .unwrap_or(0);
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&lsb.to_be_bytes());
        advance_max = advance_max.max(advance);
        min_lsb = min_lsb.min(lsb);
        if let Some(bbox) = bbox {
            let extent = i32::from(lsb) + i32::from(bbox.x_max) - i32::from(bbox.x_min);
            max_extent =
                max_extent.max(extent.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
            let rsb = i32::from(advance) - extent;
            min_rsb = min_rsb.min(rsb.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
        }
    }
    if min_lsb == i16::MAX {
        min_lsb = 0;
    }
    if min_rsb == i16::MAX {
        min_rsb = 0;
    }
    if max_extent == i16::MIN {
        max_extent = 0;
    }

    let (glyf, loca, loca_format) = glyf_loca.build();
    let mut head = subset
        .table_data(read_fonts::types::Tag::new(b"head"))
        .ok_or_else(|| FontError::SubsetFailed("intermediate has no head table".into()))?
        .as_bytes()
        .to_vec();
    let mut hhea = subset
        .table_data(read_fonts::types::Tag::new(b"hhea"))
        .ok_or_else(|| FontError::SubsetFailed("intermediate has no hhea table".into()))?
        .as_bytes()
        .to_vec();
    if head.len() < 54 || hhea.len() < 36 {
        return Err(FontError::SubsetFailed(
            "intermediate has a truncated metrics table".into(),
        ));
    }
    let bbox = font_bbox.unwrap_or_default();
    replace_i16(&mut head, 36, bbox.x_min);
    replace_i16(&mut head, 38, bbox.y_min);
    replace_i16(&mut head, 40, bbox.x_max);
    replace_i16(&mut head, 42, bbox.y_max);
    replace_i16(
        &mut head,
        50,
        if matches!(loca_format, write_fonts::tables::loca::LocaFormat::Long) {
            1
        } else {
            0
        },
    );
    replace_u16(&mut hhea, 10, advance_max);
    replace_i16(&mut hhea, 12, min_lsb);
    replace_i16(&mut hhea, 14, min_rsb);
    replace_i16(&mut hhea, 16, max_extent);
    replace_u16(&mut hhea, 34, glyph_count);

    let mut maxp = vec![0, 1, 0, 0];
    maxp.extend_from_slice(&glyph_count.to_be_bytes());
    // Conservative maxima for a decomposed, unhinted, all-simple-glyph font.
    maxp.extend_from_slice(&[0; 24]);
    replace_u16(&mut maxp, 6, max_points);
    replace_u16(&mut maxp, 8, max_contours);
    replace_u16(&mut maxp, 14, 1); // maxZones must be 1 or 2.

    let mut builder = FontBuilder::new();
    for record in subset.table_directory().table_records() {
        let table_tag = record.tag();
        if static_instance_table_is_safe(table_tag) {
            if let Some(data) = subset.table_data(table_tag) {
                builder.add_raw(table_tag, data.as_bytes().to_vec());
            }
        }
    }
    builder
        .add_raw(read_fonts::types::Tag::new(b"head"), head)
        .add_raw(read_fonts::types::Tag::new(b"hhea"), hhea)
        .add_raw(read_fonts::types::Tag::new(b"hmtx"), hmtx)
        .add_raw(read_fonts::types::Tag::new(b"maxp"), maxp)
        .add_table(&glyf)
        .map_err(|error| FontError::SubsetFailed(error.to_string()))?
        .add_table(&loca)
        .map_err(|error| FontError::SubsetFailed(error.to_string()))?;
    let bytes = builder.build();
    let output = FontRef::new(&bytes)
        .map_err(|error| FontError::SubsetFailed(format!("invalid static instance: {error}")))?;
    if output
        .table_data(read_fonts::types::Tag::new(b"fvar"))
        .is_some()
        || output
            .table_data(read_fonts::types::Tag::new(b"CFF2"))
            .is_some()
    {
        return Err(FontError::SubsetFailed(
            "static instance retained variable tables".into(),
        ));
    }
    Ok(bytes)
}

fn direction(direction: TextDirection) -> Direction {
    match direction {
        TextDirection::LeftToRight => Direction::LeftToRight,
        TextDirection::RightToLeft => Direction::RightToLeft,
        TextDirection::TopToBottom => Direction::TopToBottom,
        TextDirection::BottomToTop => Direction::BottomToTop,
    }
}

fn shaper_instance(
    font: &FontRef<'_>,
    settings: &[VariationSetting],
) -> Result<(ShaperInstance, Vec<NormalizedVariation>), FontError> {
    let variations = settings
        .iter()
        .map(|setting| {
            if !setting.value.is_finite() {
                Err(FontError::InvalidVariation(setting.tag))
            } else {
                Ok(Variation {
                    tag: tag(setting.tag),
                    value: setting.value,
                })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let instance = ShaperInstance::from_variations(font, variations);
    let normalized = if instance.coords().is_empty() {
        Vec::new()
    } else {
        font.axes()
            .iter()
            .zip(instance.coords())
            .map(|(axis, coordinate)| NormalizedVariation {
                tag: own_tag(axis.tag()),
                coordinate: coordinate.to_bits(),
            })
            .collect()
    };
    Ok((instance, normalized))
}

/// Resolve a content-addressed face and user coordinates to canonical axis
/// order and normalized F2Dot14 values.
pub fn font_instance_identity(
    bytes: &[u8],
    index: FaceIndex,
    settings: &[VariationSetting],
) -> Result<FontInstanceIdentity, FontError> {
    let info = inspect_face(bytes, index)?;
    let font = font_ref(bytes, index)?;
    let (_, variations) = shaper_instance(&font, settings)?;
    Ok(FontInstanceIdentity {
        face: info.identity,
        variations,
    })
}

fn validate_range(text: &str, range: &std::ops::Range<u32>) -> bool {
    if range.end == u32::MAX {
        return range.start == 0;
    }
    let Ok(start) = usize::try_from(range.start) else {
        return false;
    };
    let Ok(end) = usize::try_from(range.end) else {
        return false;
    };
    start <= end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)
}

/// Shape one already-itemized Unicode run with HarfRust.
pub fn shape(bytes: &[u8], request: &ShapeRequest<'_>) -> Result<ShapedRun, FontError> {
    let text_len = u32::try_from(request.text.len()).map_err(|_| FontError::TextTooLong)?;
    let face_info = inspect_face(bytes, request.face_index)?;
    let font = font_ref(bytes, request.face_index)?;
    let script = Script::from_iso15924_tag(tag(request.script))
        .ok_or(FontError::InvalidScript(request.script))?;
    let language = request
        .language
        .map(|language| {
            Language::new(language).ok_or_else(|| FontError::InvalidLanguage(language.to_string()))
        })
        .transpose()?;

    let mut features = Vec::with_capacity(request.features.len());
    for feature in request.features {
        if !validate_range(request.text, &feature.byte_range) {
            return Err(FontError::InvalidFeatureRange(feature.byte_range.clone()));
        }
        let upstream = if feature.byte_range.end == u32::MAX {
            Feature::new(tag(feature.tag), feature.value, ..)
        } else {
            Feature::new(
                tag(feature.tag),
                feature.value,
                feature.byte_range.start as usize..feature.byte_range.end as usize,
            )
        };
        features.push(upstream);
    }
    let (instance, normalized_variations) = shaper_instance(&font, request.variations)?;

    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(request.text);
    buffer.set_direction(direction(request.direction));
    buffer.set_script(script);
    if let Some(language) = language.clone() {
        buffer.set_language(language);
    }
    buffer.set_flags(
        BufferFlags::BEGINNING_OF_TEXT
            | BufferFlags::END_OF_TEXT
            | BufferFlags::PRODUCE_UNSAFE_TO_CONCAT,
    );

    let shaper_data = ShaperData::new(&font);
    let shaper = shaper_data.shaper(&font).instance(Some(&instance)).build();
    let glyph_buffer = shaper.shape(buffer, ShapeOptions::new().features(&features));

    let cluster_starts = glyph_buffer
        .glyph_infos()
        .iter()
        .map(|info| info.cluster)
        .collect::<BTreeSet<_>>();
    if let Some(&bad) = cluster_starts
        .iter()
        .find(|&&offset| offset > text_len || !request.text.is_char_boundary(offset as usize))
    {
        return Err(FontError::InvalidFeatureRange(bad..bad));
    }
    let starts = cluster_starts.into_iter().collect::<Vec<_>>();
    let mut cluster_ids = BTreeMap::new();
    let mut clusters = Vec::with_capacity(starts.len());
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(text_len);
        let id = ClusterId(position as u32);
        cluster_ids.insert(start, id);
        clusters.push(TextCluster {
            byte_range: start..end,
            text: request.text[start as usize..end as usize].to_string(),
        });
    }

    let mut glyphs = Vec::with_capacity(glyph_buffer.len());
    for (info, position) in glyph_buffer
        .glyph_infos()
        .iter()
        .zip(glyph_buffer.glyph_positions())
    {
        if info.glyph_id == 0 && request.missing_glyphs == MissingGlyphPolicy::Reject {
            return Err(FontError::MissingGlyph {
                byte_offset: info.cluster,
            });
        }
        glyphs.push(PositionedGlyph {
            source_glyph: SourceGlyphId(info.glyph_id),
            cluster: cluster_ids[&info.cluster],
            x_advance: position.x_advance,
            y_advance: position.y_advance,
            x_offset: position.x_offset,
            y_offset: position.y_offset,
            unsafe_to_break: info.flags().is_unsafe_to_break(),
            unsafe_to_concat: info.flags().is_unsafe_to_concat(),
            is_notdef: info.glyph_id == 0,
        });
    }

    // HarfRust's fontations backend currently returns horizontal advances for
    // some vertical faces even though it performs vertical substitution. Keep
    // shaping order and clusters, but source the missing advance axis from the
    // same vmtx/VVAR contract used by the PDF writer. This also supplies the
    // documented one-em fallback for faces without vertical tables.
    if matches!(
        request.direction,
        TextDirection::TopToBottom | TextDirection::BottomToTop
    ) && glyphs.iter().all(|glyph| glyph.y_advance == 0)
    {
        let source = glyphs
            .iter()
            .map(|glyph| glyph.source_glyph)
            .collect::<Vec<_>>();
        let vertical =
            pdf_vertical_metrics(bytes, request.face_index, request.variations, &source)?;
        let advances = vertical
            .glyphs
            .into_iter()
            .map(|metric| (metric.source, metric.advance_height))
            .collect::<BTreeMap<_, _>>();
        let sign = if request.direction == TextDirection::TopToBottom {
            -1
        } else {
            1
        };
        for glyph in &mut glyphs {
            let per_em = advances.get(&glyph.source_glyph).copied().unwrap_or(1000);
            glyph.x_advance = 0;
            glyph.y_advance = sign * per_em * i32::from(face_info.units_per_em) / 1000;
            glyph.x_offset = 0;
            glyph.y_offset = 0;
        }
    }

    Ok(ShapedRun {
        instance: FontInstanceIdentity {
            face: face_info.identity,
            variations: normalized_variations,
        },
        implementation: HARFRUST_SHAPER_VERSION,
        direction: request.direction,
        script: request.script,
        language: language.map(|language| language.as_str().to_owned()),
        units_per_em: face_info.units_per_em,
        glyphs,
        clusters,
    })
}

fn scale_to_sp(value: i32, size: aitex_page::Sp, units_per_em: u16) -> aitex_page::Sp {
    if units_per_em == 0 {
        return aitex_page::Sp(0);
    }
    let numerator = i128::from(value) * i128::from(size.0);
    let denominator = i128::from(units_per_em);
    let rounded = if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        (numerator - denominator / 2) / denominator
    };
    aitex_page::Sp(i64::try_from(rounded).unwrap_or_else(|_| {
        if rounded.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    }))
}

/// Convert shaped font-unit positions to final TeX scaled-point page events.
///
/// HarfRust uses positive y upward, while TeX page coordinates grow downward;
/// this boundary performs that sign conversion once.
pub fn positioned_run(
    run: &ShapedRun,
    font: aitex_page::FontInstanceId,
    size: aitex_page::Sp,
    origin_x: aitex_page::Sp,
    origin_y: aitex_page::Sp,
) -> aitex_page::GlyphRun {
    let direction = match run.direction {
        TextDirection::LeftToRight => aitex_page::TextDirection::LeftToRight,
        TextDirection::RightToLeft => aitex_page::TextDirection::RightToLeft,
        TextDirection::TopToBottom => aitex_page::TextDirection::TopToBottom,
        TextDirection::BottomToTop => aitex_page::TextDirection::BottomToTop,
    };
    aitex_page::GlyphRun::new(
        font,
        size,
        origin_x,
        origin_y,
        direction,
        run.glyphs
            .iter()
            .map(|glyph| aitex_page::PositionedGlyph {
                source_gid: aitex_page::SourceGlyphId(glyph.source_glyph.0),
                cluster: aitex_page::ClusterId(glyph.cluster.0),
                x_advance: scale_to_sp(glyph.x_advance, size, run.units_per_em),
                y_advance: scale_to_sp(-glyph.y_advance, size, run.units_per_em),
                x_offset: scale_to_sp(glyph.x_offset, size, run.units_per_em),
                y_offset: scale_to_sp(-glyph.y_offset, size, run.units_per_em),
                is_notdef: glyph.is_notdef,
            })
            .collect(),
        run.clusters
            .iter()
            .map(|cluster| aitex_page::TextCluster {
                byte_range: cluster.byte_range.clone(),
                text: cluster.text.clone(),
            })
            .collect(),
    )
    .with_language(run.language.clone())
}

/// A replaceable bytes-in/bytes-out subsetter.
pub trait FontSubsetter {
    fn implementation(&self) -> &'static str;
    fn subset(
        &self,
        bytes: &[u8],
        request: &SubsetRequest<'_>,
    ) -> Result<PreparedSubset, FontError>;
}

/// Fontations' Skera subsetter, currently retaining source GIDs.
///
/// Retained GIDs give the PDF layer a stable and explicit identity map. A
/// future dense mode needs Skera to expose its complete closure mapping; that
/// limitation is isolated behind [`FontSubsetter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SkeraSubsetter;

impl FontSubsetter for SkeraSubsetter {
    fn implementation(&self) -> &'static str {
        SKERA_SUBSETTER_VERSION
    }

    fn subset(
        &self,
        bytes: &[u8],
        request: &SubsetRequest<'_>,
    ) -> Result<PreparedSubset, FontError> {
        let info = inspect_face(bytes, request.face.index)?;
        match info.embedding {
            EmbeddingPermission::Restricted => return Err(FontError::EmbeddingRestricted),
            EmbeddingPermission::Installable
            | EmbeddingPermission::Editable
            | EmbeddingPermission::PreviewAndPrint => {}
        }
        if info.bitmap_embedding_only {
            return Err(FontError::BitmapEmbeddingOnly);
        }
        if info.format == FontFormat::Unsupported {
            return Err(FontError::UnsupportedOutline(info.format));
        }
        if info.identity != request.face {
            return Err(FontError::FaceMismatch {
                expected: request.face,
                actual: info.identity,
            });
        }

        let mut source_glyphs = request.glyphs.to_vec();
        source_glyphs.sort_unstable();
        source_glyphs.dedup();
        if let Some(&glyph) = source_glyphs
            .iter()
            .find(|glyph| glyph.0 >= info.glyph_count)
        {
            return Err(FontError::InvalidGlyph(glyph));
        }
        let mapping = source_glyphs
            .iter()
            .copied()
            .map(|source| GlyphMapping {
                source,
                subset: SubsetGlyphId(source.0),
            })
            .collect::<Vec<_>>();
        let mut unicodes = request
            .unicodes
            .iter()
            .copied()
            .map(u32::from)
            .collect::<Vec<_>>();
        unicodes.sort_unstable();
        unicodes.dedup();
        let font = font_ref(bytes, request.face.index)?;
        let (instance, variations) = shaper_instance(&font, request.variations)?;
        let needs_static_instance = info.format == FontFormat::Cff2
            || variations.iter().any(|variation| variation.coordinate != 0);
        let identity = SubsetIdentity {
            face: info.identity,
            glyphs: source_glyphs.clone(),
            unicodes: unicodes.clone(),
            variations,
            embedding: info.embedding,
            policy: "respect-fstype-v1",
            implementation: self.implementation(),
        };

        if info.no_subsetting {
            if needs_static_instance {
                return Err(FontError::VariableInstanceUnsupported);
            }
            if request.face.index.0 != 0 || FontRef::new(bytes).is_err() {
                return Err(FontError::FullFontFromCollection);
            }
            return Ok(PreparedSubset {
                identity,
                bytes: bytes.to_vec(),
                glyph_mapping: mapping,
                was_subset: false,
            });
        }

        let input_gids = source_glyphs
            .iter()
            .map(|glyph| read_fonts::types::GlyphId::new(glyph.0))
            .collect::<IntSet<_>>();
        // Skera 0.6.0 indexes the first item-variation-data subtable while
        // planning HVAR/VVAR output. Reject the malformed empty-store case at
        // our untrusted-font boundary instead of allowing its bounds panic to
        // escape the compiler.
        validate_skera_metric_variations(&font)?;
        let input_unicodes = unicodes.iter().copied().collect::<IntSet<_>>();
        let drop_tables = IntSet::empty();
        let mut layout_scripts = IntSet::empty();
        layout_scripts.invert();
        let layout_features = skera::DEFAULT_LAYOUT_FEATURES
            .iter()
            .copied()
            .collect::<IntSet<_>>();
        let mut name_ids = IntSet::<NameId>::empty();
        name_ids.insert_range(NameId::from(0)..=NameId::from(6));
        for name_id in [13, 14, 16, 17, 25] {
            name_ids.insert(NameId::from(name_id));
        }
        let mut name_languages = IntSet::<u16>::empty();
        name_languages.invert();
        let flags = skera::SubsetFlags::SUBSET_FLAGS_RETAIN_GIDS
            | skera::SubsetFlags::SUBSET_FLAGS_PASSTHROUGH_UNRECOGNIZED
            | skera::SubsetFlags::SUBSET_FLAGS_NOTDEF_OUTLINE;
        // Planning traverses the same untrusted layout tables as emission and
        // can panic before `subset_font` is reached. Keep the complete Skera
        // operation inside the containment boundary.
        let mut subset = contain_subsetter_panic(|| {
            let plan = skera::Plan::new(
                &input_gids,
                &input_unicodes,
                &font,
                flags,
                &drop_tables,
                &layout_scripts,
                &layout_features,
                &name_ids,
                &name_languages,
            );
            skera::subset_font(&font, &plan)
        })?
        .map_err(|error| FontError::SubsetFailed(error.to_string()))?;
        if needs_static_instance {
            subset = instantiate_static_subset(&font, &subset, instance.coords(), &source_glyphs)?;
        }
        FontRef::new(&subset)
            .map_err(|error| FontError::SubsetFailed(format!("invalid output: {error}")))?;

        Ok(PreparedSubset {
            identity,
            bytes: subset,
            glyph_mapping: mapping,
            was_subset: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATIN: &[u8] = include_bytes!("../tests/fonts/NotoSans-Latin-Test.ttf");
    const CJK: &[u8] = include_bytes!("../tests/fonts/NotoSansCJKsc-Test.otf");

    #[test]
    fn third_party_subsetter_panics_are_errors_at_the_font_boundary() {
        assert!(matches!(
            contain_subsetter_panic(|| panic!("malformed font")),
            Err(FontError::SubsetFailed(message))
                if message == "subsetter rejected malformed font data"
        ));
    }

    fn with_extra_tables(extra: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let font = FontRef::new(LATIN).expect("fixture font");
        let mut builder = FontBuilder::new();
        for record in font.table_directory().table_records() {
            let tag = record.tag();
            let data = font.table_data(tag).expect("listed table");
            builder.add_raw(tag, data.as_bytes().to_vec());
        }
        for (tag, bytes) in extra {
            builder.add_raw(read_fonts::types::Tag::new(tag), bytes.clone());
        }
        builder.build()
    }

    #[test]
    fn embedding_bits_choose_the_most_restrictive_permission() {
        assert_eq!(
            embedding_permissions(0),
            (EmbeddingPermission::Installable, false, false)
        );
        assert_eq!(
            embedding_permissions(0x0108),
            (EmbeddingPermission::Editable, true, false)
        );
        assert_eq!(
            embedding_permissions(0x020c),
            (EmbeddingPermission::PreviewAndPrint, false, true)
        );
        assert_eq!(
            embedding_permissions(0x000e).0,
            EmbeddingPermission::Restricted
        );
    }

    #[test]
    fn feature_ranges_follow_utf8_boundaries() {
        assert!(validate_range("aé", &(0..3)));
        assert!(!validate_range("aé", &(0..2)));
        assert!(!validate_range("aé", &(0..4)));
        assert!(validate_range("aé", &(0..u32::MAX)));
    }

    #[test]
    fn color_capabilities_name_the_required_rendering_adapter() {
        let bytes = with_extra_tables(&[
            (*b"COLR", vec![0, 1]),
            (*b"CPAL", vec![0, 0]),
            (*b"SVG ", vec![0, 0]),
            (*b"CBDT", vec![0, 0]),
        ]);
        let info = inspect_face(&bytes, FaceIndex(0)).expect("inspect color capabilities");
        assert!(info.has_color_glyphs);
        assert_eq!(
            info.color_glyph_formats,
            vec![
                ColorGlyphFormat::ColrV1,
                ColorGlyphFormat::Svg,
                ColorGlyphFormat::Cbdt,
            ]
        );
        assert_eq!(
            info.color_glyph_formats[0].render_mode(),
            crate::ColorRenderMode::Vector
        );
        assert_eq!(
            info.color_glyph_formats[2].render_mode(),
            crate::ColorRenderMode::Bitmap
        );
    }

    #[test]
    fn vertical_metrics_use_the_locked_cjk_tables() {
        let metrics = pdf_vertical_metrics(CJK, FaceIndex(0), &[], &[SourceGlyphId(12)])
            .expect("vertical metrics");
        assert!(!metrics.synthesized);
        assert_eq!(metrics.glyphs[0].advance_height, 1000);
        assert_eq!(metrics.glyphs[0].origin_x, 500);
        assert_eq!(metrics.glyphs[0].origin_y, 880);
    }

    #[test]
    fn math_constants_cross_the_font_boundary_without_upstream_types() {
        let constants_at = 10usize;
        let mut math = vec![0; constants_at + 8 + 48 * 4];
        math[0..2].copy_from_slice(&1u16.to_be_bytes());
        math[4..6].copy_from_slice(&(constants_at as u16).to_be_bytes());
        math[constants_at..constants_at + 2].copy_from_slice(&80i16.to_be_bytes());
        math[constants_at + 2..constants_at + 4].copy_from_slice(&60i16.to_be_bytes());
        math[constants_at + 4..constants_at + 6].copy_from_slice(&1200u16.to_be_bytes());
        math[constants_at + 6..constants_at + 8].copy_from_slice(&1500u16.to_be_bytes());
        math[constants_at + 12..constants_at + 14].copy_from_slice(&250i16.to_be_bytes());
        math[constants_at + 144..constants_at + 146].copy_from_slice(&40i16.to_be_bytes());
        math[constants_at + 196..constants_at + 198].copy_from_slice(&50i16.to_be_bytes());
        let bytes = with_extra_tables(&[(*b"MATH", math)]);
        let info = inspect_face(&bytes, FaceIndex(0)).expect("inspect math face");
        assert!(info.has_math);
        let constants = math_constants(&bytes, FaceIndex(0))
            .expect("parse MATH")
            .expect("MATH present");
        assert_eq!(constants.units_per_em, 1000);
        assert_eq!(constants.script_percent_scale_down, 80);
        assert_eq!(constants.script_script_percent_scale_down, 60);
        assert_eq!(constants.delimited_sub_formula_min_height, 1200);
        assert_eq!(constants.display_operator_min_height, 1500);
        assert_eq!(constants.axis_height, 250);
        assert_eq!(constants.fraction_rule_thickness, 40);
        assert_eq!(constants.radical_rule_thickness, 50);
    }
}
