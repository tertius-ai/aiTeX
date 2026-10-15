use std::sync::Arc;

use aitex_driver::pdf::{
    OpenTypeFontSource, PdfOptions, PositionedPdfSemantics, write_hybrid_with_assets_options,
    write_positioned, write_positioned_with_semantics,
};
use aitex_fonts::{
    FaceIndex, FontTag, ShapeRequest, TextDirection, VariationSetting, inspect_face,
    positioned_run, shape,
};
use aitex_page::{Document, FontInstanceId, Item, Page, Sp};

const LATIN: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/NotoSans-Latin-Test.ttf");
const ARABIC: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/Amiri-Arabic-Test.ttf");
const DEVANAGARI: &[u8] =
    include_bytes!("../../aitex-fonts/tests/fonts/NotoSansDevanagari-Test.ttf");
const HEBREW: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/NotoSansHebrew-Test.ttf");
const CFF: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/SourceSerif4-CFF-Test.otf");
const VARIABLE: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/Junicode-Variable-Test.ttf");
const THAI: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/NotoSansThai-Test.ttf");
const LAO: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/NotoSansLao-Test.ttf");
const CJK: &[u8] = include_bytes!("../../aitex-fonts/tests/fonts/NotoSansCJKsc-Test.otf");
const CFF2: &[u8] =
    include_bytes!("../../aitex-fonts/tests/fonts/SourceSerif4-CFF2-Variable-Test.otf");
const COLRV0: &[u8] =
    include_bytes!("../../aitex-fonts/tests/fonts/AmiriQuranColored-COLRv0-Test.ttf");

fn sp(points: f64) -> Sp {
    Sp((points * 65_536.0).round() as i64)
}

fn utf16be_hex(text: &str) -> String {
    let mut hex = String::from("FEFF");
    for unit in text.encode_utf16() {
        hex.push_str(&format!("{unit:04X}"));
    }
    hex
}

fn shaped(
    bytes: &[u8],
    text: &str,
    direction: TextDirection,
    script: [u8; 4],
    language: Option<&str>,
    font: u32,
    y: f64,
) -> aitex_page::GlyphRun {
    let mut request = ShapeRequest::new(text, FaceIndex(0), direction, FontTag::new(script));
    request.language = language;
    let run = shape(bytes, &request).expect("shape fixture");
    positioned_run(&run, FontInstanceId(font), sp(22.0), sp(20.0), sp(y))
}

fn shaped_variable(
    bytes: &[u8],
    text: &str,
    variations: &[VariationSetting],
    font: u32,
    y: f64,
) -> aitex_page::GlyphRun {
    let mut request = ShapeRequest::new(
        text,
        FaceIndex(0),
        TextDirection::LeftToRight,
        FontTag::new(*b"Latn"),
    );
    request.language = Some("en");
    request.variations = variations;
    let run = shape(bytes, &request).expect("shape variable fixture");
    positioned_run(&run, FontInstanceId(font), sp(22.0), sp(20.0), sp(y))
}

fn fixture_document() -> Document {
    Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![
                Item::GlyphRun(shaped(
                    LATIN,
                    "office affine",
                    TextDirection::LeftToRight,
                    *b"Latn",
                    Some("en"),
                    0,
                    30.0,
                )),
                Item::GlyphRun(shaped(
                    ARABIC,
                    "السَّلَامُ عَلَيْكُمْ",
                    TextDirection::RightToLeft,
                    *b"Arab",
                    Some("ar"),
                    1,
                    80.0,
                )),
                Item::GlyphRun(shaped(
                    DEVANAGARI,
                    "नमस्ते भारत",
                    TextDirection::LeftToRight,
                    *b"Deva",
                    Some("hi"),
                    2,
                    130.0,
                )),
                Item::GlyphRun(shaped(
                    CFF,
                    "office affine",
                    TextDirection::LeftToRight,
                    *b"Latn",
                    Some("en"),
                    3,
                    180.0,
                )),
                Item::GlyphRun(shaped(
                    HEBREW,
                    "שָׁלוֹם עוֹלָם",
                    TextDirection::RightToLeft,
                    *b"Hebr",
                    Some("he"),
                    4,
                    230.0,
                )),
                Item::GlyphRun(shaped(
                    CJK,
                    "日本語中文测试漢字",
                    TextDirection::LeftToRight,
                    *b"Hani",
                    Some("zh"),
                    5,
                    280.0,
                )),
                Item::GlyphRun(shaped(
                    THAI,
                    "ภาษาไทย ยินดีต้อนรับ",
                    TextDirection::LeftToRight,
                    *b"Thai",
                    Some("th"),
                    6,
                    330.0,
                )),
                Item::GlyphRun(shaped(
                    LAO,
                    "ສະບາຍດີ ພາສາລາວ",
                    TextDirection::LeftToRight,
                    *b"Laoo",
                    Some("lo"),
                    7,
                    380.0,
                )),
                Item::GlyphRun(shaped_variable(
                    VARIABLE,
                    "variable width",
                    &[VariationSetting {
                        tag: FontTag::new(*b"wdth"),
                        value: 120.0,
                    }],
                    8,
                    430.0,
                )),
                Item::GlyphRun(shaped_variable(
                    CFF2,
                    "variable office",
                    &[
                        VariationSetting {
                            tag: FontTag::new(*b"wght"),
                            value: 700.0,
                        },
                        VariationSetting {
                            tag: FontTag::new(*b"opsz"),
                            value: 24.0,
                        },
                    ],
                    9,
                    480.0,
                )),
                Item::Rule {
                    x: sp(20.0),
                    baseline_y: sp(510.0),
                    width: sp(140.0),
                    height: sp(0.75),
                },
            ],
        }],
    }
}

fn colrv0_document() -> Document {
    let mut alef = shaped(
        COLRV0,
        "آ",
        TextDirection::RightToLeft,
        *b"Arab",
        Some("ar"),
        10,
        120.0,
    );
    alef.origin_x = sp(100.0);
    alef.size = sp(120.0);
    let mut zero = shaped(
        COLRV0,
        "٠",
        TextDirection::RightToLeft,
        *b"Arab",
        Some("ar"),
        10,
        280.0,
    );
    zero.origin_x = sp(100.0);
    zero.size = sp(120.0);
    Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(alef), Item::GlyphRun(zero)],
        }],
    }
}

fn source(font: FontInstanceId) -> Option<OpenTypeFontSource> {
    let bytes: Arc<[u8]> = match font.0 {
        0 => Arc::from(LATIN),
        1 => Arc::from(ARABIC),
        2 => Arc::from(DEVANAGARI),
        3 => Arc::from(CFF),
        4 => Arc::from(HEBREW),
        5 => Arc::from(CJK),
        6 => Arc::from(THAI),
        7 => Arc::from(LAO),
        8 => Arc::from(VARIABLE),
        9 => Arc::from(CFF2),
        10 => Arc::from(COLRV0),
        _ => return None,
    };
    let source = OpenTypeFontSource::new(bytes, 0);
    Some(match font.0 {
        8 => source.with_variations(vec![VariationSetting {
            tag: FontTag::new(*b"wdth"),
            value: 120.0,
        }]),
        9 => source.with_variations(vec![
            VariationSetting {
                tag: FontTag::new(*b"wght"),
                value: 700.0,
            },
            VariationSetting {
                tag: FontTag::new(*b"opsz"),
                value: 24.0,
            },
        ]),
        _ => source,
    })
}

#[test]
fn native_pdf_embeds_deterministic_type0_fonts() {
    let document = fixture_document();
    let first = write_positioned(&document, PdfOptions::uncompressed(), source).expect("PDF");
    let second = write_positioned(&document, PdfOptions::uncompressed(), source).expect("PDF");

    assert_eq!(first, second);
    assert!(first.starts_with(b"%PDF-1.7"));
    let text = String::from_utf8_lossy(&first);
    assert_eq!(text.matches("/Subtype /Type0").count(), 10);
    assert_eq!(text.matches("/Subtype /CIDFontType2").count(), 8);
    assert_eq!(text.matches("/Subtype /CIDFontType0").count(), 2);
    assert_eq!(text.matches("/Subtype /OpenType").count(), 2);
    assert_eq!(text.matches("/Encoding /Identity-H").count(), 10);
    let to_unicode_fonts = text.matches("/ToUnicode").count();
    assert_eq!(
        to_unicode_fonts, 10,
        "every used font retains a glyph-level extraction fallback alongside ActualText"
    );
    assert!(text.contains("/ActualText <FEFF"));
    assert!(
        text.contains(&utf16be_hex("السَّلَامُ عَلَيْكُمْ")),
        "RTL ActualText stays in logical source order"
    );
    assert!(
        text.contains(&utf16be_hex("नमस्ते भारत")),
        "many-to-many Indic clusters keep logical text"
    );
    assert!(
        text.contains(&utf16be_hex("שָׁלוֹם עוֹלָם")),
        "RTL Hebrew ActualText stays in logical source order"
    );
    assert!(text.contains(&utf16be_hex("ภาษาไทย")));
    assert!(text.contains(&utf16be_hex("ສະບາຍດີ")));
    assert!(text.contains("] TJ"), "same-baseline runs use compact TJ");
    assert!(
        text.matches(" Tm <").count() > 1,
        "mark offsets use exact per-glyph matrices"
    );
    assert!(
        text.contains(" re f"),
        "positioned rules remain vector rules"
    );

    let compact = write_positioned(&document, PdfOptions::default(), source).expect("PDF");
    let source_bytes = LATIN.len()
        + ARABIC.len()
        + DEVANAGARI.len()
        + CFF.len()
        + HEBREW.len()
        + CJK.len()
        + THAI.len()
        + LAO.len()
        + VARIABLE.len()
        + CFF2.len();
    assert!(
        compact.len() < source_bytes,
        "embedded fonts should be subset"
    );
}

#[test]
fn colrv0_pdf_paints_ordered_solid_layers_and_keeps_logical_text() {
    let first = write_positioned(&colrv0_document(), PdfOptions::uncompressed(), source)
        .expect("COLRv0 PDF");
    let second = write_positioned(&colrv0_document(), PdfOptions::uncompressed(), source)
        .expect("repeat COLRv0 PDF");
    assert_eq!(first, second);

    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("0.800000 0.200000 0.200000 rg"));
    assert!(text.contains("0.000000 0.647059 0.313725 rg"));
    assert!(text.contains(&utf16be_hex("آ")));
    assert!(text.contains(&utf16be_hex("٠")));
    assert_eq!(text.matches(" Tj").count(), 3);
    assert_eq!(text.matches("\nq\n").count(), 3);
    assert_eq!(text.matches("\nQ\n").count(), 3);
}

#[test]
fn identical_page_resources_are_one_shared_indirect_object() {
    let page = Page {
        width_millibp: 612_000,
        height_millibp: 792_000,
        counts: [0; 10],
        items: Vec::new(),
    };
    let document = Document {
        pages: vec![page.clone(), page],
    };
    let pdf = write_positioned(&document, PdfOptions::uncompressed(), |_| None).expect("PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert_eq!(text.matches("<< /Font <<  >> >>").count(), 1);
    assert_eq!(text.matches("/Resources ").count(), 2);
}

#[test]
fn ligature_tounicode_keeps_the_complete_cluster() {
    let document = Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(shaped(
                LATIN,
                "office",
                TextDirection::LeftToRight,
                *b"Latn",
                Some("en"),
                0,
                30.0,
            ))],
        }],
    };
    let pdf = write_positioned(&document, PdfOptions::uncompressed(), source).expect("PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("006600660069"), "ffi cluster is UTF-16BE");
    assert!(
        !text.contains("/ActualText"),
        "one-glyph ligatures fit ToUnicode"
    );
}

#[test]
fn missing_font_fails_explicitly() {
    let document = fixture_document();
    assert!(matches!(
        write_positioned(&document, PdfOptions::default(), |_| None),
        Err(aitex_driver::pdf::PositionedPdfError::MissingFont(_))
    ));
}

#[test]
fn vertical_text_uses_identity_v_and_vertical_metrics() {
    let mut run = shaped(
        CJK,
        "日本語中文",
        TextDirection::TopToBottom,
        *b"Hani",
        Some("ja"),
        5,
        30.0,
    );
    run.origin_x = sp(180.0);
    let document = Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(run)],
        }],
    };
    let pdf =
        write_positioned(&document, PdfOptions::uncompressed(), source).expect("vertical PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/Encoding /Identity-V"));
    assert!(text.contains("/DW2 ["));
    assert!(text.contains("/UF5V"));
    assert_eq!(text.matches("/FontFile3").count(), 1);
}

#[test]
fn positioned_semantics_emit_a_real_structure_and_parent_tree() {
    let mut run = shaped(
        LATIN,
        "office affine",
        TextDirection::LeftToRight,
        *b"Latn",
        Some("en"),
        0,
        30.0,
    );
    run.role = aitex_page::TextRole::Heading(2);
    let document = Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(run)],
        }],
    };
    let pdf = write_positioned_with_semantics(
        &document,
        PdfOptions::uncompressed(),
        PositionedPdfSemantics::tagged("en-US"),
        source,
    )
    .expect("tagged PDF foundation");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/Type /StructTreeRoot"));
    assert!(text.contains("/ParentTree"));
    assert!(text.contains("/MarkInfo << /Marked true >>"));
    assert!(text.contains("/Lang (en-US)"));
    assert!(text.contains("/S /H2"));
    assert!(text.contains("/H2 << /MCID 0 /Lang (en) >> BDC"));
}

#[test]
fn positioned_semantics_nest_contextual_actual_text_inside_the_mcid() {
    let logical = "السَّلَامُ عَلَيْكُمْ";
    let mut run = shaped(
        ARABIC,
        logical,
        TextDirection::RightToLeft,
        *b"Arab",
        Some("ar"),
        1,
        30.0,
    );
    run.role = aitex_page::TextRole::Paragraph;
    let document = Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(run)],
        }],
    };
    let pdf = write_positioned_with_semantics(
        &document,
        PdfOptions::uncompressed(),
        PositionedPdfSemantics::tagged("ar"),
        source,
    )
    .expect("tagged contextual PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/P << /MCID 0 /Lang (ar) >> BDC\n/Span << /ActualText <"));
    assert!(text.contains(&utf16be_hex(logical)));
    assert_eq!(text.matches("EMC\n").count(), 2);
}

#[test]
fn positioned_semantics_number_nonempty_runs_per_page() {
    let mut heading = shaped(
        LATIN,
        "office",
        TextDirection::LeftToRight,
        *b"Latn",
        Some("en"),
        0,
        30.0,
    );
    heading.role = aitex_page::TextRole::Heading(1);
    let mut paragraph = shaped(
        LATIN,
        "affine",
        TextDirection::LeftToRight,
        *b"Latn",
        Some("en"),
        0,
        70.0,
    );
    paragraph.role = aitex_page::TextRole::Paragraph;
    let mut empty = paragraph.clone();
    empty.glyphs.clear();
    let document = Document {
        pages: vec![
            Page {
                width_millibp: 612_000,
                height_millibp: 792_000,
                counts: [0; 10],
                items: vec![
                    Item::GlyphRun(heading),
                    Item::GlyphRun(empty.clone()),
                    Item::GlyphRun(paragraph.clone()),
                ],
            },
            Page {
                width_millibp: 612_000,
                height_millibp: 792_000,
                counts: [0; 10],
                items: vec![Item::GlyphRun(empty), Item::GlyphRun(paragraph)],
            },
        ],
    };
    let pdf = write_positioned_with_semantics(
        &document,
        PdfOptions::uncompressed(),
        PositionedPdfSemantics::tagged("en-US"),
        source,
    )
    .expect("multi-page tagged PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert_eq!(text.matches("/Type /StructElem").count(), 3);
    assert_eq!(text.matches(" BDC\n").count(), 3);
    assert_eq!(text.matches("/MCID 0").count(), 2);
    assert_eq!(text.matches("/MCID 1").count(), 1);
    assert!(text.contains("/StructParents 0"));
    assert!(text.contains("/StructParents 1"));
    assert!(text.contains("/ParentTreeNextKey 2"));
}

#[test]
fn positioned_semantics_reject_malformed_language_tags() {
    let document = fixture_document();
    assert!(matches!(
        write_positioned_with_semantics(
            &document,
            PdfOptions::uncompressed(),
            PositionedPdfSemantics::tagged("en--US"),
            source,
        ),
        Err(aitex_driver::pdf::PositionedPdfError::InvalidLanguage(language))
            if language == "en--US"
    ));
}

#[test]
fn classic_glyphs_and_specials_are_never_silently_dropped() {
    let page = |item| Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![item],
        }],
    };
    let classic = page(Item::ClassicGlyph {
        font: FontInstanceId(0),
        code: 65,
        origin_x: Sp(0),
        origin_y: Sp(0),
        advance: Sp(0),
    });
    assert!(matches!(
        write_positioned(&classic, PdfOptions::default(), source),
        Err(aitex_driver::pdf::PositionedPdfError::UnsupportedClassicGlyph)
    ));

    let special = page(Item::Special {
        x: Sp(0),
        y: Sp(0),
        bytes: b"unsupported".to_vec(),
    });
    assert!(matches!(
        write_positioned(&special, PdfOptions::default(), source),
        Err(aitex_driver::pdf::PositionedPdfError::UnsupportedSpecial)
    ));
}

#[test]
fn hybrid_pdf_keeps_classic_dvi_and_native_text_on_one_page() {
    let classic = aitex_driver::dvi::Document {
        pages: vec![aitex_driver::dvi::Page {
            items: vec![aitex_driver::dvi::Item::Rule {
                x: sp(20.0),
                baseline_y: sp(55.0),
                width: sp(100.0),
                height: sp(1.0),
            }],
            ..Default::default()
        }],
        num: 25_400_000,
        den: 473_628_672,
        mag: 1000,
        ..Default::default()
    };
    let native = Document {
        pages: vec![Page {
            width_millibp: 0,
            height_millibp: 0,
            counts: [0; 10],
            items: vec![Item::GlyphRun(shaped(
                LATIN,
                "office",
                TextDirection::LeftToRight,
                *b"Latn",
                Some("en"),
                0,
                30.0,
            ))],
        }],
    };
    let mut positioned_source = source;
    let pdf = write_hybrid_with_assets_options(
        &classic,
        &native,
        612.0,
        792.0,
        PdfOptions::uncompressed(),
        |_, _| None,
        &mut positioned_source,
        |_, _| None,
    )
    .expect("hybrid PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/Subtype /Type0"));
    assert!(
        text.contains("/UF0"),
        "native resources have a distinct namespace"
    );
    assert!(
        text.contains(" re f"),
        "the classic DVI rule remains present"
    );
}

#[test]
fn hybrid_pdf_rejects_page_count_mismatch() {
    let classic = aitex_driver::dvi::Document {
        pages: vec![Default::default()],
        ..Default::default()
    };
    let mut positioned_source = source;
    assert!(matches!(
        write_hybrid_with_assets_options(
            &classic,
            &Document::default(),
            612.0,
            792.0,
            PdfOptions::default(),
            |_, _| None,
            &mut positioned_source,
            |_, _| None,
        ),
        Err(aitex_driver::pdf::PositionedPdfError::HybridPageCount { .. })
    ));
}

#[test]
fn non_default_variable_instance_is_materialized_as_static_truetype() {
    let variations = [VariationSetting {
        tag: FontTag::new(*b"wdth"),
        value: 120.0,
    }];
    let mut request = ShapeRequest::new(
        "variable width",
        FaceIndex(0),
        TextDirection::LeftToRight,
        FontTag::new(*b"Latn"),
    );
    request.variations = &variations;
    let shaped = shape(VARIABLE, &request).expect("shape variable instance");
    let run = positioned_run(&shaped, FontInstanceId(9), sp(18.0), sp(20.0), sp(30.0));
    let document = Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(run)],
        }],
    };
    let render = || {
        write_positioned(&document, PdfOptions::uncompressed(), |font| {
            (font == FontInstanceId(9)).then(|| {
                OpenTypeFontSource::new(Arc::<[u8]>::from(VARIABLE), 0)
                    .with_variations(variations.to_vec())
            })
        })
    };
    let first = render().expect("variable PDF");
    let second = render().expect("deterministic variable PDF");
    assert_eq!(first, second);
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("/Subtype /CIDFontType2"));
    assert!(text.contains("/FontFile2"));
    assert!(!text.contains("/Subtype /OpenType"));
}

#[test]
fn font_metrics_and_page_model_keep_source_identity_distinct() {
    let info = inspect_face(LATIN, FaceIndex(0)).expect("inspect");
    let document = fixture_document();
    let Item::GlyphRun(run) = &document.pages[0].items[0] else {
        unreachable!()
    };
    assert!(
        run.glyphs
            .iter()
            .all(|glyph| glyph.source_gid.0 < info.glyph_count)
    );
}

#[test]
#[ignore = "writes the visual-QA artifact requested by the PDF workflow"]
fn write_visual_qa_fixture() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/pdfs");
    std::fs::create_dir_all(&directory).expect("mkdir");

    let compact =
        write_positioned(&fixture_document(), PdfOptions::default(), source).expect("compact PDF");
    std::fs::write(directory.join("batch4-positioned.pdf"), compact).expect("write compact PDF");

    let classic_options = PdfOptions::default().with_compact_objects(false);
    let classic =
        write_positioned(&fixture_document(), classic_options, source).expect("classic-xref PDF");
    std::fs::write(directory.join("batch4-positioned-classic.pdf"), classic)
        .expect("write classic-xref PDF");

    let mut vertical = shaped(
        CJK,
        "日本語中文测试",
        TextDirection::TopToBottom,
        *b"Hani",
        Some("ja"),
        5,
        30.0,
    );
    vertical.origin_x = sp(180.0);
    vertical.role = aitex_page::TextRole::Paragraph;
    let vertical_document = Document {
        pages: vec![Page {
            width_millibp: 612_000,
            height_millibp: 792_000,
            counts: [0; 10],
            items: vec![Item::GlyphRun(vertical)],
        }],
    };
    let tagged = write_positioned_with_semantics(
        &vertical_document,
        PdfOptions::default(),
        PositionedPdfSemantics::tagged("ja"),
        source,
    )
    .expect("tagged vertical PDF");
    std::fs::write(directory.join("batch5-vertical-tagged.pdf"), tagged)
        .expect("write tagged vertical PDF");
}

#[test]
#[ignore = "writes the visual-QA artifact requested by the PDF workflow"]
fn write_colrv0_visual_qa_fixture() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/pdfs");
    std::fs::create_dir_all(&directory).expect("mkdir");
    let colored =
        write_positioned(&colrv0_document(), PdfOptions::default(), source).expect("COLRv0 PDF");
    std::fs::write(directory.join("colrv0-solid-layers.pdf"), colored).expect("write COLRv0 PDF");
}
