//! Produce the original native-font PDF used by the varied qualification suite.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use aitex_driver::pdf::{
    OpenTypeFontSource, PdfOptions, PositionedPdfSemantics, write_positioned_with_semantics,
};
use aitex_fonts::{
    FaceIndex, FontTag, ShapeRequest, TextDirection, VariationSetting, positioned_run, shape,
};
use aitex_page::{Document, FontInstanceId, Item, Page, Sp, TextRole};

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

#[allow(clippy::too_many_arguments)]
fn shaped(
    bytes: &[u8],
    text: &str,
    direction: TextDirection,
    script: [u8; 4],
    language: &'static str,
    font: u32,
    x: f64,
    y: f64,
    role: TextRole,
) -> aitex_page::GlyphRun {
    let mut request = ShapeRequest::new(text, FaceIndex(0), direction, FontTag::new(script));
    request.language = Some(language);
    let run = shape(bytes, &request).expect("shape qualification fixture");
    let mut positioned = positioned_run(&run, FontInstanceId(font), sp(22.0), sp(x), sp(y));
    positioned.role = role;
    positioned
}

fn variable(
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
    let run = shape(bytes, &request).expect("shape variable qualification fixture");
    let mut positioned = positioned_run(&run, FontInstanceId(font), sp(22.0), sp(20.0), sp(y));
    positioned.role = TextRole::Paragraph;
    positioned
}

fn document() -> Document {
    let width_millibp = 612_000;
    let height_millibp = 792_000;
    let horizontal = vec![
        shaped(
            LATIN,
            "office affine",
            TextDirection::LeftToRight,
            *b"Latn",
            "en",
            0,
            20.0,
            40.0,
            TextRole::Heading(1),
        ),
        shaped(
            ARABIC,
            "السَّلَامُ عَلَيْكُمْ",
            TextDirection::RightToLeft,
            *b"Arab",
            "ar",
            1,
            20.0,
            95.0,
            TextRole::Paragraph,
        ),
        shaped(
            DEVANAGARI,
            "नमस्ते भारत",
            TextDirection::LeftToRight,
            *b"Deva",
            "hi",
            2,
            20.0,
            150.0,
            TextRole::Paragraph,
        ),
        shaped(
            CFF,
            "office affine",
            TextDirection::LeftToRight,
            *b"Latn",
            "en",
            3,
            20.0,
            205.0,
            TextRole::Paragraph,
        ),
        shaped(
            HEBREW,
            "שָׁלוֹם עוֹלָם",
            TextDirection::RightToLeft,
            *b"Hebr",
            "he",
            4,
            20.0,
            260.0,
            TextRole::Paragraph,
        ),
        shaped(
            CJK,
            "日本語中文测试漢字",
            TextDirection::LeftToRight,
            *b"Hani",
            "zh",
            5,
            20.0,
            315.0,
            TextRole::Paragraph,
        ),
        shaped(
            THAI,
            "ภาษาไทย ยินดีต้อนรับ",
            TextDirection::LeftToRight,
            *b"Thai",
            "th",
            6,
            20.0,
            370.0,
            TextRole::Paragraph,
        ),
        shaped(
            LAO,
            "ສະບາຍດີ ພາສາລາວ",
            TextDirection::LeftToRight,
            *b"Laoo",
            "lo",
            7,
            20.0,
            425.0,
            TextRole::Paragraph,
        ),
        variable(
            VARIABLE,
            "variable width",
            &[VariationSetting {
                tag: FontTag::new(*b"wdth"),
                value: 120.0,
            }],
            8,
            480.0,
        ),
        variable(
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
            535.0,
        ),
    ]
    .into_iter()
    .map(Item::GlyphRun)
    .chain(std::iter::once(Item::Rule {
        x: sp(20.0),
        baseline_y: sp(575.0),
        width: sp(180.0),
        height: sp(0.75),
    }))
    .collect();

    let vertical = shaped(
        CJK,
        "日本語中文测试",
        TextDirection::TopToBottom,
        *b"Hani",
        "ja",
        5,
        220.0,
        40.0,
        TextRole::Paragraph,
    );

    let mut colored_alef = shaped(
        COLRV0,
        "آ",
        TextDirection::RightToLeft,
        *b"Arab",
        "ar",
        10,
        110.0,
        160.0,
        TextRole::Heading(2),
    );
    colored_alef.size = sp(120.0);
    let mut colored_zero = shaped(
        COLRV0,
        "٠",
        TextDirection::RightToLeft,
        *b"Arab",
        "ar",
        10,
        110.0,
        330.0,
        TextRole::Paragraph,
    );
    colored_zero.size = sp(120.0);

    Document {
        pages: vec![
            Page {
                width_millibp,
                height_millibp,
                counts: [1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                items: horizontal,
            },
            Page {
                width_millibp,
                height_millibp,
                counts: [2, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                items: vec![Item::GlyphRun(vertical)],
            },
            Page {
                width_millibp,
                height_millibp,
                counts: [3, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                items: vec![Item::GlyphRun(colored_alef), Item::GlyphRun(colored_zero)],
            },
        ],
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

fn main() {
    let mut output = None;
    let mut classic_xref = false;
    for argument in std::env::args().skip(1) {
        if argument == "--classic-xref" {
            classic_xref = true;
        } else if output.replace(PathBuf::from(argument)).is_some() {
            panic!("usage: pdf_qualification OUTPUT [--classic-xref]");
        }
    }
    let output = output.expect("usage: pdf_qualification OUTPUT [--classic-xref]");
    let options = if classic_xref {
        PdfOptions::default().with_compact_objects(false)
    } else {
        PdfOptions::default()
    };
    let document = document();
    let started = Instant::now();
    let pdf = write_positioned_with_semantics(
        &document,
        options,
        PositionedPdfSemantics::tagged("und"),
        source,
    )
    .expect("write positioned qualification PDF");
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    std::fs::write(&output, &pdf).expect("write qualification output");
    println!(
        "AITEX_POSITIONED_METRICS {{\"pdf_total_ms\":{elapsed_ms:.3},\"pdf_bytes\":{}}}",
        pdf.len()
    );
}
