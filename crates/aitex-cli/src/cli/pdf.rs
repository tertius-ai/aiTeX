/// Semantic names for PK math alphabets whose encoding is the ASCII
/// letter/digit positions but which do not ship a dvips `.enc` file. The
/// outlines remain the real bitmap glyphs; this table is only for ToUnicode.
fn ascii_math_encoding() -> Vec<String> {
    let mut encoding = vec![".notdef".to_string(); 256];
    encoding[32] = "space".to_string();
    encoding[40] = "parenleft".to_string();
    encoding[41] = "parenright".to_string();
    for (offset, name) in [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ]
    .into_iter()
    .enumerate()
    {
        encoding[48 + offset] = name.to_string();
    }
    for byte in b'A'..=b'Z' {
        encoding[byte as usize] = char::from(byte).to_string();
    }
    for byte in b'a'..=b'z' {
        encoding[byte as usize] = char::from(byte).to_string();
    }
    encoding
}

/// Recover the semantic encoding of an uncommon-size LH bitmap font.
///
/// TeX Live's CM-Super maps cover the usual design sizes with Type 1 fonts,
/// but NFSS can ask Metafont for another size (for example `lati1728`).  The
/// resulting PK name retains the LH encoding prefix: `la`, `lb`, and `lc` are
/// T2A, T2B, and T2C respectively.  Match only the documented LH family/style
/// stems so an unrelated font such as `lasy10` is not mistaken for Cyrillic.
fn lh_bitmap_encoding_file(name: &str) -> Option<&'static str> {
    let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() == name.len() {
        return None;
    }
    let (prefix, suffix) = stem.split_at_checked(2)?;
    const LH_STEMS: &[&str] = &[
        "bbx", "bi", "bl", "bm", "bmo", "bmr", "bso", "bsr", "btl", "bto", "bx", "cc",
        "ci", "dh", "fb", "ff", "fi", "fs", "it", "oc", "occ", "orm", "osl", "oti", "rb",
        "rm", "sc", "si", "sl", "so", "ss", "ssdc", "st", "sx", "tc", "ti", "tt", "ui",
        "vi", "vt", "xc",
    ];
    if !LH_STEMS.contains(&suffix) {
        return None;
    }
    match prefix {
        "la" => Some("cm-super-t2a.enc"),
        "lb" => Some("cm-super-t2b.enc"),
        "lc" => Some("cm-super-t2c.enc"),
        _ => None,
    }
}

/// Resolve Adobe-style Unicode glyph names in addition to the explicit
/// `\pdfglyphtounicode` table loaded by LaTeX. Consolidated dvips encodings use
/// names such as `uni230A.alt1` and `u1D400`; pdfTeX interprets those names even
/// when `glyphtounicode.tex` has no redundant entry for each variant.
fn unicode_for_glyph(
    tex_font: &str,
    glyph: &str,
    explicit: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    if let Some(value) = explicit.get(&format!("tfm:{tex_font}/{glyph}")) {
        return Some(value.clone());
    }
    if let Some(value) = explicit.get(glyph) {
        return Some(value.clone());
    }
    // bbding's Type 1 conversion names glyphs only `enc-N`; the package's
    // public symbol table supplies the semantics for its most common status
    // marks. These five slots are the Unicode Dingbats check/X sequence.
    if tex_font == "bbding10" {
        let value = match glyph {
            "enc-33" => "2713", // Checkmark
            "enc-34" => "2714", // CheckmarkBold
            "enc-35" => "2715", // XSolid
            "enc-36" => "2716", // XSolidBold
            "enc-37" => "2717", // XSolidBrush
            _ => return aitex_driver::fontmap::unicode_from_glyph_name(glyph),
        };
        return Some(value.to_string());
    }
    aitex_driver::fontmap::unicode_from_glyph_name(glyph)
}

/// Preserve literal printable bytes in generated EC/TS1 typewriter bitmaps.
///
/// These PK fonts are used by `listings` precisely when a source byte must
/// remain literal. Their Adobe glyph names describe the visual slot (for
/// example TS1 `minus` at byte `=`), whereas text extraction must recover the
/// source byte. Outline fonts and non-typewriter bitmaps retain the ordinary
/// glyph-semantic mapping above.
fn unicode_for_bitmap_slot(
    tex_font: &str,
    physical_name: &str,
    code: usize,
    glyph: &str,
    explicit: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    if (physical_name.starts_with("ectt") || physical_name.starts_with("tctt"))
        && (32..=126).contains(&code)
    {
        return Some(format!("{code:04X}"));
    }
    unicode_for_glyph(tex_font, glyph, explicit)
}

const HOT_PDF_FILE_BYTES: usize = 32 * 1024 * 1024;
const HOT_PDF_ASSET_BYTES: usize = 32 * 1024 * 1024;
const HOT_PDF_ASSET_ENTRIES: usize = 256;
const HOT_EPS_PDF_BYTES: usize = 32 * 1024 * 1024;
const HOT_EPS_PDF_ENTRIES: usize = 256;
const HOT_DVI_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
const HOT_TYPE1_PROGRAM_BYTES: usize = 32 * 1024 * 1024;
const HOT_PDF_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

struct HotFileAlias {
    path: String,
    signature: FileSignature,
    digest: String,
}

struct HotFileObject {
    digest: String,
    bytes: std::sync::Arc<[u8]>,
}

#[derive(Default)]
struct HotFileCache {
    aliases: std::collections::VecDeque<HotFileAlias>,
    objects: std::collections::VecDeque<HotFileObject>,
    bytes: usize,
}

impl HotFileCache {
    fn read(&mut self, path: &str) -> Option<(String, std::sync::Arc<[u8]>)> {
        let signature = file_signature(&std::fs::metadata(path).ok()?);
        if let Some(alias) = self
            .aliases
            .iter()
            .find(|alias| alias.path == path && alias.signature == signature)
            && let Some(index) = self
                .objects
                .iter()
                .position(|object| object.digest == alias.digest)
        {
            let object = self.objects.remove(index)?;
            let answer = (
                object.digest.clone(),
                std::sync::Arc::clone(&object.bytes),
            );
            self.objects.push_back(object);
            return Some(answer);
        }

        let bytes: std::sync::Arc<[u8]> = std::fs::read(path).ok()?.into();
        let digest = sha256_hex(&bytes);
        let bytes = if let Some(index) = self
            .objects
            .iter()
            .position(|object| object.digest == digest)
        {
            let object = self.objects.remove(index)?;
            let bytes = std::sync::Arc::clone(&object.bytes);
            self.objects.push_back(object);
            bytes
        } else {
            bytes
        };

        self.aliases.retain(|alias| alias.path != path);
        self.aliases.push_back(HotFileAlias {
            path: path.to_string(),
            signature,
            digest: digest.clone(),
        });
        while self.aliases.len() > 512 {
            self.aliases.pop_front();
        }

        if !self.objects.iter().any(|object| object.digest == digest)
            && bytes.len() <= HOT_PDF_FILE_BYTES
        {
            while self.bytes.saturating_add(bytes.len()) > HOT_PDF_FILE_BYTES {
                let expired = self.objects.pop_front()?;
                self.bytes = self.bytes.saturating_sub(expired.bytes.len());
            }
            self.bytes += bytes.len();
            self.objects.push_back(HotFileObject {
                digest: digest.clone(),
                bytes: std::sync::Arc::clone(&bytes),
            });
        }
        Some((digest, bytes))
    }
}

#[derive(Default)]
struct HotAssetCache {
    entries: std::collections::VecDeque<(
        (String, String, u32),
        HotAssetOutcome,
        usize,
    )>,
    bytes: usize,
}

#[derive(Clone)]
enum HotAssetOutcome {
    Ready(aitex_driver::pdf::AssetSource),
    Failed,
}

impl HotAssetCache {
    fn get(&mut self, key: &(String, String, u32)) -> Option<HotAssetOutcome> {
        let index = self.entries.iter().position(|entry| &entry.0 == key)?;
        let entry = self.entries.remove(index)?;
        let outcome = entry.1.clone();
        self.entries.push_back(entry);
        Some(outcome)
    }

    fn insert(
        &mut self,
        key: (String, String, u32),
        outcome: HotAssetOutcome,
        retained: usize,
    ) {
        if retained > HOT_PDF_ASSET_BYTES {
            return;
        }
        while self.entries.len() >= HOT_PDF_ASSET_ENTRIES
            || self.bytes.saturating_add(retained) > HOT_PDF_ASSET_BYTES
        {
            let Some(expired) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(expired.2);
        }
        self.bytes += retained;
        self.entries.push_back((key, outcome, retained));
    }
}

#[derive(Default)]
struct HotEpsPdfCache {
    entries: std::collections::VecDeque<(String, Option<std::sync::Arc<[u8]>>, usize)>,
    bytes: usize,
}

impl HotEpsPdfCache {
    fn get(&mut self, digest: &str) -> Option<Option<std::sync::Arc<[u8]>>> {
        let index = self.entries.iter().position(|entry| entry.0 == digest)?;
        let entry = self.entries.remove(index)?;
        let result = entry.1.as_ref().map(std::sync::Arc::clone);
        self.entries.push_back(entry);
        Some(result)
    }

    fn insert(&mut self, digest: String, pdf: Option<std::sync::Arc<[u8]>>) {
        let retained = pdf.as_ref().map_or(0, |bytes| bytes.len());
        if retained > HOT_EPS_PDF_BYTES {
            return;
        }
        while self.entries.len() >= HOT_EPS_PDF_ENTRIES
            || self.bytes.saturating_add(retained) > HOT_EPS_PDF_BYTES
        {
            let Some(expired) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(expired.2);
        }
        self.bytes += retained;
        self.entries.push_back((digest, pdf, retained));
    }
}

#[derive(Default)]
struct HotDviDocumentCache {
    entries: std::collections::VecDeque<(
        String,
        std::rc::Rc<aitex_driver::dvi::Document>,
        usize,
    )>,
    bytes: usize,
}

impl HotDviDocumentCache {
    fn get(&mut self, key: &str) -> Option<std::rc::Rc<aitex_driver::dvi::Document>> {
        let index = self.entries.iter().position(|entry| entry.0 == key)?;
        let entry = self.entries.remove(index)?;
        let document = std::rc::Rc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(document)
    }

    fn insert(
        &mut self,
        key: String,
        document: aitex_driver::dvi::Document,
    ) -> std::rc::Rc<aitex_driver::dvi::Document> {
        let retained = retained_dvi_bytes(&document);
        let document = std::rc::Rc::new(document);
        if retained <= HOT_DVI_DOCUMENT_BYTES {
            while self.bytes.saturating_add(retained) > HOT_DVI_DOCUMENT_BYTES {
                let Some(expired) = self.entries.pop_front() else {
                    break;
                };
                self.bytes = self.bytes.saturating_sub(expired.2);
            }
            self.bytes += retained;
            self.entries
                .push_back((key, std::rc::Rc::clone(&document), retained));
        }
        document
    }
}

struct HotPdfOutput {
    key: String,
    bytes: std::rc::Rc<[u8]>,
    dependencies: Vec<CacheDependency>,
}

/// Resolve a physical font use without consuming it. Distinct TeX font
/// instances may deliberately have the same file and size while carrying
/// independent microtype state; every DVI font number still needs to receive
/// the shared PDF resource.
fn reusable_font_resolution<T: Clone>(
    fonts: &std::collections::BTreeMap<(String, i64), T>,
    name: &str,
    at: i64,
) -> Option<T> {
    fonts.get(&(name.to_string(), at)).cloned()
}

struct HotResolvedFonts {
    key: String,
    fonts: std::collections::BTreeMap<(String, i64), aitex_driver::pdf::ClassicFontUse>,
    dependencies: Vec<CacheDependency>,
}

#[derive(Default)]
struct HotResolvedFontCache {
    entries: std::collections::VecDeque<HotResolvedFonts>,
}

impl HotResolvedFontCache {
    fn get(
        &mut self,
        key: &str,
    ) -> Option<(
        std::collections::BTreeMap<(String, i64), aitex_driver::pdf::ClassicFontUse>,
        Vec<CacheDependency>,
    )> {
        let index = self.entries.iter().position(|entry| entry.key == key)?;
        if let Err(error) = dependencies_hold(&self.entries[index].dependencies) {
            if std::env::var_os("AITEX_CACHE_TRACE").is_some() {
                eprintln!("PDF font resource cache invalid: {error}");
            }
            self.entries.remove(index);
            return None;
        }
        let entry = self.entries.remove(index)?;
        let answer = (entry.fonts.clone(), entry.dependencies.clone());
        self.entries.push_back(entry);
        Some(answer)
    }

    fn insert(
        &mut self,
        key: String,
        fonts: std::collections::BTreeMap<(String, i64), aitex_driver::pdf::ClassicFontUse>,
        dependencies: Vec<CacheDependency>,
    ) {
        self.entries.retain(|entry| entry.key != key);
        while self.entries.len() >= 8 {
            self.entries.pop_front();
        }
        self.entries.push_back(HotResolvedFonts {
            key,
            fonts,
            dependencies,
        });
    }
}

#[derive(Default)]
struct HotPdfOutputCache {
    entries: std::collections::VecDeque<HotPdfOutput>,
    bytes: usize,
}

impl HotPdfOutputCache {
    fn get(&mut self, key: &str) -> Option<(std::rc::Rc<[u8]>, Vec<CacheDependency>)> {
        let index = self.entries.iter().position(|entry| entry.key == key)?;
        if dependencies_hold(&self.entries[index].dependencies).is_err() {
            let expired = self.entries.remove(index)?;
            self.bytes = self.bytes.saturating_sub(expired.bytes.len());
            return None;
        }
        let entry = self.entries.remove(index)?;
        let answer = (
            std::rc::Rc::clone(&entry.bytes),
            entry.dependencies.clone(),
        );
        self.entries.push_back(entry);
        Some(answer)
    }

    fn insert(&mut self, key: String, bytes: Vec<u8>, dependencies: Vec<CacheDependency>) {
        if bytes.len() > HOT_PDF_OUTPUT_BYTES {
            return;
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key)
            && let Some(expired) = self.entries.remove(index)
        {
            self.bytes = self.bytes.saturating_sub(expired.bytes.len());
        }
        while self.bytes.saturating_add(bytes.len()) > HOT_PDF_OUTPUT_BYTES {
            let Some(expired) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(expired.bytes.len());
        }
        self.bytes += bytes.len();
        self.entries.push_back(HotPdfOutput {
            key,
            bytes: std::rc::Rc::from(bytes),
            dependencies,
        });
    }
}

#[derive(Default)]
struct HotType1ProgramCache {
    entries: std::collections::VecDeque<(
        (
            aitex_fonts::ClassicFaceIdentity,
            bool,
            Vec<String>,
        ),
        Option<aitex_fonts::PreparedType1>,
        usize,
    )>,
    bytes: usize,
}

impl HotType1ProgramCache {
    fn get_or_prepare(
        &mut self,
        face: &aitex_driver::pdf::ClassicFontFace,
        glyph_names: &std::collections::BTreeSet<String>,
        subset: bool,
    ) -> Option<aitex_fonts::PreparedType1> {
        let key = (
            face.identity.clone(),
            subset,
            subset
                .then(|| glyph_names.iter().cloned().collect())
                .unwrap_or_default(),
        );
        if let Some(index) = self.entries.iter().position(|entry| entry.0 == key) {
            let entry = self.entries.remove(index)?;
            let result = entry.1.clone();
            self.entries.push_back(entry);
            return result;
        }
        let prepared = if subset {
            aitex_fonts::prepare_type1_subset(
                &face.pfb,
                &face.postscript_name,
                face.slant,
                face.extend,
                glyph_names,
            )
        } else {
            aitex_fonts::prepare_type1_program(
                &face.pfb,
                &face.postscript_name,
                face.slant,
                face.extend,
            )
        };
        let retained = prepared
            .as_ref()
            .map_or(0, aitex_fonts::PreparedType1::retained_bytes);
        if retained <= HOT_TYPE1_PROGRAM_BYTES {
            while self.bytes.saturating_add(retained) > HOT_TYPE1_PROGRAM_BYTES {
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
    static HOT_PDF_FILES: std::cell::RefCell<HotFileCache> =
        std::cell::RefCell::new(HotFileCache::default());
    static HOT_DRIVER_TFMS: std::cell::RefCell<std::collections::BTreeMap<
        (String, i64), Option<aitex_engine::tfm::TfmFont>
    >> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static HOT_FONT_MAP_ENTRIES: std::cell::RefCell<std::collections::BTreeMap<
        (String, String), Option<aitex_driver::fontmap::MapEntry>
    >> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static HOT_ENCODINGS: std::cell::RefCell<std::collections::BTreeMap<
        String, Option<std::rc::Rc<Vec<String>>>
    >> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static HOT_NAMED_ENCODINGS: std::cell::RefCell<std::collections::BTreeMap<
        (String, String), Option<std::rc::Rc<Vec<String>>>
    >> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static HOT_TYPE1_ENCODINGS: std::cell::RefCell<std::collections::BTreeMap<
        String, Option<std::rc::Rc<Vec<String>>>
    >> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static HOT_TYPE1_PROGRAMS: std::cell::RefCell<HotType1ProgramCache> =
        std::cell::RefCell::new(HotType1ProgramCache::default());
    static HOT_DVI_DOCUMENTS: std::cell::RefCell<HotDviDocumentCache> =
        std::cell::RefCell::new(HotDviDocumentCache::default());
    static HOT_PDF_ASSETS: std::cell::RefCell<HotAssetCache> =
        std::cell::RefCell::new(HotAssetCache::default());
    static HOT_EPS_PDFS: std::cell::RefCell<HotEpsPdfCache> =
        std::cell::RefCell::new(HotEpsPdfCache::default());
    static HOT_PDF_OUTPUTS: std::cell::RefCell<HotPdfOutputCache> =
        std::cell::RefCell::new(HotPdfOutputCache::default());
    static HOT_RESOLVED_FONTS: std::cell::RefCell<HotResolvedFontCache> =
        std::cell::RefCell::new(HotResolvedFontCache::default());
    static LAST_PDF_DEPENDENCIES: std::cell::RefCell<Option<Vec<CacheDependency>>> =
        const { std::cell::RefCell::new(Some(Vec::new())) };
}

fn take_last_pdf_dependencies() -> Option<Vec<CacheDependency>> {
    LAST_PDF_DEPENDENCIES.with(|slot| {
        let result = slot.borrow_mut().take();
        *slot.borrow_mut() = Some(Vec::new());
        result
    })
}

fn tracked_pdf_resolve(
    log: &std::cell::RefCell<Vec<CacheDependency>>,
    track_dependencies: bool,
    request: &str,
    suffix: &str,
) -> Option<String> {
    let path = resolve_path_hot(request, suffix);
    if track_dependencies {
        log.borrow_mut()
            .push(CacheDependency::from_resolution(request, path.as_deref()));
    }
    path
}

fn retained_dvi_bytes(document: &aitex_driver::dvi::Document) -> usize {
    let items = document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .map(|item| {
            std::mem::size_of_val(item)
                + match item {
                    aitex_driver::dvi::Item::Special { bytes, .. } => bytes.len(),
                    _ => 0,
                }
        })
        .sum::<usize>();
    let pages = document.pages.len() * std::mem::size_of::<aitex_driver::dvi::Page>();
    let fonts = document
        .fonts
        .values()
        .map(|font| std::mem::size_of_val(font) + font.name.len())
        .sum::<usize>();
    std::mem::size_of_val(document)
        .saturating_add(items)
        .saturating_add(pages)
        .saturating_add(fonts)
}

fn dvi_document_difference(
    direct: &aitex_driver::dvi::Document,
    parsed: &aitex_driver::dvi::Document,
) -> String {
    if (direct.num, direct.den, direct.mag) != (parsed.num, parsed.den, parsed.mag) {
        return format!(
            "preamble is {}/{}/{} directly and {}/{}/{} after parsing",
            direct.num, direct.den, direct.mag, parsed.num, parsed.den, parsed.mag
        );
    }
    if direct.fonts != parsed.fonts {
        return format!(
            "font definitions differ: direct={:?}, parsed={:?}",
            direct.fonts, parsed.fonts
        );
    }
    if direct.pages.len() != parsed.pages.len() {
        return format!(
            "page count is {} directly and {} after parsing",
            direct.pages.len(),
            parsed.pages.len()
        );
    }
    for (page_index, (direct_page, parsed_page)) in
        direct.pages.iter().zip(&parsed.pages).enumerate()
    {
        if direct_page.width_millibp != parsed_page.width_millibp
            || direct_page.height_millibp != parsed_page.height_millibp
            || direct_page.counts != parsed_page.counts
        {
            return format!(
                "page {} metadata differs: direct={:?}, parsed={:?}",
                page_index + 1,
                (
                    direct_page.width_millibp,
                    direct_page.height_millibp,
                    direct_page.counts
                ),
                (
                    parsed_page.width_millibp,
                    parsed_page.height_millibp,
                    parsed_page.counts
                )
            );
        }
        if direct_page.items.len() != parsed_page.items.len() {
            return format!(
                "page {} has {} events directly and {} after parsing",
                page_index + 1,
                direct_page.items.len(),
                parsed_page.items.len()
            );
        }
        for (item_index, (direct_item, parsed_item)) in
            direct_page.items.iter().zip(&parsed_page.items).enumerate()
        {
            if direct_item != parsed_item {
                return format!(
                    "page {} event {} differs: direct={direct_item:?}, parsed={parsed_item:?}",
                    page_index + 1,
                    item_index + 1
                );
            }
        }
    }
    "documents differ outside the compared fields".to_string()
}

fn dvi_document_key(
    bytes: &[u8],
    fonts: &[aitex_engine::tfm::TfmFont],
    names: &[String],
    page_rotations: &[i16],
) -> String {
    let mut identity = Vec::new();
    key_field(&mut identity, "dvi", sha256_hex(bytes).as_bytes());
    for rotation in page_rotations {
        key_field(&mut identity, "rotation", &rotation.to_le_bytes());
    }
    // Set-char advances are implicit in DVI and supplied by the current TFM.
    // Include exactly those values so identical DVI bytes cannot reuse page
    // positions after a font metric file changes.
    for (name, font) in names.iter().zip(fonts) {
        key_field(&mut identity, "font-name", name.as_bytes());
        key_field(
            &mut identity,
            "font-at",
            &i64::from(font.at_size.0).to_le_bytes(),
        );
        let mut widths = Vec::with_capacity(256 * std::mem::size_of::<i64>());
        for code in 0..256u32 {
            widths.extend_from_slice(&i64::from(font.width(code).0).to_le_bytes());
        }
        key_field(&mut identity, "widths", &widths);
    }
    sha256_hex(&identity)
}

fn pdf_output_key(
    document_key: &str,
    glyph_to_unicode: &std::collections::BTreeMap<String, String>,
    options: aitex_driver::pdf::PdfOptions,
) -> String {
    let mut identity = Vec::new();
    key_field(&mut identity, "document", document_key.as_bytes());
    key_field(
        &mut identity,
        "stream-format",
        pdf_stream_format_tag(options).as_bytes(),
    );
    for (glyph, unicode) in glyph_to_unicode {
        key_field(&mut identity, "glyph", glyph.as_bytes());
        key_field(&mut identity, "unicode", unicode.as_bytes());
    }
    sha256_hex(&identity)
}

fn pdf_options_from_environment() -> aitex_driver::pdf::PdfOptions {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    aitex_driver::pdf::PdfOptions::default()
        .with_compression(std::env::var_os("AITEX_PDF_UNCOMPRESSED").is_none())
        .with_compression_workers(pdf_compression_workers(
            std::env::var_os("AITEX_PDF_COMPRESSION_WORKERS").as_deref(),
            available,
        ))
        .with_compact_objects(
            std::env::var_os("AITEX_PDF_CLASSIC_XREF").is_none()
                && std::env::var_os("AITEX_PDF_UNCOMPRESSED").is_none(),
        )
        .with_type1_subsetting(std::env::var_os("AITEX_PDF_FULL_TYPE1").is_none())
}

fn pdf_compression_workers(configured: Option<&std::ffi::OsStr>, available: usize) -> usize {
    let requested = configured
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|workers| *workers > 0)
        .unwrap_or(aitex_driver::pdf::DEFAULT_PDF_COMPRESSION_WORKERS);
    requested
        .min(aitex_driver::pdf::DEFAULT_PDF_COMPRESSION_WORKERS)
        .min(available.max(1))
}

fn pdf_stream_format_tag(options: aitex_driver::pdf::PdfOptions) -> &'static str {
    options.stream_format_tag()
}

fn pdf_font_key(
    document: &aitex_driver::dvi::Document,
    fonts: &[aitex_engine::tfm::TfmFont],
    names: &[String],
    glyph_to_unicode: &std::collections::BTreeMap<String, String>,
    pdf_map_files: &[String],
    pdf_map_lines: &[String],
    subset_type1: bool,
) -> String {
    let mut identity = Vec::new();
    key_field(
        &mut identity,
        "type1-subset",
        if subset_type1 { b"yes" } else { b"no" },
    );
    let mut used_codes = std::collections::BTreeMap::<u32, std::collections::BTreeSet<u8>>::new();
    for item in document.pages.iter().flat_map(|page| &page.items) {
        if let aitex_driver::dvi::Item::ClassicGlyph { font, code, .. } = item {
            used_codes
                .entry(font.0)
                .or_default()
                .insert((*code & 0xff) as u8);
        }
    }
    for (number, definition) in &document.fonts {
        key_field(&mut identity, "dvi-font-number", &number.to_le_bytes());
        key_field(&mut identity, "dvi-font-name", definition.name.as_bytes());
        key_field(&mut identity, "dvi-font-at", &definition.at.to_le_bytes());
        key_field(
            &mut identity,
            "dvi-font-design",
            &definition.design.to_le_bytes(),
        );
        if let Some(codes) = used_codes.get(number) {
            key_field(
                &mut identity,
                "dvi-font-used-codes",
                &codes.iter().copied().collect::<Vec<_>>(),
            );
        }
        // A loaded LaTeX format commonly carries dozens of unused TFM objects.
        // Font resource identity depends only on definitions that the DVI can
        // actually reference, so avoid hashing every width of every dormant
        // format font on every edit.
        if let Some((name, font)) = names
            .iter()
            .zip(fonts)
            .find(|(name, font)| {
                name.as_str() == definition.name
                    && i64::from(font.at_size.0) == definition.at
            })
            .or_else(|| {
                names
                    .iter()
                    .zip(fonts)
                    .find(|(name, _)| name.as_str() == definition.name)
            })
        {
            key_field(&mut identity, "engine-font-name", name.as_bytes());
            key_field(
                &mut identity,
                "engine-font-at",
                &i64::from(font.at_size.0).to_le_bytes(),
            );
            let mut widths = Vec::with_capacity(256 * std::mem::size_of::<i64>());
            for code in 0..256u32 {
                widths.extend_from_slice(&i64::from(font.width(code).0).to_le_bytes());
            }
            key_field(&mut identity, "engine-font-widths", &widths);
        } else {
            key_field(&mut identity, "engine-font-missing", definition.name.as_bytes());
        }
    }
    for (glyph, unicode) in glyph_to_unicode {
        key_field(&mut identity, "glyph", glyph.as_bytes());
        key_field(&mut identity, "unicode", unicode.as_bytes());
    }
    for file in pdf_map_files {
        key_field(&mut identity, "pdf-map-file", file.as_bytes());
    }
    for line in pdf_map_lines {
        key_field(&mut identity, "pdf-map-line", line.as_bytes());
    }
    sha256_hex(&identity)
}

fn apply_page_rotations(
    document: &mut aitex_driver::dvi::Document,
    page_rotations: &[i16],
) {
    for (page, rotation) in document.pages.iter_mut().zip(page_rotations) {
        if *rotation != 0 {
            page.items.insert(
                0,
                aitex_driver::dvi::Item::Special {
                    x: aitex_page::Sp(0),
                    y: aitex_page::Sp(0),
                    bytes: format!("aitex:page-rotate {rotation}").into_bytes(),
                },
            );
        }
    }
}

fn hot_file(path: &str) -> Option<(String, std::sync::Arc<[u8]>)> {
    HOT_PDF_FILES.with(|cache| cache.borrow_mut().read(path))
}

fn cap_parsed_cache<K: Clone + Ord, V>(cache: &mut std::collections::BTreeMap<K, V>) {
    while cache.len() >= 512 {
        let Some(first) = cache.keys().next().cloned() else {
            break;
        };
        cache.remove(&first);
    }
}

fn parsed_font_map(
    path: &str,
    wanted: &std::collections::BTreeSet<String>,
) -> Option<std::collections::BTreeMap<String, aitex_driver::fontmap::MapEntry>> {
    let (digest, bytes) = hot_file(path)?;
    HOT_FONT_MAP_ENTRIES.with(|cache| {
        let missing = wanted.iter().any(|name| {
            !cache
                .borrow()
                .contains_key(&(digest.clone(), name.clone()))
        });
        if missing {
            let text = std::str::from_utf8(&bytes).ok()?;
            let mut selected = aitex_driver::fontmap::parse_map_selected(text, wanted)
                .into_iter()
                .map(|entry| (entry.tex_name.clone(), entry))
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut cache = cache.borrow_mut();
            for name in wanted {
                cache.insert(
                    (digest.clone(), name.clone()),
                    selected.remove(name),
                );
            }
        }
        let parsed = wanted
            .iter()
            .filter_map(|name| {
                cache
                    .borrow()
                    .get(&(digest.clone(), name.clone()))
                    .cloned()
                    .flatten()
                    .map(|entry| (name.clone(), entry))
            })
            .collect();
        let mut cache = cache.borrow_mut();
        cap_parsed_cache(&mut cache);
        Some(parsed)
    })
}

fn pdf_map_directive(value: &str) -> (char, &str) {
    let value = value.trim();
    match value.as_bytes().first().copied() {
        Some(prefix @ (b'+' | b'-' | b'=')) => (prefix as char, value[1..].trim()),
        _ => ('+', value),
    }
}

fn parsed_encoding(path: &str) -> Option<std::rc::Rc<Vec<String>>> {
    let (digest, bytes) = hot_file(path)?;
    HOT_ENCODINGS.with(|cache| {
        if let Some(parsed) = cache.borrow().get(&digest) {
            return parsed.as_ref().map(std::rc::Rc::clone);
        }
        let parsed = std::str::from_utf8(&bytes)
            .ok()
            .and_then(aitex_driver::fontmap::parse_encoding)
            .map(std::rc::Rc::new);
        let mut cache = cache.borrow_mut();
        cap_parsed_cache(&mut cache);
        cache.insert(digest, parsed.as_ref().map(std::rc::Rc::clone));
        parsed
    })
}

fn parsed_named_encoding(path: &str, name: &str) -> Option<std::rc::Rc<Vec<String>>> {
    let (digest, bytes) = hot_file(path)?;
    let key = (digest, name.to_string());
    HOT_NAMED_ENCODINGS.with(|cache| {
        if let Some(parsed) = cache.borrow().get(&key) {
            return parsed.as_ref().map(std::rc::Rc::clone);
        }
        let parsed = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| aitex_driver::fontmap::parse_named_encoding(text, name))
            .map(std::rc::Rc::new);
        let mut cache = cache.borrow_mut();
        cap_parsed_cache(&mut cache);
        cache.insert(key, parsed.as_ref().map(std::rc::Rc::clone));
        parsed
    })
}

fn parsed_type1_encoding(digest: &str, bytes: &[u8]) -> Option<std::rc::Rc<Vec<String>>> {
    HOT_TYPE1_ENCODINGS.with(|cache| {
        if let Some(parsed) = cache.borrow().get(digest) {
            return parsed.as_ref().map(std::rc::Rc::clone);
        }
        let parsed = aitex_driver::fontmap::parse_type1_encoding(bytes).map(std::rc::Rc::new);
        let mut cache = cache.borrow_mut();
        cap_parsed_cache(&mut cache);
        cache.insert(digest.to_string(), parsed.as_ref().map(std::rc::Rc::clone));
        parsed
    })
}

fn parsed_driver_tfm(path: &str, at: i64) -> Option<aitex_engine::tfm::TfmFont> {
    let (digest, bytes) = hot_file(path)?;
    let key = (digest, at);
    HOT_DRIVER_TFMS.with(|cache| {
        if !cache.borrow().contains_key(&key) {
            let parsed = i32::try_from(at)
                .ok()
                .and_then(|at| aitex_engine::tfm::TfmFont::parse(&bytes, Some(Scaled(at))).ok());
            let mut cache = cache.borrow_mut();
            cap_parsed_cache(&mut cache);
            cache.insert(key.clone(), parsed);
        }
        cache.borrow().get(&key).cloned().flatten()
    })
}

/// Split the final pdfTeX synthetic tracking suffix (`+50ls`, `-20ls`, or
/// `0ls`) from a DVI font name. The synthetic metric keeps its public name,
/// while the actual glyph program and encoding come from the base font.
fn split_letterspace_font(name: &str) -> Option<(&str, i64)> {
    let stem = name.strip_suffix("ls")?;
    let bytes = stem.as_bytes();
    let mut start = bytes.len();
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    if start == bytes.len() {
        return None;
    }
    if start > 0 && matches!(bytes[start - 1], b'+' | b'-') {
        start -= 1;
        if start == 0 {
            return None;
        }
        let amount = stem[start..].parse().ok()?;
        return Some((&stem[..start], amount));
    }
    // Positive amounts always carry `+` and negative ones `-`.  Zero is the
    // sole unsigned spelling, so a base ending in digits remains unambiguous:
    // `cmr10` plus zero is `cmr100ls`, not `cmr` plus 100.
    name.strip_suffix("0ls").filter(|base| !base.is_empty()).map(|base| (base, 0))
}

fn physical_classic_font_name(mut name: &str) -> &str {
    while let Some((base, _)) = split_letterspace_font(name) {
        name = base;
    }
    name
}

fn tracked_driver_tfm(
    log: &std::cell::RefCell<Vec<CacheDependency>>,
    track_dependencies: bool,
    name: &str,
    at: i64,
) -> Option<aitex_engine::tfm::TfmFont> {
    let mut base = name;
    let mut tracking = Vec::new();
    while let Some((next, amount)) = split_letterspace_font(base) {
        tracking.push(amount);
        base = next;
    }
    let request = format!("{base}.tfm");
    let path = resolve_tfm_path_hot(base);
    if track_dependencies {
        log.borrow_mut()
            .push(CacheDependency::from_resolution(&request, path.as_deref()));
    }
    let path = path?;
    let mut font = parsed_driver_tfm(&path, at)?;
    for amount in tracking.into_iter().rev() {
        font = font.letterspaced(amount);
    }
    Some(font)
}

/// Painted-origin movement from pdfTeX's in-memory `\letterspacefont` VF.
/// The DVI font name is the only serialized trace of that synthetic VF; no
/// `<name>.vf` file exists for an ordinary driver to resolve.
fn tracked_driver_glyph_offset(
    log: &std::cell::RefCell<Vec<CacheDependency>>,
    track_dependencies: bool,
    name: &str,
    at: i64,
) -> Option<i64> {
    let mut base = name;
    let mut tracking = Vec::new();
    while let Some((next, amount)) = split_letterspace_font(base) {
        tracking.push(amount);
        base = next;
    }
    if tracking.is_empty() {
        return Some(0);
    }
    let request = format!("{base}.tfm");
    let path = resolve_tfm_path_hot(base);
    if track_dependencies {
        log.borrow_mut()
            .push(CacheDependency::from_resolution(&request, path.as_deref()));
    }
    let path = path?;
    let font = parsed_driver_tfm(&path, at)?;
    Some(
        tracking
            .into_iter()
            .rev()
            .map(|amount| i64::from(font.letterspace_glyph_offset(amount).0))
            .sum(),
    )
}

/// Render finished page events as PDF.
///
/// The normal path consumes the shared shipout contract directly. During the
/// migration, the same walk also emits private DVI in memory; qualification can
/// reparse it and compare the two semantic documents exactly, preserving DVI's
/// value as an independent oracle without imposing file I/O or a parse on users.
#[derive(Default)]
struct PdfMetrics {
    enabled: bool,
    dvi_parse: std::time::Duration,
    font_work: std::time::Duration,
    image_work: std::time::Duration,
    assembly: std::time::Duration,
    write: std::time::Duration,
    total: std::time::Duration,
}

struct ElapsedCounter {
    total: std::rc::Rc<std::cell::Cell<u128>>,
    started: Instant,
}

impl ElapsedCounter {
    fn new(total: std::rc::Rc<std::cell::Cell<u128>>) -> Self {
        Self {
            total,
            started: Instant::now(),
        }
    }
}

impl Drop for ElapsedCounter {
    fn drop(&mut self) {
        self.total.set(
            self.total
                .get()
                .saturating_add(self.started.elapsed().as_nanos()),
        );
    }
}

fn convert_to_pdf(
    out_path: &str,
    fonts: &[aitex_engine::tfm::TfmFont],
    names: &[String],
    glyph_to_unicode: &std::collections::BTreeMap<String, String>,
    pdf_map_files: &[String],
    pdf_map_lines: &[String],
    page_rotations: &[i16],
    track_dependencies: bool,
    metrics: &mut PdfMetrics,
    positioned: Option<&aitex_page::Document>,
    unicode_host: Option<&Rc<RefCell<aitex_unicode::UnicodeHost>>>,
    direct_document: Option<aitex_driver::dvi::Document>,
    private_dvi: Option<Vec<u8>>,
) -> bool {
    reset_hot_pk_resolvers();
    LAST_PDF_DEPENDENCIES.with(|slot| *slot.borrow_mut() = None);
    if !out_path.ends_with(".pdf") {
        return true;
    }
    let bytes = if let Some(bytes) = private_dvi {
        bytes
    } else {
        let Ok(bytes) = std::fs::read(out_path) else {
            eprintln!("aitex: cannot read the private output for {out_path}");
            return false;
        };
        bytes
    };
    if bytes.starts_with(b"%PDF-") {
        return true; // already converted
    }
    let find_font = |name: &str, at: i64| {
        names
            .iter()
            .enumerate()
            .find(|(i, candidate)| {
                candidate.as_str() == name
                    && fonts
                        .get(*i)
                        .is_some_and(|font| i64::from(font.at_size.0) == at)
            })
            .map(|(i, _)| i)
            // Malformed or third-party DVI files occasionally disagree in the
            // scale field. A name-only fallback still renders them, while our
            // own output always takes the exact branch above.
            .or_else(|| names.iter().position(|candidate| candidate == name))
    };
    let parse_started = (metrics.enabled && direct_document.is_none()).then(Instant::now);
    let document_key = dvi_document_key(&bytes, fonts, names, page_rotations);
    let pdf_options = pdf_options_from_environment();
    let output_key = pdf_output_key(&document_key, glyph_to_unicode, pdf_options);
    if positioned.is_none()
        && let Some((pdf, dependencies)) =
        HOT_PDF_OUTPUTS.with(|cache| cache.borrow_mut().get(&output_key))
    {
        let write_started = metrics.enabled.then(Instant::now);
        if let Err(error) = publish_output_artifact(out_path, pdf.as_ref()) {
            eprintln!("cannot write {out_path}: {error}");
            return false;
        }
        if let Some(started) = write_started {
            metrics.write = started.elapsed();
        }
        LAST_PDF_DEPENDENCIES.with(|slot| *slot.borrow_mut() = Some(dependencies));
        cli_detail!("PDF object graph reused (all page events and resources unchanged)");
        return true;
    }
    let pdf_dependencies = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let pdf_cacheable = std::rc::Rc::new(std::cell::Cell::new(track_dependencies));
    if track_dependencies && let Some(host) = unicode_host {
        for path in host.borrow().dependency_paths() {
            let shown = path.to_string_lossy();
            pdf_dependencies
                .borrow_mut()
                .push(CacheDependency::from_resolution(&shown, Some(&shown)));
        }
    }
    if std::env::var_os("AITEX_PDF_DIFFERENTIAL").is_some()
        && let Some(event_document) = direct_document.as_ref()
    {
        let mut event_document = event_document.clone();
        for page in &mut event_document.pages {
            page.items
                .retain(|item| !matches!(item, aitex_page::Item::GlyphRun(_)));
        }
        apply_page_rotations(&mut event_document, page_rotations);
        let mut parsed = match aitex_driver::dvi::parse_with_widths(&bytes, |def, code| {
            find_font(&def.name, def.at)
                .and_then(|index| fonts.get(index))
                .map_or(0, |font| i64::from(font.width(code).0))
        }) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("aitex: direct-PDF differential DVI parse failed: {error:?}");
                return false;
            }
        };
        apply_page_rotations(&mut parsed, page_rotations);
        if event_document != parsed {
            eprintln!(
                "aitex: direct-PDF page events disagree with the DVI oracle: {}",
                dvi_document_difference(&event_document, &parsed)
            );
            return false;
        }
        cli_detail!("direct PDF page events match the independent DVI oracle");
    }
    let doc = if let Some(mut document) = direct_document {
        apply_page_rotations(&mut document, page_rotations);
        HOT_DVI_DOCUMENTS.with(|cache| cache.borrow_mut().insert(document_key, document))
    } else if let Some(document) =
        HOT_DVI_DOCUMENTS.with(|cache| cache.borrow_mut().get(&document_key))
    {
        document
    } else {
        let mut document = match aitex_driver::dvi::parse_with_widths(&bytes, |def, code| {
            find_font(&def.name, def.at)
                .and_then(|i| fonts.get(i))
                .map_or(0, |font| i64::from(font.width(code).0))
        }) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("aitex: cannot parse the private DVI to make a PDF: {error:?}");
                return false;
            }
        };
        apply_page_rotations(&mut document, page_rotations);
        HOT_DVI_DOCUMENTS.with(|cache| cache.borrow_mut().insert(document_key, document))
    };
    if let Some(started) = parse_started {
        metrics.dvi_parse = started.elapsed();
    }
    let font_started = metrics.enabled.then(Instant::now);
    // Font programs and files named by `PSfile=...` specials use the same
    // long-lived kpathsea process and content-addressed byte/parse caches as
    // the engine. The pure driver still receives owned data at its boundary.
    let mut root_virtual_fonts = std::collections::BTreeMap::new();
    for definition in doc.fonts.values() {
        root_virtual_fonts
            .entry(definition.name.clone())
            .or_insert_with(|| {
                let physical_name = physical_classic_font_name(&definition.name);
                tracked_pdf_resolve(
                    &pdf_dependencies,
                    track_dependencies,
                    &format!("{physical_name}.vf"),
                    "",
                )
                .and_then(|path| hot_file(&path))
                .map(|(_, bytes)| bytes.as_ref().to_vec())
            });
    }
    let virtual_metric_fonts = std::cell::RefCell::new(std::collections::BTreeMap::<
        String,
        std::collections::BTreeMap<i64, Option<aitex_engine::tfm::TfmFont>>,
    >::new());
    // A `\letterspacefont` VF is synthesized inside pdfTeX and therefore has
    // no file for kpathsea to find. It still needs the same expansion pass so
    // its paint-only half-tracking movement reaches the PDF backend.
    let has_synthetic_virtual_font = doc
        .fonts
        .values()
        .any(|definition| split_letterspace_font(&definition.name).is_some());
    let expanded_document = if has_synthetic_virtual_font
        || root_virtual_fonts.values().any(Option::is_some)
    {
        let virtual_glyph_offsets = std::cell::RefCell::new(std::collections::BTreeMap::<
            String,
            std::collections::BTreeMap<i64, Option<i64>>,
        >::new());
        match aitex_driver::vf::expand_with_offsets(
            &doc,
            |name| {
                root_virtual_fonts.get(name).cloned().flatten().or_else(|| {
                    tracked_pdf_resolve(
                        &pdf_dependencies,
                        track_dependencies,
                        &format!("{name}.vf"),
                        "",
                    )
                    .and_then(|path| hot_file(&path))
                    .map(|(_, bytes)| bytes.as_ref().to_vec())
                })
            },
            |name, at, code| {
                let missing = virtual_metric_fonts
                    .borrow()
                    .get(name)
                    .is_none_or(|sizes| !sizes.contains_key(&at));
                if missing {
                    let font = find_font(name, at)
                        .and_then(|index| fonts.get(index).cloned())
                        .or_else(|| {
                            tracked_driver_tfm(&pdf_dependencies, track_dependencies, name, at)
                        });
                    virtual_metric_fonts
                        .borrow_mut()
                        .entry(name.to_string())
                        .or_default()
                        .insert(at, font);
                }
                virtual_metric_fonts
                    .borrow()
                    .get(name)?
                    .get(&at)?
                    .as_ref()
                    .map(|font| i64::from(font.width(code).0))
            },
            |name, at, _code| {
                let missing = virtual_glyph_offsets
                    .borrow()
                    .get(name)
                    .is_none_or(|sizes| !sizes.contains_key(&at));
                if missing {
                    let offset = tracked_driver_glyph_offset(
                        &pdf_dependencies,
                        track_dependencies,
                        name,
                        at,
                    );
                    virtual_glyph_offsets
                        .borrow_mut()
                        .entry(name.to_string())
                        .or_default()
                        .insert(at, offset);
                }
                virtual_glyph_offsets
                    .borrow()
                    .get(name)?
                    .get(&at)
                    .copied()
                    .flatten()
            },
        ) {
            Ok(document) => Some(document),
            Err(error) => {
                eprintln!("aitex: cannot expand virtual fonts for PDF: {error:?}");
                return false;
            }
        }
    } else {
        // Physical-font DVI is already in the writer's representation. Keep
        // borrowing the parsed document instead of cloning and walking every
        // character merely to discover that no virtual expansion is needed.
        None
    };
    let doc = expanded_document.as_ref().unwrap_or(&doc);
    let font_key = pdf_font_key(
        &doc,
        fonts,
        names,
        glyph_to_unicode,
        pdf_map_files,
        pdf_map_lines,
        pdf_options.subset_type1,
    );
    let resolved_fonts = if let Some((cached, dependencies)) =
        HOT_RESOLVED_FONTS.with(|cache| cache.borrow_mut().get(&font_key))
    {
        cli_detail!("PDF font resources reused");
        pdf_dependencies.borrow_mut().extend(dependencies);
        cached
    } else {
        let wanted_font_maps = doc
            .fonts
            .values()
            .map(|font| physical_classic_font_name(&font.name).to_string())
            .collect();
        let mut font_map = tracked_pdf_resolve(
            &pdf_dependencies,
            track_dependencies,
            "pdftex.map",
            "",
        )
            .and_then(|path| parsed_font_map(&path, &wanted_font_maps))
            .unwrap_or_default();
        for value in pdf_map_files {
            let (mode, name) = pdf_map_directive(value);
            if name.is_empty() {
                continue;
            }
            let Some(path) = tracked_pdf_resolve(
                &pdf_dependencies,
                track_dependencies,
                name,
                "",
            ) else {
                continue;
            };
            let Some(entries) = parsed_font_map(&path, &wanted_font_maps) else {
                continue;
            };
            for (name, entry) in entries {
                if mode == '-' {
                    font_map.remove(&name);
                } else if mode == '+' {
                    font_map.entry(name).or_insert(entry);
                } else {
                    font_map.insert(name, entry);
                }
            }
        }
        for value in pdf_map_lines {
            let (mode, line) = pdf_map_directive(value);
            for entry in aitex_driver::fontmap::parse_map(line) {
                if !wanted_font_maps.contains(&entry.tex_name) {
                    continue;
                }
                if mode == '-' {
                    font_map.remove(&entry.tex_name);
                } else if mode == '+' {
                    font_map.entry(entry.tex_name.clone()).or_insert(entry);
                } else {
                    font_map.insert(entry.tex_name.clone(), entry);
                }
            }
        }
        let consolidated_encodings = tracked_pdf_resolve(
            &pdf_dependencies,
            track_dependencies,
            "dvips-all.enc",
            "",
        );
        // Resolve all fonts before emitting any PDF objects. The driver's generic
        // API permits an unembedded font for callers that intentionally use a
        // resident PDF font; the compiler's 0.1 contract is stricter and never
        // relies on viewer substitution.
        let mut used_codes =
            std::collections::BTreeMap::<u32, std::collections::BTreeSet<u8>>::new();
        for item in doc.pages.iter().flat_map(|page| &page.items) {
            if let aitex_driver::dvi::Item::ClassicGlyph { font, code, .. } = item {
                used_codes
                    .entry(font.0)
                    .or_default()
                    .insert((*code & 0xff) as u8);
            }
        }
        let mut resolved_fonts = std::collections::BTreeMap::new();
        let mut classic_faces = std::collections::BTreeMap::<
            (String, String, u64, u64),
            std::sync::Arc<aitex_driver::pdf::ClassicFontFace>,
        >::new();
        for def in doc.fonts.values() {
        let physical_name = physical_classic_font_name(&def.name);
        let f = find_font(&def.name, def.at)
            .and_then(|index| fonts.get(index).cloned())
            .or_else(|| {
                tracked_driver_tfm(
                    &pdf_dependencies,
                    track_dependencies,
                    &def.name,
                    def.at,
                )
            });
        let Some(f) = f else {
            eprintln!(
                "aitex: cannot make PDF: DVI font {} at {}sp has no loaded metrics",
                def.name, def.at
            );
            return false;
        };
        let widths = (0..256u32)
            .map(|c| i64::from(f.width(c).0))
            .collect::<Vec<_>>();
        let map = font_map.get(physical_name);
        let postscript_name = map
            .map(|entry| entry.ps_name.clone())
            .unwrap_or_else(|| physical_name.to_string());
        let program_file = map
            .and_then(|entry| entry.font_file.clone())
            // TeX Live leaves the reversed Zapf Dingbats entry resident even
            // though it also installs the matching URW Type 1 program.
            .or_else(|| (postscript_name == "ZapfDingbats").then(|| "uzdr.pfb".to_string()))
            .unwrap_or_else(|| format!("{physical_name}.pfb"));
        let loaded_program = tracked_pdf_resolve(
            &pdf_dependencies,
            track_dependencies,
            &program_file,
            "",
        )
            .and_then(|path| hot_file(&path));
        let pfb = loaded_program
            .as_ref()
            .filter(|(_, bytes)| bytes.starts_with(&[0x80, 1]))
            .cloned();
        let true_type = loaded_program
            .as_ref()
            .filter(|(_, bytes)| {
                bytes.starts_with(b"\0\x01\0\0") || bytes.starts_with(b"true")
            })
            .cloned();
        let dpi = if def.design > 0 {
            ((600i128 * i128::from(def.at) + i128::from(def.design) / 2) / i128::from(def.design))
                .clamp(1, 100_000) as u32
        } else {
            600
        };
            let pk = if pfb.is_none() && true_type.is_none() {
                let name = format!("{physical_name}.pk");
                let request = format!("@pk:{dpi}:{name}");
                let path = resolve_cache_dependency(&request);
                pdf_dependencies
                    .borrow_mut()
                    .push(CacheDependency::from_resolution(
                        &request,
                        path.as_deref(),
                    ));
                path.and_then(|path| hot_file(&path))
            } else {
                None
            };
        if pfb.is_none() && true_type.is_none() && pk.is_none() {
            if loaded_program.is_some() {
                eprintln!(
                    "aitex: cannot make PDF: font {} uses unsupported program {}",
                    def.name, program_file
                );
                return false;
            }
            eprintln!(
                "aitex: cannot make PDF: font {} maps to {}, and no {}dpi PK bitmap was found",
                def.name, program_file, dpi
            );
            return false;
        }
        let encoding = if let Some(file) = map.and_then(|entry| entry.encoding_file.as_deref()) {
            let Some(encoding) = tracked_pdf_resolve(
                &pdf_dependencies,
                track_dependencies,
                file,
                "",
            )
                .and_then(|path| parsed_encoding(&path))
            else {
                eprintln!(
                    "aitex: cannot make PDF: font {} requires unreadable encoding {}",
                    def.name, file
                );
                return false;
            };
            encoding.as_ref().clone()
        } else if let Some((digest, pfb)) = pfb.as_ref() {
            let encoding = parsed_type1_encoding(digest, pfb).or_else(|| {
                consolidated_encodings
                    .as_deref()
                    .and_then(|path| parsed_named_encoding(path, physical_name))
            });
            let Some(encoding) = encoding else {
                eprintln!(
                    "aitex: cannot make PDF: no eight-bit encoding was found for font {}",
                    def.name
                );
                return false;
            };
            encoding.as_ref().clone()
        } else {
            if physical_name.starts_with("bbm") {
                ascii_math_encoding()
            } else {
                let inferred = if physical_name.starts_with("tc") {
                    Some("lm-ts1.enc")
                } else if physical_name.starts_with("ec") || physical_name.starts_with("dc") {
                    Some("lm-ec.enc")
                } else {
                    lh_bitmap_encoding_file(physical_name)
                };
                if let Some(file) = inferred {
                    let Some(encoding) = tracked_pdf_resolve(
                        &pdf_dependencies,
                        track_dependencies,
                        file,
                        "",
                    )
                        .and_then(|path| parsed_encoding(&path))
                    else {
                        eprintln!(
                            "aitex: cannot make PDF: bitmap font {} requires unreadable encoding {}",
                            def.name, file
                        );
                        return false;
                    };
                    encoding.as_ref().clone()
                } else if let Some(encoding) = consolidated_encodings
                    .as_deref()
                    .and_then(|path| parsed_named_encoding(path, physical_name))
                {
                    encoding.as_ref().clone()
                } else {
                    // PK carries the authoritative code-to-bitmap mapping, so
                    // an absent Adobe glyph-name encoding must not make the
                    // visual PDF impossible. Stock pdfTeX emits exactly such
                    // Type 3 fonts for uncommon symbol/logo families. Leave
                    // their codes semantically unmapped: the writer names the
                    // CharProcs by byte position, embeds every used bitmap,
                    // and omits a ToUnicode CMap when all entries are unknown.
                    eprintln!(
                        "aitex: bitmap font {} has no known semantic encoding; its glyphs will not support text extraction",
                        def.name
                    );
                    vec![".notdef".to_string(); 256]
                }
            }
        };
        if pk
            .as_ref()
            .is_some_and(|(_, bytes)| aitex_driver::pk::parse(bytes).is_none())
        {
            eprintln!(
                "aitex: cannot make PDF: malformed PK bitmap for font {}",
                def.name
            );
            return false;
        }
        let is_bitmap = pk.is_some();
        let to_unicode = encoding
            .iter()
            .take(256)
            .enumerate()
            .map(|(code, glyph)| {
                if is_bitmap {
                    unicode_for_bitmap_slot(
                        physical_name,
                        physical_name,
                        code,
                        glyph,
                        glyph_to_unicode,
                    )
                } else {
                    unicode_for_glyph(physical_name, glyph, glyph_to_unicode)
                }
            })
            .collect::<Vec<_>>();
        let slant = map.and_then(|entry| entry.slant).unwrap_or(0.0);
        let extend = map.and_then(|entry| entry.extend).unwrap_or(1.0);
        let bbox = [-100, -250, 1100, 900];
        let program = if let Some((digest, bytes)) = pfb {
            let key = (
                digest,
                postscript_name.clone(),
                slant.to_bits(),
                extend.to_bits(),
            );
            let face = classic_faces
                .entry(key)
                .or_insert_with(|| {
                    std::sync::Arc::new(aitex_driver::pdf::ClassicFontFace::type1(
                        std::sync::Arc::clone(&bytes),
                        postscript_name.clone(),
                        slant,
                        extend,
                        if pdf_options.subset_type1 {
                            aitex_driver::pdf::ClassicEmbeddingPolicy::Subset
                        } else {
                            aitex_driver::pdf::ClassicEmbeddingPolicy::Full
                        },
                        bbox,
                    ))
                })
                .clone();
            aitex_driver::pdf::ClassicFontProgram::Type1(face)
        } else if let Some((_, bytes)) = true_type {
            aitex_driver::pdf::ClassicFontProgram::TrueType {
                bytes,
                postscript_name: postscript_name.clone(),
                slant,
                extend,
                bbox,
            }
        } else {
            let (_, bytes) = pk.expect("a validated classic font has a program");
            aitex_driver::pdf::ClassicFontProgram::Pk {
                bytes,
                postscript_name: postscript_name.clone(),
                bbox,
            }
        };
            resolved_fonts.insert(
                (def.name.clone(), def.at),
                aitex_driver::pdf::ClassicFontUse {
                    program,
                    encoding: Some(encoding.into()),
                    to_unicode: to_unicode.into(),
                    widths: widths.into(),
                },
            );
        }
        let mut face_glyphs = std::collections::BTreeMap::<
            aitex_fonts::ClassicFaceIdentity,
            (
                std::sync::Arc<aitex_driver::pdf::ClassicFontFace>,
                std::collections::BTreeSet<String>,
            ),
        >::new();
        for (&number, definition) in &doc.fonts {
            let Some(font_use) = resolved_fonts.get(&(definition.name.clone(), definition.at))
            else {
                continue;
            };
            let aitex_driver::pdf::ClassicFontProgram::Type1(face) = &font_use.program else {
                continue;
            };
            let entry = face_glyphs
                .entry(face.identity.clone())
                .or_insert_with(|| (std::sync::Arc::clone(face), Default::default()));
            if let Some(encoding) = font_use.encoding.as_deref() {
                for code in used_codes.get(&number).into_iter().flatten() {
                    if let Some(name) = encoding.get(usize::from(*code)) {
                        entry.1.insert(name.clone());
                    }
                }
            }
        }
        let prepared_faces = face_glyphs
            .into_iter()
            .filter_map(|(identity, (face, glyphs))| {
                HOT_TYPE1_PROGRAMS.with(|cache| {
                    cache
                        .borrow_mut()
                        .get_or_prepare(&face, &glyphs, pdf_options.subset_type1)
                })
                .map(|prepared| (identity, prepared))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        for font_use in resolved_fonts.values_mut() {
            let aitex_driver::pdf::ClassicFontProgram::Type1(face) = &font_use.program else {
                continue;
            };
            if let Some(prepared) = prepared_faces.get(&face.identity) {
                font_use.program = aitex_driver::pdf::ClassicFontProgram::Prepared {
                    face: std::sync::Arc::clone(face),
                    prepared: prepared.clone(),
                };
            }
        }
        if pdf_cacheable.get() {
            HOT_RESOLVED_FONTS.with(|cache| {
                cache.borrow_mut().insert(
                    font_key,
                    resolved_fonts.clone(),
                    normalized_dependencies(&pdf_dependencies.borrow()),
                );
            });
        }
        resolved_fonts
    };
    if let Some(started) = font_started {
        metrics.font_work = started.elapsed();
    }
    let asset_nanos = std::rc::Rc::new(std::cell::Cell::new(0u128));
    let asset_nanos_in_resolver = std::rc::Rc::clone(&asset_nanos);
    let asset_failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let asset_failed_in_resolver = std::rc::Rc::clone(&asset_failed);
    let trace_images = std::env::var_os("AITEX_IMAGE_TRACE").is_some();
    let assembly_started = metrics.enabled.then(Instant::now);
    let measure_assets = metrics.enabled;
    let pdf_dependencies_in_resolver = std::rc::Rc::clone(&pdf_dependencies);
    let track_asset_dependencies = track_dependencies;
    let mut resolve_classic = |name: &str, at: i64| {
        reusable_font_resolution(&resolved_fonts, name, at)
    };
    let mut resolve_asset = {
        move |name: &str, page: u32| {
            let started = trace_images.then(Instant::now);
            let _elapsed = measure_assets
                .then(|| ElapsedCounter::new(std::rc::Rc::clone(&asset_nanos_in_resolver)));
            // `\pdfobj stream ... file {...}` embeds arbitrary bytes, not a
            // graphics format. Keep path resolution and dependency tracking at
            // this host boundary, but bypass image normalization entirely.
            let (name, raw_file) = name
                .strip_prefix("aitex-raw:")
                .map_or((name, false), |name| (name, true));
            let Some(path) = tracked_pdf_resolve(
                &pdf_dependencies_in_resolver,
                track_asset_dependencies,
                name,
                "",
            ) else {
                asset_failed_in_resolver.set(true);
                eprintln!("aitex: cannot make PDF: graphics asset {name:?} was not found");
                return None;
            };
            // A one-shot `aitex pass --no-cache` cannot reuse a graphics
            // object after this process exits.  Feeding those assets through
            // the process-wide hot caches used to retain up to 32 MiB of
            // source data while the writer simultaneously held the encoded
            // PDF objects and assembled the final output.  Read them into a
            // short-lived Arc instead; build/watch still take the hot path.
            let loaded = if track_asset_dependencies {
                hot_file(&path)
            } else {
                std::fs::read(&path).ok().map(|bytes| {
                    let digest = sha256_hex(&bytes);
                    (digest, std::sync::Arc::<[u8]>::from(bytes))
                })
            };
            let Some((digest, source)) = loaded else {
                asset_failed_in_resolver.set(true);
                eprintln!("aitex: cannot make PDF: graphics asset {path:?} is unreadable");
                return None;
            };
                if raw_file {
                    return Some(aitex_driver::pdf::AssetSource {
                        bytes: source,
                        pdf: None,
                        raster: None,
                        raster_bbox: None,
                    });
                }
                let extension = std::path::Path::new(&path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let key = (digest, extension.clone(), page);
                if track_asset_dependencies
                    && let Some(outcome) =
                        HOT_PDF_ASSETS.with(|cache| cache.borrow_mut().get(&key))
                {
                    match outcome {
                        HotAssetOutcome::Ready(asset) => {
                            if trace_images {
                                eprintln!(
                                    "aitex-image path={path:?} format={extension} source_bytes={} retained_bytes={} route=cache reuse=true elapsed_us={}",
                                    source.len(),
                                    asset.bytes.len()
                                        + asset.pdf.as_ref().map_or(0, |bytes| bytes.len())
                                        + asset.raster.as_ref().map_or(0, |bytes| bytes.len()),
                                    started.map_or(0, |started| started.elapsed().as_micros()),
                                );
                            }
                            return Some(asset);
                        }
                        HotAssetOutcome::Failed => {
                            asset_failed_in_resolver.set(true);
                            eprintln!(
                                "aitex: cannot make PDF: unsupported graphics asset {path:?} (cached deterministic failure)"
                            );
                            return None;
                        }
                    }
                }
                let original_bytes = source.len();
                // Keep unmodified PNG/JPEG/EPS bytes shared with the file
                // cache. Previously every direct asset was copied into a new
                // `Arc` before the PDF writer copied its encoded stream again,
                // adding up to 32 MiB to image-heavy cold builds.
                let mut bytes: std::borrow::Cow<'_, [u8]> =
                    std::borrow::Cow::Borrowed(source.as_ref());
                let direct_png = directly_embeddable_png(&bytes);
                let direct_eps = aitex_driver::pdf::directly_embeddable_eps(&bytes);
                // A native EPS Form uses the bounding box already carried by
                // the TeX special. Avoid launching `extractbb` for a value the
                // vector path cannot consume.
                let raster_bbox = (!direct_eps)
                    .then(|| raster_natural_bbox(&path, &bytes))
                    .flatten();
                let native_png = (!direct_png).then(|| normalize_rgba_png(&bytes)).flatten();
                let in_process_alpha = native_png.is_some();
                // Normalize the common eight-bit RGBA case in-process. The
                // legacy child-process path remains for other PNG flavours;
                // in particular, it strips a nominal but entirely opaque
                // alpha plane before embedding the exact RGB samples.
                if !direct_png
                    && native_png.is_none()
                    && let Some(rgb) = normalize_opaque_alpha_png(&path, &bytes)
                {
                    bytes = std::borrow::Cow::Owned(rgb);
                }
                let postscript = matches!(extension.as_str(), "eps" | "mps");
                let source_pdf = normalize_pdf_page(&path, page, &bytes)
                    .map(std::sync::Arc::<[u8]>::from)
                    .or_else(|| match extension.as_str() {
                        "eps" if !direct_eps => normalize_eps_pdf_cached(&bytes),
                        "mps" if !direct_eps => normalize_mps_pdf_cached(&bytes),
                        _ => None,
                    });
                let alpha_raster = if source_pdf.is_none() {
                    native_png.or_else(|| {
                        // Grayscale/RGB/indexed tRNS values that map exactly
                        // to a PDF color-key mask are already lossless on the
                        // native path. Normalizing those here both wasted a
                        // pair of child processes and, for RGB keys such as
                        // (0, 0, 1), could turn the transparent background
                        // opaque in the generated soft mask.
                        (!direct_png && png_has_alpha(&bytes))
                            .then(|| normalize_alpha_png(&path, &bytes))
                            .flatten()
                    })
                } else {
                    None
                };
                let discard_source = alpha_raster.is_some() || source_pdf.is_some();
                let pdf = source_pdf;
                // A damaged classic xref can keep the compact parser from
                // reading the original page tree even though MuPDF repairs it
                // for vector import. Derive the natural box from that repaired
                // page before falling back to the viewport carried by the DVI
                // special. In particular, this preserves pdfTeX's intersection
                // of a CropBox that extends beyond its MediaBox.
                let normalized_bbox = pdf
                    .as_deref()
                    .and_then(aitex_driver::pdf::first_page_pdftex_bbox);
                let raster_bbox = selected_asset_bbox(
                    &extension,
                    raster_bbox,
                    normalized_bbox,
                );
                let raster = alpha_raster.or_else(|| {
                    if directly_embeddable_png(&bytes)
                        || aitex_driver::pdf::directly_embeddable_jpeg(&bytes)
                        || direct_eps
                        || pdf.is_some()
                    {
                        None
                    } else {
                        rasterize_asset(&path, page, &bytes)
                    }
                });
                let route = if pdf.is_some() && postscript {
                    "eps-pdf-form"
                } else if pdf.is_some() {
                    "pdf-form"
                } else if raster.is_some() && in_process_alpha {
                    "png-alpha-in-process"
                } else if raster.is_some() && png_has_alpha(source.as_ref()) {
                    "png-alpha-normalized"
                } else if directly_embeddable_png(&bytes) {
                    "png-native"
                } else if aitex_driver::pdf::directly_embeddable_jpeg(&bytes) {
                    "jpeg-native"
                } else if direct_eps {
                    "eps-native"
                } else if raster.is_some() {
                    "bounded-raster-fallback"
                } else {
                    "unsupported"
                };
                if route == "unsupported" {
                    asset_failed_in_resolver.set(true);
                    if track_asset_dependencies {
                        HOT_PDF_ASSETS.with(|cache| {
                            cache.borrow_mut().insert(key, HotAssetOutcome::Failed, 0);
                        });
                    }
                    eprintln!(
                        "aitex: cannot make PDF: unsupported or unsafe graphics asset {path:?}; no bounded lossless import path succeeded"
                    );
                    return None;
                }
                // The normalized PDF or generated alpha bundle contains every
                // source byte the driver needs. Keeping the original beside it
                // would retain the same multi-megabyte asset twice.
                let asset_bytes = if discard_source {
                    std::sync::Arc::<[u8]>::from([])
                } else {
                    match bytes {
                        std::borrow::Cow::Borrowed(_) => std::sync::Arc::clone(&source),
                        std::borrow::Cow::Owned(bytes) => bytes.into(),
                    }
                };
                let asset = aitex_driver::pdf::AssetSource {
                    bytes: asset_bytes,
                    pdf,
                    raster: raster.map(Into::into),
                    raster_bbox,
                };
                let retained = asset.bytes.len()
                    + asset.pdf.as_ref().map_or(0, |bytes| bytes.len())
                    + asset.raster.as_ref().map_or(0, |bytes| bytes.len());
                if track_asset_dependencies {
                    HOT_PDF_ASSETS.with(|cache| {
                        cache.borrow_mut().insert(
                            key,
                            HotAssetOutcome::Ready(asset.clone()),
                            retained,
                        );
                    });
                }
                if trace_images {
                    eprintln!(
                        "aitex-image path={path:?} format={extension} source_bytes={original_bytes} retained_bytes={retained} peak_transient_bytes={} route={route} reuse=false elapsed_us={}",
                        original_bytes.saturating_add(retained),
                        started.map_or(0, |started| started.elapsed().as_micros()),
                    );
                }
                Some(asset)
        }
    };
    let pdf = if let (Some(positioned), Some(host)) = (positioned, unicode_host) {
        let mut resolve_positioned = |id: aitex_page::FontInstanceId| {
            host.borrow().font(id.0).map(|font| {
                aitex_driver::pdf::OpenTypeFontSource::new(
                    std::sync::Arc::clone(&font.bytes),
                    font.face_index,
                )
                .with_variations(std::sync::Arc::clone(&font.variations))
            })
        };
        match aitex_driver::pdf::write_hybrid_with_classic_assets_options(
            &doc,
            positioned,
            612.0,
            792.0,
            pdf_options,
            &mut resolve_classic,
            &mut resolve_positioned,
            &mut resolve_asset,
        ) {
            Ok(pdf) => pdf,
            Err(error) => {
                eprintln!("aitex: cannot make Unicode PDF: {error}");
                return false;
            }
        }
    } else {
        aitex_driver::pdf::write_with_classic_assets_options(
            &doc,
            612.0,
            792.0,
            pdf_options,
            &mut resolve_classic,
            &mut resolve_asset,
        )
    };
    if asset_failed.get() {
        eprintln!("aitex: PDF output was not written because a graphics asset failed");
        return false;
    }
    if let Some(started) = assembly_started {
        let driver_total = started.elapsed();
        metrics.image_work = std::time::Duration::from_nanos(
            u64::try_from(asset_nanos.get()).unwrap_or(u64::MAX),
        );
        metrics.assembly = driver_total.saturating_sub(metrics.image_work);
    }
    let write_started = metrics.enabled.then(Instant::now);
    if let Err(e) = publish_output_artifact(out_path, &pdf) {
        eprintln!("cannot write {out_path}: {e}");
        return false;
    }
    if let Some(started) = write_started {
        metrics.write = started.elapsed();
    }
    let dependencies = normalized_dependencies(&pdf_dependencies.borrow());
    LAST_PDF_DEPENDENCIES.with(|slot| {
        *slot.borrow_mut() = track_dependencies.then(|| dependencies.clone());
    });
    if pdf_cacheable.get() && positioned.is_none() {
        HOT_PDF_OUTPUTS.with(|cache| {
            cache
                .borrow_mut()
                .insert(output_key, pdf, dependencies);
        });
    }
    true
}

/// Ask MuPDF for a deterministic RGB PNG (or raw RGBA PAM) rendering of any
/// graphics asset accepted by the dvipdfmx route (PDF, PNG, JPEG, and friends).
/// This keeps decoding and
/// filesystem access at the host boundary; the driver crate remains a pure
/// bytes-to-bytes transformation. Keeping the zlib-compressed PNG scanlines
/// avoids materializing and embedding a multi-hundred-megabyte raw RGB stream.
/// EPS retains the existing Ghostscript fallback because MuPDF does not accept
/// every PostScript program.
fn png_header(bytes: &[u8]) -> Option<(u32, u32, u8, u8, u8)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n"
        || bytes.get(12..16)? != b"IHDR"
        || u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?) != 13
    {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
        u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?),
        *bytes.get(24)?,
        *bytes.get(25)?,
        *bytes.get(28)?,
    ))
}

fn png_resolution(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut p = 8usize;
    while p.checked_add(12)? <= bytes.len() {
        let len = u32::from_be_bytes(bytes.get(p..p + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(p + 4..p + 8)?;
        let data = bytes.get(p + 8..p.checked_add(8 + len)?)?;
        if kind == b"pHYs" && data.len() == 9 && data[8] == 1 {
            let x = u32::from_be_bytes(data.get(0..4)?.try_into().ok()?);
            let y = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?);
            // libpng reports pixels/metre; pdfTeX converts to DPI with C's
            // positive-number `round(0.0254 * ppm)`.
            let dpi = |ppm: u32| ((ppm as f64 * 0.0254) + 0.5).floor() as u32;
            return Some((dpi(x).max(1), dpi(y).max(1)));
        }
        p = p.checked_add(12 + len)?;
    }
    None
}

fn png_has_alpha(bytes: &[u8]) -> bool {
    let Some((_, _, _, color_type, _)) = png_header(bytes) else {
        return false;
    };
    if matches!(color_type, 4 | 6) {
        return true;
    }
    // Palette and grayscale/true-colour PNGs may carry transparency in a
    // separate tRNS chunk even though the IHDR colour type has no alpha bit.
    let mut p = 8usize;
    while p.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let Some(raw_len) = bytes.get(p..p + 4) else {
            return false;
        };
        let len = u32::from_be_bytes(raw_len.try_into().unwrap_or([0; 4])) as usize;
        let Some(next) = p.checked_add(12).and_then(|base| base.checked_add(len)) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        if bytes.get(p + 4..p + 8) == Some(b"tRNS") {
            return true;
        }
        p = next;
    }
    false
}

const FILTERED_PNG_BUNDLE: &[u8] = b"AITEX-PNG-ZLIB\0";
// The normalizer is row-streaming: this limit bounds the logical image, not
// resident memory. Scientific plots in the compatibility corpus legitimately
// reach 24,000 x 20,400 RGBA samples (just under 2 GiB decoded) while remaining
// compact PNGs. Keep a finite decompression-bomb ceiling without rejecting
// those files before their opaque alpha channel can be stripped.
const MAX_NORMALIZED_PNG_BYTES: usize = 2 * 1024 * 1024 * 1024;

pub(crate) fn rgba_png_idat(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut p = 8usize;
    let mut dimensions = None;
    let mut idat = Vec::new();
    let mut ended = false;
    while p.checked_add(12)? <= bytes.len() {
        let len = u32::from_be_bytes(bytes.get(p..p + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(p + 4..p + 8)?;
        let data_start = p + 8;
        let data_end = data_start.checked_add(len)?;
        let next = data_end.checked_add(4)?;
        let data = bytes.get(data_start..data_end)?;
        match kind {
            b"IHDR" => {
                if dimensions.is_some()
                    || data.len() != 13
                    || data[8] != 8
                    || data[9] != 6
                    || data[10] != 0
                    || data[11] != 0
                    || data[12] != 0
                {
                    return None;
                }
                let width = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
                let height = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?) as usize;
                if width == 0 || height == 0 {
                    return None;
                }
                let decoded = width.checked_mul(height)?.checked_mul(4)?;
                if decoded > MAX_NORMALIZED_PNG_BYTES {
                    return None;
                }
                dimensions = Some((width, height));
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => {
                ended = true;
                break;
            }
            _ => {}
        }
        p = next;
    }
    let (width, height) = dimensions?;
    (!idat.is_empty() && ended).then_some((width, height, idat))
}

fn paeth_predictor(left: u8, above: u8, upper_left: u8) -> u8 {
    let left = i32::from(left);
    let above = i32::from(above);
    let upper_left = i32::from(upper_left);
    let estimate = left + above - upper_left;
    let left_distance = (estimate - left).abs();
    let above_distance = (estimate - above).abs();
    let corner_distance = (estimate - upper_left).abs();
    if left_distance <= above_distance && left_distance <= corner_distance {
        left as u8
    } else if above_distance <= corner_distance {
        above as u8
    } else {
        upper_left as u8
    }
}

fn unfilter_rgba_row(filter: u8, current: &mut [u8], previous: Option<&[u8]>) -> Option<()> {
    match filter {
        0 => {}
        1 => {
            for x in 4..current.len() {
                current[x] = current[x].wrapping_add(current[x - 4]);
            }
        }
        2 => {
            let previous = previous?;
            for x in 0..current.len() {
                current[x] = current[x].wrapping_add(previous[x]);
            }
        }
        3 => {
            for x in 0..current.len() {
                let left = x.checked_sub(4).map_or(0, |at| current[at]);
                let above = previous.map_or(0, |row| row[x]);
                current[x] = current[x]
                    .wrapping_add(((u16::from(left) + u16::from(above)) / 2) as u8);
            }
        }
        4 => {
            for x in 0..current.len() {
                let left = x.checked_sub(4).map_or(0, |at| current[at]);
                let above = previous.map_or(0, |row| row[x]);
                let upper_left = x
                    .checked_sub(4)
                    .and_then(|at| previous.map(|row| row[at]))
                    .unwrap_or(0);
                current[x] =
                    current[x].wrapping_add(paeth_predictor(left, above, upper_left));
            }
        }
        _ => return None,
    }
    Some(())
}

/// Normalize the common eight-bit, non-interlaced RGBA PNG entirely in the
/// compiler process. The result keeps zlib-compressed, PNG-filtered RGB and
/// optional alpha planes, so the PDF writer does not need to inflate them
/// again. Unsupported PNG flavours retain the established Netpbm/MuPDF path.
fn normalize_rgba_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let (width, height, idat) = rgba_png_idat(bytes)?;
    let rgba_stride = width.checked_mul(4)?;
    let mut decoder = flate2::read::ZlibDecoder::new(idat.as_slice());
    let mut encoded_row = vec![0; rgba_stride.checked_add(1)?];
    let mut previous = vec![0; rgba_stride];
    let mut rgb_row = vec![0; width.checked_mul(3)?.checked_add(1)?];
    let mut alpha_row = vec![0; width.checked_add(1)?];
    // This stream is produced on every cold compile and PDF readers inflate it
    // only once. Level two is the measured middle ground: PNG filtering has
    // already removed most spatial redundancy, so the default level's extra
    // search costs conspicuous compile time on image-heavy papers.
    let mut rgb_encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(2));
    let mut alpha_encoder = None;
    for y in 0..height {
        decoder.read_exact(&mut encoded_row).ok()?;
        let filter = encoded_row[0];
        let current = &mut encoded_row[1..];
        unfilter_rgba_row(filter, current, (y > 0).then_some(previous.as_slice()))?;
        let output_filter = if y == 0 { 1 } else { 4 };
        rgb_row[0] = output_filter;
        alpha_row[0] = output_filter;
        let mut row_opaque = true;
        for pixel in 0..width {
            for component in 0..3 {
                let at = pixel * 4 + component;
                let left = pixel
                    .checked_sub(1)
                    .map_or(0, |left| current[left * 4 + component]);
                let predictor = if y == 0 {
                    left
                } else {
                    let upper_left = pixel
                        .checked_sub(1)
                        .map_or(0, |left| previous[left * 4 + component]);
                    paeth_predictor(left, previous[at], upper_left)
                };
                rgb_row[1 + pixel * 3 + component] = current[at].wrapping_sub(predictor);
            }
            let at = pixel * 4 + 3;
            row_opaque &= current[at] == 255;
            let left = pixel
                .checked_sub(1)
                .map_or(0, |left| current[left * 4 + 3]);
            let predictor = if y == 0 {
                left
            } else {
                let upper_left = pixel
                    .checked_sub(1)
                    .map_or(0, |left| previous[left * 4 + 3]);
                paeth_predictor(left, previous[at], upper_left)
            };
            alpha_row[1 + pixel] = current[at].wrapping_sub(predictor);
        }
        std::io::Write::write_all(&mut rgb_encoder, &rgb_row).ok()?;
        if !row_opaque && alpha_encoder.is_none() {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(2));
            // Rows before the first transparency were entirely 255. Recreate
            // their chosen Sub/Paeth representation instead of spending CPU
            // compressing an alpha stream that most RGBA figures never need.
            if y > 0 {
                let mut opaque_row = vec![0; width.checked_add(1)?];
                opaque_row[0] = 1;
                opaque_row[1] = 255;
                std::io::Write::write_all(&mut encoder, &opaque_row).ok()?;
                opaque_row.fill(0);
                opaque_row[0] = 4;
                for _ in 1..y {
                    std::io::Write::write_all(&mut encoder, &opaque_row).ok()?;
                }
            }
            alpha_encoder = Some(encoder);
        }
        if let Some(encoder) = alpha_encoder.as_mut() {
            std::io::Write::write_all(encoder, &alpha_row).ok()?;
        }
        previous.copy_from_slice(current);
    }
    let mut trailing = [0u8; 1];
    if decoder.read(&mut trailing).ok()? != 0 {
        return None;
    }
    let rgb = rgb_encoder.finish().ok()?;
    let alpha = match alpha_encoder {
        Some(encoder) => encoder.finish().ok()?,
        None => Vec::new(),
    };
    let mut bundle = Vec::with_capacity(
        FILTERED_PNG_BUNDLE
            .len()
            .checked_add(24)?
            .checked_add(rgb.len())?
            .checked_add(alpha.len())?,
    );
    bundle.extend_from_slice(FILTERED_PNG_BUNDLE);
    bundle.extend_from_slice(&u32::try_from(width).ok()?.to_be_bytes());
    bundle.extend_from_slice(&u32::try_from(height).ok()?.to_be_bytes());
    bundle.extend_from_slice(&(rgb.len() as u64).to_be_bytes());
    bundle.extend_from_slice(&(alpha.len() as u64).to_be_bytes());
    bundle.extend_from_slice(&rgb);
    bundle.extend_from_slice(&alpha);
    Some(bundle)
}

fn pdftex_png_xbb_size(bytes: &[u8]) -> Option<(String, String)> {
    let (width, height, ..) = png_header(bytes)?;
    let (x_dpi, y_dpi) = png_resolution(bytes).unwrap_or((72, 72));
    let converted = |pixels: u32, dpi: u32| {
        // pdfTeX's `ext_xn_over_d(one_hundred_inch, pixels, 100*dpi)`.
        let numerator = 473_628_672i128 * pixels as i128;
        let denominator = 100i128 * dpi as i128;
        let raw_sp = (numerator + denominator / 2) / denominator;
        // graphics.sty's decimal `.99626` is TeX's 65291/65536 fixed
        // fraction. `\the` then prints at most five decimal places.
        let bp_number_sp = (raw_sp * 65_291 + 32_768) / 65_536;
        format!("{:.5}", bp_number_sp as f64 / 65_536.0)
    };
    Some((converted(width, x_dpi), converted(height, y_dpi)))
}

/// Read a JPEG's pixel dimensions and JFIF density without decoding it. The
/// common plotting/camera output carries either square pixels (unit 0), a
/// DPI/DPCM pair in JFIF APP0, or TIFF resolution tags in Exif APP1. Files
/// without a usable SOF marker retain the external fallback.
fn jpeg_exif_density(segment: &[u8]) -> Option<(u8, f64, f64)> {
    let tiff = segment.strip_prefix(b"Exif\0\0")?;
    let little_endian = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |offset: usize| {
        let bytes: [u8; 2] = tiff.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
        Some(if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let u32_at = |offset: usize| {
        let bytes: [u8; 4] = tiff.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let entries = usize::from(u16_at(ifd)?);
    let entries_start = ifd.checked_add(2)?;
    let mut x_offset = None;
    let mut y_offset = None;
    let mut unit = None;
    for index in 0..entries {
        let entry = entries_start.checked_add(index.checked_mul(12)?)?;
        let tag = u16_at(entry)?;
        let kind = u16_at(entry.checked_add(2)?)?;
        let count = u32_at(entry.checked_add(4)?)?;
        match (tag, kind, count) {
            (0x011a, 5, 1..) => x_offset = Some(usize::try_from(u32_at(entry + 8)?).ok()?),
            (0x011b, 5, 1..) => y_offset = Some(usize::try_from(u32_at(entry + 8)?).ok()?),
            (0x0128, 3, 1..) => unit = Some(u16_at(entry + 8)?),
            _ => {}
        }
    }
    let rational = |offset: usize| {
        let numerator = f64::from(u32_at(offset)?);
        let denominator = f64::from(u32_at(offset.checked_add(4)?)?);
        (numerator > 0.0 && denominator > 0.0).then_some(numerator / denominator)
    };
    let x = rational(x_offset?)?;
    let y = rational(y_offset?)?;
    let unit = match unit? {
        2 => 1, // Exif inches are JFIF density unit 1.
        3 => 2, // Exif centimetres are JFIF density unit 2.
        _ => return None,
    };
    Some((unit, x, y))
}

fn jpeg_xbb_size(bytes: &[u8]) -> Option<(f64, f64)> {
    if bytes.get(..2)? != b"\xff\xd8" {
        return None;
    }
    let mut p = 2usize;
    let mut density = None;
    let mut dimensions = None;
    while p < bytes.len() {
        while bytes.get(p) == Some(&0xff) {
            p += 1;
        }
        let marker = *bytes.get(p)?;
        p += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x00 || marker == 0x01 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = u16::from_be_bytes(bytes.get(p..p.checked_add(2)?)?.try_into().ok()?) as usize;
        if length < 2 {
            return None;
        }
        let end = p.checked_add(length)?;
        let segment = bytes.get(p + 2..end)?;
        if marker == 0xe0 && segment.starts_with(b"JFIF\0") && segment.len() >= 12 {
            let units = segment[7];
            let x = u16::from_be_bytes(segment.get(8..10)?.try_into().ok()?) as f64;
            let y = u16::from_be_bytes(segment.get(10..12)?.try_into().ok()?) as f64;
            if x > 0.0 && y > 0.0 {
                density = Some((units, x, y));
            }
        }
        if marker == 0xe1 && density.is_none() {
            density = jpeg_exif_density(segment);
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) && segment.len() >= 5
        {
            let height = u16::from_be_bytes(segment.get(1..3)?.try_into().ok()?) as f64;
            let width = u16::from_be_bytes(segment.get(3..5)?.try_into().ok()?) as f64;
            if width > 0.0 && height > 0.0 {
                dimensions = Some((width, height));
            }
        }
        p = end;
    }
    let (width, height) = dimensions?;
    let (x_scale, y_scale) = match density {
        Some((1, x, y)) => (72.0 / x, 72.0 / y),
        Some((2, x, y)) => {
            // pdfTeX converts centimetre density to an integral DPI value
            // before computing the natural size.  In particular, a common
            // JFIF density of 37 pixels/cm becomes 93 DPI, not 93.98 DPI.
            // Preserve that truncation rather than extractbb's exact DPCM
            // conversion so graphics.sty's requested size matches pdfLaTeX.
            let x_dpi = (x * 2.54).trunc().max(1.0);
            let y_dpi = (y * 2.54).trunc().max(1.0);
            (72.0 / x_dpi, 72.0 / y_dpi)
        }
        _ => (1.0, 1.0),
    };
    Some((width * x_scale, height * y_scale))
}

/// Return a raster's full physical box in the same big-point coordinates that
/// `dvipdfmx.def` writes into `pdf:image bbox`. The pixel grid alone is not
/// enough: PNG pHYs and JPEG density metadata can make one source pixel a
/// fraction (or multiple) of a PDF point.
fn raster_natural_bbox(path: &str, bytes: &[u8]) -> Option<[f64; 4]> {
    if png_header(bytes).is_some() {
        let (width, height) = pdftex_png_xbb_size(bytes)?;
        return Some([0.0, 0.0, width.parse().ok()?, height.parse().ok()?]);
    }
    if bytes.starts_with(b"%PDF-") {
        // Do not fall through to extractbb when the compact parser cannot
        // inspect a damaged xref. extractbb reports the raw CropBox for such
        // files, whereas pdfTeX displays its intersection with the MediaBox.
        // The normalized-page path gets another chance to recover this box.
        return aitex_driver::pdf::first_page_pdftex_bbox(bytes);
    }
    if let Some((width, height)) = jpeg_xbb_size(bytes) {
        return Some([0.0, 0.0, width, height]);
    }
    // `extractbb` handles bitmap and PDF inputs, but TeX Live deliberately
    // rejects EPS.  Procedure-heavy EPS reaches this path after the native
    // translator declines it, so retain its original PostScript coordinate
    // system by reading the DSC header ourselves.  A numeric trailer value is
    // also accepted for files whose header advertises `(atend)`.
    if bytes.starts_with(b"%!") {
        // MetaPost is read directly by graphics.def, so its `PSfile` special
        // retains the original DSC coordinate system.  The normalized PDF
        // created at the host boundary, however, has a zero-origin page.
        // Keep the original integer box here so the PDF writer can map the
        // special's (possibly negative) lower-left corner onto that page.
        //
        // Ordinary EPS follows our synthetic normalized XBB route and writes
        // a zero-origin special, so it must retain the extent-only behavior.
        let mps = std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mps"));
        if let Some(bbox) = if mps {
            eps_graphics_bbox(bytes)
        } else {
            eps_natural_bbox(bytes)
        } {
            return Some(bbox);
        }
    }
    let output = bounded_asset_output(std::process::Command::new("extractbb").args(["-O", path]))?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for key in ["%%HiResBoundingBox: ", "%%BoundingBox: "] {
        if let Some(line) = text.lines().find_map(|line| line.strip_prefix(key)) {
            let values: Vec<f64> = line
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect();
            if let [llx, lly, urx, ury] = values.as_slice() {
                return Some([*llx, *lly, *urx, *ury]);
            }
        }
    }
    None
}

/// Choose the coordinate system retained beside a normalized graphics asset.
///
/// A selected page of a multi-page PDF can have a different CropBox from page
/// one, so its normalized one-page PDF is authoritative. EPS is different:
/// repstopdf translates the page to the origin, while later trim handling
/// still needs the original PostScript bounding box.
fn selected_asset_bbox(
    extension: &str,
    source_bbox: Option<[f64; 4]>,
    normalized_bbox: Option<[f64; 4]>,
) -> Option<[f64; 4]> {
    if extension == "pdf" {
        normalized_bbox.or(source_bbox)
    } else {
        source_bbox.or(normalized_bbox)
    }
}

fn eps_natural_bbox(bytes: &[u8]) -> Option<[f64; 4]> {
    let text = String::from_utf8_lossy(bytes);
    for key in ["%%HiResBoundingBox:", "%%BoundingBox:"] {
        for line in text.lines() {
            let Some(values) = line.trim_start().strip_prefix(key) else {
                continue;
            };
            let values: Vec<f64> = values
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect();
            if let [llx, lly, urx, ury] = values.as_slice()
                && urx > llx
                && ury > lly
            {
                // repstopdf translates the EPS natural box to a zero-origin
                // PDF page, and graphicx writes its trim/viewport box into the
                // DVI in that same translated coordinate system.  Preserve
                // the extents, not the original PostScript origin.
                return Some([0.0, 0.0, urx - llx, ury - lly]);
            }
        }
    }
    None
}

/// Read the ordinary DSC box used by graphicx when `hiresbb` was not
/// requested. Unlike `eps_natural_bbox`, retain the PostScript origin because
/// this value is handed back to graphicx as if it had read the EPS itself.
/// Numeric trailer values naturally handle a header containing `(atend)`.
fn eps_graphics_bbox(bytes: &[u8]) -> Option<[f64; 4]> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        let Some(values) = line.trim_start().strip_prefix("%%BoundingBox:") else {
            continue;
        };
        let values: Vec<f64> = values
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if let [llx, lly, urx, ury] = values.as_slice()
            && urx > llx
            && ury > lly
        {
            return Some([*llx, *lly, *urx, *ury]);
        }
    }
    None
}

fn directly_embeddable_png(bytes: &[u8]) -> bool {
    aitex_driver::pdf::directly_embeddable_png(bytes)
}

const MAX_ASSET_TOOL_OUTPUT_BYTES: u64 = 513 * 1024 * 1024;

fn asset_tool_timeout() -> std::time::Duration {
    static TIMEOUT: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        let seconds = std::env::var("AITEX_ASSET_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(1, 300);
        std::time::Duration::from_secs(seconds)
    })
}

fn wait_asset_tool(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now().checked_add(asset_tool_timeout())?;
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn bounded_asset_status(command: &mut std::process::Command) -> Option<std::process::ExitStatus> {
    let mut child = command.spawn().ok()?;
    wait_asset_tool(&mut child)
}

fn bounded_asset_output(command: &mut std::process::Command) -> Option<std::process::Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(stdout, MAX_ASSET_TOOL_OUTPUT_BYTES + 1),
            &mut bytes,
        )
        .ok()
        .map(|_| bytes)
    });
    let status = wait_asset_tool(&mut child)?;
    let stdout = reader.join().ok()??;
    if stdout.len() as u64 > MAX_ASSET_TOOL_OUTPUT_BYTES {
        return None;
    }
    Some(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// Allocate a collision-resistant private directory for one host-tool import.
fn import_directory(label: &str) -> Option<std::path::PathBuf> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir();
    loop {
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let candidate = root.join(format!(
            "aitex-{label}-import-{}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Some(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
}

/// Select and normalize one PDF page for the dependency-free driver. `clean`
/// Whether a normalized page dictionary names an array of content streams.
/// `mutool clean` has already made object dictionaries plain text at this
/// point. Requiring the PDF-name boundary prevents `/OtherContents` from
/// selecting the compatibility path.
fn pdf_page_content_is_array(bytes: &[u8]) -> bool {
    const NAME: &[u8] = b"/Contents";
    bytes.windows(NAME.len()).enumerate().any(|(at, window)| {
        if window != NAME
            || bytes
                .get(at + NAME.len())
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'#')
        {
            return false;
        }
        let mut value = at + NAME.len();
        while bytes
            .get(value)
            .is_some_and(|byte| matches!(*byte, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' '))
        {
            value += 1;
        }
        bytes.get(value) == Some(&b'[')
    })
}

/// turns xref/object streams into a classic xref table and garbage-collects
/// unreferenced pages while leaving existing streams compressed. A minority
/// of plotting PDFs split one page across many filtered content streams,
/// which a Form XObject cannot reference as an array. Only those inputs take
/// the proven decompression path so the driver can concatenate their streams;
/// avoiding `mutool clean -c` matters because its content rewriter is
/// pathologically slow on some otherwise tiny plots.
fn normalize_pdf_page(path: &str, page: u32, bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.starts_with(b"%PDF-") {
        return None;
    }
    if aitex_driver::pdf::directly_importable_single_page_pdf(bytes, page) {
        return Some(bytes.to_vec());
    }
    // A few PDFs produced by macOS append a page rotation in an incremental
    // update whose xref stream has an invalid `/Size`. pdfTeX/extractbb recover
    // the update, while MuPDF's repair mode keeps the older, unrotated page.
    // Ask extractbb for the effective page box only for incrementally updated
    // inputs, then verify that MuPDF retained the same geometry. Ordinary PDFs
    // therefore pay no extra process-launch cost.
    let expected_bbox = pdf_has_incremental_update(bytes)
        .then(|| extractbb_pdf_bbox(path, page))
        .flatten();
    let directory = import_directory("pdf")?;
    let output_path = directory.join("page.pdf");
    let status = bounded_asset_status(
        std::process::Command::new("mutool")
        .args(["clean", "-g", "-i"])
        .arg(path)
        .arg(&output_path)
        .arg(page.max(1).to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null()),
    );
    let mut result = status
        .filter(|status| status.success())
        .and_then(|_| std::fs::read(&output_path).ok())
        .filter(|pdf| pdf.starts_with(b"%PDF-"));
    if result.as_deref().is_some_and(|pdf| {
        pdf_page_content_is_array(pdf)
            || aitex_driver::pdf::first_page_content_stream_count(pdf)
                .is_some_and(|count| count > 1)
    }) {
        let status = bounded_asset_status(
            std::process::Command::new("mutool")
            .args(["clean", "-g", "-d", "-i"])
            .arg(path)
            .arg(&output_path)
            .arg(page.max(1).to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
        );
        result = status
            .filter(|status| status.success())
            .and_then(|_| std::fs::read(&output_path).ok())
            .filter(|pdf| pdf.starts_with(b"%PDF-"));
    }
    if let Some(expected_bbox) = expected_bbox
        && result
            .as_deref()
            .and_then(aitex_driver::pdf::first_page_pdftex_bbox)
            .is_none_or(|actual_bbox| !pdf_box_extents_match(expected_bbox, actual_bbox))
        && let Some(recovered) = normalize_pdf_page_with_ghostscript(path, page, &output_path)
    {
        result = Some(recovered);
    }
    let _ = std::fs::remove_file(&output_path);
    let _ = std::fs::remove_dir(&directory);
    result
}

fn pdf_has_incremental_update(bytes: &[u8]) -> bool {
    bytes
        .windows(b"startxref".len())
        .filter(|window| *window == b"startxref")
        .take(2)
        .count()
        > 1
}

fn extractbb_pdf_bbox(path: &str, page: u32) -> Option<[f64; 4]> {
    let output = bounded_asset_output(
        std::process::Command::new("extractbb")
            .args(["-O", "-p", &page.max(1).to_string(), path]),
    )?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for key in ["%%HiResBoundingBox: ", "%%BoundingBox: "] {
        let Some(line) = text.lines().find_map(|line| line.strip_prefix(key)) else {
            continue;
        };
        let values = line
            .split_whitespace()
            .filter_map(|value| value.parse::<f64>().ok())
            .collect::<Vec<_>>();
        if let [llx, lly, urx, ury] = values.as_slice()
            && urx > llx
            && ury > lly
        {
            return Some([*llx, *lly, *urx, *ury]);
        }
    }
    None
}

fn pdf_box_extents_match(left: [f64; 4], right: [f64; 4]) -> bool {
    pdf_box_extents_within(left, right, 0.02)
}

fn pdf_box_extents_within(left: [f64; 4], right: [f64; 4], tolerance: f64) -> bool {
    let [left_width, left_height] = [left[2] - left[0], left[3] - left[1]];
    let [right_width, right_height] = [right[2] - right[0], right[3] - right[1]];
    (left_width - right_width).abs() <= tolerance
        && (left_height - right_height).abs() <= tolerance
}

fn normalize_pdf_page_with_ghostscript(
    path: &str,
    page: u32,
    output_path: &std::path::Path,
) -> Option<Vec<u8>> {
    let page = page.max(1).to_string();
    // Ghostscript can repair PDFs that MuPDF deliberately preserves with the
    // wrong incremental page geometry.  Some Ghostscript releases, however,
    // leave a duplicate live entry in the repaired classic xref table.  PDF
    // viewers recover that entry, but it is not a valid input to our bounded
    // Form importer.  Write the repair separately, then let MuPDF rebuild and
    // garbage-collect its object table before the bytes cross the importer
    // boundary.
    let repaired_path = output_path.with_extension("ghostscript.pdf");
    let output = format!("-sOutputFile={}", repaired_path.to_str()?);
    let first = format!("-dFirstPage={page}");
    let last = format!("-dLastPage={page}");
    let status = bounded_asset_status(
        std::process::Command::new("gs")
            .args([
                "-q",
                "-dSAFER",
                "-dBATCH",
            "-dNOPAUSE",
            "-dUseCropBox",
            // Apply a source page's explicit /Rotate, but do not let
            // Ghostscript infer a new orientation from the marks on an
            // otherwise unrotated landscape figure.  PageByPage turned such
            // figures into portrait Forms even though pdfTeX keeps their
            // declared landscape CropBox.
            "-dAutoRotatePages=/None",
                "-sDEVICE=pdfwrite",
                "-dCompatibilityLevel=1.7",
                &first,
                &last,
                &output,
                path,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )?;
    if !status.success() {
        let _ = std::fs::remove_file(&repaired_path);
        return None;
    }
    let clean = bounded_asset_status(
        std::process::Command::new("mutool")
            .args(["clean", "-g", "-i"])
            .arg(&repaired_path)
            .arg(output_path)
            .arg("1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
    let _ = std::fs::remove_file(&repaired_path);
    clean
        .filter(|status| status.success())
        .and_then(|_| std::fs::read(output_path).ok())
        .filter(|pdf| aitex_driver::pdf::directly_importable_single_page_pdf(pdf, 1))
}

/// Convert a procedure-heavy EPS to the same vector-PDF representation used
/// by pdfLaTeX's standard epstopdf integration. `repstopdf` is part of TeX
/// Live and runs in its restricted mode here; if it is unavailable or rejects
/// the input, the caller retains the bounded raster fallback.
fn normalize_eps_pdf_cached(bytes: &[u8]) -> Option<std::sync::Arc<[u8]>> {
    let digest = format!("eps:{}", sha256_hex(bytes));
    if let Some(cached) = HOT_EPS_PDFS.with(|cache| cache.borrow_mut().get(&digest)) {
        return cached;
    }
    let result = normalize_eps_pdf_uncached(bytes).map(std::sync::Arc::<[u8]>::from);
    HOT_EPS_PDFS.with(|cache| {
        cache.borrow_mut().insert(digest, result.clone());
    });
    result
}

/// Convert MetaPost output with TeX Live's dedicated `mptopdf` format. Plain
/// Ghostscript cannot resolve MetaPost's compact `fshow` font names, whereas
/// `mptopdf` uses the same TeX font maps as pdfTeX and keeps the result vector.
fn normalize_mps_pdf_cached(bytes: &[u8]) -> Option<std::sync::Arc<[u8]>> {
    let digest = format!("mps:{}", sha256_hex(bytes));
    if let Some(cached) = HOT_EPS_PDFS.with(|cache| cache.borrow_mut().get(&digest)) {
        return cached;
    }
    let result = normalize_mps_pdf_uncached(bytes).map(std::sync::Arc::<[u8]>::from);
    HOT_EPS_PDFS.with(|cache| {
        cache.borrow_mut().insert(digest, result.clone());
    });
    result
}

fn normalize_mps_pdf_uncached(bytes: &[u8]) -> Option<Vec<u8>> {
    let directory = import_directory("mps-pdf")?;
    // mptopdf recognizes MetaPost's traditional numbered suffix and writes
    // `figure-1.pdf`; using `.mps` would make its output naming ambiguous.
    let input_path = directory.join("figure.1");
    let output_path = directory.join("figure-1.pdf");
    if std::fs::write(&input_path, bytes).is_err() {
        let _ = std::fs::remove_dir(&directory);
        return None;
    }
    let converted = bounded_asset_status(
        std::process::Command::new("mptopdf")
            .arg("figure.1")
            .current_dir(&directory)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .is_some_and(|status| status.success());
    let source = converted
        .then(|| std::fs::read(&output_path).ok())
        .flatten()
        .filter(|pdf| pdf.starts_with(b"%PDF-"));
    let result = source
        .as_deref()
        .and_then(|pdf| normalize_pdf_page(output_path.to_str()?, 1, pdf));
    for name in ["figure.1", "figure-1.pdf", "figure.pdf", "figure.log"] {
        let _ = std::fs::remove_file(directory.join(name));
    }
    let _ = std::fs::remove_dir(&directory);
    result
}

fn normalize_eps_pdf_uncached(bytes: &[u8]) -> Option<Vec<u8>> {
    let directory = import_directory("eps-pdf")?;
    let input_path = directory.join("figure.eps");
    let output_path = directory.join("figure.pdf");
    if std::fs::write(&input_path, bytes).is_err() {
        let _ = std::fs::remove_dir(&directory);
        return None;
    }
    let converted = bounded_asset_status(
        std::process::Command::new("repstopdf")
            .arg("--restricted")
            // Restricted epstopdf intentionally permits only input and output
            // names in its working directory when TeX's `openin_any=p` /
            // `openout_any=p` policy is active. Materialize the already-read
            // source bytes in the private directory instead of weakening that
            // policy or relying on a path outside it.
            .arg("--outfile=figure.pdf")
            .arg("figure.eps")
            .current_dir(&directory)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .is_some_and(|status| status.success());
    let source = converted
        .then(|| std::fs::read(&output_path).ok())
        .flatten()
        .filter(|pdf| pdf.starts_with(b"%PDF-"));
    let result = source
        .as_deref()
        .and_then(|pdf| normalize_pdf_page(output_path.to_str()?, 1, pdf));
    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&output_path);
    let _ = std::fs::remove_dir(&directory);
    result
}

/// Configure Netpbm's PNG decoder. Its low-memory row API is correct for
/// ordinary PNGs, but affected Netpbm/libpng combinations emit corrupt rows
/// for Adam7 images because each pass does not contain a complete display row.
/// Interlaced inputs therefore use libpng's whole-image path.
fn pngtopam_command(path: &str, alpha: bool, interlace: u8) -> std::process::Command {
    let mut decoder = std::process::Command::new("pngtopam");
    decoder.arg("-quiet");
    if interlace == 0 {
        decoder.arg("-byrow");
    }
    if alpha {
        decoder.arg("-alpha");
    }
    decoder.arg(path);
    decoder
}

/// Decode and re-encode one PNG plane. Netpbm's `-byrow` path uses a few MiB
/// even for hundred-megapixel figures, while ImageMagick retains the complete
/// decoded image and made child-process RSS dominate the compiler's
/// qualification measurement. See `pngtopam_command` for the Adam7 fallback.
fn netpbm_png_plane(
    path: &str,
    output: &std::path::Path,
    alpha: bool,
    interlace: u8,
) -> Option<()> {
    let mut decoder = pngtopam_command(path, alpha, interlace)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let decoded = decoder.stdout.take()?;
    let encoded = std::fs::File::create(output).ok()?;
    let encoder = bounded_asset_status(
        std::process::Command::new("pnmtopng")
        .args(["-quiet", "-force", "-compression=6"])
        .stdin(std::process::Stdio::from(decoded))
        .stdout(std::process::Stdio::from(encoded))
        .stderr(std::process::Stdio::null()),
    );
    let decoded = wait_asset_tool(&mut decoder);
    (encoder.is_some_and(|status| status.success())
        && decoded.is_some_and(|status| status.success()))
    .then_some(())
}

/// Check the eight-bit alpha plane without materializing it. `pngtopam`
/// produces a binary PGM here; consume its small header and scan the samples
/// in a fixed buffer.
fn png_alpha_is_opaque(path: &str, interlace: u8) -> Option<bool> {
    fn token(stream: &mut impl Read) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        let mut comment = false;
        loop {
            if stream.read(&mut byte).ok()? == 0 {
                return (!out.is_empty()).then_some(out);
            }
            if comment {
                if matches!(byte[0], b'\r' | b'\n') {
                    comment = false;
                }
            } else if byte[0] == b'#' && out.is_empty() {
                comment = true;
            } else if byte[0].is_ascii_whitespace() {
                if !out.is_empty() {
                    return Some(out);
                }
            } else {
                out.push(byte[0]);
            }
        }
    }

    let mut child = pngtopam_command(path, true, interlace)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stream = child.stdout.take()?;
    let magic = token(&mut stream)?;
    let width: usize = std::str::from_utf8(&token(&mut stream)?)
        .ok()?
        .parse()
        .ok()?;
    let height: usize = std::str::from_utf8(&token(&mut stream)?)
        .ok()?
        .parse()
        .ok()?;
    let max: u8 = std::str::from_utf8(&token(&mut stream)?)
        .ok()?
        .parse()
        .ok()?;
    if magic != b"P5" || max == 0 {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let mut remaining = width.checked_mul(height)?;
    let mut opaque = true;
    let mut buffer = [0u8; 16 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len());
        let got = stream.read(&mut buffer[..wanted]).ok()?;
        if got == 0 {
            opaque = false;
            break;
        }
        opaque &= buffer[..got].iter().all(|sample| *sample == max);
        remaining -= got;
    }
    drop(stream);
    let status = child.wait().ok()?;
    status.success().then_some(opaque && remaining == 0)
}

/// Preserve a transparent PNG as two compressed PNG planes. The row-streaming
/// Netpbm path turns alpha off without compositing for the RGB plane, retaining
/// even the source samples hidden below fully transparent pixels, and extracts
/// opacity as an eight-bit grayscale plane. ImageMagick remains the fallback
/// for PNG flavours Netpbm cannot normalize to the required planes. PDF readers
/// resample the image and `/SMask`
/// independently; zeroing hidden RGB (as a generic PDF conversion does) leaks
/// black into downsampled plot markers. The small bundle header lets the
/// dependency-free driver embed both IDAT streams without inflating them.
fn normalize_alpha_png(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    const BUNDLE: &[u8] = b"AITEX-RGBA-PNG\0";
    let (width, height, depth, color_type, interlace) = png_header(bytes)?;
    let directory = import_directory("alpha-png")?;
    let rgb_path = directory.join("rgb.png");
    let alpha_path = directory.join("alpha.png");
    let mut converted = depth == 8
        && color_type != 4
        && netpbm_png_plane(path, &rgb_path, false, interlace).is_some()
        && netpbm_png_plane(path, &alpha_path, true, interlace).is_some();
    if !converted {
        let _ = std::fs::remove_file(&rgb_path);
        let _ = std::fs::remove_file(&alpha_path);
        let rgb_target = format!("PNG24:{}", rgb_path.display());
        let rgb_status = bounded_asset_status(
            std::process::Command::new("convert")
            .arg(path)
            .args(["-alpha", "off", "+profile", "*"])
            .arg(rgb_target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
        );
        let alpha_status = bounded_asset_status(
            std::process::Command::new("convert")
            .arg(path)
            .args([
                "-alpha",
                "extract",
                "-depth",
                "8",
                "+profile",
                "*",
                "-define",
                "png:color-type=0",
            ])
            .arg(&alpha_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
        );
        converted = rgb_status.is_some_and(|status| status.success())
            && alpha_status.is_some_and(|status| status.success());
    }
    let result = (|| {
        if !converted {
            return None;
        }
        let rgb = std::fs::read(&rgb_path).ok()?;
        let alpha = std::fs::read(&alpha_path).ok()?;
        if !directly_embeddable_png(&rgb)
            || png_header(&rgb).map(|header| (header.0, header.1)) != Some((width, height))
            || !matches!(png_header(&alpha), Some((w, h, 8, 0, 0)) if (w, h) == (width, height))
        {
            return None;
        }
        let mut bundle = Vec::with_capacity(BUNDLE.len() + 8 + rgb.len() + alpha.len());
        bundle.extend_from_slice(BUNDLE);
        bundle.extend_from_slice(&(rgb.len() as u64).to_be_bytes());
        bundle.extend_from_slice(&rgb);
        bundle.extend_from_slice(&alpha);
        Some(bundle)
    })();
    let _ = std::fs::remove_file(&rgb_path);
    let _ = std::fs::remove_file(&alpha_path);
    let _ = std::fs::remove_dir(&directory);
    result
}

/// Turn a nominally-alpha but fully opaque PNG into an RGB PNG without
/// changing a pixel. Netpbm scans and rewrites the image row by row, so a
/// multi-thousand-pixel scientific figure does not inflate either the compiler
/// or a child process. ImageMagick remains a compatibility fallback.
fn normalize_opaque_alpha_png(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if !png_has_alpha(bytes) {
        return None;
    }
    let header = png_header(bytes)?;
    let netpbm_supported = header.2 == 8 && header.3 != 4;
    let netpbm_opaque = netpbm_supported
        .then(|| png_alpha_is_opaque(path, header.4))
        .flatten();
    let is_opaque = match netpbm_opaque {
        Some(opaque) => opaque,
        None => {
            let opaque = bounded_asset_output(
                std::process::Command::new("identify")
                    .args(["-format", "%[opaque]", path]),
            )?;
            opaque.status.success()
                && String::from_utf8_lossy(&opaque.stdout)
                    .trim()
                    .eq_ignore_ascii_case("true")
        }
    };
    if !is_opaque {
        return None;
    }
    let directory = import_directory("opaque-png")?;
    let output_path = directory.join("rgb.png");
    let mut converted = netpbm_supported
        && netpbm_png_plane(path, &output_path, false, header.4).is_some();
    if !converted {
        let _ = std::fs::remove_file(&output_path);
        let png24 = format!("PNG24:{}", output_path.display());
        converted = bounded_asset_status(
            std::process::Command::new("convert")
                .arg(path)
                .args(["-alpha", "off"])
                .arg(png24)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
            .is_some_and(|status| status.success());
    }
    let result = converted
        .then(|| std::fs::read(&output_path).ok())
        .flatten()
        .filter(|png| directly_embeddable_png(png));
    let _ = std::fs::remove_file(&output_path);
    let _ = std::fs::remove_dir(&directory);
    result
}

/// Isolate one complete PNG from a renderer's stdout. Ghostscript can emit
/// PostScript `%%Warning` text before the binary image even under `-q`; the
/// rendering itself is valid and deterministic, so rejecting it because byte
/// zero is not the PNG signature loses a safe fallback for otherwise ordinary
/// EPS figures.
fn rendered_png(bytes: &[u8]) -> Option<&[u8]> {
    let signature = b"\x89PNG\r\n\x1a\n";
    let start = bytes
        .windows(signature.len())
        .position(|window| window == signature)?;
    let mut at = start.checked_add(signature.len())?;
    while at.checked_add(12)? <= bytes.len() {
        let length = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(at + 4..at + 8)?;
        let next = at.checked_add(12)?.checked_add(length)?;
        if next > bytes.len() {
            return None;
        }
        if kind == b"IEND" {
            return bytes.get(start..next);
        }
        at = next;
    }
    None
}

fn rasterize_asset(path: &str, page: u32, bytes: &[u8]) -> Option<Vec<u8>> {
    let page = page.max(1).to_string();
    let preserve_alpha = png_has_alpha(bytes);
    let mut command = std::process::Command::new("mutool");
    command.args(["draw", "-q"]);
    if preserve_alpha {
        // PAM exposes RGBA as four raw samples. The dependency-free PDF driver
        // splits those into DeviceRGB plus an `/SMask`, preserving the source
        // alpha over arbitrary TeX content.
        command.args(["-F", "pam", "-c", "rgba"]);
    } else {
        command.args(["-F", "png", "-c", "rgb"]);
    }
    command.args(["-o", "-"]);
    if let Some((width, height, ..)) = png_header(bytes) {
        // Rendering an already-raster PNG at an arbitrary DPI resamples it
        // before it reaches the PDF. Preserve its native grid instead; MuPDF
        // then merely flattens unsupported alpha/indexed colour into the RGB
        // representation the dependency-free driver accepts.
        command.args(["-w", &width.to_string(), "-h", &height.to_string()]);
    } else {
        // PDF pages take the vector import above. Two samples per 72dpi output
        // pixel is the bounded fallback for other vector formats.
        command.args(["-r", "144"]);
    }
    command.args([path, &page]);
    let mupdf = bounded_asset_output(&mut command);
    if let Some(output) = mupdf {
        if output.status.success() {
            if preserve_alpha && output.stdout.starts_with(b"P7\n") {
                return Some(output.stdout);
            }
            if !preserve_alpha && let Some(png) = rendered_png(&output.stdout) {
                return Some(png.to_vec());
            }
        }
    }
    let output = bounded_asset_output(
        std::process::Command::new("gs")
        .args([
            "-q",
            "-dSAFER",
            "-dBATCH",
            "-dNOPAUSE",
            "-dEPSCrop",
            "-sDEVICE=png16m",
            "-r144",
            "-sOutputFile=-",
        ])
        .arg(format!("-dFirstPage={page}"))
        .arg(format!("-dLastPage={page}"))
        .arg(path),
    )?;
    output
        .status
        .success()
        .then(|| rendered_png(&output.stdout).map(Vec::from))
        .flatten()
}

#[cfg(test)]
mod pdf_tests {
    use super::{
        eps_graphics_bbox, lh_bitmap_encoding_file, pdf_box_extents_within,
        physical_classic_font_name, rendered_png, raster_natural_bbox,
        reusable_font_resolution, selected_asset_bbox, split_letterspace_font,
    };

    #[test]
    fn repaired_pdf_boxes_distinguish_rounding_from_preserved_geometry() {
        let source = [0.0, 0.0, 601.49862, 264.6866];
        assert!(pdf_box_extents_within(
            source,
            [48.86298, 304.7017, 650.3616, 569.3883],
            0.0001
        ));
        assert!(!pdf_box_extents_within(
            source,
            [0.0, 0.0, 601.5, 264.69],
            0.0001
        ));
    }

    #[test]
    fn selected_pdf_page_box_overrides_the_source_files_first_page_box() {
        let first_page = Some([10.0, 20.0, 110.0, 70.0]);
        let selected_page = Some([30.0, 40.0, 230.0, 140.0]);
        assert_eq!(
            selected_asset_bbox("pdf", first_page, selected_page),
            selected_page
        );
        assert_eq!(
            selected_asset_bbox("eps", first_page, selected_page),
            first_page
        );
    }

    #[test]
    fn eps_graphics_box_uses_numeric_bounding_box_including_atend_trailer() {
        assert_eq!(
            eps_graphics_bbox(
                b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: (atend)\n%%HiResBoundingBox: 1.2 2.3 99.4 88.5\n%%BoundingBox: 1 2 100 89\n"
            ),
            Some([1.0, 2.0, 100.0, 89.0])
        );
    }

    #[test]
    fn metapost_asset_box_retains_its_postscript_origin() {
        let source = b"%!PS\n%%BoundingBox: -11 -93 69 8\n%%HiResBoundingBox: -10.8 -92.4 68.3 7.3\n";
        assert_eq!(
            raster_natural_bbox("diagram.mps", source),
            Some([-11.0, -93.0, 69.0, 8.0])
        );
        assert_eq!(
            raster_natural_bbox("diagram.eps", source),
            Some([0.0, 0.0, 79.1, 99.7])
        );
    }

    #[test]
    fn one_physical_font_resolution_serves_multiple_dvi_font_numbers() {
        let fonts = std::collections::BTreeMap::from([(("cmr10".to_string(), 655_360), 7)]);
        assert_eq!(reusable_font_resolution(&fonts, "cmr10", 655_360), Some(7));
        assert_eq!(reusable_font_resolution(&fonts, "cmr10", 655_360), Some(7));
        assert_eq!(fonts.len(), 1);
    }

    #[test]
    fn renderer_warning_text_is_removed_around_one_complete_png() {
        let mut output = b"%%Warning: EPS adjusted its page device\n\x89PNG\r\n\x1a\n".to_vec();
        output.extend_from_slice(&[0, 0, 0, 0]);
        output.extend_from_slice(b"IEND");
        output.extend_from_slice(&[0, 0, 0, 0]);
        output.extend_from_slice(b"trailing diagnostic\n");
        let png = rendered_png(&output).expect("embedded PNG");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.ends_with(&[0, 0, 0, 0]));
        assert!(!png.windows(8).any(|window| window == b"trailing"));
    }

    #[test]
    fn pdftex_letterspace_names_resolve_to_the_physical_face() {
        assert_eq!(split_letterspace_font("pplrc9d+50ls"), Some(("pplrc9d", 50)));
        assert_eq!(split_letterspace_font("cmr100ls"), Some(("cmr10", 0)));
        assert_eq!(split_letterspace_font("cmr10-20ls"), Some(("cmr10", -20)));
        assert_eq!(physical_classic_font_name("cmr10+100ls-20ls"), "cmr10");
        assert_eq!(physical_classic_font_name("ls"), "ls");
    }

    #[test]
    fn uncommon_lh_bitmap_sizes_keep_their_cyrillic_encoding() {
        assert_eq!(lh_bitmap_encoding_file("lati1728"), Some("cm-super-t2a.enc"));
        assert_eq!(lh_bitmap_encoding_file("lbrm1095"), Some("cm-super-t2b.enc"));
        assert_eq!(lh_bitmap_encoding_file("lctt2074"), Some("cm-super-t2c.enc"));
        assert_eq!(lh_bitmap_encoding_file("lasy10"), None);
        assert_eq!(lh_bitmap_encoding_file("lato"), None);
    }
}
