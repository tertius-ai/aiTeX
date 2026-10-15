/// Render a parsed DVI document as a PDF.
///
/// `resolve` is asked for each font the DVI defines; returning `None` leaves
/// the font unembedded, which still positions its characters correctly and is
/// what happens for a font this machine does not have.
pub fn write(
    doc: &dvi::Document,
    page_width: f64,
    page_height: f64,
    resolve: impl FnMut(&str, i64) -> Option<FontSource>,
) -> Vec<u8> {
    write_with_options(doc, page_width, page_height, PdfOptions::default(), resolve)
}

pub fn write_with_options(
    doc: &dvi::Document,
    page_width: f64,
    page_height: f64,
    options: PdfOptions,
    resolve: impl FnMut(&str, i64) -> Option<FontSource>,
) -> Vec<u8> {
    write_with_assets_options(doc, page_width, page_height, options, resolve, |_, _| {
        Option::<AssetSource>::None
    })
}

/// Render a DVI document and resolve files named by its graphics specials.
/// Keeping the resolver at this boundary leaves path lookup and I/O in the CLI
/// while the driver remains a deterministic bytes-to-bytes transformation.
pub fn write_with_assets(
    doc: &dvi::Document,
    default_page_width: f64,
    default_page_height: f64,
    resolve: impl FnMut(&str, i64) -> Option<FontSource>,
    resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
) -> Vec<u8> {
    write_with_assets_options(
        doc,
        default_page_width,
        default_page_height,
        PdfOptions::default(),
        resolve,
        resolve_asset,
    )
}

const PAGE_STREAM_BATCH_BYTES: usize = 8 * 1024 * 1024;
const PAGE_STREAM_BATCH_COUNT: usize = 256;
const TYPE1_FACE_CACHE_BYTES: usize = 32 * 1024 * 1024;
const TYPE1_FACE_CACHE_ENTRIES: usize = 128;
// A PDF page starts with DeviceGray for stroking and filling, while the same
// content imported as a Form inherits both color spaces from its caller.
// Recreate the page initial state before every imported-page invocation so an
// otherwise valid one-component `sc` cannot be interpreted as RGB red.
const IMPORTED_PAGE_INITIAL_COLOR: &str = "0 G 0 g";
const PDFTEX_IGNORE_POSTSCRIPT_SPECIAL: &[u8] = b"aitex:pdftex-ignore-postscript";
const PDFTEX_EXPLICIT_DVIPDFMX_SPECIAL: &[u8] = b"aitex:pdftex-explicit-dvipdfmx";
const PDFTEX_IGNORE_NEXT_DRIVER_GRAPHIC_SPECIAL: &str =
    "aitex:pdftex-ignore-next-driver-graphic";

type Type1FaceCacheKey = (ClassicFaceIdentity, bool, Vec<String>);

#[derive(Default)]
struct Type1FaceCache {
    entries: std::collections::VecDeque<(Type1FaceCacheKey, Option<PreparedType1>, usize)>,
    bytes: usize,
}

impl Type1FaceCache {
    fn get_or_prepare(
        &mut self,
        face: &ClassicFontFace,
        glyphs: Option<&std::collections::BTreeSet<String>>,
        subset: bool,
    ) -> Option<PreparedType1> {
        let key = (
            face.identity.clone(),
            subset,
            if subset {
                glyphs
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            },
        );
        if let Some(index) = self.entries.iter().position(|entry| entry.0 == key) {
            let entry = self.entries.remove(index)?;
            let result = entry.1.clone();
            self.entries.push_back(entry);
            return result;
        }
        let prepared = if subset {
            glyphs.map_or_else(
                || {
                    prepare_type1_program(
                        &face.pfb,
                        &face.postscript_name,
                        face.slant,
                        face.extend,
                    )
                },
                |glyphs| {
                    prepare_type1_subset(
                        &face.pfb,
                        &face.postscript_name,
                        face.slant,
                        face.extend,
                        glyphs,
                    )
                },
            )
        } else {
            prepare_type1_program(
                &face.pfb,
                &face.postscript_name,
                face.slant,
                face.extend,
            )
        };
        let retained = prepared.as_ref().map_or(0, PreparedType1::retained_bytes);
        if retained <= TYPE1_FACE_CACHE_BYTES {
            while self.entries.len() >= TYPE1_FACE_CACHE_ENTRIES
                || self.bytes.saturating_add(retained) > TYPE1_FACE_CACHE_BYTES
            {
                let Some(expired) = self.entries.pop_front() else {
                    break;
                };
                self.bytes = self.bytes.saturating_sub(expired.2);
            }
            self.bytes += retained;
            self.entries.push_back((key, prepared.clone(), retained));
        }
        prepared
    }
}

thread_local! {
    static TYPE1_FACE_CACHE: std::cell::RefCell<Type1FaceCache> =
        std::cell::RefCell::new(Type1FaceCache::default());
}

struct PendingPageCache {
    key: Vec<u8>,
    destinations: Vec<CachedPageDestination>,
}

enum PendingPageContent {
    Raw(Vec<u8>),
    Encoded(std::sync::Arc<[u8]>),
}

struct PendingPageStream {
    id: usize,
    content: PendingPageContent,
    raw_len: usize,
    cache: Option<PendingPageCache>,
}

fn flush_page_streams(objects: &mut Objects, pending: &mut Vec<PendingPageStream>) {
    if pending.is_empty() {
        return;
    }
    let contents = pending
        .iter()
        .filter_map(|page| match &page.content {
            PendingPageContent::Raw(content) => Some(content.as_slice()),
            PendingPageContent::Encoded(_) => None,
        })
        .collect::<Vec<_>>();
    let streams = objects.stream_objects(b"<<", &contents);
    let mut streams = streams.into_iter();
    for page in std::mem::take(pending) {
        match page.content {
            PendingPageContent::Raw(_) => {
                let stream = streams.next().expect("one result per raw page stream");
                objects.write(page.id, &stream);
                if let Some(cache) = page.cache {
                    PAGE_CACHE.with(|page_cache| {
                        page_cache.borrow_mut().insert(
                            cache.key,
                            stream,
                            page.raw_len,
                            cache.destinations,
                        );
                    });
                }
            }
            PendingPageContent::Encoded(stream) => objects.write(page.id, &stream),
        }
    }
    debug_assert!(streams.next().is_none());
}

pub fn write_with_assets_options(
    doc: &dvi::Document,
    default_page_width: f64,
    default_page_height: f64,
    options: PdfOptions,
    mut resolve: impl FnMut(&str, i64) -> Option<FontSource>,
    resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
) -> Vec<u8> {
    let embedding = if options.subset_type1 {
        ClassicEmbeddingPolicy::Subset
    } else {
        ClassicEmbeddingPolicy::Full
    };
    write_with_classic_assets_options(
        doc,
        default_page_width,
        default_page_height,
        options,
        |name, at| {
            resolve(name, at).map(|source| source.into_classic_use(embedding))
        },
        resolve_asset,
    )
}

/// Render a DVI document from explicitly separated classic faces and uses.
pub fn write_with_classic_assets_options(
    doc: &dvi::Document,
    default_page_width: f64,
    default_page_height: f64,
    options: PdfOptions,
    resolve: impl FnMut(&str, i64) -> Option<ClassicFontUse>,
    resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
) -> Vec<u8> {
    write_with_assets_options_impl(
        doc,
        default_page_width,
        default_page_height,
        options,
        resolve,
        resolve_asset,
        None,
    )
    .expect("classic DVI output cannot contain positioned-font failures")
}

/// Render classic DVI material and native OpenType page events into one PDF.
///
/// Classic fonts, math, graphics, color, links, and outlines retain the
/// established DVI semantics. `GlyphRun` events are overlaid by the in-process
/// Type 0 writer and never acquire a lossy DVI representation.
pub fn write_hybrid_with_assets_options(
    doc: &dvi::Document,
    positioned: &aitex_page::Document,
    default_page_width: f64,
    default_page_height: f64,
    options: PdfOptions,
    mut resolve: impl FnMut(&str, i64) -> Option<FontSource>,
    resolve_positioned: &mut dyn FnMut(
        aitex_page::FontInstanceId,
    ) -> Option<OpenTypeFontSource>,
    resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
) -> Result<Vec<u8>, PositionedPdfError> {
    let embedding = if options.subset_type1 {
        ClassicEmbeddingPolicy::Subset
    } else {
        ClassicEmbeddingPolicy::Full
    };
    write_hybrid_with_classic_assets_options(
        doc,
        positioned,
        default_page_width,
        default_page_height,
        options,
        |name, at| {
            resolve(name, at).map(|source| source.into_classic_use(embedding))
        },
        resolve_positioned,
        resolve_asset,
    )
}

/// Hybrid writer variant using the face/use classic-font boundary.
pub fn write_hybrid_with_classic_assets_options(
    doc: &dvi::Document,
    positioned: &aitex_page::Document,
    default_page_width: f64,
    default_page_height: f64,
    options: PdfOptions,
    resolve: impl FnMut(&str, i64) -> Option<ClassicFontUse>,
    resolve_positioned: &mut dyn FnMut(
        aitex_page::FontInstanceId,
    ) -> Option<OpenTypeFontSource>,
    resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
) -> Result<Vec<u8>, PositionedPdfError> {
    write_with_assets_options_impl(
        doc,
        default_page_width,
        default_page_height,
        options,
        resolve,
        resolve_asset,
        Some((positioned, resolve_positioned)),
    )
}

fn normalized_eps_crop(
    page_bbox: [f64; 4],
    natural_bbox: Option<[f64; 4]>,
    special_bbox: [f64; 4],
) -> [f64; 4] {
    // epstopdf normalizes the converted page to the integer DSC BoundingBox,
    // and graphics writes that resulting full-page box into the special.  An
    // EPS HiResBoundingBox can legitimately have slightly different extents;
    // treating the already-normalized identity viewport as a crop in that
    // coordinate system expands the imported Form by a fraction of a point.
    // Preserve the exact PDF page for this common no-trim route.  Genuine
    // author viewports still take the coordinate transform below.
    if special_bbox
        .iter()
        .zip(page_bbox)
        .all(|(special, page)| (special - page).abs() <= 1e-6)
    {
        return page_bbox;
    }
    // On the pdfTeX-compatible route, graphics.sty sees the normalized PDF
    // page through `\pdfximage`: pdfTeX converts its bp extent to a TeX
    // dimension and `\Gin@defaultbp` converts that dimension back to a number.
    // The resulting full-page box is a few thousandths of a bp smaller than
    // the actual PDF page.  It is still the identity viewport, not an author
    // trim.  Preserve the adjusted viewport so the image scale matches
    // pdfTeX, but do not map it through an unrelated EPS HiResBoundingBox.
    let pdftex_page_bbox = pdftex_pdf_xbb_bbox(page_bbox);
    if special_bbox
        .iter()
        .zip(pdftex_page_bbox)
        .all(|(special, page)| (special - page).abs() <= 1e-5)
    {
        return special_bbox;
    }
    let Some(natural) = natural_bbox else {
        return page_bbox;
    };
    let natural_width = natural[2] - natural[0];
    let natural_height = natural[3] - natural[1];
    if natural_width.abs() <= f64::MIN_POSITIVE || natural_height.abs() <= f64::MIN_POSITIVE {
        return page_bbox;
    }
    let page_width = page_bbox[2] - page_bbox[0];
    let page_height = page_bbox[3] - page_bbox[1];
    [
        page_bbox[0] + (special_bbox[0] - natural[0]) * page_width / natural_width,
        page_bbox[1] + (special_bbox[1] - natural[1]) * page_height / natural_height,
        page_bbox[0] + (special_bbox[2] - natural[0]) * page_width / natural_width,
        page_bbox[1] + (special_bbox[3] - natural[1]) * page_height / natural_height,
    ]
}

fn pdftex_pdf_xbb_bbox([llx, lly, urx, ury]: [f64; 4]) -> [f64; 4] {
    let converted_extent = |extent: f64| {
        let raw_sp = ((extent as f32 as f64) * (6_578_176.0 / 100.0)).round() as i128;
        let xbb_number_sp = raw_sp * 65_291 / 65_536;
        xbb_number_sp as f64 / 65_536.0
    };
    [
        llx,
        lly,
        llx + converted_extent(urx - llx),
        lly + converted_extent(ury - lly),
    ]
}

fn normalized_eps_form_and_viewport(
    page_bbox: [f64; 4],
    natural_bbox: Option<[f64; 4]>,
    special_bbox: [f64; 4],
    clip: bool,
) -> ([f64; 4], [f64; 4]) {
    let viewport = normalized_eps_crop(page_bbox, natural_bbox, special_bbox);
    // `trim`/`viewport` changes the graphic's reference box. It does not clip
    // content unless graphicx also writes the bare `clip` flag in its PSfile
    // special. Keep the complete converted page as the Form in that common
    // no-clip case, but continue to position and scale it from the viewport.
    let form = if clip { viewport } else { page_bbox };
    (form, viewport)
}

#[derive(Default)]
struct RasterObjectCache {
    by_source: std::collections::BTreeMap<(String, u32), (usize, usize, usize)>,
    by_content: std::collections::BTreeMap<
        (usize, u64, u64, u64, usize, usize),
        (usize, usize, usize),
    >,
    masks: std::collections::BTreeMap<AlphaFingerprint, usize>,
}

/// The physical image extent pdfTeX uses in its inner image matrix.
///
/// `graphicx` obtains a PNG bounding box through TeX fixed-point arithmetic;
/// its decimal special is therefore a few thousandths of a bp below the
/// physical size.  pdfTeX embeds the same image from its integer pixel count
/// and rounded DPI, then applies the five-decimal scale computed by
/// `graphicx`.  Collapsing those two matrices against the approximate special
/// bbox changes the final scale by a few millionths, which is visible on dense
/// scientific plots.  Recover the integral DPI only when the bbox makes that
/// provenance unambiguous; arbitrary raster and converted-asset dimensions
/// retain their supplied physical extent.
fn pdftex_raster_matrix_extent(pixels: usize, physical_extent: f64) -> f64 {
    if pixels == 0 || !physical_extent.is_finite() || physical_extent <= 0.0 {
        return physical_extent;
    }
    let inferred_dpi = pixels as f64 * 72.0 / physical_extent;
    let rounded_dpi = inferred_dpi.round();
    if rounded_dpi >= 1.0 && (inferred_dpi - rounded_dpi).abs() <= 0.01 {
        pixels as f64 * 72.0 / rounded_dpi
    } else {
        physical_extent
    }
}

/// Scale an imported image to the requested box carried by an unaccompanied
/// driver special. The qualified pdfTeX route carries graphicx's actual scale
/// in a private companion; raw dvipdfmx and PGF specials retain this fallback.
fn pdftex_image_scale(requested_bp: f64, natural_bp: f64) -> f64 {
    requested_bp / natural_bp
}

/// Font size retained by pdfTeX after spelling the size relative to 100bp at
/// six decimal places. The `/Widths` array is divided by this adjusted value,
/// not by TeX's original at-size. A difference of only a few scaled points is
/// enough to move a one-decimal width across its rounding boundary (CMR7's
/// 569.447... becomes 569.5, for example).
fn pdftex_font_size(at: i64) -> i64 {
    const ONE_HUNDRED_BP: i64 = 6_578_176;
    const DECIMAL_SCALE: i64 = 1_000_000;

    let sign = if at < 0 { -1 } else { 1 };
    let magnitude = at.abs();
    let mut remainder = magnitude % ONE_HUNDRED_BP;
    for _ in 0..6 {
        remainder = (10 * remainder) % ONE_HUNDRED_BP;
    }
    if 2 * remainder >= ONE_HUNDRED_BP {
        remainder -= ONE_HUNDRED_BP;
    }
    sign * (magnitude - remainder / DECIMAL_SCALE)
}

/// Embed one normalized raster and write its placement into whichever PDF
/// content stream owns the DVI point. The same routine serves page content and
/// captured Form XObjects; keeping those paths together prevents `\pdfxform`
/// from silently dropping images used by packages such as `animate`.
fn embed_raster_image(
    output: &mut Objects,
    cache: &mut RasterObjectCache,
    image: &ImageSpecial,
    asset: &AssetSource,
    pdftex_graphic_scale: Option<(f64, f64)>,
    x: f64,
    y: f64,
    content: &mut Vec<u8>,
    xobjects: &mut Vec<(String, usize)>,
) -> bool {
    let raster_key = (image.name.clone(), image.page);
    let raster_object = cache.by_source.get(&raster_key).copied().or_else(|| {
        let raster = parse_raster(&asset.bytes)
            .or_else(|| asset.raster.as_deref().and_then(parse_raster))?;
        let content_key = raster_fingerprint(&raster);
        let value = cache
            .by_content
            .get(&content_key)
            .copied()
            .unwrap_or_else(|| {
                let value = (
                    write_raster_objects_with_masks(output, &raster, &mut cache.masks),
                    raster.width,
                    raster.height,
                );
                cache.by_content.insert(content_key, value);
                value
            });
        cache.by_source.insert(raster_key, value);
        Some(value)
    });
    let Some((image_id, raster_width, raster_height)) = raster_object else {
        return false;
    };
    let natural_bbox = asset.raster_bbox.or(image.bbox).unwrap_or([
        0.0,
        0.0,
        raster_width as f64,
        raster_height as f64,
    ]);
    let viewport = image.bbox.unwrap_or(natural_bbox);

    // The dvipdfmx bbox is a crop in the raster's physical big-point
    // coordinate system. `width`/`height` describe that cropped box, while
    // the PDF Image XObject still contains the full pixel grid.
    let intrinsic_width = (viewport[2] - viewport[0]).abs().max(0.001);
    let intrinsic_height = (viewport[3] - viewport[1]).abs().max(0.001);
    let natural_width = (natural_bbox[2] - natural_bbox[0]).abs().max(0.001);
    let natural_height = (natural_bbox[3] - natural_bbox[1]).abs().max(0.001);
    let (scale_x, scale_y) = pdftex_graphic_scale.unwrap_or_else(|| {
        match (image.width, image.height) {
            (Some(w), Some(h)) => (
                pdftex_image_scale(w, intrinsic_width),
                pdftex_image_scale(h, intrinsic_height),
            ),
            (Some(w), None) => {
                let scale = pdftex_image_scale(w, intrinsic_width);
                (scale, scale)
            }
            (None, Some(h)) => {
                let scale = pdftex_image_scale(h, intrinsic_height);
                (scale, scale)
            }
            (None, None) => (1.0, 1.0),
        }
    });
    let scale_x = encoded_pdf_value(scale_x, 5);
    let scale_y = encoded_pdf_value(scale_y, 5);
    let tx = x - scale_x * (viewport[0] - natural_bbox[0]);
    let ty = y - scale_y * (viewport[1] - natural_bbox[1]);
    let clip_width = scale_x * intrinsic_width;
    let clip_height = scale_y * intrinsic_height;
    let painted_width =
        scale_x * pdftex_raster_matrix_extent(raster_width, natural_width);
    let painted_height =
        scale_y * pdftex_raster_matrix_extent(raster_height, natural_height);
    let resource = format!("Im{image_id}");
    let clipping = if image.clip {
        format!("{x:.4} {y:.4} {clip_width:.8} {clip_height:.8} re W n ")
    } else {
        String::new()
    };
    content.extend_from_slice(
        format!(
            "q {clipping}{painted_width:.8} 0 0 {painted_height:.8} {tx:.3} {ty:.3} cm \
             /{resource} Do Q\n"
        )
        .as_bytes(),
    );
    xobjects.push((resource, image_id));
    true
}

fn write_with_assets_options_impl(
    doc: &dvi::Document,
    default_page_width: f64,
    default_page_height: f64,
    options: PdfOptions,
    mut resolve: impl FnMut(&str, i64) -> Option<ClassicFontUse>,
    mut resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
    mut positioned: Option<(
        &aitex_page::Document,
        &mut dyn FnMut(aitex_page::FontInstanceId) -> Option<OpenTypeFontSource>,
    )>,
) -> Result<Vec<u8>, PositionedPdfError> {
    // pdfTeX accepts PostScript specials emitted by an explicitly selected
    // dvips graphics backend, but its PDF shipout reports and ignores them.
    // aiTeX's source adapter records that backend choice in the private page
    // stream so the otherwise useful EPS compatibility route can distinguish
    // it from the dvipdfmx transport selected on aiTeX's behalf.
    let ignore_postscript = doc.pages.iter().any(|page| {
        page.items.iter().any(|item| {
            matches!(
                item,
                dvi::Item::Special { bytes, .. }
                    if bytes.as_slice() == PDFTEX_IGNORE_POSTSCRIPT_SPECIAL
            )
        })
    });
    // An author-selected dvipdfmx graphics backend under pdfTeX is not the
    // same thing as aiTeX's private dvipdfmx transport: pdfTeX ignores the
    // driver's page-size and graphics specials. The source adapter marks that
    // otherwise invisible choice so this host boundary can retain stock
    // pdfLaTeX semantics.
    let explicit_dvipdfmx = doc.pages.iter().any(|page| {
        page.items.iter().any(|item| {
            matches!(
                item,
                dvi::Item::Special { bytes, .. }
                    if bytes.as_slice() == PDFTEX_EXPLICIT_DVIPDFMX_SPECIAL
            )
        })
    });
    if let Some((positioned, _)) = positioned.as_ref()
        && positioned.pages.len() != doc.pages.len()
    {
        return Err(PositionedPdfError::HybridPageCount {
            dvi: doc.pages.len(),
            positioned: positioned.pages.len(),
        });
    }
    let mut o = Objects::with_options(options);
    let catalog = o.reserve();
    let pages = o.reserve();
    // dvipdfmx's PDF-special namespace is document-wide. PGF uses it to build
    // reusable shading/mask forms and dictionaries that later pages refer to.
    let mut named_ids = std::collections::BTreeMap::<String, usize>::new();
    let mut named_objects = std::collections::BTreeMap::<String, NamedPdfObject>::new();
    let mut resource_objects = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    let mut catalog_fragments = Vec::<Vec<u8>>::new();
    let mut pages_fragments = Vec::<Vec<u8>>::new();
    let mut names_fragments = Vec::<Vec<u8>>::new();
    let mut pending_named_annotations = Vec::<(usize, Vec<u8>, NamedObjectPosition)>::new();

    // dvips records the stock size in a special because DVI's preamble has no
    // page dimensions.  LaTeX's 614.295pt × 794.97pt is exactly letter-size
    // 612bp × 792bp after the TeX-point conversion.
    // A dvips `papersize` declaration changes the device state; ordinary
    // geometry emits it once near the start of the document, not once on every
    // page. Preserve that size until a later page explicitly changes it. The
    // old page-local lookup rendered page one as A4 and silently reverted page
    // two onward to Letter even though our DVI was byte-for-byte correct.
    let mut declared_page_size = None;
    let mut latex_page_size = None;
    let page_sizes: Vec<(f64, f64)> = doc
        .pages
        .iter()
        .map(|page| {
            let mut private_size = None;
            let mut private_explicit = false;
            let mut local_declared_page_size = None;
            for item in &page.items {
                let dvi::Item::Special { bytes, .. } = item else {
                    continue;
                };
                let text = String::from_utf8_lossy(bytes);
                if let Some(size) = private_page_size(&text) {
                    private_size = Some(size);
                }
                if let Some(size) = private_latex_page_size(&text) {
                    // Some legacy classes (notably revtex4) leave the logical
                    // LaTeX paper dimensions at zero even though pdfTeX's
                    // device page dimensions retain their ordinary Letter
                    // defaults.  Zero is an unspecified logical size, not a
                    // request for a zero-area PDF page.
                    if size.0.is_finite() && size.0 > 0.0 && size.1.is_finite() && size.1 > 0.0 {
                        latex_page_size = Some(size);
                    }
                }
                if text.trim() == "aitex:page-size-explicit" {
                    private_explicit = true;
                }
                if !explicit_dvipdfmx
                    && !ignore_postscript
                    && let Some(size) = special_page_size(&text)
                {
                    declared_page_size = Some(size);
                    local_declared_page_size = Some(size);
                }
            }
            private_explicit
                .then_some(private_size)
                .flatten()
                .or(local_declared_page_size)
                .or(latex_page_size)
                .or(declared_page_size)
                .or(private_size)
                .unwrap_or((default_page_width, default_page_height))
        })
        .collect();
    let page_origins = doc
        .pages
        .iter()
        .map(|page| {
            page.items
                .iter()
                .filter_map(|item| match item {
                    dvi::Item::Special { bytes, .. } => {
                        private_page_origin(&String::from_utf8_lossy(bytes))
                    }
                    _ => None,
                })
                .next_back()
                .unwrap_or((72.0, 72.0))
        })
        .collect::<Vec<_>>();

    let mut used_codes = std::collections::BTreeMap::<u32, std::collections::BTreeSet<u8>>::new();
    for item in doc.pages.iter().flat_map(|page| &page.items) {
        if let dvi::Item::ClassicGlyph { font, code, .. } = item {
            used_codes
                .entry(font.0)
                .or_default()
                .insert((*code & 0xff) as u8);
        }
    }

    // Resolve uses once, then aggregate their glyph names by physical face.
    // Point size and TeX encoding deliberately do not split this closure.
    let mut classic_font_uses = std::collections::BTreeMap::<u32, ClassicFontUse>::new();
    for (&number, definition) in &doc.fonts {
        if used_codes.contains_key(&number)
            && let Some(font_use) = resolve(&definition.name, definition.at)
        {
            classic_font_uses.insert(number, font_use);
        }
    }
    let mut face_sources = std::collections::BTreeMap::<
        ClassicFaceIdentity,
        std::sync::Arc<ClassicFontFace>,
    >::new();
    let mut face_glyphs = std::collections::BTreeMap::<
        ClassicFaceIdentity,
        std::collections::BTreeSet<String>,
    >::new();
    let mut supplied_faces =
        std::collections::BTreeMap::<ClassicFaceIdentity, PreparedType1>::new();
    for (&number, font_use) in &classic_font_uses {
        let (face, supplied) = match &font_use.program {
            ClassicFontProgram::Type1(face) => (face, None),
            ClassicFontProgram::Prepared { face, prepared } => (face, Some(prepared)),
            ClassicFontProgram::Pk { .. }
            | ClassicFontProgram::TrueType { .. }
            | ClassicFontProgram::Resident { .. } => continue,
        };
        face_sources
            .entry(face.identity.clone())
            .or_insert_with(|| std::sync::Arc::clone(face));
        if let Some(prepared) = supplied {
            supplied_faces
                .entry(face.identity.clone())
                .or_insert_with(|| prepared.clone());
        }
        let Some(encoding) = font_use.encoding.as_deref() else {
            continue;
        };
        let glyphs = face_glyphs.entry(face.identity.clone()).or_default();
        for code in used_codes.get(&number).into_iter().flatten() {
            if let Some(name) = encoding.get(usize::from(*code)) {
                glyphs.insert(name.clone());
            }
        }
    }
    let prepared_faces = face_sources
        .iter()
        .map(|(identity, face)| {
            let supplied = supplied_faces
                .get(identity)
                .filter(|prepared| {
                    face_glyphs
                        .get(identity)
                        .is_none_or(|glyphs| prepared.covers(glyphs))
                })
                .cloned();
            let prepared = supplied.or_else(|| {
                TYPE1_FACE_CACHE.with(|cache| {
                    cache.borrow_mut().get_or_prepare(
                        face,
                        face_glyphs.get(identity),
                        options.subset_type1,
                    )
                })
            });
            (identity.clone(), prepared)
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    // Fonts first: every page's resource dictionary names them.
    let mut font_objs: std::collections::BTreeMap<u32, PdfFont> = std::collections::BTreeMap::new();
    let mut positioned_font_objs = std::collections::BTreeMap::new();
    if let Some((document, resolver)) = positioned.as_mut() {
        let usage = collect_positioned_usage_with_policy(document, true)?;
        for (font_id, font_usage) in &usage {
            let source = (*resolver)(*font_id).ok_or(PositionedPdfError::MissingFont(*font_id))?;
            positioned_font_objs.insert(
                *font_id,
                write_positioned_font(&mut o, *font_id, font_usage, &source)?,
            );
        }
    }
    let mut font_program_objects = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    let mut font_descriptor_objects = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    let mut font_dictionary_objects = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    let mut to_unicode_objects = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    for (&num, def) in &doc.fonts {
        let Some(used) = used_codes.get(&num) else {
            continue;
        };
        let Some(src) = classic_font_uses.get(&num) else {
            continue;
        };
        let at = def.at.max(1);
        let dvi_advances: Vec<i64> = (0..256)
            .map(|c| src.widths.get(c).copied().unwrap_or(0))
            .collect();
        if let ClassicFontProgram::Pk {
            bytes,
            postscript_name,
            ..
        } = &src.program
            && let Some(pk) = crate::pk::parse(bytes)
        {
            // PK lookup is for a 600dpi output device. kpathsea may return a
            // nearby magstep bitmap (for example a 602dpi file for a 600dpi
            // request), but pdfTeX still paints every bitmap pixel at the
            // requested device-pixel size. Deriving the matrix from PK's
            // `hppp` instead shrinks that font by 600/602 and moves every
            // glyph edge on the page.
            let at_pt = def.at as f64 / 65536.0;
            let pixel_matrix = (72.27 / (600.0 * at_pt) * 100_000.0).floor() / 100_000.0;
            if pixel_matrix.is_finite() && pixel_matrix > 0.0 {
                let font_id = o.reserve();
                let type3_advances = (0..256usize)
                    .map(|code| {
                        src.widths.get(code).copied().unwrap_or(0) as f64 / at as f64 / pixel_matrix
                    })
                    .collect::<Vec<_>>();
                let notdef_id = o.reserve();
                o.write_stream(notdef_id, b"<<", b"0 0 d0\n");
                let mut char_procs = format!("/.notdef {notdef_id} 0 R");
                let first = usize::from(*used.first().unwrap_or(&0));
                let last = usize::from(*used.last().unwrap_or(&0));
                let mut differences = first.to_string();
                let mut bbox = [0.0f64; 4];
                let mut have_bbox = false;
                for code in first..=last {
                    if used.contains(&(code as u8))
                        && let Some(glyph) = pk.glyphs.get(&(code as u8))
                    {
                        let id = o.reserve();
                        let charproc = type3_charproc(glyph, type3_advances[code]);
                        o.write_stream(id, b"<<", &charproc);
                        char_procs.push_str(&format!(" /g{code:02X} {id} 0 R"));
                        differences.push_str(&format!(" /g{code:02X}"));
                        let left = f64::from(-glyph.x_offset);
                        let bottom = f64::from(glyph.y_offset - glyph.height as i32 + 1);
                        let right = left + glyph.width as f64 + 1.0;
                        let top = bottom + glyph.height as f64;
                        if have_bbox {
                            bbox[0] = bbox[0].min(left);
                            bbox[1] = bbox[1].min(bottom);
                            bbox[2] = bbox[2].max(right);
                            bbox[3] = bbox[3].max(top);
                        } else {
                            bbox = [left, bottom, right, top];
                            have_bbox = true;
                        }
                    } else {
                        differences.push_str(" /.notdef");
                    }
                }
                let encoded_advances = type3_advances
                    .iter()
                    .map(|advance| {
                        pdf_number(*advance, 2)
                            .parse::<f64>()
                            .unwrap_or(*advance)
                            * pixel_matrix
                            * at as f64
                    })
                    .collect::<Vec<_>>();
                let widths = type3_advances[first..=last]
                    .iter()
                    .map(|advance| pdf_number(*advance, 2))
                    .collect::<Vec<_>>()
                    .join(" ");
                let to_unicode_id = to_unicode_cmap(&src.to_unicode).map(|cmap| {
                    o.write_interned_stream(&mut to_unicode_objects, b"<<", &cmap)
                });
                let to_unicode = to_unicode_id
                    .map(|id| format!(" /ToUnicode {id} 0 R"))
                    .unwrap_or_default();
                o.write(
                    font_id,
                    format!(
                        "<< /Type /Font /Subtype /Type3 /Name /{} \
                         /FontBBox [{:.4} {:.4} {:.4} {:.4}] \
                         /FontMatrix [{pixel_matrix:.5} 0 0 {pixel_matrix:.5} 0 0] \
                         /CharProcs << {char_procs} >> \
                         /Encoding << /Type /Encoding /Differences [{differences}] >> \
                         /FirstChar {first} /LastChar {last} /Widths [{widths}] \
                         /Resources << >>{to_unicode} >>",
                        pdf_name(postscript_name),
                        bbox[0],
                        bbox[1],
                        bbox[2],
                        bbox[3]
                    )
                    .as_bytes(),
                );
                font_objs.insert(
                    num,
                    PdfFont {
                        id: font_id,
                        advances: dvi_advances,
                        encoded_advances,
                        at,
                        extend: 1.0,
                        slant: 0.0,
                    },
                );
                continue;
            }
        }
        let (postscript_name, slant, extend, bbox, prepared, true_type) = match &src.program {
            ClassicFontProgram::Type1(face) | ClassicFontProgram::Prepared { face, .. } => (
                face.postscript_name.as_str(),
                face.slant,
                face.extend,
                face.bbox,
                prepared_faces.get(&face.identity).cloned().flatten(),
                None,
            ),
            ClassicFontProgram::Resident {
                postscript_name,
                slant,
                extend,
                bbox,
            } => (
                postscript_name.as_str(),
                *slant,
                *extend,
                *bbox,
                None,
                None,
            ),
            ClassicFontProgram::Pk {
                postscript_name,
                bbox,
                ..
            } => (postscript_name.as_str(), 0.0, 1.0, *bbox, None, None),
            ClassicFontProgram::TrueType {
                bytes,
                postscript_name,
                slant,
                extend,
                bbox,
            } => (
                postscript_name.as_str(),
                *slant,
                *extend,
                *bbox,
                None,
                Some(bytes.as_ref()),
            ),
        };
        let slant = if slant.is_finite() { slant } else { 0.0 };
        let extend = if extend.is_finite() && extend > 0.0 {
            extend
        } else {
            1.0
        };
        let embedded_transform = prepared
            .as_ref()
            .is_some_and(PreparedType1::embedded_transform);
        let file_id = prepared.as_ref().map(|prepared| {
            let [length1, length2, length3] = prepared.lengths();
            let dictionary = format!(
                "<< /Length1 {} /Length2 {} /Length3 {}",
                length1,
                length2,
                length3,
            );
            o.write_interned_stream(
                &mut font_program_objects,
                dictionary.as_bytes(),
                prepared.program(),
            )
        }).or_else(|| {
            true_type.map(|bytes| {
                let dictionary = format!("<< /Length1 {}", bytes.len());
                o.write_interned_stream(
                    &mut font_program_objects,
                    dictionary.as_bytes(),
                    bytes,
                )
            })
        });
        let base_name = pdf_name(
            prepared
                .as_ref()
                .map_or(postscript_name, PreparedType1::embedded_name),
        );

        let [x0, y0, x1, y1] = bbox;
        let flags = if src.encoding.is_some() { 32 } else { 4 };
        let italic_angle = -slant.atan().to_degrees();
        let mut desc = format!(
            "<< /Type /FontDescriptor /FontName /{} /Flags {flags} \
             /FontBBox [{x0} {y0} {x1} {y1}] /ItalicAngle {italic_angle:.4} /Ascent {y1} \
             /Descent {y0} /CapHeight {y1} /StemV 80",
            base_name
        );
        if let Some(f) = file_id {
            desc.push_str(&format!(
                " /{} {f} 0 R",
                if true_type.is_some() {
                    "FontFile2"
                } else {
                    "FontFile"
                }
            ));
        }
        desc.push_str(" >>");
        let desc_id = o.write_interned(&mut font_descriptor_objects, desc.into_bytes());

        // pdfTeX records classic Widths to one decimal place. It first rounds
        // the font size relative to 100bp at six decimal places and divides by
        // that retained scaled-point value. Using the original TeX at-size is
        // observably different at half-decimal boundaries in CMR7/CMMI7 and
        // lets the error accumulate across dense formulae.
        // The TFM advances below remain exact scaled-point values, and run
        // bookkeeping repairs the remaining decimal-width error at the next
        // explicit DVI position.
        // `ExtendFont` transforms the base outline and its advance. The TFM
        // already contains that effective advance, while the PDF text matrix
        // below applies the transform. Divide the PDF width by the extension
        // so the resulting advance still lands at the DVI coordinate.
        let text_extend = if embedded_transform { 1.0 } else { extend };
        let pdf_font_size = pdftex_font_size(at).max(1);
        let encoded_widths = dvi_advances
            .iter()
            .map(|advance| {
                let value =
                    *advance as f64 * 1000.0 / pdf_font_size as f64 / text_extend;
                let rendered = pdf_number(value, 1);
                let represented = rendered.parse::<f64>().unwrap_or(value);
                (
                    rendered,
                    represented * at as f64 / 1000.0 * text_extend,
                )
            })
            .collect::<Vec<_>>();
        let first = usize::from(*used.first().unwrap_or(&0));
        let last = usize::from(*used.last().unwrap_or(&0));
        let widths = encoded_widths[first..=last]
            .iter()
            .map(|(rendered, _)| rendered.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let encoding = src
            .encoding
            .as_deref()
            .map(encoding_dictionary)
            .unwrap_or_default();
        let to_unicode_id = to_unicode_cmap(&src.to_unicode).map(|cmap| {
            o.write_interned_stream(&mut to_unicode_objects, b"<<", &cmap)
        });
        let to_unicode = to_unicode_id
            .map(|id| format!(" /ToUnicode {id} 0 R"))
            .unwrap_or_default();
        let font_dictionary = format!(
                "<< /Type /Font /Subtype /{} /BaseFont /{} /FirstChar {first} /LastChar {last} \
                 /Widths [{widths}] /FontDescriptor {desc_id} 0 R{encoding}{to_unicode} >>",
                if true_type.is_some() { "TrueType" } else { "Type1" },
                base_name
            )
            .into_bytes();
        let font_id = o.write_interned(&mut font_dictionary_objects, font_dictionary);
        font_objs.insert(
            num,
            PdfFont {
                id: font_id,
                advances: dvi_advances,
                encoded_advances: encoded_widths
                    .into_iter()
                    .map(|(_, advance)| advance)
                    .collect(),
                at,
                extend: if embedded_transform { 1.0 } else { extend },
                slant: if embedded_transform { 0.0 } else { slant },
            },
        );
    }

    let page_objects = (0..doc.pages.len())
        .map(|_| (o.reserve(), o.reserve()))
        .collect::<Vec<_>>();
    let page_tree = {
        let page_ids = page_objects
            .iter()
            .map(|(page, _)| *page)
            .collect::<Vec<_>>();
        PageTreePlan::reserve(&mut o, pages, &page_ids)
    };
    for (page_index, (page_id, _)) in page_objects.iter().enumerate() {
        named_ids.insert(format!("page{}", page_index + 1), *page_id);
    }
    // `\pdfpageref` shares pdfTeX's logical object-number namespace with
    // `\pdfobj`. Resolve every page alias before processing any page because
    // an annotation on page one may refer to an alias whose marker happens to
    // ride on a later shipped page.
    for page in &doc.pages {
        for item in &page.items {
            let dvi::Item::Special { bytes, .. } = item else {
                continue;
            };
            let text = String::from_utf8_lossy(bytes);
            if let Some((name, page_number)) = pdf_page_ref_special(text.trim())
                && let Some((page_id, _)) = page_objects.get(page_number - 1)
            {
                named_ids.insert(name.to_string(), *page_id);
            }
        }
    }
    let mut destinations = std::collections::BTreeMap::<Vec<u8>, Destination>::new();
    let mut outlines = Vec::<Outline>::new();
    let mut doc_info = std::collections::BTreeMap::<&'static str, Vec<u8>>::new();
    // PDF XObjects are document-wide resources. A paper may place the same
    // multi-megabyte image on many pages; writing a fresh object at every `Do`
    // made both the output and the in-memory object table grow by that factor.
    // Raster objects are independent of placement/cropping. Imported PDF forms
    // also include their source crop in the key because it becomes the form's
    // `/BBox`.
    let mut raster_cache = RasterObjectCache::default();
    let mut imported_form_ids = std::collections::BTreeMap::<(String, u32, [u64; 4]), usize>::new();
    let mut imported_form_content_ids =
        std::collections::BTreeMap::<((usize, u64, u64), [u64; 4]), usize>::new();
    let mut eps_form_content_ids =
        std::collections::BTreeMap::<((usize, u64, u64), [u64; 4]), usize>::new();
    let mut pending_page_streams = Vec::<PendingPageStream>::new();
    let mut pending_page_bytes = 0usize;
    let mut use_outlines = false;
    // dvips/dvipdfmx color specials form a document stream, not a fresh stack
    // on every page. A color group may begin on one page and end on another.
    let mut persistent_colors = vec![Color::BLACK];
    for (page_index, (page, (page_width, page_height))) in doc
        .pages
        .iter()
        .zip(page_sizes.iter().copied())
        .enumerate()
    {
        let (page_origin_x, page_origin_y) = page_origins[page_index];
        let positioned_page = positioned
            .as_ref()
            .and_then(|(document, _)| document.pages.get(page_index));
        let has_positioned_text = positioned_page.is_some_and(|page| {
            page.items
                .iter()
                .any(|item| matches!(item, aitex_page::Item::GlyphRun(_)))
        });
        let (page_id, content_id) = page_objects[page_index];
        let page_parent = page_tree.page_parents[page_index];
        let page_rotation = page.items.iter().find_map(|item| match item {
            dvi::Item::Special { bytes, .. } => {
                special_page_rotation(&String::from_utf8_lossy(bytes))
            }
            _ => None,
        });
        let cache_key = (!has_positioned_text && persistent_colors == [Color::BLACK])
            .then_some(())
            .and_then(|()| page
            .items
            .iter()
            .all(|item| match item {
                dvi::Item::Special { bytes, .. } => cacheable_page_special(bytes),
                dvi::Item::GlyphRun(_) => false,
                _ => true,
            })
            .then(|| page_cache_key(page, page_width, page_height, &font_objs, options)));
        if let Some((stream, raw_len, cached_destinations)) = cache_key
            .as_deref()
            .and_then(|key| PAGE_CACHE.with(|cache| cache.borrow_mut().get(key)))
        {
            for destination in cached_destinations {
                destinations.entry(destination.name).or_insert(Destination {
                    page: page_id,
                    x: destination.x,
                    y: destination.y,
                    view: destination.view,
                });
            }
            let resources = page_font_resources(page, &font_objs);
            let resource_id = o.write_interned(
                &mut resource_objects,
                format!("<< /Font << {resources} >> /XObject <<  >> >>").into_bytes(),
            );
            let rotation = page_rotation
                .filter(|angle| *angle != 0)
                .map(|angle| format!(" /Rotate {angle}"))
                .unwrap_or_default();
            o.write(
                page_id,
                format!(
                    "<< /Type /Page /Parent {page_parent} 0 R /MediaBox [0 0 {page_width:.2} {page_height:.2}] \
                     /Resources {resource_id} 0 R \
                     /Contents {content_id} 0 R{rotation} >>"
                )
                .as_bytes(),
            );
            pending_page_bytes = pending_page_bytes.saturating_add(raw_len);
            pending_page_streams.push(PendingPageStream {
                id: content_id,
                content: PendingPageContent::Encoded(stream),
                raw_len,
                cache: None,
            });
            if pending_page_streams.len() >= PAGE_STREAM_BATCH_COUNT
                || pending_page_bytes >= PAGE_STREAM_BATCH_BYTES
            {
                flush_page_streams(&mut o, &mut pending_page_streams);
                pending_page_bytes = 0;
            }
            continue;
        }
        let destinations_before = cache_key.as_ref().map(|_| {
            destinations
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        });

        // Where a text run is, and where its pen has got to.
        //
        // This bookkeeping is the whole correctness of the page. A `Tj` string
        // does not carry positions: the viewer sets the first glyph at the text
        // matrix and then *advances by each glyph's width*. So a run may only
        // hold characters the DVI put exactly where those advances land. The
        // first version of this file appended every character of a page into one
        // string, and a page reading `One.` with `1` at the foot came out as
        // `One.1` on the first line — the folio dragged up eleven inches
        // because nothing checked that the DVI agreed with the pen.
        struct Run {
            font: u32,
            v: i64,
            pen: i64,
            encoded_pen: f64,
            encoded_glyph_scale: f64,
            encoded_tj_scale: f64,
        }
        struct TextState {
            run: Option<Run>,
            open: bool,
            selected_font: Option<(usize, i64)>,
            // Encoded (horizontal scale, slant, x, y) of the PDF text line
            // matrix. `TJ` advances the text matrix but not this line matrix,
            // so a later `Td` can still address its next origin exactly.
            line_matrix: Option<(f64, f64, f64, f64)>,
        }
        let mut c: Vec<u8> = Vec::new();
        // dvips's `background <color>` is page paint, not current text color.
        // Emit it before every other item even if the declaration occurs later
        // in the DVI page, and isolate it so the ordinary color stack still
        // begins at PDF's default black.
        if let Some(background) = page.items.iter().filter_map(|item| match item {
            dvi::Item::Special { bytes, .. } => {
                special_background(&String::from_utf8_lossy(bytes))
            }
            _ => None,
        }).last() {
            c.extend_from_slice(b"q\n");
            c.extend_from_slice(background.operators().as_bytes());
            c.extend_from_slice(
                format!("0 0 {} {} re f\nQ\n", pdf_number(page_width, 4), pdf_number(page_height, 4))
                    .as_bytes(),
            );
        }
        let mut text_state = TextState {
            run: None,
            open: false,
            selected_font: None,
            line_matrix: None,
        };
        let mut font_expansion = 1.0f64;
        let close_text = |out: &mut Vec<u8>, state: &mut TextState| {
            if state.run.take().is_some() {
                out.extend_from_slice(b")] TJ");
            }
            if state.open {
                out.extend_from_slice(b" ET\n");
                state.open = false;
                state.selected_font = None;
                state.line_matrix = None;
            }
        };
        let mut colors = persistent_colors.clone();
        let incoming_color = colors.last().copied().unwrap_or(Color::BLACK);
        if incoming_color != Color::BLACK {
            c.extend_from_slice(incoming_color.operators().as_bytes());
        }
        // PDF's initial stroking and nonstroking colors are black. Keep this
        // as `None` after an opaque literal or unmatched save/restore boundary;
        // full equality is then required before any color operator is elided.
        let mut emitted_color = Some(incoming_color);
        let mut pdf_saves = Vec::<(f64, f64)>::new();
        // pdfTeX keeps the local coordinate system installed by a
        // `setorigin` literal through any immediately following `direct` or
        // `page` literals.  It restores page coordinates only when ordinary
        // positioned content resumes.  This matters for packages such as
        // changebar, which open a graphics-state scope in a setorigin literal
        // and close it in the following direct literal.
        let mut pending_pdf_literal_origin = None::<(f64, f64)>;
        let mut pdf_color_stacks = std::collections::BTreeMap::<i64, (String, Vec<String>)>::new();
        // dvipdfmx's bcontent/econtent pair makes DVI material relative to the
        // point where the block began. PGF uses nested blocks to interleave
        // ordinary TeX text with raw PDF paths.
        let mut pdf_content_coords = Vec::<(f64, f64)>::new();
        // PGF's dvipdfmx driver suspends a content block with the exact
        // `q -1 0 0 -1 0 0 cm q`, econtent, bcontent sequence, then restores
        // it with bcontent, `Q Q Q`. In direct pdfTeX, text already placed in
        // the suspended block advances the current PDF transform; the DVI
        // transport otherwise loses that advance when econtent closes its
        // graphics-state scope. Carry only that delta across PGF's qualified
        // suspend/restore pair. Ordinary content blocks retain dvipdfmx's
        // stated DVI positions.
        let mut pdf_suspend_flip = false;
        let mut pdf_suspended_offsets = Vec::<(f64, f64)>::new();
        let mut pdf_content_advance = None::<(f64, f64, usize)>;
        let mut pdf_transform_depth = 0usize;
        let mut annotations = Vec::<usize>::new();
        let mut xobjects = Vec::<(String, usize)>::new();
        let mut page_resource_fragments = Vec::<Vec<u8>>::new();
        let mut page_fragments = Vec::<Vec<u8>>::new();
        let mut xobject_capture: Option<PdfXObjectCapture> = None;
        let mut link_start: Option<(i64, i64)> = None;
        let mut link_end: Option<(i64, i64)> = None;
        let mut link_segments = Vec::<LinkSegment>::new();
        let mut dvipdfmx_link = None::<(usize, Vec<u8>)>;
        let mut rotations = Vec::<Rotation>::new();
        let mut ignore_next_driver_graphic = false;
        let mut precise_eps_size = None;
        let mut pdftex_graphic_scale = None;
        for item in &page.items {
            let continues_pdf_literal = match item {
                dvi::Item::Special { bytes, .. } => {
                    let text = String::from_utf8_lossy(bytes);
                    let trimmed = text.trim();
                    trimmed.starts_with("aitex:pdfliteral-direct ")
                        || trimmed.starts_with("aitex:pdfliteral-page ")
                }
                _ => false,
            };
            if !continues_pdf_literal
                && let Some((x, y)) = pending_pdf_literal_origin.take()
            {
                close_text(&mut c, &mut text_state);
                c.extend_from_slice(format!("1 0 0 1 {:.4} {:.4} cm\n", -x, -y).as_bytes());
            }
            match item {
                dvi::Item::ClassicGlyph {
                    font,
                    code,
                    origin_x,
                    origin_y,
                    ..
                } => {
                    let font = font.0;
                    let h = origin_x.0;
                    let v = origin_y.0;
                    let Some(pdf_font) = font_objs.get(&font) else {
                        continue;
                    };
                    let id = pdf_font.id;
                    let at = pdf_font.at;
                    let capture_origin = xobject_capture.as_ref().map(|capture| {
                        capture
                            .content_coords
                            .last()
                            .copied()
                            .unwrap_or((capture.origin_h, capture.origin_v))
                    });
                    if let Some(capture) = xobject_capture.as_mut() {
                        capture.fonts.insert(id);
                    }
                    let output = xobject_capture
                        .as_mut()
                        .map_or(&mut c, |capture| &mut capture.content);
                    // A run survives only along one baseline in one font.
                    if text_state
                        .run
                        .as_ref()
                        .is_some_and(|r| r.font != font || r.v != v)
                    {
                        output.extend_from_slice(b")] TJ");
                        text_state.run = None;
                    }
                    match text_state.run.as_mut() {
                        None => {
                            // TeX's reference point is one inch in from the
                            // top-left corner; PDF measures from the
                            // bottom-left, upwards.
                            let (x, y) = if let Some((origin_h, origin_v)) = capture_origin {
                                (pt(h - origin_h), -pt(v - origin_v))
                            } else {
                                let mut x = page_origin_x + pt(h);
                                let mut y = page_height - page_origin_y - pt(v);
                                if let Some((origin_x, origin_y)) =
                                    pdf_content_coords.last().or_else(|| pdf_saves.last())
                                {
                                    x -= origin_x;
                                    y -= origin_y;
                                }
                                (x, y)
                            };
                            let exact_size = pt(at);
                            // pdfTeX spells classic font sizes to four decimal
                            // places even though text coordinates use its
                            // three-digit default. Matching that quantization
                            // is visually observable at small hinted Type 1
                            // sizes: 11.955 and 11.9552 rasterize differently.
                            let encoded_size = encoded_pdf_value(exact_size, 4);
                            let exact_horizontal_scale = pdf_font.extend * font_expansion;
                            let encoded_horizontal_scale =
                                encoded_pdf_value(exact_horizontal_scale, 6);
                            let encoded_slant = encoded_pdf_value(pdf_font.slant, 6);
                            let encoded_x = encoded_pdf_value(x, 3);
                            let encoded_y = encoded_pdf_value(y, 3);
                            if text_state.open {
                                output.push(b'\n');
                            } else {
                                output.extend_from_slice(b"BT ");
                                text_state.open = true;
                            }
                            if text_state.selected_font != Some((id, at)) {
                                let size = pdf_number(exact_size, 4);
                                output.extend_from_slice(format!("/F{id} {size} Tf ").as_bytes());
                                text_state.selected_font = Some((id, at));
                            }
                            let relative = text_state.line_matrix.and_then(
                                |(old_scale, old_slant, old_x, old_y)| {
                                    if old_scale != encoded_horizontal_scale
                                        || old_slant != encoded_slant
                                        || old_scale.abs() <= f64::MIN_POSITIVE
                                    {
                                        return None;
                                    }
                                    let exact_ty = encoded_y - old_y;
                                    let exact_tx =
                                        (encoded_x - old_x - exact_ty * old_slant) / old_scale;
                                    let (tx, encoded_tx) = encoded_pdf_number(exact_tx, 3);
                                    let (ty, encoded_ty) = encoded_pdf_number(exact_ty, 3);
                                    let moved_x =
                                        old_x + encoded_tx * old_scale + encoded_ty * old_slant;
                                    let moved_y = old_y + encoded_ty;
                                    ((moved_x - encoded_x).abs() <= 0.000_051
                                        && (moved_y - encoded_y).abs() <= 0.000_051)
                                        .then_some((tx, ty, moved_x, moved_y))
                                },
                            );
                            if let Some((tx, ty, moved_x, moved_y)) = relative {
                                output.extend_from_slice(format!("{tx} {ty} Td [(").as_bytes());
                                text_state.line_matrix = Some((
                                    encoded_horizontal_scale,
                                    encoded_slant,
                                    moved_x,
                                    moved_y,
                                ));
                            } else {
                                let horizontal_scale = pdf_number(exact_horizontal_scale, 6);
                                let slant = pdf_number(pdf_font.slant, 6);
                                let x = pdf_number(x, 3);
                                let y = pdf_number(y, 3);
                                output.extend_from_slice(
                                    format!(
                                        "{horizontal_scale} 0 {slant} 1 {x} {y} Tm [("
                                    )
                                    .as_bytes(),
                                );
                                text_state.line_matrix = Some((
                                    encoded_horizontal_scale,
                                    encoded_slant,
                                    encoded_x,
                                    encoded_y,
                                ));
                            }
                            let encoded_size_scale = if exact_size == 0.0 {
                                1.0
                            } else {
                                encoded_size / exact_size
                            };
                            text_state.run = Some(Run {
                                font,
                                v,
                                pen: h,
                                encoded_pen: h as f64,
                                encoded_glyph_scale: encoded_size_scale
                                    * encoded_horizontal_scale
                                    / pdf_font.extend,
                                encoded_tj_scale: at as f64 / 1000.0
                                    * encoded_size_scale
                                    * encoded_horizontal_scale,
                            });
                        }
                        Some(r) => {
                            // Anywhere the DVI and the pen disagree — a word
                            // space, a kern, an accumulated rounding — is a `TJ`
                            // displacement, in 1000ths of the font size, moving
                            // the pen *left* for a positive number.
                            if r.pen != h {
                                let off = -(h as f64 - r.encoded_pen)
                                    / r.encoded_tj_scale.max(f64::MIN_POSITIVE);
                                let (rendered, represented) = encoded_pdf_number(off, 0);
                                if rendered != "0" {
                                    output.extend_from_slice(format!("){rendered}(").as_bytes());
                                    r.encoded_pen -= represented * r.encoded_tj_scale;
                                }
                                r.pen = h;
                            }
                        }
                    }
                    let byte = (*code & 0xff) as u8;
                    let base = pdf_font.advances.get(byte as usize).copied().unwrap_or(0);
                    let encoded_base = pdf_font
                        .encoded_advances
                        .get(byte as usize)
                        .copied()
                        .unwrap_or(base as f64);
                    let advance = (base as f64 * font_expansion).round() as i64;
                    escape(&[byte], output);
                    if let Some(r) = text_state.run.as_mut() {
                        r.pen += advance;
                        r.encoded_pen += encoded_base * r.encoded_glyph_scale;
                    }
                    if capture_origin.is_none() && link_start.is_some() {
                        let x = page_origin_x + pt(h);
                        let y = page_height - page_origin_y - pt(v);
                        let rect = transformed_rect(
                            [x - 1.0, y - 2.0, x + pt(advance) + 1.0, y + 10.0],
                            &rotations,
                        );
                        add_link_segment(&mut link_segments, v, rotations.len(), rect);
                    }
                }
                dvi::Item::Rule {
                    x,
                    baseline_y,
                    width,
                    height,
                } => {
                    let h = x.0;
                    let v = baseline_y.0;
                    let width = width.0;
                    let height = height.0;
                    let capture_origin = xobject_capture.as_ref().map(|capture| {
                        capture
                            .content_coords
                            .last()
                            .copied()
                            .unwrap_or((capture.origin_h, capture.origin_v))
                    });
                    let output = xobject_capture
                        .as_mut()
                        .map_or(&mut c, |capture| &mut capture.content);
                    close_text(output, &mut text_state);
                    // §589: a rule with a non-positive dimension is not set.
                    if width <= 0 || height <= 0 {
                        continue;
                    }
                    let global_x = page_origin_x + pt(h);
                    let global_y = page_height - page_origin_y - pt(v);
                    if capture_origin.is_none() && link_start.is_some() {
                        let rect = transformed_rect(
                            [
                                global_x,
                                global_y,
                                global_x + pt(width),
                                global_y + pt(height),
                            ],
                            &rotations,
                        );
                        add_link_segment(&mut link_segments, v, rotations.len(), rect);
                    }
                    let (mut x, mut y) = if let Some((origin_h, origin_v)) = capture_origin {
                        (pt(h - origin_h), -pt(v - origin_v))
                    } else {
                        (global_x, global_y)
                    };
                    if capture_origin.is_none()
                        && let Some((origin_x, origin_y)) =
                            pdf_content_coords.last().or_else(|| pdf_saves.last())
                    {
                        x -= origin_x;
                        y -= origin_y;
                    }
                    let x = pdf_number(x, 4);
                    let y = pdf_number(y, 4);
                    let width = pdf_number(pt(width), 4);
                    let height = pdf_number(pt(height), 4);
                    output.extend_from_slice(
                        format!("{x} {y} {width} {height} re f\n").as_bytes(),
                    );
                }
                // Native runs are emitted from the positioned companion below.
                // Keeping them in this shared page list preserves event order
                // for differential checks without sending glyph IDs through
                // the classic eight-bit font machinery.
                dvi::Item::GlyphRun(_) => {}
                dvi::Item::Special { x, y, bytes } => {
                    let h = x.0;
                    let v = y.0;
                    let text = String::from_utf8_lossy(bytes);
                    let trimmed = text.trim();
                    if trimmed == PDFTEX_IGNORE_NEXT_DRIVER_GRAPHIC_SPECIAL {
                        ignore_next_driver_graphic = true;
                        continue;
                    }
                    if let Some(size) = private_eps_size(trimmed) {
                        precise_eps_size = Some(size);
                        continue;
                    }
                    if let Some(scale) = private_pdftex_graphic_scale(trimmed) {
                        pdftex_graphic_scale = Some(scale);
                        continue;
                    }

                    if pdf_page_ref_special(trimmed).is_some() {
                        continue;
                    }

                    // The source adapter uses dvipdfmx as aiTeX's private
                    // transport even when an author explicitly selected the
                    // dvips backend.  In that latter state stock pdfTeX
                    // ignores *all* driver drawing specials.  Suppressing
                    // only PSfile/color commands is insufficient: PGF's
                    // substituted backend emits `pdf:`/`x:` commands, which
                    // would otherwise draw a picture that stock pdfTeX left
                    // blank (while its ordinary TeX labels remain at their
                    // untransformed DVI positions).
                    if ignore_postscript
                        && (trimmed.starts_with("pdf:")
                            || trimmed.starts_with("ps:")
                            || trimmed.starts_with("x:"))
                    {
                        continue;
                    }

                    // A bxobj/exobj pair diverts PDF specials into a reusable
                    // Form XObject. PGF defines shadings this way at the start
                    // of a page; flattening them into the page leaves `/Sh sh`
                    // with no matching resource and makes strict readers reject
                    // the otherwise visually plausible PDF.
                    if let Some(capture) = xobject_capture.as_mut() {
                        close_text(&mut capture.content, &mut text_state);
                    }
                    if xobject_capture.is_some() {
                        if let Some(extra) = pdf_command_special(trimmed, "exobj") {
                            let capture = xobject_capture.take().expect("capture exists");
                            write_captured_xobject(
                                &mut o,
                                capture,
                                extra.trim_start().as_bytes(),
                                &named_ids,
                            );
                            continue;
                        }
                        if let Some((name, initial)) = pdf_dvipdfmx_object_special(bytes) {
                            let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                            named_ids.insert(name.to_string(), id);
                            let capture = xobject_capture.as_ref().expect("capture exists");
                            named_objects.insert(
                                name.to_string(),
                                NamedPdfObject {
                                    id,
                                    initial: initial.to_vec(),
                                    puts: Vec::new(),
                                    stream: None,
                                    complete: true,
                                    position: Some(NamedObjectPosition {
                                        x: pt(h - capture.origin_h),
                                        y: -pt(v - capture.origin_v),
                                        page: page_id,
                                    }),
                                },
                            );
                            continue;
                        }
                        if let Some((target, value)) = pdf_named_special(trimmed, "pdf:put") {
                            if target == "resources" {
                                xobject_capture
                                    .as_mut()
                                    .expect("capture exists")
                                    .resources
                                    .push(value.as_bytes().to_vec());
                            } else if let Some(object) = named_objects.get_mut(target) {
                                object.puts.push(value.as_bytes().to_vec());
                            }
                            continue;
                        }
                        if trimmed == "pdf:bcontent" || trimmed == "pdf: bcontent" {
                            let capture = xobject_capture.as_mut().expect("capture exists");
                            let (old_h, old_v) = capture
                                .content_coords
                                .last()
                                .copied()
                                .unwrap_or((capture.origin_h, capture.origin_v));
                            let dx = pt(h - old_h);
                            let dy = -pt(v - old_v);
                            capture.content.extend_from_slice(
                                format!("q\n1 0 0 1 {dx:.4} {dy:.4} cm\n").as_bytes(),
                            );
                            capture.content_coords.push((h, v));
                            continue;
                        }
                        if trimmed == "pdf:econtent" || trimmed == "pdf: econtent" {
                            let capture = xobject_capture.as_mut().expect("capture exists");
                            if capture.content_coords.pop().is_some() {
                                capture.content.extend_from_slice(b"Q\n");
                            }
                            continue;
                        }
                        if let Some(literal) = trimmed.strip_prefix("pdf:code") {
                            let capture = xobject_capture.as_mut().expect("capture exists");
                            capture
                                .content
                                .extend_from_slice(literal.trim_start().as_bytes());
                            capture.content.push(b'\n');
                            continue;
                        }
                        if let Some(literal) = pdf_content_special(trimmed) {
                            let capture = xobject_capture.as_mut().expect("capture exists");
                            let (origin_h, origin_v) = capture
                                .content_coords
                                .last()
                                .copied()
                                .unwrap_or((capture.origin_h, capture.origin_v));
                            let dx = pt(h - origin_h);
                            let dy = -pt(v - origin_v);
                            capture.content.extend_from_slice(
                                format!("q\n1 0 0 1 {dx:.4} {dy:.4} cm\n").as_bytes(),
                            );
                            capture.content.extend_from_slice(literal.as_bytes());
                            capture.content.extend_from_slice(b"\nQ\n");
                            continue;
                        }
                        if let Some(scale) = trimmed.strip_prefix("x:scale") {
                            let numbers = scale
                                .split_whitespace()
                                .filter_map(|field| field.parse::<f64>().ok())
                                .collect::<Vec<_>>();
                            if numbers.len() >= 2 {
                                let capture =
                                    xobject_capture.as_mut().expect("capture exists");
                                let sx = numbers[0];
                                let sy = numbers[1];
                                let (origin_h, origin_v) = capture
                                    .content_coords
                                    .last()
                                    .copied()
                                    .unwrap_or((capture.origin_h, capture.origin_v));
                                let x = pt(h - origin_h);
                                let y = -pt(v - origin_v);
                                capture.content.extend_from_slice(
                                    format!(
                                        "{sx:.8} 0 0 {sy:.8} {:.8} {:.8} cm\n",
                                        (1.0 - sx) * x,
                                        (1.0 - sy) * y
                                    )
                                    .as_bytes(),
                                );
                            }
                            continue;
                        }
                        if let Some(transform) = trimmed.strip_prefix("pdf:btrans") {
                            let mut fields = transform.split_whitespace();
                            let kind = fields.next();
                            let numbers = fields
                                .filter_map(|field| field.parse::<f64>().ok())
                                .collect::<Vec<_>>();
                            let (mut a, mut b, mut cc, mut d, mut e, mut f) =
                                (1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
                            match kind {
                                Some("matrix") if numbers.len() >= 6 => {
                                    (a, b, cc, d, e, f) = (
                                        numbers[0],
                                        numbers[1],
                                        numbers[2],
                                        numbers[3],
                                        numbers[4],
                                        numbers[5],
                                    );
                                }
                                Some("rotate") if !numbers.is_empty() => {
                                    let radians = numbers[0].to_radians();
                                    (a, b, cc, d) = (
                                        radians.cos(),
                                        radians.sin(),
                                        -radians.sin(),
                                        radians.cos(),
                                    );
                                }
                                _ => {}
                            }
                            let capture = xobject_capture.as_mut().expect("capture exists");
                            let (origin_h, origin_v) = capture
                                .content_coords
                                .last()
                                .copied()
                                .unwrap_or((capture.origin_h, capture.origin_v));
                            let x = pt(h - origin_h);
                            let y = -pt(v - origin_v);
                            e += (1.0 - a) * x - cc * y;
                            f += (1.0 - d) * y - b * x;
                            capture.content.extend_from_slice(
                                format!(
                                    "q\n{a:.8} {b:.8} {cc:.8} {d:.8} {e:.8} {f:.8} cm\n"
                                )
                                .as_bytes(),
                            );
                            continue;
                        }
                        if trimmed == "pdf:etrans" {
                            xobject_capture
                                .as_mut()
                                .expect("capture exists")
                                .content
                                .extend_from_slice(b"Q\n");
                            continue;
                        }
                        if let Some(image) = special_dvipdfmx_image(trimmed) {
                            let graphic_scale = pdftex_graphic_scale.take();
                            if core::mem::take(&mut ignore_next_driver_graphic) {
                                continue;
                            }
                            if let Some(asset) = resolve_asset(&image.name, image.page) {
                                let (origin_h, origin_v) = xobject_capture
                                    .as_ref()
                                    .expect("capture exists")
                                    .content_coords
                                    .last()
                                    .copied()
                                    .unwrap_or_else(|| {
                                        let capture = xobject_capture
                                            .as_ref()
                                            .expect("capture exists");
                                        (capture.origin_h, capture.origin_v)
                                    });
                                let x = pt(h - origin_h);
                                let y = -pt(v - origin_v);
                                let natural_bbox = asset
                                    .raster_bbox
                                    .or(image.bbox)
                                    .unwrap_or([0.0, 0.0, 1.0, 1.0]);
                                let viewport = image.bbox.unwrap_or(natural_bbox);
                                let intrinsic_width =
                                    (viewport[2] - viewport[0]).abs().max(0.001);
                                let intrinsic_height =
                                    (viewport[3] - viewport[1]).abs().max(0.001);
                                let form_key = (
                                    image.name.clone(),
                                    image.page,
                                    natural_bbox.map(f64::to_bits),
                                );
                                let imported_pdf =
                                    if let Some(id) = imported_form_ids.get(&form_key) {
                                        Some(*id)
                                    } else {
                                        asset.pdf.as_deref().and_then(|pdf| {
                                            let crop = natural_bbox.map(f64::to_bits);
                                            let content_key = (bytes_fingerprint(pdf), crop);
                                            let id = if let Some(id) =
                                                imported_form_content_ids.get(&content_key)
                                            {
                                                *id
                                            } else {
                                                let id = import_pdf_form(
                                                    &mut o,
                                                    pdf,
                                                    natural_bbox,
                                                )?;
                                                imported_form_content_ids.insert(content_key, id);
                                                id
                                            };
                                            imported_form_ids.insert(form_key, id);
                                            Some(id)
                                        })
                                    };
                                let capture =
                                    xobject_capture.as_mut().expect("capture exists");
                                if let Some(form_id) = imported_pdf {
                                    let (sx, sy) = graphic_scale.unwrap_or_else(|| {
                                        match (image.width, image.height) {
                                            (Some(w), Some(h)) => (
                                                pdftex_image_scale(w, intrinsic_width),
                                                pdftex_image_scale(h, intrinsic_height),
                                            ),
                                            (Some(w), None) => {
                                                let scale =
                                                    pdftex_image_scale(w, intrinsic_width);
                                                (scale, scale)
                                            }
                                            (None, Some(h)) => {
                                                let scale =
                                                    pdftex_image_scale(h, intrinsic_height);
                                                (scale, scale)
                                            }
                                            (None, None) => (1.0, 1.0),
                                        }
                                    });
                                    let sx = encoded_pdf_value(sx, 5);
                                    let sy = encoded_pdf_value(sy, 5);
                                    let tx = x - sx * viewport[0];
                                    let ty = y - sy * viewport[1];
                                    let clip_width = sx.abs() * intrinsic_width;
                                    let clip_height = sy.abs() * intrinsic_height;
                                    let resource = format!("Fm{form_id}");
                                    let clipping = if image.clip {
                                        format!(
                                            "{x:.4} {y:.4} {clip_width:.8} {clip_height:.8} re W n "
                                        )
                                    } else {
                                        String::new()
                                    };
                                    capture.content.extend_from_slice(
                                        format!(
                                            "q {clipping}{IMPORTED_PAGE_INITIAL_COLOR} \
                                             {sx:.8} 0 0 {sy:.8} {tx:.3} {ty:.3} cm \
                                             /{resource} Do Q\n"
                                        )
                                        .as_bytes(),
                                    );
                                    capture.xobjects.push((resource, form_id));
                                } else {
                                    embed_raster_image(
                                        &mut o,
                                        &mut raster_cache,
                                        &image,
                                        &asset,
                                        graphic_scale,
                                        x,
                                        y,
                                        &mut capture.content,
                                        &mut capture.xobjects,
                                    );
                                }
                            }
                            continue;
                        }
                        if let Some((name, _)) = pdf_named_special(trimmed, "pdf:uxobj") {
                            if let Some(id) = named_ids.get(name).copied() {
                                let capture = xobject_capture.as_mut().expect("capture exists");
                                let (origin_h, origin_v) = capture
                                    .content_coords
                                    .last()
                                    .copied()
                                    .unwrap_or((capture.origin_h, capture.origin_v));
                                let dx = pt(h - origin_h);
                                let dy = -pt(v - origin_v);
                                capture.content.extend_from_slice(
                                    format!(
                                        "q 1 0 0 1 {dx:.8} {dy:.8} cm /{} Do Q\n",
                                        pdf_name(name)
                                    )
                                    .as_bytes(),
                                );
                                capture.xobjects.push((name.to_string(), id));
                            }
                            continue;
                        }
                        if let Some(color) = special_color(trimmed) {
                            if ignore_postscript {
                                continue;
                            }
                            colors.push(color);
                            xobject_capture
                                .as_mut()
                                .expect("capture exists")
                                .content
                                .extend_from_slice(color.operators().as_bytes());
                            continue;
                        }
                        if trimmed == "color pop" {
                            if ignore_postscript {
                                continue;
                            }
                            if colors.len() > 1 {
                                colors.pop();
                            }
                            let color = colors.last().copied().unwrap_or(Color::BLACK);
                            xobject_capture
                                .as_mut()
                                .expect("capture exists")
                                .content
                                .extend_from_slice(color.operators().as_bytes());
                            continue;
                        }
                        // pdf:bcolor/pdf:ecolor are stack synchronization hints;
                        // the actual color push/pop specials carry the state.
                        continue;
                    }

                    if let Some((name, bbox)) = pdf_xobject_begin(trimmed) {
                        close_text(&mut c, &mut text_state);
                        let id = o.reserve();
                        named_ids.insert(name.to_string(), id);
                        xobject_capture = Some(PdfXObjectCapture {
                            id,
                            bbox,
                            origin_h: h,
                            origin_v: v,
                            content: b"q\n".to_vec(),
                            content_coords: vec![(h, v)],
                            fonts: std::collections::BTreeSet::new(),
                            resources: Vec::new(),
                            xobjects: Vec::new(),
                        });
                        continue;
                    }
                    if let Some((name, initial)) = pdf_dvipdfmx_object_special(bytes) {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        named_objects.insert(
                            name.to_string(),
                            NamedPdfObject {
                                id,
                                initial: initial.to_vec(),
                                puts: Vec::new(),
                                stream: None,
                                complete: true,
                                position: Some(NamedObjectPosition {
                                    x: page_origin_x + pt(h),
                                    y: page_height - page_origin_y - pt(v),
                                    page: page_id,
                                }),
                            },
                        );
                        continue;
                    }
                    if let Some((name, initial)) = pdf_named_special(trimmed, "pdf:obj") {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        named_objects.insert(
                            name.to_string(),
                        NamedPdfObject {
                            id,
                            initial: initial.as_bytes().to_vec(),
                            puts: Vec::new(),
                            stream: None,
                            complete: false,
                            position: Some(NamedObjectPosition {
                                    x: page_origin_x + pt(h),
                                    y: page_height - page_origin_y - pt(v),
                                    page: page_id,
                                }),
                            },
                        );
                        continue;
                    }
                    if let Some((file, name, initial, payload)) =
                        pdf_dvipdfmx_stream_special(trimmed)
                    {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        let stream = if file {
                            std::str::from_utf8(&payload)
                                .ok()
                                .and_then(|filename| {
                                    resolve_asset(&format!("aitex-raw:{filename}"), 1)
                                })
                                .map(|asset| asset.bytes.as_ref().to_vec())
                        } else {
                            Some(payload)
                        };
                        named_objects.insert(
                            name.to_string(),
                            NamedPdfObject {
                                id,
                                initial,
                                puts: Vec::new(),
                                stream,
                                complete: false,
                                position: Some(NamedObjectPosition {
                                    x: page_origin_x + pt(h),
                                    y: page_height - page_origin_y - pt(v),
                                    page: page_id,
                                }),
                            },
                        );
                        continue;
                    }
                    if let Some((name, dictionary, data)) =
                        pdf_object_stream_special(text.as_ref(), "aitex:pdfobjstream")
                    {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        named_objects.insert(
                            name.to_string(),
                            NamedPdfObject {
                                id,
                                initial: dictionary.as_bytes().to_vec(),
                                puts: Vec::new(),
                                stream: Some(data.as_bytes().to_vec()),
                                complete: false,
                                position: Some(NamedObjectPosition {
                                    x: page_origin_x + pt(h),
                                    y: page_height - page_origin_y - pt(v),
                                    page: page_id,
                                }),
                            },
                        );
                        continue;
                    }
                    if let Some((name, dictionary, filename)) =
                        pdf_object_stream_special(text.as_ref(), "aitex:pdfobjfile")
                    {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        if let Some(asset) = resolve_asset(&format!("aitex-raw:{filename}"), 1) {
                            named_objects.insert(
                                name.to_string(),
                                NamedPdfObject {
                                    id,
                                    initial: dictionary.as_bytes().to_vec(),
                                    puts: Vec::new(),
                                    stream: Some(asset.bytes.as_ref().to_vec()),
                                    complete: false,
                                    position: Some(NamedObjectPosition {
                                        x: page_origin_x + pt(h),
                                        y: page_height - page_origin_y - pt(v),
                                        page: page_id,
                                    }),
                                },
                            );
                        }
                        continue;
                    }
                    if let Some((target, value)) = pdf_named_special(trimmed, "pdf:put") {
                        close_text(&mut c, &mut text_state);
                        match target {
                            "catalog" => catalog_fragments.push(value.as_bytes().to_vec()),
                            "pages" => pages_fragments.push(value.as_bytes().to_vec()),
                            "names" => names_fragments.push(value.as_bytes().to_vec()),
                            "thispage" => page_fragments.push(value.as_bytes().to_vec()),
                            "resources" => page_resource_fragments.push(value.as_bytes().to_vec()),
                            _ => {
                                let id = named_ids
                                    .get(target)
                                    .copied()
                                    .unwrap_or_else(|| o.reserve());
                                named_ids.insert(target.to_string(), id);
                                named_objects
                                    .entry(target.to_string())
                                    .or_insert_with(|| NamedPdfObject {
                                        id,
                                        initial: b"<<>>".to_vec(),
                                        puts: Vec::new(),
                                        stream: None,
                                        complete: false,
                                        position: Some(NamedObjectPosition {
                                            x: page_origin_x + pt(h),
                                            y: page_height - page_origin_y - pt(v),
                                            page: page_id,
                                        }),
                                    })
                                    .puts
                                    .push(value.as_bytes().to_vec());
                            }
                        }
                        continue;
                    }
                    if let Some((name, dictionary)) = pdf_named_special(trimmed, "pdf:bann") {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        link_start = Some((h, v));
                        link_end = None;
                        link_segments.clear();
                        dvipdfmx_link = Some((id, dictionary.as_bytes().to_vec()));
                        continue;
                    }
                    if trimmed == "pdf:eann" {
                        close_text(&mut c, &mut text_state);
                        link_end = Some((h, v));
                        if let (Some((id, dictionary)), Some(start)) =
                            (dvipdfmx_link.take(), link_start)
                        {
                            let end = link_end.unwrap_or((h, v));
                            let mut rect = if let Some(first) = link_segments.first() {
                                first.rect
                            } else {
                                let sx = page_origin_x + pt(start.0);
                                let ex = page_origin_x + pt(end.0);
                                let sy = page_height - page_origin_y - pt(start.1);
                                let ey = page_height - page_origin_y - pt(end.1);
                                [
                                    sx.min(ex) - 1.0,
                                    sy.min(ey) - 2.0,
                                    sx.max(ex) + 1.0,
                                    sy.max(ey) + 10.0,
                                ]
                            };
                            for segment in link_segments.iter().skip(1) {
                                rect[0] = rect[0].min(segment.rect[0]);
                                rect[1] = rect[1].min(segment.rect[1]);
                                rect[2] = rect[2].max(segment.rect[2]);
                                rect[3] = rect[3].max(segment.rect[3]);
                            }
                            for coordinate in [0usize, 2] {
                                rect[coordinate] = rect[coordinate].clamp(0.0, page_width);
                            }
                            for coordinate in [1usize, 3] {
                                rect[coordinate] = rect[coordinate].clamp(0.0, page_height);
                            }
                            let inner = dictionary_inner(&dictionary).unwrap_or(&dictionary);
                            let body = format!(
                                "<< /Rect [{:.4} {:.4} {:.4} {:.4}] {} >>",
                                rect[0],
                                rect[1],
                                rect[2],
                                rect[3],
                                String::from_utf8_lossy(inner),
                            )
                            .into_bytes();
                            pending_named_annotations.push((
                                id,
                                body,
                                NamedObjectPosition {
                                    x: page_origin_x + pt(start.0),
                                    y: page_height - page_origin_y - pt(start.1),
                                    page: page_id,
                                },
                            ));
                            annotations.push(id);
                        }
                        link_start = None;
                        link_end = None;
                        link_segments.clear();
                        continue;
                    }
                    if let Some((name, width, height, depth, dictionary)) =
                        pdf_dvipdfmx_annotation_special(trimmed)
                    {
                        close_text(&mut c, &mut text_state);
                        let id = named_ids.get(name).copied().unwrap_or_else(|| o.reserve());
                        named_ids.insert(name.to_string(), id);
                        let x = page_origin_x + pt(h);
                        let baseline = page_height - page_origin_y - pt(v);
                        let inner = dictionary_inner(dictionary.as_bytes())
                            .unwrap_or(dictionary.as_bytes());
                        pending_named_annotations.push((
                            id,
                            format!(
                                "<< /Rect [{:.4} {:.4} {:.4} {:.4}] {} >>",
                                x,
                                baseline - depth,
                                x + width,
                                baseline + height,
                                String::from_utf8_lossy(inner),
                            )
                            .into_bytes(),
                            NamedObjectPosition {
                                x,
                                y: baseline,
                                page: page_id,
                            },
                        ));
                        annotations.push(id);
                        continue;
                    }
                    if trimmed.starts_with("pdf:dest") {
                        if let Some(name) = special_string(trimmed, "pdf:dest") {
                            destinations.entry(name).or_insert(Destination {
                                page: page_id,
                                x: page_origin_x + pt(h),
                                y: page_height - page_origin_y - pt(v),
                                view: special_destination_view(trimmed),
                            });
                        }
                        continue;
                    }
                    if let Some((name, _)) = pdf_named_special(trimmed, "pdf:uxobj") {
                        close_text(&mut c, &mut text_state);
                        if let Some(id) = named_ids.get(name).copied() {
                            let mut x = page_origin_x + pt(h);
                            let mut y = page_height - page_origin_y - pt(v);
                            if let Some((origin_x, origin_y)) = pdf_content_coords.last() {
                                x -= origin_x;
                                y -= origin_y;
                            }
                            let resource = pdf_name(name);
                            c.extend_from_slice(
                                format!("q 1 0 0 1 {x:.8} {y:.8} cm /{resource} Do Q\n").as_bytes(),
                            );
                            xobjects.push((resource, id));
                        }
                        continue;
                    }

                    if let Some(amount) = trimmed
                        .strip_prefix("aitex:fontexpand ")
                        .and_then(|value| value.parse::<i64>().ok())
                    {
                        close_text(&mut c, &mut text_state);
                        font_expansion = 1.0 + amount.clamp(-999, 1000) as f64 / 1000.0;
                        continue;
                    }

                    if trimmed == "pdf:bcontent" || trimmed == "pdf: bcontent" {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let current_x = page_origin_x + pt(h);
                        let current_y = page_height - page_origin_y - pt(v);
                        let (x, y) = (current_x, current_y);
                        pdf_suspend_flip = false;
                        // A PGF content block nested inside pdfTeX's
                        // `\pdfsave` is expressed relative to that saved
                        // current point.  Treating its DVI position as an
                        // absolute page coordinate translates inline TikZ
                        // material twice (and commonly moves the whole
                        // picture outside its clip rectangle).
                        let (old_x, old_y) = pdf_content_coords
                            .last()
                            .or_else(|| pdf_saves.last())
                            .copied()
                            .unwrap_or((0.0, 0.0));
                        c.extend_from_slice(
                            format!("q\n1 0 0 1 {:.4} {:.4} cm\n", x - old_x, y - old_y).as_bytes(),
                        );
                        pdf_content_coords.push((x, y));
                        continue;
                    }
                    if trimmed == "pdf:econtent" || trimmed == "pdf: econtent" {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let closed_origin = pdf_content_coords.pop();
                        if closed_origin.is_some() {
                            c.extend_from_slice(b"Q\n");
                        }
                        if pdf_suspend_flip && let Some((origin_x, origin_y)) = closed_origin {
                            let current_x = page_origin_x + pt(h);
                            let current_y = page_height - page_origin_y - pt(v);
                            pdf_suspended_offsets
                                .push((current_x - origin_x, current_y - origin_y));
                        } else if pdf_suspended_offsets.is_empty() && pdf_transform_depth == 0 {
                            pdf_content_advance = None;
                        }
                        pdf_suspend_flip = false;
                        continue;
                    }
                    if let Some(literal) = trimmed.strip_prefix("pdf:code") {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let literal = literal.trim();
                        pdf_suspend_flip = literal
                            .split_whitespace()
                            .eq(["q", "-1", "0", "0", "-1", "0", "0", "cm", "q"]);
                        c.extend_from_slice(literal.as_bytes());
                        c.push(b'\n');
                        if literal.split_whitespace().eq(["Q", "Q", "Q"]) {
                            if let Some((dx, dy)) = pdf_suspended_offsets.pop()
                                && (dx != 0.0 || dy != 0.0)
                            {
                                let (old_x, old_y) = pdf_content_advance
                                    .filter(|(_, _, depth)| *depth == pdf_transform_depth)
                                    .map(|(x, y, _)| (x, y))
                                    .unwrap_or((0.0, 0.0));
                                pdf_content_advance =
                                    Some((old_x + dx, old_y + dy, pdf_transform_depth));
                            }
                        }
                        continue;
                    }
                    if let Some(literal) = pdf_content_special(trimmed) {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let mut x = page_origin_x + pt(h);
                        let mut y = page_height - page_origin_y - pt(v);
                        if let Some((origin_x, origin_y)) = pdf_content_coords.last() {
                            x -= origin_x;
                            y -= origin_y;
                        }
                        c.extend_from_slice(
                            format!("q\n1 0 0 1 {x:.4} {y:.4} cm\n").as_bytes(),
                        );
                        c.extend_from_slice(literal.as_bytes());
                        c.extend_from_slice(b"\nQ\n");
                        continue;
                    }
                    if let Some(scale) = trimmed.strip_prefix("x:scale") {
                        close_text(&mut c, &mut text_state);
                        let numbers = scale
                            .split_whitespace()
                            .filter_map(|field| field.parse::<f64>().ok())
                            .collect::<Vec<_>>();
                        if numbers.len() >= 2 {
                            let sx = numbers[0];
                            let sy = numbers[1];
                            let mut x = page_origin_x + pt(h);
                            let mut y = page_height - page_origin_y - pt(v);
                            if let Some((origin_x, origin_y)) = pdf_content_coords.last() {
                                x -= origin_x;
                                y -= origin_y;
                            }
                            c.extend_from_slice(
                                format!(
                                    "{sx:.8} 0 0 {sy:.8} {:.8} {:.8} cm\n",
                                    (1.0 - sx) * x,
                                    (1.0 - sy) * y
                                )
                                .as_bytes(),
                            );
                        }
                        continue;
                    }
                    if let Some(transform) = trimmed.strip_prefix("pdf:btrans") {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let mut fields = transform.split_whitespace();
                        let kind = fields.next();
                        let numbers = fields
                            .filter_map(|field| field.parse::<f64>().ok())
                            .collect::<Vec<_>>();
                        let (mut a, mut b, mut cc, mut d, mut e, mut f) =
                            (1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
                        match kind {
                            Some("matrix") if numbers.len() >= 6 => {
                                (a, b, cc, d, e, f) = (
                                    numbers[0], numbers[1], numbers[2], numbers[3], numbers[4],
                                    numbers[5],
                                );
                            }
                            Some("rotate") if !numbers.is_empty() => {
                                let radians = numbers[0].to_radians();
                                (a, b, cc, d) =
                                    (radians.cos(), radians.sin(), -radians.sin(), radians.cos());
                            }
                            _ => {}
                        }
                        if let Some((dx, dy, depth)) = pdf_content_advance
                            && pdf_transform_depth == depth
                        {
                            e += dx;
                            f += dy;
                        }
                        pdf_transform_depth += 1;
                        let x = page_origin_x + pt(h);
                        let y = page_height - page_origin_y - pt(v);
                        let (origin_x, origin_y) =
                            pdf_content_coords.last().copied().unwrap_or((0.0, 0.0));
                        let cp_x = x - origin_x;
                        let cp_y = y - origin_y;
                        e += (1.0 - a) * cp_x - cc * cp_y;
                        f += (1.0 - d) * cp_y - b * cp_x;
                        c.extend_from_slice(
                            format!("q\n{a:.8} {b:.8} {cc:.8} {d:.8} {e:.8} {f:.8} cm\n")
                                .as_bytes(),
                        );
                        continue;
                    }
                    if trimmed == "pdf:etrans" {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        c.extend_from_slice(b"Q\n");
                        pdf_transform_depth = pdf_transform_depth.saturating_sub(1);
                        continue;
                    }
                    if trimmed.starts_with("pdf:bcolor") || trimmed == "pdf:ecolor" {
                        continue;
                    }

                    // Direct-pdf graphics primitives are carried through the
                    // private DVI as positioned specials.  pdfTeX establishes
                    // the current DVI point as the local PDF origin before a
                    // `\pdfsave`; matching that transform is what lets Xy-pic
                    // express all of its paths in small local coordinates.
                    if trimmed == "aitex:pdfsave" {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let x = page_origin_x + pt(h);
                        let y = page_height - page_origin_y - pt(v);
                        c.extend_from_slice(format!("1 0 0 1 {x:.4} {y:.4} cm\nq\n").as_bytes());
                        pdf_saves.push((x, y));
                        continue;
                    }
                    if trimmed == "aitex:pdfrestore" {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        if let Some((x, y)) = pdf_saves.pop() {
                            c.extend_from_slice(
                                format!("Q\n1 0 0 1 {:.4} {:.4} cm\n", -x, -y).as_bytes(),
                            );
                        }
                        continue;
                    }
                    if let Some(literal) = trimmed
                        .strip_prefix("aitex:pdfliteral-direct ")
                        .or_else(|| trimmed.strip_prefix("aitex:pdfliteral-page "))
                    {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        c.extend_from_slice(literal.as_bytes());
                        c.push(b'\n');
                        continue;
                    }
                    if let Some(literal) = trimmed
                        .strip_prefix("aitex:pdfliteral-setorigin ")
                        // Accept the pre-mode transport as setorigin so old
                        // checkpoints retain pdfTeX's default semantics.
                        .or_else(|| trimmed.strip_prefix("aitex:pdfliteral "))
                    {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let mut x = page_origin_x + pt(h);
                        let mut y = page_height - page_origin_y - pt(v);
                        // `setorigin` is relative to the current PDF origin.
                        // A surrounding `\pdfsave` or dvipdfmx content block
                        // has already translated that origin to its DVI point;
                        // applying the absolute page point again moves Xy-pic
                        // and similar literal paths twice.
                        if let Some((origin_x, origin_y)) =
                            pdf_content_coords.last().or_else(|| pdf_saves.last())
                        {
                            x -= origin_x;
                            y -= origin_y;
                        }
                        c.extend_from_slice(format!("1 0 0 1 {x:.4} {y:.4} cm\n").as_bytes());
                        c.extend_from_slice(literal.as_bytes());
                        c.push(b'\n');
                        pending_pdf_literal_origin = Some((x, y));
                        continue;
                    }
                    if let Some(matrix) = trimmed.strip_prefix("aitex:pdfsetmatrix ") {
                        close_text(&mut c, &mut text_state);
                        c.extend_from_slice(format!("{matrix} 0 0 cm\n").as_bytes());
                        continue;
                    }
                    if let Some(rest) = trimmed.strip_prefix("aitex:pdfcolorstackinit ") {
                        let mut fields = rest.splitn(2, ' ');
                        if let Some(stack) = fields.next().and_then(|value| value.parse::<i64>().ok())
                        {
                            let initial = fields.next().unwrap_or("0 g 0 G").to_string();
                            pdf_color_stacks
                                .entry(stack)
                                .or_insert_with(|| (initial, Vec::new()));
                        }
                        continue;
                    }
                    if let Some(rest) = trimmed.strip_prefix("aitex:pdfcolorstack ") {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let mut fields = rest.splitn(3, ' ');
                        if let (Some(stack), Some(action)) = (fields.next(), fields.next()) {
                            if let Ok(stack) = stack.parse::<i64>() {
                                let entry = pdf_color_stacks
                                    .entry(stack)
                                    .or_insert_with(|| (String::from("0 g 0 G"), Vec::new()));
                                match action {
                                    "push" => {
                                        let next = fields.next().unwrap_or_default().to_string();
                                        entry.1.push(entry.0.clone());
                                        entry.0 = next;
                                        c.extend_from_slice(entry.0.as_bytes());
                                        c.push(b'\n');
                                    }
                                    "set" => {
                                        entry.0 = fields.next().unwrap_or_default().to_string();
                                        c.extend_from_slice(entry.0.as_bytes());
                                        c.push(b'\n');
                                    }
                                    "pop" => {
                                        entry.0 = entry
                                            .1
                                            .pop()
                                            .unwrap_or_else(|| String::from("0 g 0 G"));
                                        c.extend_from_slice(entry.0.as_bytes());
                                        c.push(b'\n');
                                    }
                                    "current" => {
                                        c.extend_from_slice(entry.0.as_bytes());
                                        c.push(b'\n');
                                    }
                                    _ => {}
                                }
                            }
                        }
                        continue;
                    }

                    if let Some(rest) = trimmed.strip_prefix("aitex:pdfannot ") {
                        let mut fields = rest.splitn(4, ' ');
                        let parsed = fields
                            .next()
                            .and_then(|value| value.parse::<i64>().ok())
                            .zip(fields.next().and_then(|value| value.parse::<i64>().ok()))
                            .zip(fields.next().and_then(|value| value.parse::<i64>().ok()))
                            .zip(fields.next());
                        if let Some((((width, height), depth), dictionary)) = parsed {
                            let x = page_origin_x + pt(h);
                            let baseline = page_height - page_origin_y - pt(v);
                            let rect = [
                                x,
                                baseline - pt(depth),
                                x + pt(width),
                                baseline + pt(height),
                            ];
                            let body = dictionary
                                .strip_prefix("<<")
                                .and_then(|value| value.strip_suffix(">>"))
                                .unwrap_or(dictionary);
                            let body = replace_aitex_object_refs(body.as_bytes(), &named_ids);
                            let id = o.reserve();
                            o.write(
                                id,
                                format!(
                                    "<< /Type /Annot /Rect [{:.4} {:.4} {:.4} {:.4}] {} >>",
                                    rect[0].min(rect[2]).clamp(0.0, page_width),
                                    rect[1].min(rect[3]).clamp(0.0, page_height),
                                    rect[0].max(rect[2]).clamp(0.0, page_width),
                                    rect[1].max(rect[3]).clamp(0.0, page_height),
                                    String::from_utf8_lossy(&body),
                                )
                                .as_bytes(),
                            );
                            annotations.push(id);
                        }
                        continue;
                    }

                    if let Some(color) = special_color(trimmed) {
                        if ignore_postscript {
                            continue;
                        }
                        colors.push(color);
                        if emitted_color != Some(color) {
                            close_text(&mut c, &mut text_state);
                            c.extend_from_slice(color.operators().as_bytes());
                            emitted_color = Some(color);
                        }
                        continue;
                    }
                    if trimmed == "color pop" {
                        if ignore_postscript {
                            continue;
                        }
                        if colors.len() > 1 {
                            colors.pop();
                        }
                        let color = colors.last().copied().unwrap_or(Color::BLACK);
                        if emitted_color != Some(color) {
                            close_text(&mut c, &mut text_state);
                            c.extend_from_slice(color.operators().as_bytes());
                            emitted_color = Some(color);
                        }
                        continue;
                    }

                    if let Some(angle) = special_rotation_start(trimmed) {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        let x = page_origin_x + pt(h);
                        let y = page_height - page_origin_y - pt(v);
                        let (old_x, old_y) =
                            pdf_content_coords.last().copied().unwrap_or((0.0, 0.0));
                        let radians = angle.to_radians();
                        let (sin, cos) = radians.sin_cos();
                        c.extend_from_slice(
                            format!(
                                "q\n{cos:.10} {sin:.10} {:.10} {cos:.10} {:.4} {:.4} cm\n",
                                -sin,
                                x - old_x,
                                y - old_y
                            )
                            .as_bytes(),
                        );
                        pdf_content_coords.push((x, y));
                        rotations.push(Rotation { x, y, radians });
                        continue;
                    }
                    if trimmed.starts_with("ps: currentpoint grestore moveto") {
                        close_text(&mut c, &mut text_state);
                        emitted_color = None;
                        if rotations.pop().is_some() {
                            pdf_content_coords.pop();
                            c.extend_from_slice(b"Q\n");
                        }
                        continue;
                    }

                    if let Some(image) = special_dvipdfmx_image(trimmed) {
                        let graphic_scale = pdftex_graphic_scale.take();
                        close_text(&mut c, &mut text_state);
                        if core::mem::take(&mut ignore_next_driver_graphic) {
                            continue;
                        }
                        if let Some(asset) = resolve_asset(&image.name, image.page) {
                            let natural_bbox = asset
                                .raster_bbox
                                .or(image.bbox)
                                .unwrap_or([0.0, 0.0, 1.0, 1.0]);
                            let viewport = image.bbox.unwrap_or(natural_bbox);
                            let intrinsic_width =
                                (viewport[2] - viewport[0]).abs().max(0.001);
                            let intrinsic_height =
                                (viewport[3] - viewport[1]).abs().max(0.001);
                            let mut x = page_origin_x + pt(h);
                            let mut y = page_height - page_origin_y - pt(v);
                            let relative_origin =
                                pdf_saves.last().or_else(|| pdf_content_coords.last());
                            if let Some((origin_x, origin_y)) = relative_origin {
                                x -= origin_x;
                                y -= origin_y;
                            }
                            let form_key = (
                                image.name.clone(),
                                image.page,
                                natural_bbox.map(f64::to_bits),
                            );
                            let imported_pdf = if let Some(id) = imported_form_ids.get(&form_key) {
                                Some(*id)
                            } else {
                                asset.pdf.as_deref().and_then(|pdf| {
                                    // `bbox` in a dvipdfmx special is the
                                    // requested viewport, not the imported
                                    // page's Form box. pdfTeX keeps the full
                                    // natural page as the Form and translates
                                    // it relative to that viewport. Cropping
                                    // the Form here made author-supplied
                                    // `bb=...` options discard most of a PDF.
                                    let crop = natural_bbox.map(f64::to_bits);
                                    let content_key = (bytes_fingerprint(pdf), crop);
                                    let id = if let Some(id) =
                                        imported_form_content_ids.get(&content_key)
                                    {
                                        *id
                                    } else {
                                        let id = import_pdf_form(
                                            &mut o,
                                            pdf,
                                            natural_bbox,
                                        )?;
                                        imported_form_content_ids.insert(content_key, id);
                                        id
                                    };
                                    imported_form_ids.insert(form_key, id);
                                    Some(id)
                                })
                            };
                            if let Some(form_id) = imported_pdf {
                                // A normalized Form uses the same coordinates
                                // as the bbox carried by the special. Scale the
                                // requested crop itself, not a reconstructed
                                // full-image size; the latter double-counted
                                // trim offsets and distorted width-only images.
                                let (sx, sy) = graphic_scale.unwrap_or_else(|| {
                                    match (image.width, image.height) {
                                        (Some(w), Some(h)) => (
                                            pdftex_image_scale(w, intrinsic_width),
                                            pdftex_image_scale(h, intrinsic_height),
                                        ),
                                        (Some(w), None) => {
                                            let scale = pdftex_image_scale(w, intrinsic_width);
                                            (scale, scale)
                                        }
                                        (None, Some(h)) => {
                                            let scale = pdftex_image_scale(h, intrinsic_height);
                                            (scale, scale)
                                        }
                                        (None, None) => (1.0, 1.0),
                                    }
                                });
                                let sx = encoded_pdf_value(sx, 5);
                                let sy = encoded_pdf_value(sy, 5);
                                let tx = x - sx * viewport[0];
                                let ty = y - sy * viewport[1];
                                let clip_width = sx.abs() * intrinsic_width;
                                let clip_height = sy.abs() * intrinsic_height;
                                let resource = format!("Fm{form_id}");
                                let clipping = if image.clip {
                                    format!(
                                        "{x:.4} {y:.4} {clip_width:.8} {clip_height:.8} re W n "
                                    )
                                } else {
                                    String::new()
                                };
                                c.extend_from_slice(
                                    format!(
                                        "q {clipping}{IMPORTED_PAGE_INITIAL_COLOR} \
                                         {sx:.8} 0 0 {sy:.8} {tx:.3} {ty:.3} cm \
                                         /{resource} Do Q\n"
                                    )
                                    .as_bytes(),
                                );
                                xobjects.push((resource, form_id));
                                continue;
                            }

                            // Rasters we already understand are better embedded
                            // directly: rasterizing them through MuPDF changes
                            // sampling resolution and antialiasing before the
                            // page renderer sees them. PDFs use the imported
                            // Form above; unsupported raster encodings and
                            // procedure-heavy EPS use the host fallback.
                            embed_raster_image(
                                &mut o,
                                &mut raster_cache,
                                &image,
                                &asset,
                                graphic_scale,
                                x,
                                y,
                                &mut c,
                                &mut xobjects,
                            );
                        }
                        continue;
                    }

                    if let Some(eps) = special_eps(trimmed) {
                        let ignore_graphic = core::mem::take(&mut ignore_next_driver_graphic);
                        let precise_size = precise_eps_size.take();
                        // Stock pdfLaTeX ignores a raw EPS special from an
                        // explicitly selected dvipdfmx backend, but its
                        // begin-document epstopdf hook still converts and
                        // includes ordinary `\includegraphics{file.eps}`.
                        // The source adapter's exact-size companion marks that
                        // qualified path; an unaccompanied author special
                        // remains ignored.
                        if ignore_postscript
                            || (explicit_dvipdfmx && precise_size.is_none())
                            || ignore_graphic
                        {
                            continue;
                        }
                        close_text(&mut c, &mut text_state);
                        if let Some(asset) = resolve_asset(&eps.name, 1) {
                            let intrinsic_width = (eps.urx - eps.llx).abs().max(0.001);
                            let intrinsic_height = (eps.ury - eps.lly).abs().max(0.001);
                            let (width, height) = precise_size.unwrap_or_else(|| match (eps.width, eps.height) {
                                (Some(w), Some(h)) => (w, h),
                                (Some(w), None) => (w, w * intrinsic_height / intrinsic_width),
                                (None, Some(h)) => (h * intrinsic_width / intrinsic_height, h),
                                (None, None) => (intrinsic_width, intrinsic_height),
                            });
                            let mut x = page_origin_x + pt(h);
                            let mut y = page_height - page_origin_y - pt(v);
                            if let Some((origin_x, origin_y)) =
                                pdf_saves.last().or_else(|| pdf_content_coords.last())
                            {
                                x -= origin_x;
                                y -= origin_y;
                            }

                            // pdfLaTeX's standard EPS route runs repstopdf and
                            // embeds the resulting vector page. The host may
                            // provide that normalized page for PostScript too
                            // complicated for the dependency-free translator;
                            // preserve it as a Form before considering a
                            // bounded raster fallback.
                            let imported_pdf = asset.pdf.as_deref().and_then(|pdf| {
                                let page_bbox = first_page_effective_bbox(pdf)?;
                                // repstopdf translates the EPS natural box to
                                // the normalized page origin. The PSfile box,
                                // however, may be a smaller trim/viewport in
                                // the original EPS coordinates. Map that box
                                // into the normalized PDF page, but preserve
                                // the full Form unless `clip` was explicit.
                                let (form_bbox, viewport) = normalized_eps_form_and_viewport(
                                    page_bbox,
                                    asset.raster_bbox,
                                    [eps.llx, eps.lly, eps.urx, eps.ury],
                                    eps.clip,
                                );
                                let crop = form_bbox.map(f64::to_bits);
                                let content_key = (bytes_fingerprint(pdf), crop);
                                let form_id = if let Some(id) = imported_form_content_ids.get(&content_key) {
                                    Some(*id)
                                } else {
                                    import_pdf_form(&mut o, pdf, form_bbox).inspect(|id| {
                                        imported_form_content_ids.insert(content_key, *id);
                                    })
                                }?;
                                Some((form_id, viewport))
                            });
                            if let Some((form_id, bbox)) = imported_pdf {
                                let source_width = (bbox[2] - bbox[0]).abs().max(0.001);
                                let source_height = (bbox[3] - bbox[1]).abs().max(0.001);
                                let sx = width / source_width;
                                let sy = height / source_height;
                                let tx = x - sx * bbox[0];
                                let ty = y - sy * bbox[1];
                                let name = format!("Fm{form_id}");
                                c.extend_from_slice(
                                    format!(
                                        "q {IMPORTED_PAGE_INITIAL_COLOR} \
                                         {sx:.8} 0 0 {sy:.8} {tx:.4} {ty:.4} cm \
                                         /{name} Do Q\n"
                                    )
                                    .as_bytes(),
                                );
                                xobjects.push((name, form_id));
                                continue;
                            }
                            let (form_content, vector_supported) = eps_content(&asset.bytes);
                            let raster = (!vector_supported)
                                .then(|| asset.raster.as_deref().and_then(parse_raster))
                                .flatten();
                            let (form_id, matrix) = if let Some(raster) = raster {
                                let content_key = raster_fingerprint(&raster);
                                let image_id = raster_cache
                                    .by_content
                                    .get(&content_key)
                                    .map(|entry| entry.0)
                                    .unwrap_or_else(|| {
                                        let value = (
                                            write_raster_objects_with_masks(
                                                &mut o,
                                                &raster,
                                                &mut raster_cache.masks,
                                            ),
                                            raster.width,
                                            raster.height,
                                        );
                                        raster_cache.by_content.insert(content_key, value);
                                        value.0
                                    });
                                (image_id, (width, height, 0.0, 0.0))
                            } else {
                                let bbox = [
                                    eps.llx.to_bits(),
                                    eps.lly.to_bits(),
                                    eps.urx.to_bits(),
                                    eps.ury.to_bits(),
                                ];
                                let key = (bytes_fingerprint(&form_content), bbox);
                                let form_id = *eps_form_content_ids.entry(key).or_insert_with(|| {
                                    let form_id = o.reserve();
                                    let dictionary = format!(
                                        "<< /Type /XObject /Subtype /Form /BBox [{} {} {} {}] \
                                         /Resources << >>",
                                        eps.llx, eps.lly, eps.urx, eps.ury,
                                    );
                                    let form =
                                        o.stream_object(dictionary.as_bytes(), &form_content);
                                    o.write_owned(form_id, form);
                                    form_id
                                });
                                let sx = width / intrinsic_width;
                                let sy = height / intrinsic_height;
                                (form_id, (sx, sy, -sx * eps.llx, -sy * eps.lly))
                            };

                            let (sx, sy, dx, dy) = matrix;
                            let tx = x + dx;
                            let ty = y + dy;
                            let name = format!("Im{form_id}");
                            c.extend_from_slice(
                                format!("q {sx:.8} 0 0 {sy:.8} {tx:.4} {ty:.4} cm /{name} Do Q\n")
                                    .as_bytes(),
                            );
                            xobjects.push((name, form_id));
                        }
                        continue;
                    }

                    // Hyperref's dvips backend brackets a link or destination
                    // with these deterministic PostScript calls.  The
                    // following pdfmark special names what the bracket means.
                    if trimmed.starts_with("ps:SDict begin") && trimmed.ends_with("H.S end") {
                        link_start = Some((h, v));
                        link_end = None;
                        link_segments.clear();
                        continue;
                    }
                    if trimmed.starts_with("ps:SDict begin") && trimmed.ends_with("H.R end") {
                        link_end = Some((h, v));
                        continue;
                    }

                    if trimmed.contains("/ANN pdfmark") {
                        let target = special_string(trimmed, "/URI")
                            .map(LinkTarget::Uri)
                            .or_else(|| {
                                special_string(trimmed, "/Dest").map(LinkTarget::Destination)
                            });
                        if let (Some(target), Some(start)) = (target, link_start) {
                            let end = link_end.unwrap_or((h, v));
                            let action = match target {
                                LinkTarget::Uri(uri) => {
                                    format!("/A << /S /URI /URI {} >>", pdf_string(&uri))
                                }
                                LinkTarget::Destination(dest) => {
                                    format!("/Dest {}", pdf_string(&dest))
                                }
                            };
                            // Hyperref supplies annotation appearance through
                            // `\pdfstartlink attr{...}`. The engine carries
                            // those fields into this pdfmark-shaped special;
                            // preserve the safe scalar fields instead of
                            // replacing every link with an invisible border.
                            let mut appearance = String::new();
                            if let Some(border) = special_number_array(trimmed, "/Border") {
                                appearance.push_str(" /Border ");
                                appearance.push_str(&border);
                            }
                            if let Some(color) = special_number_array(trimmed, "/C") {
                                appearance.push_str(" /C ");
                                appearance.push_str(&color);
                            }
                            if let Some(highlight) = special_name(trimmed, "/H") {
                                appearance.push_str(" /H ");
                                appearance.push_str(&highlight);
                            }
                            if link_segments.is_empty() {
                                let sx = page_origin_x + pt(start.0);
                                let ex = page_origin_x + pt(end.0);
                                let sy = page_height - page_origin_y - pt(start.1);
                                let ey = page_height - page_origin_y - pt(end.1);
                                link_segments.push(LinkSegment {
                                    baseline: start.1,
                                    transform_depth: rotations.len(),
                                    rect: [
                                        sx.min(ex) - 1.0,
                                        sy.min(ey) - 2.0,
                                        sx.max(ex) + 1.0,
                                        sy.max(ey) + 10.0,
                                    ],
                                });
                            }
                            for segment in &link_segments {
                                let rect = [
                                    segment.rect[0].clamp(0.0, page_width),
                                    segment.rect[1].clamp(0.0, page_height),
                                    segment.rect[2].clamp(0.0, page_width),
                                    segment.rect[3].clamp(0.0, page_height),
                                ];
                                if rect[2] <= rect[0] || rect[3] <= rect[1] {
                                    continue;
                                }
                                let id = o.reserve();
                                o.write(
                                    id,
                                    format!(
                                        "<< /Type /Annot /Subtype /Link /Rect [{:.4} {:.4} {:.4} {:.4}] \
                                         {action}{appearance} >>",
                                        rect[0], rect[1], rect[2], rect[3],
                                    )
                                    .as_bytes(),
                                );
                                annotations.push(id);
                            }
                        }
                        link_start = None;
                        link_end = None;
                        link_segments.clear();
                        continue;
                    }

                    if trimmed.contains("/DEST pdfmark") {
                        if let Some(name) = special_string(trimmed, "/Dest") {
                            let at = link_start.unwrap_or((h, v));
                            destinations.entry(name).or_insert(Destination {
                                page: page_id,
                                x: page_origin_x + pt(at.0),
                                y: page_height - page_origin_y - pt(at.1) + 12.0,
                                view: special_destination_view(trimmed),
                            });
                        }
                        link_start = None;
                        link_end = None;
                        continue;
                    }

                    if trimmed.contains("/OUT pdfmark") {
                        if let (Some(title), Some(destination)) = (
                            special_string(trimmed, "/Title"),
                            special_string(trimmed, "/Dest"),
                        ) {
                            outlines.push(Outline {
                                title,
                                destination,
                                descendants: special_integer(trimmed, "/Count").unwrap_or(0),
                            });
                        }
                        continue;
                    }

                    if trimmed.contains("/DOCINFO pdfmark") {
                        for (pdf_key, key) in [
                            ("/Title", "Title"),
                            ("/Author", "Author"),
                            ("/Subject", "Subject"),
                            ("/Creator", "Creator"),
                            ("/Keywords", "Keywords"),
                            ("/Producer", "Producer"),
                        ] {
                            if let Some(value) = special_string(trimmed, pdf_key) {
                                doc_info.insert(key, value);
                            }
                        }
                        continue;
                    }

                    if trimmed.contains("/PageMode /UseOutlines")
                        || trimmed.contains("/PageMode/UseOutlines")
                    {
                        use_outlines = true;
                    }
                }
            }
        }
        if let Some((x, y)) = pending_pdf_literal_origin.take() {
            close_text(&mut c, &mut text_state);
            c.extend_from_slice(format!("1 0 0 1 {:.4} {:.4} cm\n", -x, -y).as_bytes());
        }
        persistent_colors = colors;
        if let Some(capture) = xobject_capture.take() {
            // A malformed/unclosed special must not leave a reserved object
            // with offset zero in the output xref. Preserve what was captured.
            let mut capture = capture;
            close_text(&mut capture.content, &mut text_state);
            write_captured_xobject(&mut o, capture, b"", &named_ids);
        }
        close_text(&mut c, &mut text_state);

        let mut positioned_resources = Vec::new();
        if let Some(page) = positioned_page {
            for item in &page.items {
                let aitex_page::Item::GlyphRun(run) = item else {
                    continue;
                };
                let font = positioned_font_objs
                    .get(&run.font)
                    .ok_or(PositionedPdfError::MissingFont(run.font))?;
                write_positioned_run(&mut c, page_height, run, font, None)?;
                let (name, id) = font
                    .resource(run.direction)
                    .ok_or(PositionedPdfError::UnsupportedDirection(run.direction))?;
                positioned_resources.push(format!("/{name} {id} 0 R"));
            }
        }

        let mut res = page_font_resources(page, &font_objs);
        positioned_resources.sort();
        positioned_resources.dedup();
        if !res.is_empty() && !positioned_resources.is_empty() {
            res.push(' ');
        }
        res.push_str(&positioned_resources.join(" "));
        xobjects.sort_unstable();
        xobjects.dedup();
        let xres = xobjects
            .iter()
            .map(|(name, id)| format!("/{name} {id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        let extra_resources =
            String::from_utf8(pdf_resource_fragments(&page_resource_fragments, &named_ids))
                .unwrap_or_default();
        let resource_id = o.write_interned(
            &mut resource_objects,
            format!("<< /Font << {res} >> /XObject << {xres} >>{extra_resources} >>")
                .into_bytes(),
        );
        let annots = if annotations.is_empty() {
            String::new()
        } else {
            format!(
                " /Annots [{}]",
                annotations
                    .iter()
                    .map(|id| format!("{id} 0 R"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let rotation = page_rotation
            .filter(|angle| *angle != 0)
            .map(|angle| format!(" /Rotate {angle}"))
            .unwrap_or_default();
        let page_extra = pdf_dictionary_fragments(
            &page_fragments,
            &named_ids,
            Some(NamedObjectPosition {
                x: 0.0,
                y: 0.0,
                page: page_id,
            }),
        );
        o.write(
            page_id,
            format!(
                "<< /Type /Page /Parent {page_parent} 0 R /MediaBox [0 0 {page_width:.2} {page_height:.2}] \
                 /Resources {resource_id} 0 R \
                 /Contents {content_id} 0 R{annots}{rotation}{} >>",
                String::from_utf8_lossy(&page_extra),
            )
            .as_bytes(),
        );
        let cache = if let (Some(key), Some(before)) = (cache_key, destinations_before)
            && annotations.is_empty()
            && xobjects.is_empty()
            && page_resource_fragments.is_empty()
        {
            let cached_destinations = destinations
                .iter()
                .filter(|(name, destination)| {
                    !before.contains(*name) && destination.page == page_id
                })
                .map(|(name, destination)| CachedPageDestination {
                    name: name.clone(),
                    x: destination.x,
                    y: destination.y,
                    view: destination.view,
                })
                .collect();
            Some(PendingPageCache {
                key,
                destinations: cached_destinations,
            })
        } else {
            None
        };
        pending_page_bytes = pending_page_bytes.saturating_add(c.len());
        pending_page_streams.push(PendingPageStream {
            id: content_id,
            raw_len: c.len(),
            content: PendingPageContent::Raw(c),
            cache,
        });
        if pending_page_streams.len() >= PAGE_STREAM_BATCH_COUNT
            || pending_page_bytes >= PAGE_STREAM_BATCH_BYTES
        {
            flush_page_streams(&mut o, &mut pending_page_streams);
            pending_page_bytes = 0;
        }
    }
    flush_page_streams(&mut o, &mut pending_page_streams);

    for (id, body, position) in pending_named_annotations {
        o.write(
            id,
            &replace_aitex_object_refs(
                &replace_named_refs_at(&body, &named_ids, Some(position)),
                &named_ids,
            ),
        );
    }

    for object in named_objects.values() {
        let body = merged_named_object(object, &named_ids);
        if let Some(stream) = &object.stream {
            // pdfTeX's `\pdfobj stream attr{...}` accepts dictionary entries,
            // not a complete `<< ... >>` object. `Objects::write_stream`, on
            // the other hand, deliberately takes a dictionary beginning with
            // `<<` and appends `/Length` plus the closing delimiter. Passing
            // the raw attributes produced malformed objects such as
            // `/N 3 /Filter ... >> stream`, which permissive readers rendered
            // but MuPDF correctly rejected. Also accept a complete dictionary
            // from the private transport for forward compatibility.
            let attributes = body.trim_ascii();
            let attributes = attributes
                .strip_prefix(b"<<")
                .and_then(|inner| inner.strip_suffix(b">>"))
                .unwrap_or(attributes)
                .trim_ascii();
            let mut dictionary = b"<<".to_vec();
            if !attributes.is_empty() {
                dictionary.push(b' ');
                dictionary.extend_from_slice(attributes);
            }
            o.write_stream(object.id, &dictionary, stream);
        } else {
            o.write(object.id, &body);
        }
    }

    let pages_extra = pdf_dictionary_fragments(&pages_fragments, &named_ids, None);
    page_tree.write(&mut o, pages, &pages_extra);
    let names_id = if destinations.is_empty() {
        None
    } else {
        let id = o.reserve();
        let entries = destinations
            .iter()
            .map(|(name, dest)| {
                let view = match dest.view {
                    DestinationView::Xyz => format!("/XYZ {:.4} {:.4} null", dest.x, dest.y),
                    DestinationView::Fit => String::from("/Fit"),
                    DestinationView::FitB => String::from("/FitB"),
                    DestinationView::FitH => format!("/FitH {:.4}", dest.y),
                    DestinationView::FitBH => format!("/FitBH {:.4}", dest.y),
                    DestinationView::FitV => format!("/FitV {:.4}", dest.x),
                    DestinationView::FitBV => format!("/FitBV {:.4}", dest.x),
                };
                format!("{} [{} 0 R {view}]", pdf_string(name), dest.page)
            })
            .collect::<Vec<_>>()
            .join(" ");
        o.write(id, format!("<< /Names [{entries}] >>").as_bytes());
        Some(id)
    };

    let outlines_id = if outlines.is_empty() {
        None
    } else {
        let root = o.reserve();
        let ids: Vec<usize> = outlines.iter().map(|_| o.reserve()).collect();
        let mut parents = vec![None; outlines.len()];
        let mut children = vec![Vec::<usize>::new(); outlines.len()];
        let mut top = Vec::<usize>::new();
        let mut stack = Vec::<(usize, usize)>::new();
        for (i, outline) in outlines.iter().enumerate() {
            while stack.last().is_some_and(|(_, remaining)| *remaining == 0) {
                stack.pop();
            }
            parents[i] = stack.last().map(|(parent, _)| *parent);
            if let Some(parent) = parents[i] {
                children[parent].push(i);
            } else {
                top.push(i);
            }
            for (_, remaining) in &mut stack {
                *remaining = remaining.saturating_sub(1);
            }
            let descendants = outline.descendants.unsigned_abs() as usize;
            if descendants > 0 {
                stack.push((i, descendants));
            }
        }
        for (i, (outline, id)) in outlines.iter().zip(ids.iter()).enumerate() {
            let siblings = parents[i].map_or(top.as_slice(), |parent| children[parent].as_slice());
            let position = siblings
                .iter()
                .position(|candidate| *candidate == i)
                .unwrap_or(0);
            let prev = position
                .checked_sub(1)
                .map(|j| format!(" /Prev {} 0 R", ids[siblings[j]]))
                .unwrap_or_default();
            let next = siblings
                .get(position + 1)
                .map(|next| format!(" /Next {} 0 R", ids[*next]))
                .unwrap_or_default();
            let parent = parents[i].map_or(root, |parent| ids[parent]);
            let first = children[i]
                .first()
                .map(|child| format!(" /First {} 0 R", ids[*child]))
                .unwrap_or_default();
            let last = children[i]
                .last()
                .map(|child| format!(" /Last {} 0 R", ids[*child]))
                .unwrap_or_default();
            let count = (!children[i].is_empty())
                .then(|| format!(" /Count {}", outline.descendants))
                .unwrap_or_default();
            o.write(
                *id,
                format!(
                    "<< /Title {} /Parent {parent} 0 R /Dest {}{prev}{next}{first}{last}{count} >>",
                    pdf_hex(&outline.title),
                    pdf_string(&outline.destination)
                )
                .as_bytes(),
            );
        }
        o.write(
            root,
            format!(
                "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
                ids[*top.first().unwrap_or(&0)],
                ids[*top.last().unwrap_or(&(ids.len() - 1))],
                ids.len()
            )
            .as_bytes(),
        );
        Some(root)
    };

    let info_id = if doc_info.is_empty() {
        None
    } else {
        let id = o.reserve();
        let entries = doc_info
            .iter()
            .map(|(key, value)| format!("/{key} {}", pdf_hex(value)))
            .collect::<Vec<_>>()
            .join(" ");
        o.write(id, format!("<< {entries} >>").as_bytes());
        Some(id)
    };

    let names_extra = pdf_dictionary_fragments(&names_fragments, &named_ids, None);
    let names = if names_id.is_some() || !names_extra.is_empty() {
        format!(
            " /Names <<{}{} >>",
            names_id
                .map(|id| format!(" /Dests {id} 0 R"))
                .unwrap_or_default(),
            String::from_utf8_lossy(&names_extra),
        )
    } else {
        String::new()
    };
    let outline = outlines_id
        .map(|id| format!(" /Outlines {id} 0 R"))
        .unwrap_or_default();
    let page_mode = if use_outlines {
        " /PageMode /UseOutlines"
    } else {
        ""
    };
    let catalog_extra = pdf_dictionary_fragments(&catalog_fragments, &named_ids, None);
    o.write(
        catalog,
        format!(
            "<< /Type /Catalog /Pages {pages} 0 R{names}{outline}{page_mode}{} >>",
            String::from_utf8_lossy(&catalog_extra),
        )
        .as_bytes(),
    );
    Ok(o.finish(catalog, info_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classic(font: u32, code: u32, h: i64, v: i64) -> dvi::Item {
        dvi::Item::ClassicGlyph {
            font: aitex_page::FontInstanceId(font),
            code,
            origin_x: aitex_page::Sp(h),
            origin_y: aitex_page::Sp(v),
            advance: aitex_page::Sp(0),
        }
    }

    /// Existing structural tests intentionally inspect readable PDF operators.
    /// Production callers use the compact default; dedicated tests below cover
    /// that encoding and its exact decoded contents.
    fn write(
        doc: &dvi::Document,
        page_width: f64,
        page_height: f64,
        resolve: impl FnMut(&str, i64) -> Option<FontSource>,
    ) -> Vec<u8> {
        super::write_with_options(
            doc,
            page_width,
            page_height,
            PdfOptions::uncompressed(),
            resolve,
        )
    }

    fn write_with_assets(
        doc: &dvi::Document,
        page_width: f64,
        page_height: f64,
        resolve: impl FnMut(&str, i64) -> Option<FontSource>,
        resolve_asset: impl FnMut(&str, u32) -> Option<AssetSource>,
    ) -> Vec<u8> {
        super::write_with_assets_options(
            doc,
            page_width,
            page_height,
            PdfOptions::uncompressed(),
            resolve,
            resolve_asset,
        )
    }

    fn push_special(page: &mut dvi::Page, h: i64, v: i64, text: &str) {
        page.items.push(dvi::Item::Special {
            x: aitex_page::Sp(h),
            y: aitex_page::Sp(v),
            bytes: text.as_bytes().to_vec(),
        });
    }

    #[test]
    fn generated_pdf_numbers_are_compact_and_never_negative_zero() {
        assert_eq!(pdf_number(1.0, 6), "1");
        assert_eq!(pdf_number(9.962640099626402, 8), "9.9626401");
        assert_eq!(pdf_number(-0.0000001, 6), "0");
        assert_eq!(pdf_number(0.167, 6), "0.167");
        assert_eq!(pdf_number(f64::INFINITY, 6), "0");

        let seven_point = 7 * 65_536;
        let pdf_size = pdftex_font_size(seven_point);
        assert_eq!(pdf_size, 458_749);
        assert_eq!(
            pdf_number(261_235.0 * 1000.0 / pdf_size as f64, 1),
            "569.5",
            "CMR7 width 18 follows pdfTeX's retained font size"
        );
        assert_eq!(
            pdf_number(238_206.0 * 1000.0 / pdf_size as f64, 1),
            "519.3",
            "CMMI7 width 14 follows pdfTeX's retained font size"
        );

        for precision in [0, 2, 4, 6, 8, 10] {
            for value in [-2.5, -0.5, 0.5, 1.5, 2.5, 9.962640099626402] {
                let (rendered, encoded) = encoded_pdf_number(value, precision);
                let parsed = rendered.parse::<f64>().expect("generated PDF number");
                assert_eq!(parsed, encoded, "{value} at precision {precision}");
            }
            for numerator in -1_000..=1_000 {
                let value = f64::from(numerator) * std::f64::consts::PI / 37.0;
                let (rendered, encoded) = encoded_pdf_number(value, precision);
                let parsed = rendered.parse::<f64>().expect("generated PDF number");
                assert_eq!(parsed, encoded, "{value} at precision {precision}");
            }
        }
    }

    #[test]
    fn png_matrix_extent_recovers_pdftex_integer_dpi_from_fixed_point_bbox() {
        assert_eq!(
            pdftex_raster_matrix_extent(1224, 611.99852),
            612.0
        );
        assert_eq!(
            pdftex_raster_matrix_extent(1168, 583.99858),
            584.0
        );
        assert_eq!(
            pdftex_raster_matrix_extent(8, 3.7),
            3.7,
            "an arbitrary converted-raster extent must not be quantized"
        );
        assert_eq!(
            encoded_pdf_value(
                pdftex_image_scale(512.13487 * 72.0 / 72.27, 3568.991350),
                5
            ),
            0.14296,
            "an unaccompanied special uses its requested box directly"
        );
    }

    fn first_flate_stream(pdf: &[u8]) -> Vec<u8> {
        use std::io::Read;

        let marker = b"/Filter /FlateDecode /Length ";
        let at = pdf
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("Flate stream");
        let length_start = at + marker.len();
        let length_end = pdf[length_start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map(|offset| length_start + offset)
            .expect("stream length terminator");
        let length = std::str::from_utf8(&pdf[length_start..length_end])
            .expect("ASCII stream length")
            .parse::<usize>()
            .expect("numeric stream length");
        let stream_marker = b"stream\n";
        let stream_start = pdf[length_end..]
            .windows(stream_marker.len())
            .position(|window| window == stream_marker)
            .map(|offset| length_end + offset + stream_marker.len())
            .expect("stream body");
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(&pdf[stream_start..stream_start + length])
            .read_to_end(&mut decoded)
            .expect("valid zlib stream");
        decoded
    }

    #[test]
    fn compact_output_flate_encodes_repetitive_streams_deterministically() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        for row in 0..200 {
            page.items.push(dvi::Item::Rule {
                x: aitex_page::Sp(0),
                baseline_y: aitex_page::Sp(row * 65_536),
                width: aitex_page::Sp(6_553_600),
                height: aitex_page::Sp(32_768),
            });
        }
        doc.pages.push(page);

        let compact = super::write(&doc, 612.0, 792.0, |_, _| None);
        let repeated = super::write(&doc, 612.0, 792.0, |_, _| None);
        let plain = write(&doc, 612.0, 792.0, |_, _| None);
        assert_eq!(compact, repeated, "compression is not deterministic");
        assert!(
            compact
                .windows(b"/Filter /FlateDecode".len())
                .any(|window| window == b"/Filter /FlateDecode")
        );
        assert!(first_flate_stream(&compact).windows(4).any(|w| w == b"re f"));
        assert!(compact.len() < plain.len() / 2);
    }

    #[test]
    fn short_streams_stay_plain_when_compression_would_make_them_larger() {
        let objects = Objects::new();
        let stream = objects.stream_object(b"<<", b"x");
        assert!(!stream.windows(12).any(|window| window == b"/FlateDecode"));
        assert!(stream.windows(1).any(|window| window == b"x"));
    }

    #[test]
    fn compression_worker_budget_does_not_change_stream_bytes() {
        let contents = (0..12)
            .map(|index| {
                (0..131_072)
                    .map(|offset| ((offset * 31 + index * 17) % 251) as u8)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let content_refs = contents
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>();
        let one = Objects::with_options(PdfOptions {
            compress_streams: true,
            compression_workers: 1,
            compact_objects: true,
            subset_type1: true,
        });
        let four = Objects::with_options(PdfOptions {
            compress_streams: true,
            compression_workers: 4,
            compact_objects: true,
            subset_type1: true,
        });

        assert_eq!(one.options.stream_format_tag(), four.options.stream_format_tag());
        assert_eq!(
            one.stream_objects(b"<<", &content_refs),
            four.stream_objects(b"<<", &content_refs)
        );
    }

    #[test]
    fn identical_type1_resources_are_shared_across_dvi_sizes() {
        let clear = vec![b'A'; 1024];
        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        pfb.extend_from_slice(&clear);
        pfb.extend_from_slice(&[0x80, 2, 3, 0, 0, 0, 1, 2, 3]);
        pfb.extend_from_slice(&[0x80, 1, 2, 0, 0, 0, b'0', b'0']);
        pfb.extend_from_slice(&[0x80, 3]);
        let pfb: std::sync::Arc<[u8]> = pfb.into();

        let mut doc = dvi::Document::default();
        for (number, at) in [(0, 655_360), (1, 1_310_720)] {
            doc.fonts.insert(
                number,
                dvi::FontDef {
                    name: "example".into(),
                    at,
                    design: 655_360,
                    checksum: 0,
                },
            );
        }
        let mut page = dvi::Page::default();
        page.items.push(classic(0, 65, 0, 0));
        page.items.push(classic(1, 65, 655_360, 0));
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, at| {
            Some(FontSource {
                postscript_name: "Example".into(),
                pfb: Some(std::sync::Arc::clone(&pfb)),
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: vec![None; 256].into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![at / 2; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/Length1 ").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/Type /FontDescriptor").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/Subtype /Type1").count(), 1, "{pdf}");
    }

    #[test]
    fn one_classic_face_serves_multiple_sizes_and_encodings() {
        let clear = vec![b'A'; 1024];
        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        pfb.extend_from_slice(&clear);
        pfb.extend_from_slice(&[0x80, 2, 3, 0, 0, 0, 1, 2, 3]);
        pfb.extend_from_slice(&[0x80, 1, 2, 0, 0, 0, b'0', b'0']);
        pfb.extend_from_slice(&[0x80, 3]);
        let face = std::sync::Arc::new(ClassicFontFace::type1(
            pfb,
            "Example",
            0.0,
            1.0,
            ClassicEmbeddingPolicy::Subset,
            [0, 0, 1000, 1000],
        ));

        let mut doc = dvi::Document::default();
        for (number, name, at) in [
            (0, "example-a", 655_360),
            (1, "example-a", 1_310_720),
            (2, "example-c", 655_360),
        ] {
            doc.fonts.insert(
                number,
                dvi::FontDef {
                    name: name.into(),
                    at,
                    design: 655_360,
                    checksum: 0,
                },
            );
        }
        let mut page = dvi::Page::default();
        page.items.push(classic(0, 65, 0, 0));
        page.items.push(classic(1, 65, 655_360, 0));
        page.items.push(classic(2, 66, 1_310_720, 0));
        doc.pages.push(page);

        let out = super::write_with_classic_assets_options(
            &doc,
            612.0,
            792.0,
            PdfOptions::uncompressed(),
            |name, at| {
                let alternate = name == "example-c";
                let mut encoding = vec![".notdef".to_string(); 256];
                encoding[if alternate { 66 } else { 65 }] =
                    if alternate { "C" } else { "A" }.to_string();
                let mut to_unicode = vec![None; 256];
                to_unicode[if alternate { 66 } else { 65 }] = Some(
                    if alternate { "0043" } else { "0041" }.to_string(),
                );
                Some(ClassicFontUse {
                    program: ClassicFontProgram::Type1(std::sync::Arc::clone(&face)),
                    encoding: Some(encoding.into()),
                    to_unicode: to_unicode.into(),
                    widths: vec![at / 2; 256].into(),
                })
            },
            |_, _| None,
        );
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/Length1 ").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/Type /FontDescriptor").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/Subtype /Type1").count(), 2, "{pdf}");
        assert_eq!(pdf.matches("/ToUnicode ").count(), 2, "{pdf}");
    }

    #[test]
    fn classic_eight_bit_truetype_map_embeds_fontfile2() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "mapped-ttf".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, 65, 0, 0));
        doc.pages.push(page);
        let mut encoding = vec![".notdef".to_string(); 256];
        encoding[65] = "A".to_string();
        let out = super::write_with_classic_assets_options(
            &doc,
            612.0,
            792.0,
            PdfOptions::uncompressed(),
            |_, at| {
                Some(ClassicFontUse {
                    program: ClassicFontProgram::TrueType {
                        bytes: std::sync::Arc::from(b"\0\x01\0\0test-truetype".as_slice()),
                        postscript_name: "MappedTrueType".to_string(),
                        slant: 0.0,
                        extend: 1.0,
                        bbox: [0, -200, 1000, 800],
                    },
                    encoding: Some(encoding.clone().into()),
                    to_unicode: vec![None; 256].into(),
                    widths: vec![at / 2; 256].into(),
                })
            },
            |_, _| None,
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Subtype /TrueType"), "{pdf}");
        assert!(pdf.contains("/FontFile2 "), "{pdf}");
        assert!(pdf.contains("test-truetype"), "{pdf}");
    }

    #[test]
    fn page_resources_name_only_fonts_used_on_that_page() {
        let fonts = [(0, 11), (1, 22)]
            .into_iter()
            .map(|(number, id)| {
                (
                    number,
                    PdfFont {
                        id,
                        advances: vec![0; 256],
                        encoded_advances: vec![0.0; 256],
                        at: 655_360,
                        extend: 1.0,
                        slant: 0.0,
                    },
                )
            })
            .collect();
        let mut page = dvi::Page::default();
        page.items.push(classic(1, 65, 0, 0));
        assert_eq!(page_font_resources(&page, &fonts), "/F22 22 0 R");
    }

    #[test]
    fn an_empty_document_is_still_a_pdf() {
        let doc = dvi::Document::default();
        let out = write(&doc, 612.0, 792.0, |_, _| None);
        assert!(out.starts_with(b"%PDF-"));
        assert!(out.ends_with(b"%%EOF\n"));
        assert!(String::from_utf8_lossy(&out).contains("/Type /Catalog"));
    }

    /// The page count is what `done_check` compares against pdfTeX, so a page
    /// tree that says the wrong number is the one error that would look like
    /// success.
    #[test]
    fn the_page_tree_counts_its_pages() {
        let mut doc = dvi::Document::default();
        doc.pages.push(dvi::Page::default());
        doc.pages.push(dvi::Page::default());
        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/Type /Pages /Count 2"), "{s}");
    }

    /// A page's folio sits eleven inches below its first line. If both go into
    /// one `Tj` string the viewer draws them side by side, because a string
    /// carries no positions — this is the bug the run bookkeeping exists for,
    /// and it shipped once.
    #[test]
    fn a_glyph_on_another_line_starts_another_run() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655360,
                design: 655360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, b'A' as u32, 0, 0));
        page
            .items
            .push(classic(0, b'B' as u32, 0, 40_000_000)); // the foot of the page
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "CMR10".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![327_680; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("BT ").count(), 1, "{s}");
        assert_eq!(s.matches(" Tf ").count(), 1, "{s}");
        assert!(s.contains(" 9.9626 Tf "), "{s}");
        assert_eq!(s.matches("Tm [(").count(), 1, "{s}");
        assert_eq!(s.matches("Td [(").count(), 1, "{s}");
        assert!(!s.contains("(AB)"), "the two glyphs share a run: {s}");
    }

    #[test]
    fn a_graphics_operator_closes_the_classic_text_object() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, b'A' as u32, 0, 0));
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(0),
            baseline_y: aitex_page::Sp(655_360),
            width: aitex_page::Sp(327_680),
            height: aitex_page::Sp(65_536),
        });
        page.items.push(classic(0, b'B' as u32, 0, 1_310_720));
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "CMR10".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![327_680; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("BT ").count(), 2, "{s}");
        assert_eq!(s.matches(" ET\n").count(), 2, "{s}");
        assert!(s.contains(")] TJ ET\n72 710.0374 4.9813 0.9963 re f\nBT "), "{s}");
    }

    #[test]
    fn redundant_colors_do_not_split_text_but_raw_literals_invalidate_them() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "color push rgb 0 0 0");
        page.items.push(classic(0, b'A' as u32, 0, 0));
        push_special(&mut page, 0, 0, "color push gray 0");
        page.items.push(classic(0, b'B' as u32, 0, 655_360));
        push_special(&mut page, 0, 0, "aitex:pdfliteral 1 0 0 rg");
        push_special(&mut page, 0, 0, "color pop");
        page.items.push(classic(0, b'C' as u32, 0, 1_310_720));
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "CMR10".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![327_680; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("BT ").count(), 2, "{s}");
        assert_eq!(s.matches("0 0 0 rg").count(), 1, "{s}");
        let literal = s.find("1 0 0 rg").expect("red literal");
        let reset = s[literal..].find("0 0 0 rg 0 0 0 RG").expect("color reset");
        assert!(reset > 0, "{s}");
    }

    #[test]
    fn dvi_color_stack_survives_a_page_break() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(&mut first, 0, 0, "color push rgb 1 0 0");
        first.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(0),
            baseline_y: aitex_page::Sp(65_536),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        let mut second = dvi::Page::default();
        second.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(0),
            baseline_y: aitex_page::Sp(65_536),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        push_special(&mut second, 0, 0, "color pop");
        second.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(131_072),
            baseline_y: aitex_page::Sp(65_536),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        doc.pages.extend([first, second]);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(
            pdf.matches("1 0 0 rg 1 0 0 RG").count(),
            2,
            "red must be selected both where it is pushed and at page two's start: {pdf}"
        );
        let second_red = pdf.rfind("1 0 0 rg 1 0 0 RG").expect("page two red");
        let reset = pdf[second_red..]
            .find("0 0 0 rg 0 0 0 RG")
            .expect("page two pop restores black");
        assert!(reset > 0, "{pdf}");
    }

    /// Two glyphs a word-space apart on one line stay in one run, with the gap
    /// as a `TJ` displacement — otherwise every space on every page opens a new
    /// text object, and the displacement is what makes that unnecessary.
    #[test]
    fn a_gap_on_the_same_line_becomes_a_tj_displacement() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655360,
                design: 655360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, b'A' as u32, 0, 0));
        // 327680sp of glyph, then a 327680sp gap: half the font size, so the
        // displacement is half of 1000.
        page.items.push(classic(0, b'B' as u32, 655_360, 0));
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "CMR10".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![327_680; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("Tm [(").count(), 1, "{s}");
        assert!(s.contains("(A)-500(B)"), "{s}");
    }

    #[test]
    fn pdftex_decimal_widths_fold_rounding_into_the_next_real_gap() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let advance = 291_271;
        let mut page = dvi::Page::default();
        page.items.push(classic(0, b'A' as u32, 0, 0));
        page.items.push(classic(0, b'B' as u32, advance, 0));
        page
            .items
            .push(classic(0, b'C' as u32, advance * 2 + 6_554, 0));
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "CMR10".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![advance; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/FirstChar 65 /LastChar 67"), "{pdf}");
        assert!(pdf.contains("/Widths [444.4 444.4"), "{pdf}");
        assert!(pdf.contains("[(AB)-10(C)] TJ"), "{pdf}");
    }

    #[test]
    fn map_slant_and_extension_transform_the_outline_but_not_the_dvi_advance() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "phvr8rn".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, b'A' as u32, 0, 0));
        page.items.push(classic(0, b'B' as u32, 262_144, 0));
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "NimbusSansNarrow-Regular".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.167,
                extend: 0.8,
                // 0.4 em after extension; the base outline width is 0.5 em.
                widths: vec![262_144; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Widths [500 500"), "{pdf}");
        assert!(pdf.contains("0.8 0 0.167 1"), "{pdf}");
        assert!(pdf.contains("[(AB)] TJ"), "{pdf}");
    }

    /// The engine carries pdfTeX's substituted-font choice through its private
    /// DVI as specials.  The driver must use that choice both for the glyph
    /// outline and for text-run advance bookkeeping; otherwise expanded words
    /// look wider while the following DVI positions still assume base widths.
    #[test]
    fn private_font_expansion_scales_text_and_resets_between_runs() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:fontexpand 20");
        page.items.push(classic(0, b'A' as u32, 0, 0));
        // A 0.5em base advance expanded by 2% is 334234sp after rounding.
        page.items.push(classic(0, b'B' as u32, 334_234, 0));
        push_special(&mut page, 668_468, 0, "aitex:fontexpand 0");
        page.items.push(classic(0, b'C' as u32, 668_468, 0));
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "CMR10".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: Vec::new().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![327_680; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("1.02 0 0 1"), "{pdf}");
        assert!(pdf.contains("[(AB)] TJ"), "{pdf}");
        assert!(pdf.contains("1 0 0 1"), "{pdf}");
        assert!(pdf.contains("[(C)] TJ"), "{pdf}");
    }

    #[test]
    fn prepared_type1_is_byte_identical_to_inline_preparation() {
        let clear =
            b"%!PS-AdobeFont\n/FontName /Example def\n/FontMatrix [0.001 0 0 0.001 0 0] def\n";
        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        pfb.extend_from_slice(clear);
        pfb.extend_from_slice(&[0x80, 2, 3, 0, 0, 0, 1, 2, 3]);
        pfb.extend_from_slice(&[0x80, 1, 2, 0, 0, 0, b'0', b'0']);
        pfb.extend_from_slice(&[0x80, 3]);
        let prepared = prepare_type1_program(&pfb, "Example", 0.167, 0.82)
            .expect("prepares Type 1 program");

        let mut document = dvi::Document::default();
        document.fonts.insert(
            0,
            dvi::FontDef {
                name: "example".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, 65, 0, 0));
        document.pages.push(page);
        let source = |pfb, prepared_type1| FontSource {
            postscript_name: "Example".into(),
            pfb,
            prepared_type1,
            pk: None,
            encoding: None,
            to_unicode: vec![None; 256].into(),
            slant: 0.167,
            extend: 0.82,
            widths: vec![327_680; 256].into(),
            bbox: [0, 0, 1000, 1000],
        };
        let inline = write(&document, 612.0, 792.0, |_, _| {
            Some(source(Some(pfb.clone().into()), None))
        });
        let cached = write(&document, 612.0, 792.0, |_, _| {
            Some(source(None, Some(prepared.clone())))
        });
        assert_eq!(inline, cached);
    }

    #[test]
    fn reencoded_font_gets_its_postscript_name_and_to_unicode_cmap() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "ec-lmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, 65, 0, 0));
        doc.pages.push(page);

        let mut encoding = vec![".notdef".to_string(); 256];
        encoding[0] = "Gamma".to_string();
        encoding[65] = "A".to_string();
        let mut to_unicode = vec![None; 256];
        to_unicode[0] = Some("0393".to_string());
        to_unicode[65] = Some("0041".to_string());
        let out = write(&doc, 612.0, 792.0, |_, _| {
            Some(FontSource {
                postscript_name: "LMRoman10-Regular".to_string(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: Some(encoding.clone().into()),
                to_unicode: to_unicode.clone().into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![327_680; 256].into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/BaseFont /LMRoman10-Regular"), "{pdf}");
        assert!(pdf.contains("/Differences [0 /Gamma /.notdef"), "{pdf}");
        assert!(pdf.contains("/ToUnicode "), "{pdf}");
        assert!(pdf.contains("<00> <0393>"), "{pdf}");
        assert!(pdf.contains("<41> <0041>"), "{pdf}");
    }

    #[test]
    fn postscript_strings_decode_octal_and_balanced_parentheses() {
        assert_eq!(
            special_string(r"[/Title (\376\377\000A\(B\)) /DOCINFO", "/Title"),
            Some(vec![0xfe, 0xff, 0, b'A', b'(', b'B', b')'])
        );
        assert_eq!(
            special_string(
                r"[/Action <</Subtype/URI/URI(https://example.test/a?x=1)>> /ANN",
                "/URI"
            ),
            Some(b"https://example.test/a?x=1".to_vec())
        );
        assert_eq!(
            special_string(r"[/URI (https://example.test/^^c3^^bcber) /ANN", "/URI"),
            Some("https://example.test/über".as_bytes().to_vec())
        );
        assert_eq!(
            special_integer("ps:SDict begin [/Count 1/Dest (section.1) /OUT", "/Count"),
            Some(1)
        );
        assert_eq!(
            special_integer(
                r"ps:SDict begin [/Count 1/Dest (section.1) cvn/Title (Root) /OUT pdfmark end",
                "/Count"
            ),
            Some(1)
        );
        assert_eq!(
            special_number_array("[/Border[0 0 1]/C[1 0 0]/H/I", "/Border"),
            Some(String::from("[0 0 1]"))
        );
        assert_eq!(
            special_number_array("[/Border[0 0 1]/C[1 0 0]/H/I", "/C"),
            Some(String::from("[1 0 0]"))
        );
        assert_eq!(
            special_name("[/Border[0 0 1]/C[1 0 0]/H/I", "/H"),
            Some(String::from("/I"))
        );
    }

    #[test]
    fn dvipdfmx_raw_pdf_color_operators_are_preserved() {
        assert_eq!(
            special_color("color push 1 g 1 G"),
            Some(Color(1.0, 1.0, 1.0))
        );
        assert_eq!(
            special_color("color push 0.25 0.5 0.75 rg 0.25 0.5 0.75 RG"),
            Some(Color(0.25, 0.5, 0.75))
        );
        assert_eq!(
            special_color("color push 0 1 1 0 k 0 1 1 0 K"),
            Some(Color(1.0, 0.0, 0.0))
        );
        assert_eq!(
            special_color("color push NavyBlue"),
            Some(Color::cmyk(0.94, 0.54, 0.0, 0.0))
        );
        assert_eq!(
            special_color("color push ForestGreen"),
            Some(Color::cmyk(0.91, 0.0, 0.88, 0.12))
        );
        assert_eq!(
            special_background("background rgb 0 0 1"),
            Some(Color(0.0, 0.0, 1.0))
        );
        assert_eq!(special_background("background \"newpath clip"), None);
    }

    #[test]
    fn dvips_background_paints_the_complete_page_behind_content() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "background rgb 0 0 1");
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(0),
            baseline_y: aitex_page::Sp(0),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        let background = pdf.find("0 0 1 rg 0 0 1 RG\n0 0 612 792 re f").unwrap();
        let foreground = pdf.rfind(" re f\n").unwrap();
        assert!(background < foreground, "{pdf}");
    }

    #[test]
    fn trimmed_eps_box_is_mapped_into_the_normalized_pdf_page() {
        assert_eq!(
            normalized_eps_crop(
                [0.0, 0.0, 446.0, 394.0],
                Some([0.0, 0.0, 445.5, 393.0]),
                [0.0, 0.0, 446.0, 394.0],
            ),
            [0.0, 0.0, 446.0, 394.0]
        );
        let pdftex_box = pdftex_pdf_xbb_bbox([0.0, 0.0, 395.0, 192.0]);
        assert_eq!(
            normalized_eps_crop(
                [0.0, 0.0, 395.0, 192.0],
                Some([0.0, 0.0, 394.5, 191.0]),
                pdftex_box,
            ),
            pdftex_box
        );
        assert_eq!(
            normalized_eps_crop(
                [0.0, 0.0, 420.0, 315.0],
                Some([0.0, 0.0, 420.0, 315.0]),
                [10.0, 10.0, 410.0, 305.0],
            ),
            [10.0, 10.0, 410.0, 305.0]
        );
        assert_eq!(
            normalized_eps_crop(
                [0.0, 0.0, 529.0, 538.0],
                Some([31.0, 156.0, 560.0, 694.0]),
                [31.0, 156.0, 560.0, 694.0],
            ),
            [0.0, 0.0, 529.0, 538.0]
        );
        assert_eq!(
            normalized_eps_crop(
                [0.0, 0.0, 100.0, 50.0],
                Some([10.0, 20.0, 210.0, 120.0]),
                [30.0, 40.0, 190.0, 100.0],
            ),
            [10.0, 10.0, 90.0, 40.0]
        );
        assert_eq!(
            normalized_eps_form_and_viewport(
                [0.0, 0.0, 342.0, 182.0],
                Some([0.0, 0.0, 342.0, 182.0]),
                [20.0, 0.0, 342.0, 152.0],
                false,
            ),
            (
                [0.0, 0.0, 342.0, 182.0],
                [20.0, 0.0, 342.0, 152.0]
            )
        );
        assert_eq!(
            normalized_eps_form_and_viewport(
                [0.0, 0.0, 342.0, 182.0],
                Some([0.0, 0.0, 342.0, 182.0]),
                [20.0, 0.0, 342.0, 152.0],
                true,
            ),
            (
                [20.0, 0.0, 342.0, 152.0],
                [20.0, 0.0, 342.0, 152.0]
            )
        );
        assert!(!special_eps(
            "PSfile=plot.eps llx=20 lly=0 urx=342 ury=152 rwi=2088"
        )
        .unwrap()
        .clip);
        assert!(special_eps(
            "PSfile=plot.eps llx=20 lly=0 urx=342 ury=152 rwi=2088 clip"
        )
        .unwrap()
        .clip);
        assert!(!special_dvipdfmx_image(
            "pdf:image bbox 20 0 342 152 clip 0 width 208.8pt (plot.pdf)"
        )
        .unwrap()
        .clip);
        assert!(special_dvipdfmx_image(
            "pdf:image bbox 20 0 342 152 clip 1 width 208.8pt (plot.pdf)"
        )
        .unwrap()
        .clip);
    }

    #[test]
    fn package_specials_become_pdf_resources_and_navigation() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "papersize=614.295pt,794.96999pt");
        push_special(&mut page, 0, 0, "color push rgb 1 0 0");
        page.items.push(classic(0, b'A' as u32, 0, 0));
        push_special(&mut page, 327_680, 0, "color pop");
        push_special(&mut page, 0, 655_360, "ps:SDict begin H.S end");
        push_special(&mut page, 655_360, 655_360, "ps:SDict begin H.R end");
        push_special(
            &mut page,
            655_360,
            655_360,
            "ps:SDict begin [/Border[0 0 1]/C[1 0 0]/H/I \
             /Action <</Subtype/URI/URI(https://example.test/)>> /Subtype /Link \
             H.B /ANN pdfmark end",
        );
        push_special(&mut page, 0, 0, "ps:SDict begin H.S end");
        push_special(
            &mut page,
            0,
            0,
            "ps:SDict begin [/Dest (section.1) cvn /DEST pdfmark end",
        );
        push_special(
            &mut page,
            0,
            0,
            r"ps:SDict begin [/Count 1/Dest (section.1) cvn /Title (\376\377\000T) /OUT pdfmark end",
        );
        push_special(
            &mut page,
            0,
            0,
            r"ps:SDict begin [/Count 0/Dest (section.1.1) cvn /Title (\376\377\000C) /OUT pdfmark end",
        );
        push_special(
            &mut page,
            0,
            1_310_720,
            "PSfile=\"figure.eps\" llx=0 lly=0 urx=100 ury=50 rwi=720",
        );
        push_special(
            &mut page,
            655_360,
            1_310_720,
            "aitex:pdfannot 1310720 655360 327680 << /Subtype /Text /Contents (note) >>",
        );
        doc.pages.push(page);

        let out = write_with_assets(
            &doc,
            1.0,
            1.0,
            |_, _| {
                Some(FontSource {
                    postscript_name: "CMR10".to_string(),
                    pfb: None,
                    prepared_type1: None,
                    pk: None,
                    encoding: None,
                    to_unicode: Vec::new().into(),
                    slant: 0.0,
                    extend: 1.0,
                    widths: vec![327_680; 256].into(),
                    bbox: [0, 0, 1000, 1000],
                })
            },
            |name, _| {
                (name == "figure.eps").then(|| AssetSource {
                    bytes: b"1 0.5 0 setrgbcolor\nnewpath 0 0 moveto 100 50 lineto stroke\n"
                        .to_vec()
                        .into(),
                    pdf: None,
                    raster: None,
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/MediaBox [0 0 612.00 792.00]"), "{pdf}");
        assert!(pdf.contains("1 0 0 rg"), "{pdf}");
        assert!(pdf.contains("/Subtype /Link"), "{pdf}");
        assert!(pdf.contains("/Subtype /Text"), "{pdf}");
        assert!(pdf.contains("/Contents (note)"), "{pdf}");
        assert!(pdf.contains("https://example.test/"), "{pdf}");
        assert!(pdf.contains("/Border [0 0 1]"), "{pdf}");
        assert!(pdf.contains("/C [1 0 0]"), "{pdf}");
        assert!(pdf.contains("/H /I"), "{pdf}");
        assert!(pdf.contains("/Names << /Dests"), "{pdf}");
        assert!(pdf.contains("/Type /Outlines"), "{pdf}");
        assert_eq!(
            pdf.matches("/First ").count(),
            2,
            "nested outline missing: {pdf}"
        );
        assert!(
            pdf.contains("/Count 1"),
            "nested outline count missing: {pdf}"
        );
        assert!(pdf.contains("/Subtype /Form"), "{pdf}");
        assert!(pdf.contains(" Do Q"), "{pdf}");
    }

    #[test]
    fn pdftex_numbered_objects_remap_references_and_embed_raw_files() {
        let mut names = std::collections::BTreeMap::new();
        names.insert(String::from("aitexobj7"), 41usize);
        assert_eq!(
            replace_aitex_object_refs(b"<< /F 7 0 R /Text (7 0 R) >>", &names),
            b"<< /F 41 0 R /Text (7 0 R) >>"
        );

        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "aitex:pdfobjfile @aitexobj1 18\n/Type/EmbeddedFilenote.txt",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf:obj @aitexobj2 << /Type/Filespec /EF << /F 1 0 R >> >>",
        );
        push_special(
            &mut page,
            65_536,
            65_536,
            "aitex:pdfannot 65536 65536 0 << /Subtype /FileAttachment /FS 2 0 R >>",
        );
        doc.pages.push(page);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "aitex-raw:note.txt").then(|| AssetSource {
                    bytes: b"attached bytes".to_vec().into(),
                    pdf: None,
                    raster: None,
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("attached bytes"), "{pdf}");
        assert!(pdf.contains("<< /Type/EmbeddedFile"), "{pdf}");
        assert!(
            !pdf.contains("\n/Type/EmbeddedFile"),
            "stream attributes must be inside a dictionary: {pdf}"
        );
        assert!(pdf.contains("/Type/Filespec"), "{pdf}");
        assert!(pdf.contains("/Subtype /FileAttachment"), "{pdf}");
        assert!(!pdf.contains("/F 1 0 R"), "logical reference was not remapped: {pdf}");
        assert!(!pdf.contains("/FS 2 0 R"), "logical reference was not remapped: {pdf}");
    }

    #[test]
    fn pdftex_page_refs_bind_logical_numbers_to_real_page_objects() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(
            &mut first,
            0,
            0,
            "pdf:obj @aitexobj2 << /Kind /Test /P 1 0 R >>",
        );
        doc.pages.push(first);
        let mut second = dvi::Page::default();
        push_special(&mut second, 0, 0, "aitex:pdfpageref @aitexobj1 2");
        doc.pages.push(second);

        let bytes = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&bytes);
        // With no fonts the writer reserves catalog 1, pages 2, then page and
        // content pairs (3,4) and (5,6). The logical page reference must name
        // the second page object, even though its marker appeared later.
        assert!(pdf.contains("/Kind /Test /P 5 0 R"), "{pdf}");
    }

    #[test]
    fn paper_size_persists_until_a_later_page_changes_it() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(&mut first, 0, 0, "papersize=597.50787pt,845.04684pt");
        doc.pages.push(first);
        doc.pages.push(dvi::Page::default());
        let mut third = dvi::Page::default();
        push_special(&mut third, 0, 0, "papersize=614.295pt,794.96999pt");
        push_special(&mut third, 0, 0, "aitex:page-rotate 90");
        doc.pages.push(third);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(
            pdf.matches("/MediaBox [0 0 595.28 841.89]").count(),
            2,
            "the A4 declaration must carry onto page two: {pdf}"
        );
        assert_eq!(
            pdf.matches("/MediaBox [0 0 612.00 792.00]").count(),
            1,
            "the later Letter declaration must replace it: {pdf}"
        );
        assert_eq!(
            pdf.matches("/Rotate 90").count(),
            1,
            "a rotation applies only to the page that carries it: {pdf}"
        );
    }

    #[test]
    fn declared_paper_size_outlives_private_per_page_fallbacks() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(
            &mut first,
            0,
            0,
            "aitex:page-size 614.295pt,794.96999pt",
        );
        push_special(&mut first, 0, 0, "papersize=597.50787pt,845.04684pt");
        doc.pages.push(first);
        let mut second = dvi::Page::default();
        push_special(
            &mut second,
            0,
            0,
            "aitex:page-size 614.295pt,794.96999pt",
        );
        doc.pages.push(second);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(
            pdf.matches("/MediaBox [0 0 595.28 841.89]").count(),
            2,
            "the private page-two fallback must not replace declared A4 state: {pdf}"
        );
    }

    #[test]
    fn last_paper_size_on_a_page_overrides_the_private_pdftex_default() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(
            &mut first,
            0,
            0,
            "aitex:page-size 614.295pt,794.96999pt",
        );
        doc.pages.push(first);
        let mut second = dvi::Page::default();
        push_special(
            &mut second,
            0,
            0,
            "aitex:page-size 614.295pt,794.96999pt",
        );
        push_special(&mut second, 0, 0, "papersize=792bp,612bp");
        doc.pages.push(second);
        let mut third = dvi::Page::default();
        push_special(
            &mut third,
            0,
            0,
            "aitex:page-size 614.295pt,794.96999pt",
        );
        push_special(&mut third, 0, 0, "papersize=612bp,792bp");
        doc.pages.push(third);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/MediaBox [0 0 612.00 792.00]").count(), 2, "{pdf}");
        assert_eq!(pdf.matches("/MediaBox [0 0 792.00 612.00]").count(), 1, "{pdf}");
    }

    #[test]
    fn local_paper_size_overrides_the_carried_latex_default() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(
            &mut first,
            0,
            0,
            "aitex:latex-page-size 614.295pt,794.96999pt",
        );
        doc.pages.push(first);
        let mut second = dvi::Page::default();
        push_special(&mut second, 0, 0, "papersize=792bp,612bp");
        doc.pages.push(second);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/MediaBox [0 0 612.00 792.00]").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/MediaBox [0 0 792.00 612.00]").count(), 1, "{pdf}");
    }

    #[test]
    fn explicit_private_page_size_overrides_driver_paper_state() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:page-size 201.502bp,190.245bp");
        push_special(&mut page, 0, 0, "aitex:page-size-explicit");
        push_special(&mut page, 0, 0, "papersize=612bp,792bp");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/MediaBox [0 0 201.50 190.25]"), "{pdf}");
    }

    #[test]
    fn explicit_dvipdfmx_backend_does_not_override_pdftex_page_size() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:page-size 614.295pt,794.96999pt");
        push_special(&mut page, 0, 0, "aitex:pdftex-explicit-dvipdfmx");
        push_special(&mut page, 0, 0, "papersize=597.50787pt,845.04684pt");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/MediaBox [0 0 612.00 792.00]"), "{pdf}");
        assert!(!pdf.contains("/MediaBox [0 0 595.28 841.89]"), "{pdf}");
    }

    #[test]
    fn latex_page_size_survives_the_private_dvipdfmx_transport() {
        let mut doc = dvi::Document::default();
        let mut first = dvi::Page::default();
        push_special(&mut first, 0, 0, "aitex:page-size 614.295pt,794.96999pt");
        push_special(&mut first, 0, 0, "aitex:pdftex-explicit-dvipdfmx");
        push_special(
            &mut first,
            0,
            0,
            "aitex:latex-page-size 597.50787pt,845.04684pt",
        );
        push_special(&mut first, 0, 0, "papersize=597.50787pt,845.04684pt");
        doc.pages.push(first);
        doc.pages.push(dvi::Page::default());

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(
            pdf.matches("/MediaBox [0 0 595.28 841.89]").count(),
            2,
            "the logical class size persists after its first-page marker: {pdf}"
        );
    }

    #[test]
    fn zero_latex_page_size_does_not_override_pdftex_device_size() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:page-size 614.295pt,794.96999pt");
        push_special(&mut page, 0, 0, "aitex:latex-page-size 0.0pt,0.0pt");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/MediaBox [0 0 612.00 792.00]"), "{pdf}");
        assert!(!pdf.contains("/MediaBox [0 0 0.00 0.00]"), "{pdf}");
    }

    #[test]
    fn procedure_heavy_eps_uses_the_raster_fallback() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "PSfile=\"plot.eps\" llx=0 lly=0 urx=2 ury=1 rwi=720",
        );
        doc.pages.push(page);
        let mut ppm = b"P6\n# generated\n2 1\n255\n".to_vec();
        // The first pixel starts with ASCII whitespace. It is image data, not
        // additional header padding, and must not be skipped.
        ppm.extend_from_slice(&[32, 40, 50, 200, 210, 220]);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "plot.eps").then(|| AssetSource {
                    bytes: b"/plot-procedure { 0 0 moveto } def plot-procedure"
                        .to_vec()
                        .into(),
                    pdf: None,
                    raster: Some(ppm.clone().into()),
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Subtype /Image /Width 2 /Height 1"), "{pdf}");
        assert!(
            out.windows(6)
                .any(|window| window == [32, 40, 50, 200, 210, 220])
        );
    }

    #[test]
    fn explicit_dvipdfmx_keeps_epstopdf_eps_but_ignores_raw_postscript() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:pdftex-explicit-dvipdfmx");
        push_special(
            &mut page,
            0,
            0,
            "PSfile=\"raw.eps\" llx=0 lly=0 urx=2 ury=1 rwi=720",
        );
        push_special(&mut page, 0, 0, "aitex:eps-size 72.27pt,36.135pt");
        push_special(
            &mut page,
            0,
            0,
            "PSfile=\"converted.eps\" llx=0 lly=0 urx=2 ury=1 rwi=720",
        );
        doc.pages.push(page);
        let mut ppm = b"P6\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[10, 20, 30, 200, 210, 220]);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                assert_ne!(name, "raw.eps", "raw dvipdfmx EPS must stay ignored");
                (name == "converted.eps").then(|| AssetSource {
                    bytes: Vec::new().into(),
                    pdf: None,
                    raster: Some(ppm.clone().into()),
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Subtype /Form /BBox [0 0 2 1]"), "{pdf}");
    }

    #[test]
    fn pdftex_compatibility_marker_ignores_dvips_psfile_graphics() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:page-size 614.295pt,794.96999pt");
        push_special(&mut page, 0, 0, "aitex:pdftex-ignore-postscript");
        push_special(&mut page, 0, 0, "papersize=597.50787pt,845.04684pt");
        push_special(&mut page, 0, 0, "color push rgb 1 0 0");
        push_special(&mut page, 0, 0, "pdf:bxobj @ignored width 10bp height 10bp");
        push_special(&mut page, 0, 0, "pdf:bcontent");
        push_special(&mut page, 0, 0, "pdf:code 0 0 m 10 10 l S");
        push_special(&mut page, 0, 0, "pdf:econtent");
        push_special(&mut page, 0, 0, "pdf:exobj");
        push_special(&mut page, 0, 0, "pdf:content 0 0 m 20 20 l S");
        push_special(
            &mut page,
            0,
            0,
            "ps: gsave currentpoint currentpoint translate 90 neg rotate",
        );
        push_special(&mut page, 0, 0, "x:scale 0.5 0.5");
        push_special(
            &mut page,
            0,
            0,
            "PSfile=\"plot.eps\" llx=0 lly=0 urx=2 ury=1 rwi=720",
        );
        doc.pages.push(page);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |_, _| panic!("an ignored PostScript special must not resolve its asset"),
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/MediaBox [0 0 612.00 792.00]"), "{pdf}");
        assert!(!pdf.contains("1 0 0 rg"), "{pdf}");
        assert!(!pdf.contains("/Subtype /Image"), "{pdf}");
        assert!(!pdf.contains("/Subtype /Form"), "{pdf}");
        assert!(!pdf.contains("0 0 m 10 10 l S"), "{pdf}");
        assert!(!pdf.contains("0 0 m 20 20 l S"), "{pdf}");
    }

    #[test]
    fn pdftex_compatibility_marker_ignores_one_dvipdfmx_graphic() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "aitex:pdftex-ignore-next-driver-graphic",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf:image bbox 0 0 4 2 clip 0 width 72.27pt (ignored.png)",
        );
        doc.pages.push(page);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |_, _| panic!("an ignored dvipdfmx special must not resolve its asset"),
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(!pdf.contains("/Subtype /Image"), "{pdf}");
        assert!(!pdf.contains("/Subtype /Form"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_image_special_becomes_a_pdf_image() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:image page 3 bbox 0 0 4 2 clip 0 width 72.27pt (figures/a\\(b\\).png)",
        );
        push_special(
            &mut page,
            196_608,
            262_144,
            "pdf:image page 7 bbox 0 0 4 2 clip 0 width 72.27pt \
             (Spectrum for I=0 (Critical line concentration).png)",
        );
        doc.pages.push(page);
        let mut ppm = b"P6\n4 2\n255\n".to_vec();
        ppm.extend_from_slice(&[127; 4 * 2 * 3]);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, page| {
                assert!(
                    (name == "figures/a(b).png" && page == 3)
                        || (name == "Spectrum for I=0 (Critical line concentration).png"
                            && page == 7)
                );
                Some(AssetSource {
                    bytes: Vec::new().into(),
                    pdf: None,
                    raster: Some(ppm.clone().into()),
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Subtype /Image /Width 4 /Height 2"), "{pdf}");
        assert_eq!(
            pdf.matches("/Subtype /Image").count(),
            1,
            "a repeated source must reuse one document-wide image object: {pdf}"
        );
        assert_eq!(
            pdf.matches(" Do Q").count(),
            2,
            "both placements remain: {pdf}"
        );
        // An ordinary unaccompanied dvipdfmx special carries its complete
        // requested TeX dimension, so the driver divides by the pixel extent.
        assert!(
            pdf.contains("72.00000000 0 0 36.00000000 72.996 718.007 cm"),
            "{pdf}"
        );
    }

    #[test]
    fn pdftex_graphic_scale_companion_wins_over_ambiguous_requested_box() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "aitex:pdftex-graphic-scale 0.25,0.5",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf:image bbox 0 0 4 2 clip 0 width 72.27pt height 72.27pt (scaled.ppm)",
        );
        doc.pages.push(page);
        let mut ppm = b"P6\n4 2\n255\n".to_vec();
        ppm.extend_from_slice(&[127; 4 * 2 * 3]);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "scaled.ppm").then(|| AssetSource {
                    bytes: ppm.clone().into(),
                    pdf: None,
                    raster: None,
                    raster_bbox: Some([0.0, 0.0, 4.0, 2.0]),
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("1.00000000 0 0 1.00000000 72.000 720.000 cm"),
            "the companion must preserve graphicx's resolved matrix: {pdf}"
        );
        assert!(
            !pdf.contains("72.00000000 0 0 72.00000000"),
            "the ambiguous requested box must not be divided a second time: {pdf}"
        );
    }

    #[test]
    fn dvipdfmx_image_without_bbox_uses_the_rasters_natural_box() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "pdf:image width 56.90587pt height 56.90369pt (heatmap.png) \
             << /Interpolate false >>",
        );
        doc.pages.push(page);
        let mut ppm = b"P6\n4 2\n255\n".to_vec();
        ppm.extend_from_slice(&[127; 4 * 2 * 3]);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "heatmap.png").then(|| AssetSource {
                    bytes: ppm.clone().into(),
                    pdf: None,
                    raster: None,
                    raster_bbox: Some([0.0, 0.0, 4.0, 2.0]),
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Subtype /Image /Width 4 /Height 2"), "{pdf}");
        assert!(pdf.contains("/Im"), "{pdf}");
        assert!(pdf.contains(" Do Q"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_raster_crop_uses_physical_bbox_not_pixel_dimensions() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "pdf:image bbox 1 0.5 3 1.5 clip 0 width 72.27pt (dense.png)",
        );
        doc.pages.push(page);
        let mut ppm = b"P6\n8 4\n255\n".to_vec();
        ppm.extend_from_slice(&[127; 8 * 4 * 3]);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "dense.png").then(|| AssetSource {
                    bytes: Vec::new().into(),
                    pdf: None,
                    raster: Some(ppm.clone().into()),
                    // Eight pixels at 144 dpi occupy four PDF points.
                    raster_bbox: Some([0.0, 0.0, 4.0, 2.0]),
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("144.00000000 0 0 72.00000000 36.000 702.000 cm"),
            "{pdf}"
        );
    }

    #[test]
    fn dvipdfmx_pdf_special_imports_a_vector_form_and_its_resources() {
        let mut source = Objects::with_options(PdfOptions::uncompressed());
        let catalog = source.reserve();
        let pages = source.reserve();
        let page = source.reserve();
        let resources = source.reserve();
        let contents = source.reserve();
        let contents_array = source.reserve();
        let graphics_state = source.reserve();
        source.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        source.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        source.write(
            page,
            format!(
                "<< /Group << /Type /Group /S /Transparency >> /Type /Page \
                 /Parent {pages} 0 R /MediaBox [0 0 120 90] \
                 /CropBox [10 20 110 70] /Resources {resources} 0 R \
                 /Contents {contents_array} 0 R >>"
            )
            .as_bytes(),
        );
        source.write(
            resources,
            format!("<< /ExtGState << /GS0 {graphics_state} 0 R >> >>").as_bytes(),
        );
        let drawing = b"/GS0 gs 10 20 100 50 re f\n";
        let mut stream = format!("<< /Length {} >>\nstream\n", drawing.len()).into_bytes();
        stream.extend_from_slice(drawing);
        stream.extend_from_slice(b"endstream");
        source.write(contents, &stream);
        source.write(contents_array, format!("[{contents} 0 R]").as_bytes());
        source.write(graphics_state, b"<< /Type /ExtGState /ca 0.5 >>");
        let mut source = source.finish(catalog, None);
        assert_eq!(first_page_bbox(&source), Some([10.0, 20.0, 110.0, 70.0]));
        assert_eq!(pdf_page_count(&source), Some(1));
        assert_eq!(first_page_content_stream_count(&source), Some(1));
        assert!(directly_importable_single_page_pdf(&source, 1));
        assert!(!directly_importable_single_page_pdf(&source, 2));
        // MuPDF's classic-xref normalizer writes generation 65536 for the
        // free object-zero entry. It is outside a live indirect reference's
        // u16 range but must not make an otherwise valid page unimportable.
        let free = find_bytes(&source, b"0000000000 65535 f").unwrap();
        source[free + 11..free + 16].copy_from_slice(b"65536");
        assert!(directly_importable_single_page_pdf(&source, 1));

        let mut doc = dvi::Document::default();
        let mut dvi_page = dvi::Page::default();
        push_special(
            &mut dvi_page,
            65_536,
            131_072,
            "pdf:epdf page 1 pagebox cropbox bbox 10 20 110 70 clip 0 \
             width 72.27pt (vector.pdf)",
        );
        push_special(
            &mut dvi_page,
            0,
            0,
            "pdf:epdf page 1 pagebox cropbox bbox 30 25 90 60 clip 0 \
             width 72.27pt (vector.pdf)",
        );
        push_special(
            &mut dvi_page,
            0,
            0,
            "PSfile=\"vector.eps\" llx=10 lly=20 urx=110 ury=70 rwi=722",
        );
        doc.pages.push(dvi_page);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                matches!(name, "vector.pdf" | "vector.eps").then(|| AssetSource {
                    bytes: b"%PDF-vector-original".to_vec().into(),
                    pdf: Some(source.clone().into()),
                    raster: None,
                    raster_bbox: Some([10.0, 20.0, 110.0, 70.0]),
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Subtype /Form /FormType 1"), "{pdf}");
        assert!(pdf.contains("/BBox [10 20 110 70]"), "{pdf}");
        assert!(pdf.contains("/GS0 gs 10 20 100 50 re f"), "{pdf}");
        assert_eq!(pdf.matches("/GS0 gs 10 20 100 50 re f").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/Subtype /Form /FormType 1").count(), 1, "{pdf}");
        assert_eq!(pdf.matches("/Type /Catalog").count(), 1, "{pdf}");
        assert!(pdf.contains("/ExtGState << /GS0"), "{pdf}");
        assert_eq!(pdf.matches("0 G 0 g").count(), 3, "{pdf}");
        assert!(
            pdf.contains("/Group << /Type /Group /S /Transparency >>"),
            "{pdf}"
        );
        assert!(
            pdf.contains("q 0 G 0 g 0.72000000 0 0 0.72000000"),
            "{pdf}"
        );
        assert!(
            pdf.contains("q 0 G 0 g 1.20000000 0 0 1.20000000 36.000 690.000 cm /Fm"),
            "a custom viewport without `clip` must translate but preserve the natural Form: {pdf}"
        );
        assert!(
            pdf.contains("q 0 G 0 g 0.72200000 0 0 0.72200000"),
            "{pdf}"
        );
        assert_eq!(pdf.matches(" cm /Fm").count(), 3, "{pdf}");
        assert!(!pdf.contains("/Subtype /Image"), "{pdf}");
    }

    #[test]
    fn pdftex_natural_box_clips_a_crop_box_to_the_media_box() {
        let mut source = Objects::with_options(PdfOptions::uncompressed());
        let catalog = source.reserve();
        let pages = source.reserve();
        let page = source.reserve();
        let contents = source.reserve();
        source.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        source.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        source.write(
            page,
            format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 300] \
                 /CropBox [-10 -20 110 220] /Resources << >> \
                 /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        source.write(contents, b"<< /Length 0 >>\nstream\nendstream");
        let source = source.finish(catalog, None);

        assert_eq!(
            first_page_effective_bbox(&source),
            Some([-10.0, -20.0, 110.0, 220.0])
        );
        assert_eq!(
            first_page_pdftex_bbox(&source),
            Some([0.0, 0.0, 110.0, 220.0])
        );

        let mut output = Objects::with_options(PdfOptions::uncompressed());
        let form = import_pdf_form(
            &mut output,
            &source,
            first_page_pdftex_bbox(&source).expect("clipped page box"),
        )
        .expect("page form");
        let output = String::from_utf8_lossy(&output.finish(form, None)).into_owned();
        assert!(output.contains("/BBox [0 0 110 220]"), "{output}");
        assert!(!output.contains("/BBox [-10 -20 110 220]"), "{output}");
    }

    #[test]
    fn pdftex_natural_box_resolves_an_indirect_crop_box() {
        let mut source = Objects::with_options(PdfOptions::uncompressed());
        let catalog = source.reserve();
        let pages = source.reserve();
        let page = source.reserve();
        let contents = source.reserve();
        let crop_box = source.reserve();
        source.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        source.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        source.write(
            page,
            format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 960 540] \
                 /CropBox {crop_box} 0 R /Resources << >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        source.write(contents, b"<< /Length 0 >>\nstream\nendstream");
        source.write(crop_box, b"[0 120.04 960 392.03]");
        let source = source.finish(catalog, None);

        assert_eq!(
            first_page_bbox(&source),
            Some([0.0, 120.04, 960.0, 392.03])
        );
        assert_eq!(
            first_page_pdftex_bbox(&source),
            Some([0.0, 120.04, 960.0, 392.03])
        );
    }

    #[test]
    fn pdf_rectangle_normalizes_opposite_corner_order() {
        assert_eq!(
            pdf_box(b"[21.69773 278.57947 580.97778 64.34197]"),
            Some([21.69773, 64.34197, 580.97778, 278.57947])
        );
    }

    #[test]
    fn rotated_pdf_page_reports_effective_box_and_imports_with_page_matrix() {
        let mut source = Objects::with_options(PdfOptions::uncompressed());
        let catalog = source.reserve();
        let pages = source.reserve();
        let page = source.reserve();
        let contents = source.reserve();
        source.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        source.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        source.write(
            page,
            format!(
                "<< /Type /Page /Parent {pages} 0 R /Rotate 90 \
                 /MediaBox [0 0 120 90] /CropBox [10 20 110 70] \
                 /Resources << >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        let drawing = b"10 20 100 50 re f\n";
        let mut stream = format!("<< /Length {} >>\nstream\n", drawing.len()).into_bytes();
        stream.extend_from_slice(drawing);
        stream.extend_from_slice(b"endstream");
        source.write(contents, &stream);
        let source = source.finish(catalog, None);

        assert_eq!(first_page_rotation(&source), Some(90));
        assert_eq!(first_page_bbox(&source), Some([10.0, 20.0, 110.0, 70.0]));
        let effective = [10.0, 20.0, 60.0, 120.0];
        assert_eq!(first_page_effective_bbox(&source), Some(effective));
        assert!(directly_importable_single_page_pdf(&source, 1));

        let mut output = Objects::with_options(PdfOptions::uncompressed());
        let form = import_pdf_form(&mut output, &source, effective).expect("rotated form");
        let output = String::from_utf8_lossy(&output.finish(form, None)).into_owned();
        assert!(output.contains("/BBox [10 20 110 70]"), "{output}");
        assert!(output.contains("/Matrix [0 -1 1 0 -10 130]"), "{output}");
    }

    #[test]
    fn dvipdfmx_pdf_special_preserves_a_compressed_content_stream_once() {
        let mut source = Objects::with_options(PdfOptions::uncompressed());
        let catalog = source.reserve();
        let pages = source.reserve();
        let page = source.reserve();
        let contents = source.reserve();
        source.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        source.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        source.write(
            page,
            format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 10 10] \
                 /Resources << >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        let encoded = b"30203020313020313020726520660A>";
        let mut stream = format!(
            "<< /Filter /ASCIIHexDecode /Length {} >>\nstream\n",
            encoded.len()
        )
        .into_bytes();
        stream.extend_from_slice(encoded);
        stream.extend_from_slice(b"\nendstream");
        source.write(contents, &stream);
        let source = source.finish(catalog, None);

        let mut doc = dvi::Document::default();
        let mut dvi_page = dvi::Page::default();
        push_special(
            &mut dvi_page,
            0,
            0,
            "pdf:epdf page 1 pagebox cropbox bbox 0 0 10 10 clip 0 \
             width 10bp (compressed.pdf)",
        );
        doc.pages.push(dvi_page);
        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "compressed.pdf").then(|| AssetSource {
                    bytes: Vec::new().into(),
                    pdf: Some(source.clone().into()),
                    raster: None,
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/Filter /ASCIIHexDecode"), "{pdf}");
        assert_eq!(pdf.matches("30203020313020313020726520660A>").count(), 1);
        assert_eq!(pdf.matches("/Type /Catalog").count(), 1, "{pdf}");
    }

    #[test]
    fn pdftex_literal_modes_preserve_setorigin_and_page_coordinates() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "aitex:pdfliteral-setorigin q 2 Tr",
        );
        push_special(
            &mut page,
            196_608,
            262_144,
            "aitex:pdfliteral-setorigin Q",
        );
        push_special(
            &mut page,
            0,
            0,
            "aitex:pdfliteral-direct 0.5 w",
        );
        push_special(&mut page, 0, 0, "aitex:pdfliteral-page 0 g");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains(
                "1 0 0 1 72.9963 718.0075 cm\nq 2 Tr\n1 0 0 1 -72.9963 -718.0075 cm\n"
            ),
            "{pdf}"
        );
        assert!(
            pdf.contains(
                "1 0 0 1 74.9888 716.0149 cm\nQ\n0.5 w\n0 g\n\
                 1 0 0 1 -74.9888 -716.0149 cm\n"
            ),
            "{pdf}"
        );
    }

    #[test]
    fn pdftex_setorigin_stays_active_through_an_adjacent_direct_literal() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "aitex:pdfliteral-setorigin q 0.65 g 0.65 G",
        );
        push_special(
            &mut page,
            65_536,
            131_072,
            "aitex:pdfliteral-direct 1.99252 w 10 0 m 10 36 l S Q",
        );
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(0),
            baseline_y: aitex_page::Sp(0),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains(
                "1 0 0 1 72.9963 718.0075 cm\nq 0.65 g 0.65 G\n\
                 1.99252 w 10 0 m 10 36 l S Q\n\
                 1 0 0 1 -72.9963 -718.0075 cm\n"
            ),
            "{pdf}"
        );
    }

    #[test]
    fn pdf_color_stack_pop_restores_its_declared_initial_literal() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "aitex:pdfcolorstackinit 1 /TRP1 gs",
        );
        push_special(
            &mut page,
            0,
            0,
            "aitex:pdfcolorstack 1 push /TRP.5 gs",
        );
        push_special(&mut page, 0, 0, "aitex:pdfcolorstackinit 1 /TRP1 gs");
        push_special(&mut page, 0, 0, "aitex:pdfcolorstack 1 pop");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("/TRP.5 gs\n/TRP1 gs\n"), "{pdf}");
    }

    #[test]
    fn positioned_pdf_literals_restore_the_page_coordinate_system() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 65_536, 131_072, "aitex:pdfsave");
        push_special(
            &mut page,
            65_536,
            131_072,
            "aitex:pdfliteral 0.398 w 0 0 m 10 0 l S",
        );
        push_special(&mut page, 65_536, 131_072, "aitex:pdfsetmatrix 0 1 -1 0");
        push_special(&mut page, 65_536, 131_072, "aitex:pdfrestore");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("1 0 0 1 72.9963 718.0075 cm\nq\n"), "{pdf}");
        assert!(
            pdf.contains(
                "1 0 0 1 0.0000 0.0000 cm\n0.398 w 0 0 m 10 0 l S\n\
                 1 0 0 1 -0.0000 -0.0000 cm\n"
            ),
            "{pdf}"
        );
        assert_eq!(pdf.matches("1 0 0 1 72.9963 718.0075 cm\n").count(), 1);
        assert!(pdf.contains("0 1 -1 0 0 0 cm\n"), "{pdf}");
        assert!(pdf.contains("Q\n1 0 0 1 -72.9963 -718.0075 cm\n"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_content_inside_pdfsave_is_relative_to_the_saved_point() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 655_360, 131_072, "aitex:pdfsave");
        push_special(&mut page, 1_310_720, 196_608, "pdf:bcontent");
        push_special(&mut page, 1_310_720, 196_608, "pdf:code 0 0 m 10 10 l S");
        push_special(&mut page, 1_310_720, 196_608, "pdf:econtent");
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(1_310_720),
            baseline_y: aitex_page::Sp(196_608),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        push_special(&mut page, 655_360, 131_072, "aitex:pdfrestore");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("q\n1 0 0 1 9.9626 -0.9963 cm\n0 0 m 10 10 l S\nQ\n"),
            "{pdf}"
        );
        assert!(
            pdf.contains("9.9626 -0.9963 0.9963 0.9963 re f\n"),
            "{pdf}"
        );
    }

    #[test]
    fn dvipdfmx_content_blocks_position_paths_and_dvi_material_together() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "pdf:bcontent");
        push_special(&mut page, 0, 0, "pdf:code 0 0 m 10 10 l S");
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(655_360),
            baseline_y: aitex_page::Sp(655_360),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        push_special(&mut page, 0, 0, "pdf:econtent");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("q\n1 0 0 1 72.0000 720.0000 cm\n"), "{pdf}");
        assert!(pdf.contains("0 0 m 10 10 l S\n"), "{pdf}");
        assert!(pdf.contains("9.9626 -9.9626 0.9963 0.9963 re f\n"), "{pdf}");
    }

    #[test]
    fn pgf_suspend_content_carries_the_text_advance_into_the_restored_scope() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "pdf:btrans matrix 1 0 0 1 0 0");
        push_special(&mut page, 0, 0, "pdf:bcontent");
        push_special(&mut page, 65_536, 0, "pdf:code q -1 0 0 -1 0 0 cm q");
        push_special(&mut page, 65_536, 0, "pdf:econtent");
        push_special(&mut page, 65_536, 0, "pdf:bcontent");
        push_special(&mut page, 65_536, 0, "pdf:code -1 0 0 -1 0 0 cm");
        push_special(&mut page, 65_536, 0, "pdf:econtent");
        push_special(&mut page, 65_536, 0, "pdf:code 0 J [] 0 d");
        push_special(&mut page, 65_536, 0, "pdf:bcontent");
        push_special(&mut page, 65_536, 0, "pdf:code Q Q Q");
        push_special(
            &mut page,
            65_536,
            0,
            "pdf:btrans matrix 1 0 0 1 0 0",
        );
        push_special(&mut page, 65_536, 0, "pdf:etrans");
        push_special(
            &mut page,
            65_536,
            0,
            "pdf:btrans matrix 1 0 0 1 0 0",
        );
        push_special(&mut page, 65_536, 0, "pdf:etrans");
        push_special(&mut page, 65_536, 0, "pdf:etrans");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("Q\nq\n1 0 0 1 72.9963 720.0000 cm\n"),
            "{pdf}"
        );
        let corrected =
            "q\n1.00000000 0.00000000 0.00000000 1.00000000 0.99626401 0.00000000 cm\n";
        assert_eq!(
            pdf.matches(corrected).count(),
            2,
            "the advance must apply to sibling transforms at the suspended depth: {pdf}"
        );
    }

    #[test]
    fn private_pdftex_origin_positions_tight_page_content() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "aitex:page-origin 10bp,20bp");
        push_special(&mut page, 0, 0, "pdf:bcontent");
        push_special(&mut page, 0, 0, "pdf:code 0 0 m 10 10 l S");
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(655_360),
            baseline_y: aitex_page::Sp(655_360),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        push_special(&mut page, 0, 0, "pdf:econtent");
        doc.pages.push(page);

        let out = write(&doc, 100.0, 200.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("q\n1 0 0 1 10.0000 180.0000 cm\n"), "{pdf}");
        assert!(pdf.contains("9.9626 -9.9626 0.9963 0.9963 re f\n"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_content_special_is_positioned_and_graphics_state_isolated() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf: content 0.398 w 0 0 m 10 0 l S",
        );
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains(
                "q\n1 0 0 1 72.9963 718.0075 cm\n0.398 w 0 0 m 10 0 l S\nQ\n"
            ),
            "{pdf}"
        );
    }

    #[test]
    fn dvipdfmx_content_special_is_positioned_inside_form_xobject() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:bxobj @drawing width 20bp height 20bp",
        );
        push_special(
            &mut page,
            131_072,
            262_144,
            "pdf:content 0 0 m 10 0 l S",
        );
        push_special(&mut page, 65_536, 131_072, "pdf:exobj");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("q\n1 0 0 1 0.9963 -1.9925 cm\n0 0 m 10 0 l S\nQ\n"),
            "{pdf}"
        );
    }

    #[test]
    fn dvipdfmx_form_captures_dvi_glyphs_and_their_font_resource() {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:bxobj @appearance width 20bp height 20bp",
        );
        page.items.push(classic(0, b'A' as u32, 131_072, 262_144));
        push_special(&mut page, 65_536, 131_072, "pdf:exobj");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, at| {
            Some(FontSource {
                postscript_name: "CMR10".into(),
                pfb: None,
                prepared_type1: None,
                pk: None,
                encoding: None,
                to_unicode: vec![None; 256].into(),
                slant: 0.0,
                extend: 1.0,
                widths: vec![at / 2; 256].into(),
                bbox: [0, -200, 1000, 800],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        let form_start = pdf.find("/Subtype /Form").expect("Form XObject");
        let form = &pdf[form_start..];
        let stream_start = form.find("stream\n").expect("Form stream") + "stream\n".len();
        let stream_end = form[stream_start..]
            .find("endstream")
            .expect("end of Form stream")
            + stream_start;
        let content = &form[stream_start..stream_end];

        assert!(form[..stream_start].contains("/Font << /F"), "{pdf}");
        assert!(content.contains("0.996 -1.993 Tm [(A)] TJ ET"), "{pdf}");
        assert_eq!(pdf.matches("(A)").count(), 1, "{pdf}");
    }

    #[test]
    fn dvipdfmx_form_xobject_accepts_an_explicit_bbox() {
        assert_eq!(
            pdf_xobject_begin(
                "pdf:bxobj @pgftransgroup6 bbox -18.14194 -18.14194 212.03444 212.03444"
            ),
            Some((
                "pgftransgroup6",
                [-18.14194, -18.14194, 212.03444, 212.03444]
            ))
        );
        assert_eq!(
            pdf_xobject_begin("pdf:bxobj @drawing width 20bp height 30bp depth 2bp"),
            Some(("drawing", [0.0, -2.0, 20.0, 30.0]))
        );
    }

    #[test]
    fn dvipdfmx_complete_object_parser_preserves_stream_bytes() {
        let special = b"pdf: object @mesh << /Length 4 >>\nstream\n\xff\0A\x80endstream ";
        let (name, body) = pdf_dvipdfmx_object_special(special).expect("complete object");
        assert_eq!(name, "mesh");
        assert_eq!(body, b"<< /Length 4 >>\nstream\n\xff\0A\x80endstream");
        assert!(pdf_dvipdfmx_object_special(b"pdf:obj @mesh <<>>").is_none());
    }

    #[test]
    fn dvipdfmx_complete_stream_objects_are_available_to_captured_forms() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "pdf:bxobj @plot width 20bp height 20bp",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf: object @mesh << /Length 4 >>\nstream\nDATAendstream",
        );
        push_special(&mut page, 0, 0, "pdf: close @mesh");
        push_special(
            &mut page,
            0,
            0,
            "pdf: put @resources << /Shading << /Mesh @mesh >> >>",
        );
        push_special(&mut page, 0, 0, "pdf:code /Mesh sh");
        push_special(&mut page, 0, 0, "pdf: exobj");
        push_special(&mut page, 0, 0, "pdf:uxobj @plot");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(pdf.contains("stream\nDATAendstream"), "{pdf}");
        assert!(pdf.contains("/Shading << /Mesh "), "{pdf}");
        assert!(pdf.contains("/Mesh sh"), "{pdf}");
        assert!(!pdf.contains("@mesh"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_bbox_form_scopes_content_blocks_away_from_following_dvi_material() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "pdf:bxobj @pgftransgroup6 bbox -18.14194 -18.14194 212.03444 212.03444",
        );
        // `bxobj` begins an implicit content block. PGF closes and reopens it
        // while constructing a transparency group, then leaves the last block
        // for `exobj` to close.
        push_special(&mut page, 0, 0, "pdf:econtent");
        push_special(&mut page, 65_536, 131_072, "pdf:bcontent");
        push_special(&mut page, 65_536, 131_072, "pdf:code 0 0 m 10 0 l S");
        push_special(&mut page, 65_536, 131_072, "pdf:econtent");
        push_special(&mut page, 0, 0, "pdf:bcontent");
        push_special(&mut page, 0, 0, "pdf:exobj");
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(655_360),
            baseline_y: aitex_page::Sp(655_360),
            width: aitex_page::Sp(65_536),
            height: aitex_page::Sp(65_536),
        });
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/Subtype /Form /FormType 1").count(), 1, "{pdf}");
        assert!(
            pdf.contains("/BBox [-18.14194000 -18.14194000 212.03444000 212.03444000]"),
            "{pdf}"
        );
        assert!(
            pdf.contains("q\n1 0 0 1 0.9963 -1.9925 cm\n0 0 m 10 0 l S\nQ\n"),
            "{pdf}"
        );
        assert!(
            pdf.contains("81.9626 710.0374 0.9963 0.9963 re f\n"),
            "{pdf}"
        );
    }

    #[test]
    fn dvipdfmx_raster_and_transform_are_captured_inside_form_xobject() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:bxobj @frame width 1bp height 1bp",
        );
        push_special(&mut page, 65_536, 131_072, "pdf:btrans");
        push_special(&mut page, 65_536, 131_072, "x:scale 0.5 0.5");
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:image bbox 0 0 1 1 clip 0 width 2bp (frame.ppm)",
        );
        push_special(&mut page, 65_536, 131_072, "pdf:etrans");
        push_special(&mut page, 65_536, 131_072, "pdf:exobj");
        push_special(&mut page, 65_536, 131_072, "pdf:uxobj @frame");
        doc.pages.push(page);

        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "frame.ppm").then(|| AssetSource {
                    bytes: b"P6\n1 1\n255\n\xff\0\0".to_vec().into(),
                    pdf: None,
                    raster: None,
                    raster_bbox: Some([0.0, 0.0, 1.0, 1.0]),
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/Subtype /Image").count(), 1, "{pdf}");
        assert!(
            pdf.contains("0.50000000 0 0 0.50000000 0.00000000"),
            "{pdf}"
        );
        assert!(pdf.contains("/XObject << /Im"), "{pdf}");
        assert!(pdf.contains("/Im") && pdf.contains(" Do Q"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_pdf_page_is_captured_and_placed_at_saved_origin() {
        let mut source = Objects::with_options(PdfOptions::uncompressed());
        let catalog = source.reserve();
        let pages = source.reserve();
        let page = source.reserve();
        let contents = source.reserve();
        source.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        source.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        source.write(
            page,
            format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 10 10] \
                 /Resources << >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        let drawing = b"0 0 10 10 re f\n";
        let mut stream = format!("<< /Length {} >>\nstream\n", drawing.len()).into_bytes();
        stream.extend_from_slice(drawing);
        stream.extend_from_slice(b"endstream");
        source.write(contents, &stream);
        let source = source.finish(catalog, None);

        let mut doc = dvi::Document::default();
        let mut dvi_page = dvi::Page::default();
        push_special(
            &mut dvi_page,
            0,
            0,
            "pdf:bxobj @frame width 10bp height 10bp",
        );
        push_special(
            &mut dvi_page,
            0,
            0,
            "pdf:epdf page 1 pagebox cropbox bbox 0 0 10 10 clip 0 width 10bp (frame.pdf)",
        );
        push_special(&mut dvi_page, 0, 0, "pdf:exobj");
        push_special(&mut dvi_page, 0, 0, "pdf:uxobj @frame");
        push_special(&mut dvi_page, 65_536, 131_072, "aitex:pdfsave");
        push_special(
            &mut dvi_page,
            65_536,
            131_072,
            "aitex:pdfliteral 0 0 10 10 re W n",
        );
        push_special(
            &mut dvi_page,
            65_536,
            131_072,
            "pdf:epdf page 1 pagebox cropbox bbox 0 0 10 10 clip 0 width 10bp (frame.pdf)",
        );
        push_special(&mut dvi_page, 65_536, 131_072, "aitex:pdfrestore");
        doc.pages.push(dvi_page);

        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "frame.pdf").then(|| AssetSource {
                    bytes: Vec::new().into(),
                    pdf: Some(source.clone().into()),
                    raster: None,
                    raster_bbox: Some([0.0, 0.0, 10.0, 10.0]),
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(pdf.matches("/Subtype /Form /FormType 1").count(), 2, "{pdf}");
        assert!(pdf.contains("0 0 10 10 re f"), "{pdf}");
        assert!(pdf.contains("/XObject << /Fm"), "{pdf}");
        assert!(pdf.contains("0 0 10 10 re W n"), "{pdf}");
        assert!(pdf.contains("q 0 G 0 g 1.00000000 0 0 1.00000000"), "{pdf}");
        assert!(
            !pdf.contains("q 0.0000 0.0000 10.00000000 10.00000000 re W n"),
            "`clip 0` must not add a second image clip inside pdfsave: {pdf}"
        );
    }

    #[test]
    fn dvipdfmx_named_objects_forms_and_resources_stay_scoped() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 0, 0, "pdf:obj @spaces <<>>");
        push_special(
            &mut page,
            0,
            0,
            "pdf:put @spaces << /Pattern [/Pattern /DeviceRGB] >>",
        );
        push_special(&mut page, 0, 0, "pdf:bxobj @shade width 10pt height 20pt");
        push_special(
            &mut page,
            0,
            0,
            "pdf:put @resources << /ColorSpace @spaces \
             /Shading << /Sh << /ShadingType 2 /ColorSpace /DeviceRGB \
             /Coords [0 0 10 0] /Function << /FunctionType 2 /Domain [0 1] \
             /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >> >> >>",
        );
        push_special(&mut page, 0, 0, "pdf:code /Sh sh");
        push_special(&mut page, 0, 0, "pdf:exobj");
        push_special(
            &mut page,
            0,
            0,
            "pdf:bxobj @mask width 10pt height 20pt depth 1pt",
        );
        push_special(&mut page, 0, 0, "pdf:code q");
        push_special(&mut page, 0, 0, "pdf:uxobj @shade");
        push_special(&mut page, 0, 0, "pdf:code Q");
        push_special(
            &mut page,
            0,
            0,
            "pdf:exobj <</Group <</S/Transparency /CS/DeviceGray>> >>",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf:put @resources << /ColorSpace @spaces >>",
        );
        push_special(&mut page, 65_536, 131_072, "pdf:uxobj @mask");
        doc.pages.push(page);

        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert_eq!(
            pdf.matches("/Subtype /Form /FormType 1").count(),
            2,
            "{pdf}"
        );
        assert_eq!(pdf.matches("/Sh sh").count(), 1, "{pdf}");
        assert!(pdf.contains("/Shading << /Sh << /ShadingType 2"), "{pdf}");
        assert!(pdf.contains("/Pattern [/Pattern /DeviceRGB]"), "{pdf}");
        assert!(
            pdf.contains("/Group <</S/Transparency /CS/DeviceGray>>"),
            "{pdf}"
        );
        assert!(
            pdf.contains("q 1 0 0 1 72.99626401 718.00747198 cm /mask"),
            "{pdf}"
        );
        assert!(!pdf.contains("@spaces"), "{pdf}");
        assert!(!pdf.contains("@shade"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_pdfmanagement_objects_positions_and_annotations_are_valid() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(
            &mut page,
            0,
            0,
            "pdf:fstream @pdf.obj1 (profile.icc) << /N 3 /Alternate /DeviceRGB >>",
        );
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:obj @position [@thispage /XYZ @xpos @ypos null]",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf:obj @tree << /Type /StructTreeRoot >>",
        );
        push_special(
            &mut page,
            0,
            0,
            "pdf:put @catalog << /StructTreeRoot @tree /OutputIntents [@pdf.obj1] >>",
        );
        push_special(&mut page, 0, 0, "pdf:put @pages << /Lang (en-US) >>");
        push_special(
            &mut page,
            0,
            0,
            "pdf:put @thispage << /StructParents 0 /Tabs /S >>",
        );
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:dest (section.1) [@thispage /XYZ @xpos @ypos null]",
        );
        push_special(
            &mut page,
            65_536,
            131_072,
            "pdf:bann @pdf.lnk1 << /Type /Annot /Subtype /Link /A << /S /GoTo /D (section.1) >> >>",
        );
        page.items.push(dvi::Item::Rule {
            x: aitex_page::Sp(65_536),
            baseline_y: aitex_page::Sp(131_072),
            width: aitex_page::Sp(131_072),
            height: aitex_page::Sp(65_536),
        });
        push_special(&mut page, 196_608, 131_072, "pdf:eann");
        push_special(
            &mut page,
            0,
            0,
            "pdf:obj @objr << /Type /OBJR /Obj @pdf.lnk1 /Pg @page1 >>",
        );
        doc.pages.push(page);

        let out = write_with_assets(
            &doc,
            612.0,
            792.0,
            |_, _| None,
            |name, _| {
                (name == "aitex-raw:profile.icc").then(|| AssetSource {
                    bytes: b"ICC profile".to_vec().into(),
                    pdf: None,
                    raster: None,
                    raster_bbox: None,
                })
            },
        );
        let pdf = String::from_utf8_lossy(&out);
        assert!(!pdf.contains('@'), "unresolved dvipdfmx name in {pdf}");
        assert!(pdf.contains("ICC profile"), "{pdf}");
        assert!(pdf.contains("[3 0 R /XYZ 72.9963 718.0075 null]"), "{pdf}");
        assert!(pdf.contains("/StructTreeRoot"), "{pdf}");
        assert!(pdf.contains("/StructParents 0 /Tabs /S"), "{pdf}");
        assert!(pdf.contains("/Lang (en-US)"), "{pdf}");
        assert!(pdf.contains("/Annots ["), "{pdf}");
        assert!(pdf.contains("/Type /OBJR /Obj"), "{pdf}");
        assert!(pdf.contains("(section.1) [3 0 R /XYZ"), "{pdf}");
    }

    #[test]
    fn dvipdfmx_scale_is_anchored_at_the_current_point() {
        let mut doc = dvi::Document::default();
        let mut page = dvi::Page::default();
        push_special(&mut page, 655_360, 131_072, "x:scale 0.5 0.25");
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| None);
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("0.50000000 0 0 0.25000000 40.98132005 538.50560399 cm"),
            "{pdf}"
        );
    }

    #[test]
    fn pk_fonts_use_pdftex_pixel_geometry_and_image_masks() {
        let mut pk = vec![247, 89, 0];
        pk.extend_from_slice(&10_485_760i32.to_be_bytes());
        pk.extend_from_slice(&0i32.to_be_bytes());
        pk.extend_from_slice(&545_903i32.to_be_bytes()); // nearby 602dpi bitmap
        pk.extend_from_slice(&545_903i32.to_be_bytes());
        pk.extend_from_slice(&[0xe0, 9, 65, 0, 0, 0, 2, 2, 2, 0, 2, 0x90, 245]);

        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            0,
            dvi::FontDef {
                name: "bitmap".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(0, 65, 0, 0));
        doc.pages.push(page);
        let out = write(&doc, 612.0, 792.0, |_, _| {
            let mut widths = vec![0; 256];
            widths[65] = 327_600;
            Some(FontSource {
                postscript_name: "bitmap".into(),
                pfb: None,
                prepared_type1: None,
                pk: Some(pk.clone().into()),
                encoding: None,
                to_unicode: vec![None; 256].into(),
                slant: 0.0,
                extend: 1.0,
                widths: widths.into(),
                bbox: [0, 0, 1000, 1000],
            })
        });
        let pdf = String::from_utf8_lossy(&out);
        assert!(
            pdf.contains("/FontMatrix [0.01204 0 0 0.01204 0 0]"),
            "{pdf}"
        );
        assert!(pdf.contains("/FirstChar 65 /LastChar 65"), "{pdf}");
        assert!(pdf.contains("BI\n/W 2\n/H 2\n/IM true"), "{pdf}");
        assert!(pdf.contains("/F /AHx\nID\n8040>"), "{pdf}");
        assert!(
            !pdf.contains(" re f\n"),
            "bitmap pixels became vector rectangles"
        );
    }

    #[test]
    fn png_raster_reuses_its_flate_stream() {
        fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            // The parser bounds-checks the CRC field but deliberately leaves
            // validation to the renderer that produced the PNG.
            png.extend_from_slice(&[0; 4]);
        }

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        // A valid zlib stream for one PNG row: filter byte 0, then RGB 1,2,3.
        let idat = [
            0x78, 0x01, 0x01, 0x04, 0x00, 0xfb, 0xff, 0x00, 0x01, 0x02, 0x03, 0x00, 0x0f, 0x00,
            0x07,
        ];
        chunk(&mut png, b"IDAT", &idat);
        chunk(&mut png, b"IEND", &[]);

        let raster = parse_raster(&png).expect("RGB PNG");
        let objects = Objects::new();
        let object = image_object(&objects, &raster, None);
        let text = String::from_utf8_lossy(&object);
        assert!(text.contains("/Filter /FlateDecode"), "{text}");
        assert!(text.contains("/Predictor 15"), "{text}");
        assert!(object.windows(idat.len()).any(|window| window == idat));
        assert!(object.len() < 400, "the image was not expanded to raw RGB");
    }

    #[test]
    fn grayscale_and_indexed_png_keep_native_samples_and_transparency() {
        fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            png.extend_from_slice(&[0; 4]);
        }
        fn png(depth: u8, color: u8, extras: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            chunk(
                &mut png,
                b"IHDR",
                &[0, 0, 0, 2, 0, 0, 0, 1, depth, color, 0, 0, 0],
            );
            for (kind, data) in extras {
                chunk(&mut png, kind, data);
            }
            chunk(&mut png, b"IDAT", &[0x78, 0x01, 1]);
            chunk(&mut png, b"IEND", &[]);
            png
        }

        let gray = parse_raster(&png(1, 0, &[])).expect("one-bit grayscale PNG");
        let gray_object = image_object(&Objects::new(), &gray, None);
        let gray_text = String::from_utf8_lossy(&gray_object);
        assert!(gray_text.contains("/ColorSpace /DeviceGray"), "{gray_text}");
        assert!(gray_text.contains("/BitsPerComponent 1"), "{gray_text}");
        assert!(gray_text.contains("/Colors 1"), "{gray_text}");

        let indexed = parse_raster(&png(
            2,
            3,
            &[(b"PLTE", &[10, 20, 30, 220, 210, 200]), (b"tRNS", &[0, 255])],
        ))
        .expect("two-bit indexed PNG");
        let indexed_object = image_object(&Objects::new(), &indexed, None);
        let indexed_text = String::from_utf8_lossy(&indexed_object);
        assert!(
            indexed_text.contains("/ColorSpace [/Indexed /DeviceRGB 1 <0A141EDCD2C8>]"),
            "{indexed_text}"
        );
        assert!(indexed_text.contains("/BitsPerComponent 2"), "{indexed_text}");
        assert!(indexed_text.contains("/Mask [0 0]"), "{indexed_text}");
    }

    #[test]
    fn equivalent_alpha_planes_share_one_soft_mask_object() {
        fn pam(rgb: [u8; 6]) -> Vec<u8> {
            let mut bytes =
                b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
                    .to_vec();
            bytes.extend_from_slice(&[
                rgb[0], rgb[1], rgb[2], 32, rgb[3], rgb[4], rgb[5], 224,
            ]);
            bytes
        }
        let first = parse_raster(&pam([1, 2, 3, 4, 5, 6])).expect("first RGBA");
        let second = parse_raster(&pam([7, 8, 9, 10, 11, 12])).expect("second RGBA");
        let mut objects = Objects::new();
        let mut masks = std::collections::BTreeMap::new();
        write_raster_objects_with_masks(&mut objects, &first, &mut masks);
        write_raster_objects_with_masks(&mut objects, &second, &mut masks);
        let text = String::from_utf8_lossy(&objects.debug_body()).into_owned();
        assert_eq!(masks.len(), 1, "{text}");
        assert_eq!(text.matches("/Subtype /Image").count(), 3, "{text}");
        assert_eq!(text.matches("/SMask ").count(), 2, "{text}");
    }

    #[test]
    fn rgba_pam_becomes_an_rgb_image_with_a_soft_mask() {
        let mut pam =
            b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
        pam.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 192]);
        let raster = parse_raster(&pam).expect("RGBA PAM");
        assert!(matches!(
            raster.alpha.as_ref(),
            Some(AlphaEncoding::Raw(alpha)) if alpha == &[0, 192]
        ));

        let mut objects = Objects::new();
        let image = write_raster_objects(&mut objects, &raster);
        let body = objects.debug_body();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("/ColorSpace /DeviceGray"), "{text}");
        assert!(
            text.contains(&format!("/SMask {} 0 R", image - 1)),
            "{text}"
        );
        assert!(
            body.windows(6)
                .any(|window| window == [10, 20, 30, 40, 50, 60]),
            "RGB samples were not deinterleaved"
        );
    }

    #[test]
    fn compressed_rgba_bundle_preserves_rgb_and_mask_streams() {
        fn png(color_type: u8, idat: &[u8]) -> Vec<u8> {
            fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
                png.extend_from_slice(&(data.len() as u32).to_be_bytes());
                png.extend_from_slice(kind);
                png.extend_from_slice(data);
                png.extend_from_slice(&[0; 4]);
            }
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            chunk(
                &mut png,
                b"IHDR",
                &[0, 0, 0, 2, 0, 0, 0, 1, 8, color_type, 0, 0, 0],
            );
            chunk(&mut png, b"IDAT", idat);
            chunk(&mut png, b"IEND", &[]);
            png
        }

        let rgb_idat = [1, 2, 3, 4];
        let alpha_idat = [5, 6, 7];
        let rgb = png(2, &rgb_idat);
        let alpha = png(0, &alpha_idat);
        let mut bundle = ALPHA_PNG_BUNDLE.to_vec();
        bundle.extend_from_slice(&(rgb.len() as u64).to_be_bytes());
        bundle.extend_from_slice(&rgb);
        bundle.extend_from_slice(&alpha);

        let raster = parse_raster(&bundle).expect("compressed RGBA bundle");
        assert!(matches!(
            &raster.encoding,
            RasterEncoding::FlatePng(bytes) if bytes == &rgb_idat
        ));
        assert!(matches!(
            raster.alpha.as_ref(),
            Some(AlphaEncoding::FlatePng(bytes)) if bytes == &alpha_idat
        ));
        let mut objects = Objects::new();
        write_raster_objects(&mut objects, &raster);
        let body = objects.debug_body();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("/Colors 3"), "{text}");
        assert!(text.contains("/Colors 1"), "{text}");
        assert!(text.contains("/SMask"), "{text}");
    }

    #[test]
    fn compressed_alpha_bundle_accepts_a_grayscale_colour_plane() {
        fn png(color_type: u8, idat: &[u8]) -> Vec<u8> {
            fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
                png.extend_from_slice(&(data.len() as u32).to_be_bytes());
                png.extend_from_slice(kind);
                png.extend_from_slice(data);
                png.extend_from_slice(&[0; 4]);
            }
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            chunk(
                &mut png,
                b"IHDR",
                &[0, 0, 0, 2, 0, 0, 0, 1, 8, color_type, 0, 0, 0],
            );
            chunk(&mut png, b"IDAT", idat);
            chunk(&mut png, b"IEND", &[]);
            png
        }

        let gray_idat = [1, 2, 3];
        let alpha_idat = [4, 5, 6];
        let gray = png(0, &gray_idat);
        let alpha = png(0, &alpha_idat);
        let mut bundle = ALPHA_PNG_BUNDLE.to_vec();
        bundle.extend_from_slice(&(gray.len() as u64).to_be_bytes());
        bundle.extend_from_slice(&gray);
        bundle.extend_from_slice(&alpha);

        let raster = parse_raster(&bundle).expect("compressed grayscale alpha bundle");
        assert!(matches!(raster.color, RasterColor::Gray));
        let mut objects = Objects::new();
        write_raster_objects(&mut objects, &raster);
        let body = objects.debug_body();
        let text = String::from_utf8_lossy(&body);
        assert_eq!(text.matches("/ColorSpace /DeviceGray").count(), 2, "{text}");
        assert!(text.contains("/SMask"), "{text}");
    }

    #[test]
    fn filtered_png_bundle_accepts_an_optional_alpha_stream() {
        let rgb = [1, 2, 3, 4];
        let alpha = [5, 6, 7];
        let mut bundle = FILTERED_PNG_BUNDLE.to_vec();
        bundle.extend_from_slice(&2u32.to_be_bytes());
        bundle.extend_from_slice(&1u32.to_be_bytes());
        bundle.extend_from_slice(&(rgb.len() as u64).to_be_bytes());
        bundle.extend_from_slice(&(alpha.len() as u64).to_be_bytes());
        bundle.extend_from_slice(&rgb);
        bundle.extend_from_slice(&alpha);

        let raster = parse_raster(&bundle).expect("filtered PNG bundle");
        assert_eq!((raster.width, raster.height), (2, 1));
        assert!(matches!(
            &raster.encoding,
            RasterEncoding::FlatePng(bytes) if bytes == &rgb
        ));
        assert!(matches!(
            raster.alpha.as_ref(),
            Some(AlphaEncoding::FlatePng(bytes)) if bytes == &alpha
        ));

        let mut opaque = FILTERED_PNG_BUNDLE.to_vec();
        opaque.extend_from_slice(&2u32.to_be_bytes());
        opaque.extend_from_slice(&1u32.to_be_bytes());
        opaque.extend_from_slice(&(rgb.len() as u64).to_be_bytes());
        opaque.extend_from_slice(&0u64.to_be_bytes());
        opaque.extend_from_slice(&rgb);
        assert!(parse_raster(&opaque).is_some_and(|raster| raster.alpha.is_none()));
    }

    #[test]
    fn rgb_jpeg_is_embedded_without_decoding() {
        // Minimal marker sequence sufficient for the header parser. The PDF
        // writer deliberately treats the entropy-coded payload as opaque.
        let jpeg = [
            0xff, 0xd8, // SOI
            0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, // APP0
            0xff, 0xc2, 0x00, 0x08, 0x08, 0x00, 0x50, 0x00, 0x78, 0x03, // SOF2: 120x80 RGB
            0xff, 0xd9, // EOI
        ];
        let raster = parse_raster(&jpeg).expect("RGB JPEG");
        assert_eq!((raster.width, raster.height), (120, 80));
        let objects = Objects::new();
        let object = image_object(&objects, &raster, None);
        let text = String::from_utf8_lossy(&object);
        assert!(text.contains("/Filter /DCTDecode"), "{text}");
        assert!(object.windows(jpeg.len()).any(|window| window == jpeg));
    }

    #[test]
    fn grayscale_jpeg_is_embedded_without_rgb_expansion() {
        let jpeg = [
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x08, 0x08, 0x00, 0x50, 0x00, 0x78, 0x01,
            0xff, 0xd9,
        ];
        let raster = parse_raster(&jpeg).expect("grayscale JPEG");
        let object = image_object(&Objects::new(), &raster, None);
        let text = String::from_utf8_lossy(&object);
        assert!(text.contains("/ColorSpace /DeviceGray"), "{text}");
        assert!(text.contains("/Filter /DCTDecode"), "{text}");
        assert!(object.windows(jpeg.len()).any(|window| window == jpeg));
    }

    #[test]
    fn adobe_cmyk_and_ycck_jpegs_are_embedded_with_pdf_sample_inversion() {
        for transform in [0, 2] {
            let jpeg = [
                0xff, 0xd8, // SOI
                0xff, 0xee, 0x00, 0x0e, b'A', b'd', b'o', b'b', b'e', 0x00, 0x64, 0x00,
                0x00, 0x00, 0x00, transform, // Adobe APP14
                0xff, 0xc0, 0x00, 0x08, 0x08, 0x00, 0x50, 0x00, 0x78, 0x04, // 120x80 CMYK
                0xff, 0xd9, // EOI
            ];
            let raster = parse_raster(&jpeg).expect("CMYK JPEG");
            assert_eq!((raster.width, raster.height), (120, 80));
            let object = image_object(&Objects::new(), &raster, None);
            let text = String::from_utf8_lossy(&object);
            assert!(text.contains("/ColorSpace /DeviceCMYK"), "{text}");
            assert!(text.contains("/Decode [1 0 1 0 1 0 1 0]"), "{text}");
            assert!(text.contains("/Filter /DCTDecode"), "{text}");
            assert!(object.windows(jpeg.len()).any(|window| window == jpeg));
        }
    }
}
