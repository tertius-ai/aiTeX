use aitex_fonts::{
    ColorGlyphBrush, ColorGlyphFormat, EmbeddingPermission, FaceIndex, FontError, FontFormat,
    FontSubsetter, FontTag, MissingGlyphPolicy, RgbaColor, ShapeFeature, ShapeRequest,
    SkeraSubsetter, SubsetRequest, TextDirection, VariationSetting, colr_v0_glyph, inspect_face,
    shape,
};

const LATIN: &[u8] = include_bytes!("fonts/NotoSans-Latin-Test.ttf");
const ARABIC: &[u8] = include_bytes!("fonts/Amiri-Arabic-Test.ttf");
const HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Test.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Test.ttf");
const VARIABLE: &[u8] = include_bytes!("fonts/Junicode-Variable-Test.ttf");
const THAI: &[u8] = include_bytes!("fonts/NotoSansThai-Test.ttf");
const LAO: &[u8] = include_bytes!("fonts/NotoSansLao-Test.ttf");
const CJK: &[u8] = include_bytes!("fonts/NotoSansCJKsc-Test.otf");
const CFF2: &[u8] = include_bytes!("fonts/SourceSerif4-CFF2-Variable-Test.otf");
const COLRV0: &[u8] = include_bytes!("fonts/AmiriQuranColored-COLRv0-Test.ttf");

fn request<'a>(text: &'a str, direction: TextDirection, script: [u8; 4]) -> ShapeRequest<'a> {
    ShapeRequest::new(text, FaceIndex(0), direction, FontTag::new(script))
}

#[test]
fn latin_ligature_keeps_the_original_cluster() {
    let text = "office";
    let run =
        shape(LATIN, &request(text, TextDirection::LeftToRight, *b"Latn")).expect("shape Latin");

    assert!(
        run.glyphs.len() < text.chars().count(),
        "expected a ligature"
    );
    assert_eq!(
        run.clusters
            .iter()
            .map(|cluster| cluster.text.as_str())
            .collect::<String>(),
        text
    );
    assert!(run.glyphs.iter().all(|glyph| !glyph.is_notdef));
}

#[test]
fn opentype_features_are_explicit_inputs() {
    let text = "office";
    let normal = shape(LATIN, &request(text, TextDirection::LeftToRight, *b"Latn"))
        .expect("shape with default features");
    let features = [ShapeFeature::global(FontTag::new(*b"liga"), 0)];
    let mut without_ligatures = request(text, TextDirection::LeftToRight, *b"Latn");
    without_ligatures.features = &features;
    let unligated = shape(LATIN, &without_ligatures).expect("shape without ligatures");

    assert!(unligated.glyphs.len() > normal.glyphs.len());
    assert_eq!(unligated.glyphs.len(), text.chars().count());
}

#[test]
fn right_to_left_runs_retain_logical_unicode() {
    for (font, text, script) in [
        (ARABIC, "السَّلَامُ عَلَيْكُمْ", *b"Arab"),
        (HEBREW, "שָׁלוֹם עוֹלָם", *b"Hebr"),
    ] {
        let mut shape_request = request(text, TextDirection::RightToLeft, script);
        shape_request.language = Some(if script == *b"Arab" { "ar" } else { "he" });
        let run = shape(font, &shape_request).expect("shape RTL text");

        let first = run.glyphs.first().expect("glyphs").cluster.0;
        let last = run.glyphs.last().expect("glyphs").cluster.0;
        assert!(first > last, "visual glyphs should be in RTL order");
        assert_eq!(
            run.clusters
                .iter()
                .map(|cluster| cluster.text.as_str())
                .collect::<String>(),
            text,
            "clusters remain in logical order"
        );
    }
}

#[test]
fn vertical_shaping_uses_the_vertical_advance_axis() {
    let run = shape(
        CJK,
        &request("日本語", TextDirection::TopToBottom, *b"Hani"),
    )
    .expect("shape vertical CJK");
    assert!(run.glyphs.iter().all(|glyph| glyph.x_advance == 0));
    assert!(run.glyphs.iter().all(|glyph| glyph.y_advance < 0));

    let positioned = aitex_fonts::positioned_run(
        &run,
        aitex_page::FontInstanceId(0),
        aitex_page::Sp(655_360),
        aitex_page::Sp(0),
        aitex_page::Sp(0),
    );
    assert!(positioned.glyphs.iter().all(|glyph| glyph.x_advance.0 == 0));
    assert!(positioned.glyphs.iter().all(|glyph| glyph.y_advance.0 > 0));
}

#[test]
fn colrv0_layers_are_resolved_and_survive_subsetting() {
    let info = inspect_face(COLRV0, FaceIndex(0)).expect("inspect COLRv0 face");
    assert_eq!(info.color_glyph_formats, vec![ColorGlyphFormat::ColrV0]);

    let run = shape(COLRV0, &request("آ", TextDirection::RightToLeft, *b"Arab"))
        .expect("shape colored Arabic glyph");
    assert_eq!(run.glyphs.len(), 1);
    let base = run.glyphs[0].source_glyph;
    let colored = colr_v0_glyph(COLRV0, FaceIndex(0), &[], base, 0)
        .expect("resolve color glyph")
        .expect("COLRv0 layers");
    assert!(matches!(
        colr_v0_glyph(COLRV0, FaceIndex(0), &[], base, 1),
        Err(FontError::ColorPaletteMissing(1))
    ));
    assert_eq!(colored.layers.len(), 2);
    assert_eq!(
        colored.layers[0].brush,
        ColorGlyphBrush::Foreground { alpha: 255 }
    );
    assert_eq!(
        colored.layers[1].brush,
        ColorGlyphBrush::Solid(RgbaColor::new(204, 51, 51, 255))
    );

    let mut glyphs = run.source_glyphs();
    glyphs.extend(colored.layers.iter().map(|layer| layer.source_glyph));
    glyphs.sort_unstable();
    glyphs.dedup();
    let unicodes = run.unicodes();
    let subset = SkeraSubsetter
        .subset(COLRV0, &SubsetRequest::for_run(&run, &glyphs, &unicodes))
        .expect("subset COLRv0 face");
    assert!(subset.was_subset);
    assert!(subset.bytes.len() < COLRV0.len());
    assert_eq!(
        colr_v0_glyph(&subset.bytes, FaceIndex(0), &[], base, 0)
            .expect("resolve subset color glyph"),
        Some(colored)
    );
}

#[test]
fn devanagari_shaping_is_not_scalar_to_glyph_mapping() {
    let text = "नमस्ते भारत";
    let mut shape_request = request(text, TextDirection::LeftToRight, *b"Deva");
    shape_request.language = Some("hi");
    let run = shape(DEVANAGARI, &shape_request).expect("shape Devanagari");

    assert_ne!(run.glyphs.len(), text.chars().count());
    assert!(run.glyphs.iter().all(|glyph| !glyph.is_notdef));
    assert!(
        run.clusters
            .iter()
            .any(|cluster| cluster.text.chars().count() > 1)
    );
}

#[test]
fn cjk_thai_and_lao_shape_and_survive_subsetting() {
    for (font, text, script, language) in [
        (CJK, "日本語中文测试漢字", *b"Hani", "zh"),
        (THAI, "ภาษาไทย ยินดีต้อนรับ", *b"Thai", "th"),
        (LAO, "ສະບາຍດີ ພາສາລາວ", *b"Laoo", "lo"),
    ] {
        let mut shape_request = request(text, TextDirection::LeftToRight, script);
        shape_request.language = Some(language);
        let before = shape(font, &shape_request).expect("shape fixture");
        assert!(before.glyphs.iter().all(|glyph| !glyph.is_notdef));
        let glyphs = before.source_glyphs();
        let unicodes = before.unicodes();
        let subset = SkeraSubsetter
            .subset(font, &SubsetRequest::for_run(&before, &glyphs, &unicodes))
            .expect("subset fixture");
        let after = shape(&subset.bytes, &shape_request).expect("shape prepared subset");
        let rendered = |run: &aitex_fonts::ShapedRun| {
            run.glyphs
                .iter()
                .map(|glyph| {
                    (
                        glyph.source_glyph,
                        glyph.cluster,
                        glyph.x_advance,
                        glyph.y_advance,
                        glyph.x_offset,
                        glyph.y_offset,
                        glyph.is_notdef,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rendered(&before), rendered(&after));
        assert_eq!(before.clusters, after.clusters);
    }
}

#[test]
fn shaped_glyphs_survive_a_deterministic_skera_subset() {
    let text = "السَّلَامُ عَلَيْكُمْ";
    let mut shape_request = request(text, TextDirection::RightToLeft, *b"Arab");
    shape_request.language = Some("ar");
    let before = shape(ARABIC, &shape_request).expect("shape original");
    let glyphs = before.source_glyphs();
    let unicodes = before.unicodes();
    let subset_request = SubsetRequest::for_run(&before, &glyphs, &unicodes);
    let first = SkeraSubsetter
        .subset(ARABIC, &subset_request)
        .expect("subset font");
    let second = SkeraSubsetter
        .subset(ARABIC, &subset_request)
        .expect("repeat subset");

    assert!(first.was_subset);
    assert!(first.bytes.len() < ARABIC.len());
    assert_eq!(first, second, "subsetting must be byte-deterministic");
    assert!(
        first
            .glyph_mapping
            .iter()
            .all(|mapping| mapping.source.0 == mapping.subset.0),
        "retain-GID mode has an explicit identity map"
    );

    let after = shape(&first.bytes, &shape_request).expect("shape subset");
    assert_eq!(before.glyphs, after.glyphs);
    assert_eq!(before.clusters, after.clusters);
}

#[test]
fn missing_glyphs_are_never_silent() {
    let text = "🦀";
    let strict = request(text, TextDirection::LeftToRight, *b"Zyyy");
    assert!(shape(LATIN, &strict).is_err());

    let mut explicit = request(text, TextDirection::LeftToRight, *b"Zyyy");
    explicit.missing_glyphs = MissingGlyphPolicy::PreserveNotdef;
    let run = shape(LATIN, &explicit).expect("explicit .notdef");
    assert!(run.glyphs.iter().all(|glyph| glyph.is_notdef));
}

#[test]
fn fixture_faces_are_installable_and_content_addressed() {
    let first = inspect_face(LATIN, FaceIndex(0)).expect("inspect");
    let second = inspect_face(LATIN, FaceIndex(0)).expect("inspect again");
    assert_eq!(first.identity, second.identity);
    assert_eq!(first.identity.digest.to_hex().len(), 64);
    assert!(first.glyph_count > 1);
}

#[test]
fn malformed_font_bytes_fail_at_the_boundary() {
    for bytes in [b"".as_slice(), b"OTTO".as_slice(), &LATIN[..31]] {
        assert!(inspect_face(bytes, FaceIndex(0)).is_err());
        assert!(shape(bytes, &request("a", TextDirection::LeftToRight, *b"Latn")).is_err());
    }
}

fn with_fs_type(bytes: &[u8], fs_type: u16) -> Vec<u8> {
    use read_fonts::types::Tag;

    let font = read_fonts::FontRef::new(bytes).expect("font");
    let record = font
        .table_directory()
        .table_records()
        .iter()
        .find(|record| record.tag() == Tag::new(b"OS/2"))
        .expect("OS/2 table");
    // OS/2 fsType follows version, xAvgCharWidth, usWeightClass and
    // usWidthClass: four two-byte fields from the table start.
    let offset = record.offset() as usize + 8;
    let mut changed = bytes.to_vec();
    changed[offset..offset + 2].copy_from_slice(&fs_type.to_be_bytes());
    changed
}

fn with_empty_hvar_store(bytes: &[u8]) -> Vec<u8> {
    use read_fonts::types::Tag;

    let font = read_fonts::FontRef::new(bytes).expect("font");
    let record = font
        .table_directory()
        .table_records()
        .iter()
        .find(|record| record.tag() == Tag::new(b"HVAR"))
        .expect("HVAR table");
    let hvar_offset = record.offset() as usize;
    let store_offset = u32::from_be_bytes(
        bytes[hvar_offset + 4..hvar_offset + 8]
            .try_into()
            .expect("HVAR variation-store offset"),
    ) as usize;
    let data_count_offset = hvar_offset + store_offset + 6;
    let mut changed = bytes.to_vec();
    changed[data_count_offset..data_count_offset + 2].copy_from_slice(&0u16.to_be_bytes());
    changed
}

#[test]
fn malformed_empty_hvar_store_fails_without_panicking() {
    let malformed = with_empty_hvar_store(CFF2);
    let face = inspect_face(&malformed, FaceIndex(0)).expect("inspect malformed HVAR face");
    let glyphs = [aitex_fonts::SourceGlyphId(0)];
    let request = SubsetRequest::new(face.identity, &glyphs, &[]);

    assert!(matches!(
        SkeraSubsetter.subset(&malformed, &request),
        Err(FontError::InvalidFont(message))
            if message == "HVAR item variation store contains no variation data"
    ));
}

#[test]
fn embedding_and_no_subsetting_permissions_are_enforced() {
    let run = shape(
        LATIN,
        &request("office", TextDirection::LeftToRight, *b"Latn"),
    )
    .expect("shape");
    let glyphs = run.source_glyphs();
    let unicodes = run.unicodes();
    let subset_request = SubsetRequest::for_run(&run, &glyphs, &unicodes);

    let restricted = with_fs_type(LATIN, 0x0002);
    assert_eq!(
        inspect_face(&restricted, FaceIndex(0))
            .expect("inspect")
            .embedding,
        EmbeddingPermission::Restricted
    );
    assert_eq!(
        SkeraSubsetter.subset(&restricted, &subset_request),
        Err(FontError::EmbeddingRestricted)
    );

    let bitmap_only = with_fs_type(LATIN, 0x0200);
    assert_eq!(
        SkeraSubsetter.subset(&bitmap_only, &subset_request),
        Err(FontError::BitmapEmbeddingOnly)
    );

    let no_subsetting = with_fs_type(LATIN, 0x0100);
    let no_subset_face = inspect_face(&no_subsetting, FaceIndex(0))
        .expect("inspect no-subsetting font")
        .identity;
    let no_subset_request = SubsetRequest::new(no_subset_face, &glyphs, &unicodes);
    let prepared = SkeraSubsetter
        .subset(&no_subsetting, &no_subset_request)
        .expect("whole-font fallback");
    assert!(!prepared.was_subset);
    assert_eq!(prepared.bytes, no_subsetting);
}

#[test]
fn subset_requests_are_bound_to_exact_font_bytes() {
    let run = shape(
        LATIN,
        &request("office", TextDirection::LeftToRight, *b"Latn"),
    )
    .expect("shape");
    let glyphs = run.source_glyphs();
    let unicodes = run.unicodes();
    let subset_request = SubsetRequest::for_run(&run, &glyphs, &unicodes);
    let changed = with_fs_type(LATIN, 0x0100);

    assert!(matches!(
        SkeraSubsetter.subset(&changed, &subset_request),
        Err(FontError::FaceMismatch { .. })
    ));
}

#[test]
fn variable_coordinates_are_normalized_into_instance_and_subset_identity() {
    let face = inspect_face(VARIABLE, FaceIndex(0)).expect("inspect variable font");
    assert!(
        face.axes
            .iter()
            .any(|axis| axis.tag == FontTag::new(*b"wdth"))
    );

    let text = "variable width";
    let ordinary = shape(
        VARIABLE,
        &request(text, TextDirection::LeftToRight, *b"Latn"),
    )
    .expect("shape default instance");
    let variations = [VariationSetting {
        tag: FontTag::new(*b"wdth"),
        value: 120.0,
    }];
    let mut wider_request = request(text, TextDirection::LeftToRight, *b"Latn");
    wider_request.variations = &variations;
    let wider = shape(VARIABLE, &wider_request).expect("shape wide instance");

    assert!(ordinary.instance.variations.is_empty());
    assert!(!wider.instance.variations.is_empty());
    assert_ne!(ordinary.instance, wider.instance);
    assert_ne!(
        ordinary
            .glyphs
            .iter()
            .map(|glyph| glyph.x_advance)
            .sum::<i32>(),
        wider
            .glyphs
            .iter()
            .map(|glyph| glyph.x_advance)
            .sum::<i32>()
    );

    let glyphs = wider.source_glyphs();
    let unicodes = wider.unicodes();
    let mut subset_request = SubsetRequest::for_run(&wider, &glyphs, &unicodes);
    subset_request.variations = &variations;
    let subset = SkeraSubsetter
        .subset(VARIABLE, &subset_request)
        .expect("subset variable font");
    assert_eq!(subset.identity.variations, wider.instance.variations);
    let static_info = inspect_face(&subset.bytes, FaceIndex(0)).expect("inspect static instance");
    assert_eq!(static_info.format, FontFormat::TrueType);
    assert!(static_info.axes.is_empty());
    let source_metrics =
        aitex_fonts::pdf_face_metrics(VARIABLE, FaceIndex(0), &variations, &glyphs)
            .expect("variable metrics");
    let static_metrics = aitex_fonts::pdf_face_metrics(&subset.bytes, FaceIndex(0), &[], &glyphs)
        .expect("static metrics");
    assert_eq!(source_metrics.glyph_widths, static_metrics.glyph_widths);
}

#[test]
fn cff2_instance_uses_the_same_static_subset_contract() {
    let face = inspect_face(CFF2, FaceIndex(0)).expect("inspect CFF2 variable font");
    assert_eq!(face.format, FontFormat::Cff2);
    assert!(!face.axes.is_empty());
    let variations = [
        VariationSetting {
            tag: FontTag::new(*b"wght"),
            value: 700.0,
        },
        VariationSetting {
            tag: FontTag::new(*b"opsz"),
            value: 24.0,
        },
    ];
    let mut shape_request = request("variable office", TextDirection::LeftToRight, *b"Latn");
    shape_request.variations = &variations;
    let run = shape(CFF2, &shape_request).expect("shape CFF2 instance");
    let glyphs = run.source_glyphs();
    let unicodes = run.unicodes();
    let mut subset_request = SubsetRequest::for_run(&run, &glyphs, &unicodes);
    subset_request.variations = &variations;
    let subset = SkeraSubsetter
        .subset(CFF2, &subset_request)
        .expect("instantiate and subset CFF2");
    let output = inspect_face(&subset.bytes, FaceIndex(0)).expect("inspect static subset");
    assert_eq!(output.format, FontFormat::TrueType);
    assert!(output.axes.is_empty());
    let source_metrics = aitex_fonts::pdf_face_metrics(CFF2, FaceIndex(0), &variations, &glyphs)
        .expect("CFF2 metrics");
    let static_metrics = aitex_fonts::pdf_face_metrics(&subset.bytes, FaceIndex(0), &[], &glyphs)
        .expect("static metrics");
    assert_eq!(source_metrics.glyph_widths, static_metrics.glyph_widths);
}
