const OPEN_TYPE_SUBSET_CACHE_BYTES: usize = 64 * 1024 * 1024;
const OPEN_TYPE_SUBSET_CACHE_ENTRIES: usize = 128;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OpenTypeSubsetCacheKey {
    digest: [u8; 32],
    face_index: u32,
    variations: Vec<([u8; 4], i16)>,
    glyphs: Vec<u32>,
    unicodes: Vec<u32>,
    implementation: &'static str,
}

#[derive(Default)]
struct OpenTypeSubsetCache {
    entries: std::collections::VecDeque<(
        OpenTypeSubsetCacheKey,
        Result<aitex_fonts::PreparedSubset, aitex_fonts::FontError>,
        usize,
    )>,
    bytes: usize,
}

impl OpenTypeSubsetCache {
    fn get(
        &mut self,
        key: &OpenTypeSubsetCacheKey,
    ) -> Option<Result<aitex_fonts::PreparedSubset, aitex_fonts::FontError>> {
        let index = self.entries.iter().position(|entry| &entry.0 == key)?;
        let entry = self.entries.remove(index)?;
        let result = entry.1.clone();
        self.entries.push_back(entry);
        Some(result)
    }

    fn insert(
        &mut self,
        key: OpenTypeSubsetCacheKey,
        result: Result<aitex_fonts::PreparedSubset, aitex_fonts::FontError>,
    ) {
        if let Some(index) = self.entries.iter().position(|entry| entry.0 == key) {
            if let Some(replaced) = self.entries.remove(index) {
                self.bytes = self.bytes.saturating_sub(replaced.2);
            }
        }
        let retained = result.as_ref().map_or(0, |subset| subset.bytes.len());
        if retained > OPEN_TYPE_SUBSET_CACHE_BYTES {
            return;
        }
        while self.entries.len() >= OPEN_TYPE_SUBSET_CACHE_ENTRIES
            || self.bytes.saturating_add(retained) > OPEN_TYPE_SUBSET_CACHE_BYTES
        {
            let Some(expired) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(expired.2);
        }
        self.bytes = self.bytes.saturating_add(retained);
        self.entries.push_back((key, result, retained));
    }
}

fn open_type_subsets() -> &'static std::sync::Mutex<OpenTypeSubsetCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<OpenTypeSubsetCache>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(OpenTypeSubsetCache::default()))
}

#[derive(Default)]
struct PositionedFontUsage {
    glyphs: std::collections::BTreeSet<aitex_fonts::SourceGlyphId>,
    unicodes: std::collections::BTreeSet<char>,
    variants: std::collections::BTreeSet<PositionedGlyphVariant>,
    horizontal: bool,
    vertical: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PositionedGlyphVariant {
    source_gid: u32,
    text: String,
}

struct PositionedPdfFont {
    horizontal_object_id: Option<usize>,
    vertical_object_id: Option<usize>,
    horizontal_resource_name: String,
    vertical_resource_name: String,
    widths: std::collections::BTreeMap<aitex_fonts::Cid, i32>,
    vertical_metrics:
        std::collections::BTreeMap<aitex_fonts::Cid, aitex_fonts::PdfGlyphVerticalMetric>,
    code_to_cid: std::collections::BTreeMap<aitex_fonts::PdfCode, aitex_fonts::Cid>,
    cid_to_subset:
        std::collections::BTreeMap<aitex_fonts::Cid, aitex_fonts::SubsetGlyphId>,
    variant_codes: std::collections::BTreeMap<PositionedGlyphVariant, aitex_fonts::PdfCode>,
    color_glyphs:
        std::collections::BTreeMap<aitex_fonts::PdfCode, Vec<aitex_fonts::ColorGlyphLayer>>,
}

impl PositionedPdfFont {
    fn resource(&self, direction: aitex_page::TextDirection) -> Option<(&str, usize)> {
        if matches!(
            direction,
            aitex_page::TextDirection::TopToBottom | aitex_page::TextDirection::BottomToTop
        ) {
            self.vertical_object_id
                .map(|id| (self.vertical_resource_name.as_str(), id))
        } else {
            self.horizontal_object_id
                .map(|id| (self.horizontal_resource_name.as_str(), id))
        }
    }
}

fn positioned_glyph_variant(
    run: &aitex_page::GlyphRun,
    glyph_index: usize,
) -> Result<PositionedGlyphVariant, PositionedPdfError> {
    let glyph = &run.glyphs[glyph_index];
    let cluster = run.clusters.get(glyph.cluster.0 as usize).ok_or(
        PositionedPdfError::InvalidCluster {
            font: run.font,
            cluster: glyph.cluster,
        },
    )?;
    let first = !run.glyphs[..glyph_index]
        .iter()
        .any(|earlier| earlier.cluster == glyph.cluster);
    Ok(PositionedGlyphVariant {
        source_gid: glyph.source_gid.0,
        // A PDF character code can map to a whole shaped cluster. Additional
        // glyphs in that cluster map to whitespace so readers which ignore
        // ActualText neither duplicate the source nor emit U+FFFD.
        text: if first && !cluster.text.is_empty() {
            cluster.text.clone()
        } else {
            String::from(" ")
        },
    })
}

fn positioned_code(
    run: &aitex_page::GlyphRun,
    glyph_index: usize,
    font: &PositionedPdfFont,
) -> Result<aitex_fonts::PdfCode, PositionedPdfError> {
    let variant = positioned_glyph_variant(run, glyph_index)?;
    font.variant_codes.get(&variant).copied().ok_or(
        PositionedPdfError::GlyphOutsidePdfCodeSpace(run.glyphs[glyph_index].source_gid),
    )
}

fn positioned_sp(value: aitex_page::Sp) -> f64 {
    pt(value.0)
}

fn collect_positioned_usage_with_policy(
    document: &aitex_page::Document,
    allow_classic: bool,
) -> Result<
    std::collections::BTreeMap<aitex_page::FontInstanceId, PositionedFontUsage>,
    PositionedPdfError,
> {
    let mut usage = std::collections::BTreeMap::<
        aitex_page::FontInstanceId,
        PositionedFontUsage,
    >::new();
    for item in document.pages.iter().flat_map(|page| &page.items) {
        let run = match item {
            aitex_page::Item::GlyphRun(run) => run,
            aitex_page::Item::ClassicGlyph { .. } if !allow_classic => {
                return Err(PositionedPdfError::UnsupportedClassicGlyph);
            }
            aitex_page::Item::Special { .. } if !allow_classic => {
                return Err(PositionedPdfError::UnsupportedSpecial);
            }
            aitex_page::Item::ClassicGlyph { .. }
            | aitex_page::Item::Special { .. }
            | aitex_page::Item::Rule { .. } => continue,
        };
        if run.glyphs.is_empty() {
            continue;
        }
        let font_usage = usage.entry(run.font).or_default();
        if matches!(
            run.direction,
            aitex_page::TextDirection::TopToBottom | aitex_page::TextDirection::BottomToTop
        ) {
            font_usage.vertical = true;
        } else {
            font_usage.horizontal = true;
        }
        for (glyph_index, glyph) in run.glyphs.iter().enumerate() {
            u16::try_from(glyph.source_gid.0)
                .map_err(|_| PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid))?;
            let cluster = run.clusters.get(glyph.cluster.0 as usize).ok_or(
                PositionedPdfError::InvalidCluster {
                    font: run.font,
                    cluster: glyph.cluster,
                },
            )?;
            font_usage
                .glyphs
                .insert(aitex_fonts::SourceGlyphId(glyph.source_gid.0));
            font_usage.unicodes.extend(cluster.text.chars());
            font_usage
                .variants
                .insert(positioned_glyph_variant(run, glyph_index)?);
        }
    }
    Ok(usage)
}

fn collect_positioned_usage(
    document: &aitex_page::Document,
) -> Result<
    std::collections::BTreeMap<aitex_page::FontInstanceId, PositionedFontUsage>,
    PositionedPdfError,
> {
    collect_positioned_usage_with_policy(document, false)
}

fn subset_cache_key(
    instance: &aitex_fonts::FontInstanceIdentity,
    glyphs: &[aitex_fonts::SourceGlyphId],
    unicodes: &[char],
) -> OpenTypeSubsetCacheKey {
    OpenTypeSubsetCacheKey {
        digest: instance.face.digest.0,
        face_index: instance.face.index.0,
        variations: instance
            .variations
            .iter()
            .map(|variation| (variation.tag.0, variation.coordinate))
            .collect(),
        glyphs: glyphs.iter().map(|glyph| glyph.0).collect(),
        unicodes: unicodes.iter().copied().map(u32::from).collect(),
        implementation: aitex_fonts::SKERA_SUBSETTER_VERSION,
    }
}

fn cached_open_type_subset(
    source: &OpenTypeFontSource,
    glyphs: &[aitex_fonts::SourceGlyphId],
    unicodes: &[char],
) -> Result<aitex_fonts::PreparedSubset, aitex_fonts::FontError> {
    use aitex_fonts::FontSubsetter as _;

    let info = aitex_fonts::inspect_face(&source.bytes, source.face_index)?;
    let instance = aitex_fonts::font_instance_identity(
        &source.bytes,
        source.face_index,
        &source.variations,
    )?;
    let key = subset_cache_key(&instance, glyphs, unicodes);
    if let Some(prepared) = &source.prepared_subset {
        let identity = &prepared.identity;
        let exact = identity.face == info.identity
            && identity.glyphs == glyphs
            && identity.unicodes
                == unicodes.iter().copied().map(u32::from).collect::<Vec<_>>()
            && identity.variations == instance.variations
            && identity.embedding == info.embedding
            && identity.policy == "respect-fstype-v1"
            && identity.implementation == aitex_fonts::SKERA_SUBSETTER_VERSION;
        if exact {
            return Ok(prepared.as_ref().clone());
        }
    }
    if let Some(result) = open_type_subsets()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
    {
        return result;
    }
    let request = aitex_fonts::SubsetRequest {
        face: info.identity,
        glyphs,
        unicodes,
        variations: &source.variations,
    };
    let result = aitex_fonts::SkeraSubsetter.subset(&source.bytes, &request);
    open_type_subsets()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key, result.clone());
    result
}

fn subset_prefix(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (0..6)
        .map(|shift| ((hash >> (shift * 5)) as u8 % 26 + b'A') as char)
        .collect()
}

fn utf16be_hex(text: &str, bom: bool) -> String {
    let mut output = String::from("<");
    if bom {
        output.push_str("FEFF");
    }
    for unit in text.encode_utf16() {
        output.push_str(&format!("{unit:04X}"));
    }
    output.push('>');
    output
}

fn positioned_to_unicode_cmap(
    mappings: &std::collections::BTreeMap<aitex_fonts::PdfCode, Option<String>>,
) -> Option<Vec<u8>> {
    let mappings = mappings
        .iter()
        .filter_map(|(code, text)| text.as_ref().map(|text| (*code, text)))
        .collect::<Vec<_>>();
    if mappings.is_empty() {
        return None;
    }
    let mut output = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n".to_vec();
    for chunk in mappings.chunks(100) {
        output.extend_from_slice(format!("{} beginbfchar\n", chunk.len()).as_bytes());
        for (code, text) in chunk {
            output.extend_from_slice(
                format!("<{:04X}> {}\n", code.0, utf16be_hex(text, false)).as_bytes(),
            );
        }
        output.extend_from_slice(b"endbfchar\n");
    }
    output.extend_from_slice(b"endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    Some(output)
}

fn cid_widths(widths: &std::collections::BTreeMap<aitex_fonts::Cid, i32>) -> (i32, String) {
    let mut frequencies = std::collections::BTreeMap::<i32, usize>::new();
    for width in widths.values() {
        *frequencies.entry(*width).or_default() += 1;
    }
    let default_width = frequencies
        .into_iter()
        .max_by_key(|(width, count)| (*count, std::cmp::Reverse(*width)))
        .map_or(1000, |(width, _)| width);
    let values = widths
        .iter()
        .filter(|(_, width)| **width != default_width)
        .map(|(cid, width)| format!("{} [{width}]", cid.0))
        .collect::<Vec<_>>()
        .join(" ");
    (default_width, values)
}

fn cid_vertical_widths(
    metrics: &std::collections::BTreeMap<aitex_fonts::Cid, aitex_fonts::PdfGlyphVerticalMetric>,
    widths: &std::collections::BTreeMap<aitex_fonts::Cid, i32>,
) -> (i32, i32, String) {
    let mut frequencies = std::collections::BTreeMap::<(i32, i32), usize>::new();
    for metric in metrics.values() {
        *frequencies
            .entry((metric.origin_y, -metric.advance_height))
            .or_default() += 1;
    }
    let (default_origin_y, default_advance) = frequencies
        .into_iter()
        .max_by_key(|(pair, count)| (*count, std::cmp::Reverse(*pair)))
        .map_or((880, -1000), |(pair, _)| pair);
    let values = metrics
        .iter()
        .filter_map(|(cid, metric)| {
            let default_origin_x = widths.get(cid).copied().unwrap_or(1000) / 2;
            (metric.origin_y != default_origin_y
                || -metric.advance_height != default_advance
                || metric.origin_x != default_origin_x)
                .then(|| {
                    format!(
                        "{} [{} {} {}]",
                        cid.0, -metric.advance_height, metric.origin_x, metric.origin_y
                    )
                })
        })
        .collect::<Vec<_>>()
        .join(" ");
    (default_origin_y, default_advance, values)
}

fn write_positioned_font(
    objects: &mut Objects,
    font_id: aitex_page::FontInstanceId,
    usage: &PositionedFontUsage,
    source: &OpenTypeFontSource,
) -> Result<PositionedPdfFont, PositionedPdfError> {
    let source_info = aitex_fonts::inspect_face(&source.bytes, source.face_index)?;
    let mut variant_codes = std::collections::BTreeMap::new();
    let mut seen_sources = std::collections::BTreeSet::new();
    let mut next_variant_code = source_info.glyph_count;
    for variant in &usage.variants {
        let raw_code = if seen_sources.insert(variant.source_gid) {
            variant.source_gid
        } else {
            let code = next_variant_code;
            next_variant_code = next_variant_code.saturating_add(1);
            code
        };
        let code = u16::try_from(raw_code).map_err(|_| {
            PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                variant.source_gid,
            ))
        })?;
        variant_codes.insert(variant.clone(), aitex_fonts::PdfCode(code));
    }
    let mut glyphs = usage.glyphs.clone();
    let mut color_glyphs = std::collections::BTreeMap::new();
    let requested_glyphs = usage.glyphs.iter().copied().collect::<Vec<_>>();
    for (glyph, color_glyph) in aitex_fonts::colr_v0_glyphs(
        &source.bytes,
        source.face_index,
        &source.variations,
        &requested_glyphs,
        source.palette_index,
    )? {
        let code = aitex_fonts::PdfCode(u16::try_from(glyph.0).map_err(|_| {
            PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(glyph.0))
        })?);
        for layer in &color_glyph.layers {
            u16::try_from(layer.source_glyph.0).map_err(|_| {
                PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                    layer.source_glyph.0,
                ))
            })?;
            glyphs.insert(layer.source_glyph);
        }
        color_glyphs.insert(code, color_glyph.layers);
    }
    let glyphs = glyphs.into_iter().collect::<Vec<_>>();
    let unicodes = usage.unicodes.iter().copied().collect::<Vec<_>>();
    let metrics = aitex_fonts::pdf_face_metrics(
        &source.bytes,
        source.face_index,
        &source.variations,
        &glyphs,
    )?;
    let subset = cached_open_type_subset(source, &glyphs, &unicodes)?;
    let embedded_info = aitex_fonts::inspect_face(&subset.bytes, aitex_fonts::FaceIndex(0))?;
    let source_to_subset = subset
        .glyph_mapping
        .iter()
        .map(|mapping| {
            u16::try_from(mapping.source.0).map_err(|_| {
                PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                    mapping.source.0,
                ))
            })?;
            let subset = u16::try_from(mapping.subset.0).map_err(|_| {
                PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                    mapping.subset.0,
                ))
            })?;
            Ok((mapping.source.0, aitex_fonts::SubsetGlyphId(u32::from(subset))))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, PositionedPdfError>>()?;
    let source_widths = metrics
        .glyph_widths
        .iter()
        .map(|width| (width.source.0, width.width))
        .collect::<std::collections::BTreeMap<_, _>>();
    let source_vertical_metrics = if usage.vertical {
        aitex_fonts::pdf_vertical_metrics(
            &source.bytes,
            source.face_index,
            &source.variations,
            &glyphs,
        )?
        .glyphs
        .into_iter()
        .map(|metric| (metric.source.0, metric))
        .collect()
    } else {
        std::collections::BTreeMap::new()
    };
    let mut code_sources = glyphs
        .iter()
        .map(|glyph| {
            let value = glyph.0 as u16;
            (aitex_fonts::PdfCode(value), glyph.0)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for (variant, code) in &variant_codes {
        code_sources.insert(*code, variant.source_gid);
    }
    let code_to_cid = code_sources
        .keys()
        .map(|code| (*code, aitex_fonts::Cid(code.0)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let cid_to_subset = code_sources
        .iter()
        .map(|(code, source_gid)| {
            let subset = source_to_subset.get(source_gid).copied().ok_or(
                PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                    *source_gid,
                )),
            )?;
            Ok((aitex_fonts::Cid(code.0), subset))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, PositionedPdfError>>()?;
    let widths = code_sources
        .iter()
        .map(|(code, source_gid)| {
            let width = source_widths.get(source_gid).copied().ok_or(
                PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                    *source_gid,
                )),
            )?;
            Ok((aitex_fonts::Cid(code.0), width))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, PositionedPdfError>>()?;
    let vertical_metrics = if usage.vertical {
        code_sources
            .iter()
            .map(|(code, source_gid)| {
                let metric = source_vertical_metrics.get(source_gid).cloned().ok_or(
                    PositionedPdfError::GlyphOutsidePdfCodeSpace(aitex_page::SourceGlyphId(
                        *source_gid,
                    )),
                )?;
                Ok((aitex_fonts::Cid(code.0), metric))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, PositionedPdfError>>()?
    } else {
        std::collections::BTreeMap::new()
    };
    let base_name = if subset.was_subset {
        format!("{}+{}", subset_prefix(&subset.bytes), metrics.postscript_name)
    } else {
        metrics.postscript_name.clone()
    };
    let pdf_base_name = pdf_name(&base_name);

    let font_file_id = objects.reserve();
    match embedded_info.format {
        aitex_fonts::FontFormat::TrueType => objects.write_stream(
            font_file_id,
            format!("<< /Length1 {}", subset.bytes.len()).as_bytes(),
            &subset.bytes,
        ),
        aitex_fonts::FontFormat::Cff => objects.write_stream(
            font_file_id,
            b"<< /Subtype /OpenType",
            &subset.bytes,
        ),
        aitex_fonts::FontFormat::Cff2 | aitex_fonts::FontFormat::Unsupported => {
            return Err(aitex_fonts::FontError::UnsupportedOutline(embedded_info.format).into());
        }
    }

    let descriptor_id = objects.reserve();
    let [x0, y0, x1, y1] = metrics.bounding_box;
    let flags = 32 + i32::from(metrics.italic_angle != 0.0) * 64;
    let font_file_entry = match embedded_info.format {
        aitex_fonts::FontFormat::TrueType => "FontFile2",
        aitex_fonts::FontFormat::Cff => "FontFile3",
        _ => unreachable!(),
    };
    objects.write(
        descriptor_id,
        format!(
            "<< /Type /FontDescriptor /FontName /{pdf_base_name} /Flags {flags} \
             /FontBBox [{x0} {y0} {x1} {y1}] /ItalicAngle {:.4} /Ascent {} \
             /Descent {} /CapHeight {} /StemV 80 /{font_file_entry} {font_file_id} 0 R >>",
            metrics.italic_angle, metrics.ascent, metrics.descent, metrics.cap_height
        )
        .as_bytes(),
    );

    let cid_to_gid = if embedded_info.format == aitex_fonts::FontFormat::TrueType
        && cid_to_subset
            .iter()
            .all(|(cid, subset)| u32::from(cid.0) == subset.0)
    {
        String::from("/Identity")
    } else if embedded_info.format == aitex_fonts::FontFormat::TrueType {
        let last = cid_to_subset.keys().next_back().copied().unwrap_or(aitex_fonts::Cid(0));
        let mut mapping = vec![0u8; (usize::from(last.0) + 1) * 2];
        for (cid, gid) in &cid_to_subset {
            let offset = usize::from(cid.0) * 2;
            mapping[offset..offset + 2].copy_from_slice(&(gid.0 as u16).to_be_bytes());
        }
        let id = objects.reserve();
        objects.write_stream(id, b"<<", &mapping);
        format!("{id} 0 R")
    } else {
        String::new()
    };
    let (default_width, width_entries) = cid_widths(&widths);
    let widths_dictionary = if width_entries.is_empty() {
        String::new()
    } else {
        format!(" /W [{width_entries}]")
    };
    let vertical_widths_dictionary = if usage.vertical {
        let (origin_y, advance, entries) = cid_vertical_widths(&vertical_metrics, &widths);
        if entries.is_empty() {
            format!(" /DW2 [{origin_y} {advance}]")
        } else {
            format!(" /DW2 [{origin_y} {advance}] /W2 [{entries}]")
        }
    } else {
        String::new()
    };
    let subtype = if embedded_info.format == aitex_fonts::FontFormat::TrueType {
        "CIDFontType2"
    } else {
        "CIDFontType0"
    };
    let cid_to_gid_entry = if cid_to_gid.is_empty() {
        String::new()
    } else {
        format!(" /CIDToGIDMap {cid_to_gid}")
    };
    let descendant_id = objects.reserve();
    objects.write(
        descendant_id,
        format!(
            "<< /Type /Font /Subtype /{subtype} /BaseFont /{pdf_base_name} \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor {descriptor_id} 0 R /DW {default_width}{widths_dictionary}\
             {vertical_widths_dictionary}{cid_to_gid_entry} >>"
        )
        .as_bytes(),
    );

    let mut to_unicode = variant_codes
        .iter()
        .map(|(variant, code)| (*code, Some(variant.text.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    for code in code_sources.keys() {
        to_unicode.entry(*code).or_insert_with(|| Some(String::from(" ")));
    }
    let to_unicode_id = positioned_to_unicode_cmap(&to_unicode).map(|cmap| {
        let id = objects.reserve();
        objects.write_stream(id, b"<<", &cmap);
        id
    });
    let to_unicode = to_unicode_id
        .map(|id| format!(" /ToUnicode {id} 0 R"))
        .unwrap_or_default();
    let mut type0 = |encoding: &str| {
        let object_id = objects.reserve();
        objects.write(
            object_id,
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{pdf_base_name} /Encoding /{encoding} \
                 /DescendantFonts [{descendant_id} 0 R]{to_unicode} >>"
            )
            .as_bytes(),
        );
        object_id
    };
    let horizontal_object_id = usage.horizontal.then(|| type0("Identity-H"));
    let vertical_object_id = usage.vertical.then(|| type0("Identity-V"));
    Ok(PositionedPdfFont {
        horizontal_object_id,
        vertical_object_id,
        // Keep a separate namespace from classic DVI resources (`/F<object>`)
        // so a hybrid page can never alias fonts by accident.
        horizontal_resource_name: format!("UF{}", font_id.0),
        vertical_resource_name: format!("UF{}V", font_id.0),
        widths,
        vertical_metrics,
        code_to_cid,
        cid_to_subset,
        variant_codes,
        color_glyphs,
    })
}

fn run_intrinsically_needs_actual_text(run: &aitex_page::GlyphRun) -> bool {
    if run.direction == aitex_page::TextDirection::RightToLeft
        || run.glyphs.iter().any(|glyph| glyph.is_notdef)
    {
        return true;
    }
    let mut counts = vec![0usize; run.clusters.len()];
    for glyph in &run.glyphs {
        let Some(count) = counts.get_mut(glyph.cluster.0 as usize) else {
            return true;
        };
        *count += 1;
    }
    counts.into_iter().any(|count| count != 1)
}

fn run_needs_actual_text(run: &aitex_page::GlyphRun, font: &PositionedPdfFont) -> bool {
    let has_color_glyphs = !font.color_glyphs.is_empty();
    if run_intrinsically_needs_actual_text(run)
        || run.glyphs.iter().any(|glyph| {
            u16::try_from(glyph.source_gid.0).is_ok_and(|code| {
                let code = aitex_fonts::PdfCode(code);
                has_color_glyphs && font.color_glyphs.contains_key(&code)
            })
        })
    {
        return true;
    }
    false
}

fn valid_language_tag(language: &str) -> bool {
    let mut subtags = language.split('-');
    let Some(primary) = subtags.next() else {
        return false;
    };
    (1..=8).contains(&primary.len())
        && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        && subtags.all(|subtag| {
            (1..=8).contains(&subtag.len())
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn role_name(role: aitex_page::TextRole) -> String {
    match role {
        aitex_page::TextRole::Span => String::from("Span"),
        aitex_page::TextRole::Paragraph => String::from("P"),
        aitex_page::TextRole::Heading(level @ 1..=6) => format!("H{level}"),
        aitex_page::TextRole::Heading(_) => String::from("H"),
        aitex_page::TextRole::Quote => String::from("Quote"),
        aitex_page::TextRole::Code => String::from("Code"),
        aitex_page::TextRole::Link => String::from("Link"),
        aitex_page::TextRole::Formula => String::from("Formula"),
        _ => String::from("Span"),
    }
}

struct MarkedRun<'a> {
    mcid: usize,
    role: String,
    language: Option<&'a str>,
}

fn write_positioned_color_run(
    output: &mut Vec<u8>,
    page_height: f64,
    run: &aitex_page::GlyphRun,
    font: &PositionedPdfFont,
    resource_name: &str,
    size: f64,
) -> Result<(), PositionedPdfError> {
    let mut pen_x = 0_i64;
    let mut pen_y = 0_i64;
    for (glyph_index, glyph) in run.glyphs.iter().enumerate() {
        let code = positioned_code(run, glyph_index, font)?;
        let Some(cid) = font.code_to_cid.get(&code) else {
            return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(
                glyph.source_gid,
            ));
        };
        if !font.cid_to_subset.contains_key(cid) {
            return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(
                glyph.source_gid,
            ));
        }
        let mut x = 72.0
            + positioned_sp(aitex_page::Sp(
                run.origin_x.0 + pen_x + glyph.x_offset.0,
            ));
        let mut y = page_height
            - 72.0
            - positioned_sp(aitex_page::Sp(
                run.origin_y.0 + pen_y + glyph.y_offset.0,
            ));
        if matches!(
            run.direction,
            aitex_page::TextDirection::TopToBottom | aitex_page::TextDirection::BottomToTop
        ) {
            let metric = font
                .vertical_metrics
                .get(cid)
                .ok_or(PositionedPdfError::GlyphOutsidePdfCodeSpace(
                    glyph.source_gid,
                ))?;
            x -= f64::from(metric.origin_x) * size / 1000.0;
            y -= f64::from(metric.origin_y) * size / 1000.0;
        }

        let source_code = aitex_fonts::PdfCode(
            u16::try_from(glyph.source_gid.0)
                .map_err(|_| PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid))?,
        );
        if let Some(layers) = font.color_glyphs.get(&source_code) {
            for layer in layers {
                let layer_code = aitex_fonts::PdfCode(
                    u16::try_from(layer.source_glyph.0).map_err(|_| {
                        PositionedPdfError::GlyphOutsidePdfCodeSpace(
                            aitex_page::SourceGlyphId(layer.source_glyph.0),
                        )
                    })?,
                );
                let Some(layer_cid) = font.code_to_cid.get(&layer_code) else {
                    return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(
                        aitex_page::SourceGlyphId(layer.source_glyph.0),
                    ));
                };
                if !font.cid_to_subset.contains_key(layer_cid) {
                    return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(
                        aitex_page::SourceGlyphId(layer.source_glyph.0),
                    ));
                }
                let color = match layer.brush {
                    aitex_fonts::ColorGlyphBrush::Foreground { alpha: 0 }
                    | aitex_fonts::ColorGlyphBrush::Solid(aitex_fonts::RgbaColor {
                        alpha: 0,
                        ..
                    }) => continue,
                    aitex_fonts::ColorGlyphBrush::Foreground { alpha: 255 } => String::new(),
                    aitex_fonts::ColorGlyphBrush::Solid(color) if color.alpha == 255 => format!(
                        "{:.6} {:.6} {:.6} rg\n",
                        f64::from(color.red) / 255.0,
                        f64::from(color.green) / 255.0,
                        f64::from(color.blue) / 255.0,
                    ),
                    aitex_fonts::ColorGlyphBrush::Foreground { alpha }
                    | aitex_fonts::ColorGlyphBrush::Solid(aitex_fonts::RgbaColor {
                        alpha,
                        ..
                    }) => {
                        return Err(PositionedPdfError::UnsupportedColorAlpha {
                            glyph: aitex_page::SourceGlyphId(layer.source_glyph.0),
                            alpha,
                        });
                    }
                    _ => {
                        return Err(PositionedPdfError::UnsupportedColorBrush(
                            aitex_page::SourceGlyphId(layer.source_glyph.0),
                        ));
                    }
                };
                output.extend_from_slice(b"q\n");
                output.extend_from_slice(color.as_bytes());
                output.extend_from_slice(
                    format!(
                        "BT /{resource_name} {size:.8} Tf\n1 0 0 1 {x:.8} {y:.8} Tm <{:04X}> Tj\nET\nQ\n",
                        layer_code.0
                    )
                    .as_bytes(),
                );
            }
        } else {
            output.extend_from_slice(
                format!(
                    "BT /{resource_name} {size:.8} Tf\n1 0 0 1 {x:.8} {y:.8} Tm <{:04X}> Tj\nET\n",
                    code.0
                )
                .as_bytes(),
            );
        }
        pen_x = pen_x.saturating_add(glyph.x_advance.0);
        pen_y = pen_y.saturating_add(glyph.y_advance.0);
    }
    Ok(())
}

fn write_positioned_run(
    output: &mut Vec<u8>,
    page_height: f64,
    run: &aitex_page::GlyphRun,
    font: &PositionedPdfFont,
    marked: Option<MarkedRun<'_>>,
) -> Result<(), PositionedPdfError> {
    if run.glyphs.is_empty() {
        return Ok(());
    }
    let actual_text = run_needs_actual_text(run, font).then(|| run.logical_text());
    if let Some(marked) = &marked {
        output.extend_from_slice(format!("/{} << /MCID {}", marked.role, marked.mcid).as_bytes());
        if let Some(language) = marked.language {
            output.extend_from_slice(
                format!(" /Lang {}", pdf_string(language.as_bytes())).as_bytes(),
            );
        }
        output.extend_from_slice(b" >> BDC\n");
        // Poppler honors ActualText on an ordinary Span but ignores it when it
        // shares the property dictionary that binds an MCID to the structure
        // tree. Keep the semantic role/MCID as the outer marked content and
        // nest the portable replacement-text span inside it.
        if let Some(text) = &actual_text {
            output.extend_from_slice(
                format!("/Span << /ActualText {} >> BDC\n", utf16be_hex(text, true)).as_bytes(),
            );
        }
    } else if let Some(text) = &actual_text {
        output.extend_from_slice(
            format!("/Span << /ActualText {} >> BDC\n", utf16be_hex(text, true)).as_bytes(),
        );
    }
    let size = positioned_sp(run.size);
    let Some((resource_name, _)) = font.resource(run.direction) else {
        return Err(PositionedPdfError::UnsupportedDirection(run.direction));
    };
    if !font.color_glyphs.is_empty()
        && run.glyphs.iter().any(|glyph| {
            u16::try_from(glyph.source_gid.0).is_ok_and(|code| {
                font.color_glyphs
                    .contains_key(&aitex_fonts::PdfCode(code))
            })
        })
    {
        write_positioned_color_run(
            output,
            page_height,
            run,
            font,
            resource_name,
            size,
        )?;
        if actual_text.is_some() {
            output.extend_from_slice(b"EMC\n");
        }
        if marked.is_some() {
            output.extend_from_slice(b"EMC\n");
        }
        return Ok(());
    }
    output.extend_from_slice(format!("BT /{resource_name} {size:.8} Tf\n").as_bytes());

    let horizontal = !matches!(
        run.direction,
        aitex_page::TextDirection::TopToBottom | aitex_page::TextDirection::BottomToTop
    ) && run
        .glyphs
        .iter()
        .all(|glyph| glyph.y_offset.0 == 0 && glyph.y_advance.0 == 0);
    if horizontal && run.size.0 != 0 {
        let first = &run.glyphs[0];
        let x = 72.0 + positioned_sp(aitex_page::Sp(run.origin_x.0 + first.x_offset.0));
        let y = page_height - 72.0 - positioned_sp(run.origin_y);
        output.extend_from_slice(format!("1 0 0 1 {x:.8} {y:.8} Tm\n[").as_bytes());
        let mut pen = 0i64;
        let mut previous_origin = first.x_offset.0;
        let mut previous_cid = None;
        for (index, glyph) in run.glyphs.iter().enumerate() {
            let code = positioned_code(run, index, font)?;
            let Some(cid) = font.code_to_cid.get(&code) else {
                return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid));
            };
            if !font.cid_to_subset.contains_key(cid) {
                return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid));
            }
            if index > 0 {
                let desired_origin = pen.saturating_add(glyph.x_offset.0);
                let delta = desired_origin.saturating_sub(previous_origin);
                let nominal = f64::from(
                    *font
                        .widths
                        .get(&previous_cid.expect("the preceding glyph has a CID"))
                        .unwrap_or(&1000),
                );
                let desired = delta as f64 * 1000.0 / run.size.0 as f64;
                let adjustment = nominal - desired;
                if adjustment.abs() > 0.000_001 {
                    output.extend_from_slice(format!(" {adjustment:.8} ").as_bytes());
                }
                previous_origin = desired_origin;
            }
            output.extend_from_slice(format!("<{:04X}>", code.0).as_bytes());
            previous_cid = Some(*cid);
            pen = pen.saturating_add(glyph.x_advance.0);
        }
        output.extend_from_slice(b"] TJ\n");
    } else {
        let mut pen_x = 0i64;
        let mut pen_y = 0i64;
        for (glyph_index, glyph) in run.glyphs.iter().enumerate() {
            let code = positioned_code(run, glyph_index, font)?;
            let Some(cid) = font.code_to_cid.get(&code) else {
                return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid));
            };
            if !font.cid_to_subset.contains_key(cid) {
                return Err(PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid));
            }
            let mut x = 72.0
                + positioned_sp(aitex_page::Sp(
                    run.origin_x.0 + pen_x + glyph.x_offset.0,
                ));
            let mut y = page_height
                - 72.0
                - positioned_sp(aitex_page::Sp(
                    run.origin_y.0 + pen_y + glyph.y_offset.0,
                ));
            if matches!(
                run.direction,
                aitex_page::TextDirection::TopToBottom | aitex_page::TextDirection::BottomToTop
            ) {
                let metric = font
                    .vertical_metrics
                    .get(cid)
                    .ok_or(PositionedPdfError::GlyphOutsidePdfCodeSpace(glyph.source_gid))?;
                x -= f64::from(metric.origin_x) * size / 1000.0;
                y -= f64::from(metric.origin_y) * size / 1000.0;
            }
            output.extend_from_slice(
                format!("1 0 0 1 {x:.8} {y:.8} Tm <{:04X}> Tj\n", code.0).as_bytes(),
            );
            pen_x = pen_x.saturating_add(glyph.x_advance.0);
            pen_y = pen_y.saturating_add(glyph.y_advance.0);
        }
    }
    output.extend_from_slice(b"ET\n");
    if actual_text.is_some() {
        output.extend_from_slice(b"EMC\n");
    }
    if marked.is_some() {
        output.extend_from_slice(b"EMC\n");
    }
    Ok(())
}

/// Render an already-positioned document with embedded OpenType Type 0 fonts.
///
/// This is the standalone native shaped-text route. Classic TeX shares the
/// page vocabulary but continues through the classic serializer above.
pub fn write_positioned(
    document: &aitex_page::Document,
    options: PdfOptions,
    resolve: impl FnMut(aitex_page::FontInstanceId) -> Option<OpenTypeFontSource>,
) -> Result<Vec<u8>, PositionedPdfError> {
    write_positioned_with_semantics(
        document,
        options,
        PositionedPdfSemantics::default(),
        resolve,
    )
}

/// Render a positioned document with an optional semantic structure tree.
pub fn write_positioned_with_semantics(
    document: &aitex_page::Document,
    options: PdfOptions,
    semantics: PositionedPdfSemantics<'_>,
    mut resolve: impl FnMut(aitex_page::FontInstanceId) -> Option<OpenTypeFontSource>,
) -> Result<Vec<u8>, PositionedPdfError> {
    if semantics.tag_text
        && !semantics
            .document_language
            .is_some_and(valid_language_tag)
    {
        return Err(PositionedPdfError::InvalidLanguage(
            semantics.document_language.unwrap_or_default().to_string(),
        ));
    }
    let usage = collect_positioned_usage(document)?;
    let mut objects = Objects::with_version(options, "1.7");
    let catalog_id = objects.reserve();
    let pages_id = objects.reserve();
    let struct_root_id = semantics.tag_text.then(|| objects.reserve());
    let parent_tree_id = semantics.tag_text.then(|| objects.reserve());
    let mut fonts = std::collections::BTreeMap::new();
    for (font_id, font_usage) in &usage {
        let source = resolve(*font_id).ok_or(PositionedPdfError::MissingFont(*font_id))?;
        fonts.insert(
            *font_id,
            write_positioned_font(&mut objects, *font_id, font_usage, &source)?,
        );
    }

    let mut page_ids = Vec::with_capacity(document.pages.len());
    let mut structure_elements = Vec::new();
    let mut parent_tree_pages = Vec::new();
    let mut resource_objects = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    for (page_index, page) in document.pages.iter().enumerate() {
        let page_id = objects.reserve();
        let content_id = objects.reserve();
        let page_width = f64::from(page.width_millibp) / 1000.0;
        let page_height = f64::from(page.height_millibp) / 1000.0;
        let mut content = Vec::new();
        let mut used_fonts = std::collections::BTreeSet::new();
        let mut page_structure = Vec::new();
        for item in &page.items {
            match item {
                aitex_page::Item::ClassicGlyph { .. } => {
                    return Err(PositionedPdfError::UnsupportedClassicGlyph);
                }
                aitex_page::Item::GlyphRun(run) => {
                    if run.glyphs.is_empty() {
                        continue;
                    }
                    let font = fonts
                        .get(&run.font)
                        .ok_or(PositionedPdfError::MissingFont(run.font))?;
                    used_fonts.insert((
                        run.font,
                        matches!(
                            run.direction,
                            aitex_page::TextDirection::TopToBottom
                                | aitex_page::TextDirection::BottomToTop
                        ),
                    ));
                    let marked = if let Some(struct_root_id) = struct_root_id {
                        if let Some(language) = run.language.as_deref()
                            && !valid_language_tag(language)
                        {
                            return Err(PositionedPdfError::InvalidLanguage(
                                language.to_string(),
                            ));
                        }
                        let mcid = page_structure.len();
                        let role = role_name(run.role);
                        let element_id = objects.reserve();
                        let language = run
                            .language
                            .as_deref()
                            .map(|language| {
                                format!(" /Lang {}", pdf_string(language.as_bytes()))
                            })
                            .unwrap_or_default();
                        objects.write(
                            element_id,
                            format!(
                                "<< /Type /StructElem /S /{role} /P {struct_root_id} 0 R \
                                 /Pg {page_id} 0 R /K {mcid}{language} >>"
                            )
                            .as_bytes(),
                        );
                        page_structure.push(element_id);
                        structure_elements.push(element_id);
                        Some(MarkedRun {
                            mcid,
                            role,
                            language: run.language.as_deref(),
                        })
                    } else {
                        None
                    };
                    write_positioned_run(&mut content, page_height, run, font, marked)?;
                }
                aitex_page::Item::Rule {
                    x,
                    baseline_y,
                    width,
                    height,
                } => {
                    let x = 72.0 + positioned_sp(*x);
                    let y = page_height - 72.0 - positioned_sp(*baseline_y);
                    let width = positioned_sp(*width);
                    let height = positioned_sp(*height);
                    content.extend_from_slice(
                        format!("{x:.8} {y:.8} {width:.8} {height:.8} re f\n").as_bytes(),
                    );
                }
                aitex_page::Item::Special { .. } => {
                    return Err(PositionedPdfError::UnsupportedSpecial);
                }
            }
        }
        objects.write_stream(content_id, b"<<", &content);
        let resources = used_fonts
            .iter()
            .filter_map(|(font_id, vertical)| {
                let font = fonts.get(font_id)?;
                let direction = if *vertical {
                    aitex_page::TextDirection::TopToBottom
                } else {
                    aitex_page::TextDirection::LeftToRight
                };
                font.resource(direction)
            })
            .map(|(name, id)| format!("/{name} {id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        let resource_id = objects.write_interned(
            &mut resource_objects,
            format!("<< /Font << {resources} >> >>").into_bytes(),
        );
        let structure = semantics
            .tag_text
            .then(|| format!(" /StructParents {page_index}"))
            .unwrap_or_default();
        objects.write(
            page_id,
            format!(
                "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {page_width:.3} {page_height:.3}] \
                 /Resources {resource_id} 0 R /Contents {content_id} 0 R{structure} >>"
            )
            .as_bytes(),
        );
        page_ids.push(page_id);
        parent_tree_pages.push(page_structure);
    }
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    objects.write(
        pages_id,
        format!(
            "<< /Type /Pages /Count {} /Kids [{kids}] >>",
            page_ids.len()
        )
        .as_bytes(),
    );
    let semantics_dictionary = if let (Some(struct_root_id), Some(parent_tree_id)) =
        (struct_root_id, parent_tree_id)
    {
        let kids = structure_elements
            .iter()
            .map(|id| format!("{id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        let nums = parent_tree_pages
            .iter()
            .enumerate()
            .map(|(index, elements)| {
                let elements = elements
                    .iter()
                    .map(|id| format!("{id} 0 R"))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{index} [{elements}]")
            })
            .collect::<Vec<_>>()
            .join(" ");
        objects.write(parent_tree_id, format!("<< /Nums [{nums}] >>").as_bytes());
        objects.write(
            struct_root_id,
            format!(
                "<< /Type /StructTreeRoot /K [{kids}] /ParentTree {parent_tree_id} 0 R \
                 /ParentTreeNextKey {} >>",
                parent_tree_pages.len()
            )
            .as_bytes(),
        );
        format!(
            " /StructTreeRoot {struct_root_id} 0 R /MarkInfo << /Marked true >> /Lang {}",
            pdf_string(
                semantics
                    .document_language
                    .expect("tagged semantics validated")
                    .as_bytes()
            )
        )
    } else {
        String::new()
    };
    objects.write(
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R{semantics_dictionary} >>").as_bytes(),
    );
    Ok(objects.finish(catalog_id, None))
}

#[cfg(test)]
mod positioned_cache_tests {
    use super::*;

    fn glyph(source_gid: u32, cluster: u32) -> aitex_page::PositionedGlyph {
        aitex_page::PositionedGlyph {
            source_gid: aitex_page::SourceGlyphId(source_gid),
            cluster: aitex_page::ClusterId(cluster),
            x_advance: aitex_page::Sp(1),
            y_advance: aitex_page::Sp(0),
            x_offset: aitex_page::Sp(0),
            y_offset: aitex_page::Sp(0),
            is_notdef: false,
        }
    }

    fn clustered_run(
        glyphs: Vec<aitex_page::PositionedGlyph>,
        texts: &[&str],
    ) -> aitex_page::GlyphRun {
        let mut byte_start = 0_u32;
        let clusters = texts
            .iter()
            .map(|text| {
                let byte_end = byte_start + u32::try_from(text.len()).expect("test text length");
                let cluster = aitex_page::TextCluster {
                    byte_range: byte_start..byte_end,
                    text: (*text).to_string(),
                };
                byte_start = byte_end;
                cluster
            })
            .collect();
        aitex_page::GlyphRun::new(
            aitex_page::FontInstanceId(0),
            aitex_page::Sp(10),
            aitex_page::Sp(0),
            aitex_page::Sp(0),
            aitex_page::TextDirection::LeftToRight,
            glyphs,
            clusters,
        )
    }

    fn run(glyphs: Vec<aitex_page::PositionedGlyph>, text: &str) -> aitex_page::GlyphRun {
        clustered_run(glyphs, &[text])
    }

    fn key(glyph: u32) -> OpenTypeSubsetCacheKey {
        OpenTypeSubsetCacheKey {
            digest: [0; 32],
            face_index: 0,
            variations: Vec::new(),
            glyphs: vec![glyph],
            unicodes: Vec::new(),
            implementation: "test",
        }
    }

    #[test]
    fn subset_cache_is_bounded_and_replaces_duplicate_keys() {
        let mut cache = OpenTypeSubsetCache::default();
        for glyph in 0..(OPEN_TYPE_SUBSET_CACHE_ENTRIES as u32 + 2) {
            cache.insert(key(glyph), Err(aitex_fonts::FontError::TextTooLong));
        }
        assert_eq!(cache.entries.len(), OPEN_TYPE_SUBSET_CACHE_ENTRIES);

        let newest = key(OPEN_TYPE_SUBSET_CACHE_ENTRIES as u32 + 1);
        cache.insert(newest.clone(), Err(aitex_fonts::FontError::TextTooLong));
        assert_eq!(cache.entries.len(), OPEN_TYPE_SUBSET_CACHE_ENTRIES);
        assert_eq!(cache.entries.back().map(|entry| &entry.0), Some(&newest));
    }

    #[test]
    fn glyph_codes_used_in_distinct_clusters_retain_distinct_semantics() {
        let document = aitex_page::Document {
            pages: vec![aitex_page::Page {
                items: vec![
                    aitex_page::Item::GlyphRun(run(vec![glyph(7, 0)], "x")),
                    aitex_page::Item::GlyphRun(run(vec![glyph(7, 0), glyph(8, 0)], "yz")),
                    aitex_page::Item::GlyphRun(clustered_run(
                        vec![glyph(7, 0), glyph(10, 1)],
                        &["x", "w"],
                    )),
                    aitex_page::Item::GlyphRun(run(vec![glyph(9, 0)], "q")),
                ],
                ..Default::default()
            }],
        };
        let usage = collect_positioned_usage(&document).expect("valid positioned usage");
        let font = &usage[&aitex_page::FontInstanceId(0)];
        let expected = [
            (7, "x"),
            (7, "yz"),
            (8, " "),
            (9, "q"),
            (10, "w"),
        ]
        .into_iter()
        .map(|(source_gid, text)| PositionedGlyphVariant {
            source_gid,
            text: text.to_string(),
        })
        .collect();
        assert_eq!(font.variants, expected);
    }
}
