/// Write the finished pages out as DVI. Mirrors what `aitex-cli` does for the
/// layered engine -- same `shipout::ship_out`, same `DviWriter`.
/// The DVI font numbers *and* the metrics behind them. Leaders are the only
/// thing in the shipper that needs a width: their copies sit on a grid measured
/// from the enclosing box's left edge, so it has to know how far the list has
/// come (`tex.web` §627).
struct ShipFonts<'a> {
    nums: &'a [usize],
    metrics: aitex_engine::fastset::FontTableRef<'a>,
}

impl aitex_engine::shipout::FontNumbers for ShipFonts<'_> {
    fn dvi_font(&self, engine_font: usize) -> Option<usize> {
        self.nums.get(engine_font).copied()
    }
    fn char_width(&self, engine_font: usize, code: u32) -> aitex_engine::scaled::Scaled {
        use aitex_engine::node::CharMetrics;
        self.metrics.width(engine_font, code)
    }
}

/// Ships pages to DVI as the engine finishes them, so a long document costs one
/// page of memory rather than all of them.
///
/// Fonts are defined the first time a page uses one — DVI allows a `fnt_def`
/// anywhere before its use and repeats them all in the postamble, which is what
/// lets this work without knowing the whole document up front.
#[derive(Clone)]
struct Shipper {
    dvi: DviWriter,
    comment: String,
    nums: Vec<usize>,
    defined: Vec<bool>,
    pages: usize,
    max_v: Scaled,
    max_h: Scaled,
    page_rotations: Vec<i16>,
    /// Canonical positioned events captured by the exact same shipout walk.
    /// PDF consumes these objects directly; the simultaneously written DVI is
    /// retained during migration as an independent differential oracle.
    page_events: Vec<aitex_page::Page>,
    capture_page_events: bool,
    /// During the differential migration PDF jobs retain private DVI bytes in
    /// memory as an oracle, but never round-trip them through the output path.
    finished_dvi: Option<Vec<u8>>,
    elapsed: std::time::Duration,
    measure: bool,
}

/// Remove a stale ordinary artifact after DVI finalization fails, but never
/// unlink the explicitly supported null sink. This policy is deliberately
/// centralized because both writers discover some DVI limits only at finish.
fn is_output_sink(out_path: &str) -> bool {
    if out_path == "/dev/null" {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt as _;
        if std::fs::metadata(out_path).is_ok_and(|metadata| metadata.file_type().is_char_device()) {
            return true;
        }
    }
    false
}

fn may_remove_failed_dvi_artifact(out_path: &str) -> bool {
    !is_output_sink(out_path) && !filesystem_watch_mode()
}

fn remove_failed_dvi_artifact(out_path: &str) {
    if may_remove_failed_dvi_artifact(out_path) {
        let _ = std::fs::remove_file(out_path);
    }
}

/// Native watch is a long-lived publisher: a viewer must see either the last
/// complete artifact or the next complete artifact, never a partially written
/// file. Ordinary one-shot builds retain their direct-write fast path.
fn filesystem_watch_mode() -> bool {
    let mut arguments = std::env::args().skip(1);
    arguments.next().as_deref() == Some("watch") && !arguments.any(|argument| argument == "--stdin")
}

fn watch_staging_path(path: &std::path::Path) -> std::path::PathBuf {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aitex-output");
    parent.join(format!(".{name}.aitex-watch-{}.pending", std::process::id()))
}

fn read_output_artifact(path: &str) -> std::io::Result<Vec<u8>> {
    if filesystem_watch_mode() {
        let staged = watch_staging_path(std::path::Path::new(path));
        if staged.is_file() {
            return std::fs::read(staged);
        }
    }
    std::fs::read(path)
}

fn promote_watch_output(path: &str) -> std::io::Result<()> {
    let path = std::path::Path::new(path);
    let staged = watch_staging_path(path);
    if staged.is_file() {
        std::fs::rename(staged, path)?;
    }
    Ok(())
}

fn discard_watch_output(path: &str) {
    let _ = std::fs::remove_file(watch_staging_path(std::path::Path::new(path)));
}

fn publish_output_artifact(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    if !filesystem_watch_mode() || is_output_sink(path) {
        return std::fs::write(path, bytes);
    }
    static NEXT_TEMPORARY: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    use std::io::Write as _;
    let requested = std::path::Path::new(path);
    let path = watch_staging_path(requested);
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = requested
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aitex-output");
    let suffix = NEXT_TEMPORARY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{name}.aitex-watch-{}-{suffix}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

impl Shipper {
    fn new(input: &str, measure: bool, capture_page_events: bool) -> Self {
        Self {
            dvi: DviWriter::new(),
            comment: format!(" aiTeX fast-engine output from {input} "),
            nums: Vec::new(),
            defined: Vec::new(),
            pages: 0,
            max_v: Scaled::ZERO,
            max_h: Scaled::ZERO,
            page_rotations: Vec::new(),
            page_events: Vec::new(),
            capture_page_events,
            finished_dvi: None,
            elapsed: std::time::Duration::ZERO,
            measure,
        }
    }

    fn retained_bytes(&self) -> usize {
        self.dvi.output_len()
            + self.comment.len()
            + self.nums.len() * core::mem::size_of::<usize>()
            + self.defined.len()
            + self.page_rotations.len() * core::mem::size_of::<i16>()
            + self.finished_dvi.as_ref().map_or(0, Vec::len)
            + self
                .page_events
                .iter()
                .map(|page| page.items.len() * core::mem::size_of::<aitex_page::Item>())
                .sum::<usize>()
    }

    fn checkpoint_identity(&self) -> [u8; 32] {
        let mut identity = Vec::new();
        key_field(&mut identity, "dvi", self.dvi.committed_bytes());
        key_field(&mut identity, "comment", self.comment.as_bytes());
        key_field(&mut identity, "pages", &self.pages.to_le_bytes());
        key_field(&mut identity, "max-v", &self.max_v.0.to_le_bytes());
        key_field(&mut identity, "max-h", &self.max_h.0.to_le_bytes());
        for number in &self.nums {
            key_field(&mut identity, "font-number", &number.to_le_bytes());
        }
        let defined = self.defined.iter().map(|value| u8::from(*value)).collect::<Vec<_>>();
        key_field(&mut identity, "defined", &defined);
        for rotation in &self.page_rotations {
            key_field(&mut identity, "rotation", &rotation.to_le_bytes());
        }
        sha256(&identity)
    }

    fn ship(
        &mut self,
        page: &aitex_engine::node::TeXBox,
        ts: &aitex_engine::fastset::Typeset,
        meta: ShipoutMeta,
    ) {
        let started = self.measure.then(Instant::now);
        // TeX does not freeze `\mag` or open the DVI until the first page is
        // shipped. The engine validates it at that boundary and passes the
        // frozen value here for both copies in the DVI header.
        if self.pages == 0 {
            self.dvi.preamble(&self.comment, meta.magnification);
        }
        // Any font this page uses and the file has not defined yet. The walk is
        // the price of not knowing the whole document up front; defining every
        // *loaded* font instead would be cheaper and would put font definitions
        // in the file for fonts nothing draws in, which TeX does not do.
        let mut used = vec![false; ts.fonts.len()];
        mark_fonts(&page.list, &mut used);
        if self.defined.len() < ts.fonts.len() {
            self.defined.resize(ts.fonts.len(), false);
            self.nums.resize(ts.fonts.len(), 0);
        }
        for (i, u) in used.iter().enumerate() {
            if !u || self.defined[i] {
                continue;
            }
            if let Some(tf) = ts.fonts.get(i) {
                self.nums[i] = self.dvi.define_font(FontDef {
                    checksum: tf.checksum,
                    at_size: tf.at_size,
                    design_size: tf.design_size,
                    name: ts.font_names.get(i).cloned().unwrap_or_default(),
                });
                self.defined[i] = true;
            }
        }
        self.pages += 1;
        self.page_rotations.push(meta.pdf_page_rotation);
        self.dvi.begin_page(meta.counts);
        // A public DVI must remain byte/semantic-compatible with TeX, whose
        // format has no page-size fields. The direct-PDF path, however, uses a
        // private in-memory DVI as its migration boundary. Carry pdfTeX's
        // per-page dimensions across that boundary as a private special
        // understood by our PDF driver. Keep it distinct from dvips'
        // document-state `papersize` special: a class may emit A4 once, and a
        // private Letter fallback on page two must not override that choice.
        // Per-page private metadata still lets `standalone` crop every page.
        let private_page_size = self.capture_page_events.then(|| {
            let (width, height) = meta.pdf_page_size(page);
            format!(
                "aitex:page-size {}pt,{}pt",
                aitex_engine::scaled::print_scaled(width),
                aitex_engine::scaled::print_scaled(height)
            )
        });
        if let Some(special) = private_page_size.as_deref() {
            self.dvi.special(special);
        }
        if self.capture_page_events && meta.pdf_page_size_explicit {
            self.dvi.special("aitex:page-size-explicit");
        }
        let private_page_origin = self.capture_page_events.then(|| {
            format!(
                "aitex:page-origin {}pt,{}pt",
                aitex_engine::scaled::print_scaled(meta.pdf_h_origin),
                aitex_engine::scaled::print_scaled(meta.pdf_v_origin)
            )
        });
        if let Some(special) = private_page_origin.as_deref() {
            self.dvi.special(special);
        }
        let private_page_resources = (self.capture_page_events
            && !meta.pdf_page_resources.is_empty())
        .then(|| format!("pdf:put @resources <<{}>>", meta.pdf_page_resources));
        if let Some(special) = private_page_resources.as_deref() {
            self.dvi.special(special);
        }
        let private_page_attributes = (self.capture_page_events
            && !meta.pdf_page_attributes.is_empty())
        .then(|| format!("pdf:put @thispage <<{}>>", meta.pdf_page_attributes));
        if let Some(special) = private_page_attributes.as_deref() {
            self.dvi.special(special);
        }
        // DVI's reference point is *already* one inch in from the paper's
        // top-left corner, and `tex.web` §640 ships a page from (0,0) relative
        // to it. Adding 72pt here moved every page an inch down and right of
        // where pdftex puts it — invisible in a text dump, an inch wrong on
        // paper. `\hoffset`/`\voffset` are the only displacements, and both
        // default to zero.
        let fonts = ShipFonts {
            nums: &self.nums,
            metrics: aitex_engine::fastset::FontTableRef(&ts.fonts),
        };
        if self.capture_page_events {
            let mut page_items = Vec::new();
            let _ = aitex_engine::shipout::ship_out_box_with_page_items(
                &mut self.dvi,
                page,
                meta.h_offset,
                meta.v_offset,
                &fonts,
                &mut page_items,
            );
            let mut private_items = Vec::with_capacity(5);
            if let Some(special) = private_page_size {
                private_items.push(aitex_page::Item::Special {
                    x: aitex_page::Sp(0),
                    y: aitex_page::Sp(0),
                    bytes: special.into_bytes(),
                });
            }
            if meta.pdf_page_size_explicit {
                private_items.push(aitex_page::Item::Special {
                    x: aitex_page::Sp(0),
                    y: aitex_page::Sp(0),
                    bytes: b"aitex:page-size-explicit".to_vec(),
                });
            }
            if let Some(special) = private_page_origin {
                private_items.push(aitex_page::Item::Special {
                    x: aitex_page::Sp(0),
                    y: aitex_page::Sp(0),
                    bytes: special.into_bytes(),
                });
            }
            if let Some(special) = private_page_resources {
                private_items.push(aitex_page::Item::Special {
                    x: aitex_page::Sp(0),
                    y: aitex_page::Sp(0),
                    bytes: special.into_bytes(),
                });
            }
            if let Some(special) = private_page_attributes {
                private_items.push(aitex_page::Item::Special {
                    x: aitex_page::Sp(0),
                    y: aitex_page::Sp(0),
                    bytes: special.into_bytes(),
                });
            }
            private_items.append(&mut page_items);
            self.page_events.push(aitex_page::Page {
                width_millibp: 0,
                height_millibp: 0,
                counts: meta.counts,
                items: private_items,
            });
        } else {
            let _ = ship_out_box(
                &mut self.dvi,
                page,
                meta.h_offset,
                meta.v_offset,
                &fonts,
            );
        }
        self.dvi.end_page();
        let (max_v, max_h) = meta.dvi_extent(page);
        self.max_v = self.max_v.max(max_v);
        self.max_h = self.max_h.max(max_h);
        if let Some(started) = started {
            self.elapsed += started.elapsed();
        }
    }

    /// Move the captured page stream into the driver's document wrapper.
    /// Font definitions preserve the exact DVI numbering chosen during the
    /// same shipout, so no item remap or page-sized allocation is needed.
    fn take_page_document(
        &mut self,
        ts: &aitex_engine::fastset::Typeset,
        magnification: u32,
    ) -> aitex_driver::dvi::Document {
        let mut fonts = std::collections::BTreeMap::new();
        for (engine_font, defined) in self.defined.iter().copied().enumerate() {
            if !defined {
                continue;
            }
            let Some(font) = ts.fonts.get(engine_font) else {
                continue;
            };
            let Some(&number) = self.nums.get(engine_font) else {
                continue;
            };
            fonts.insert(
                number as u32,
                aitex_driver::dvi::FontDef {
                    name: ts.font_names.get(engine_font).cloned().unwrap_or_default(),
                    at: i64::from(font.at_size.0),
                    design: i64::from(font.design_size.0),
                    checksum: font.checksum,
                },
            );
        }
        aitex_driver::dvi::Document {
            pages: core::mem::take(&mut self.page_events),
            fonts,
            num: 25_400_000,
            den: 473_628_672,
            mag: magnification,
        }
    }

    fn finish(&mut self, out_path: &str) -> bool {
        // §642 never opens a DVI file until the first page ships. In
        // particular, an INITEX format run ends this way rather than writing a
        // syntactically valid but semantically spurious zero-page DVI.
        if self.pages == 0 {
            let m = String::from("No pages of output.");
            log_tail_push(m);
            return false;
        }
        self.dvi.note_extent(self.max_v, self.max_h);
        let dvi = core::mem::replace(&mut self.dvi, DviWriter::new());
        let bytes = match dvi.finish() {
            Ok(bytes) => bytes,
            Err(error) => {
                let m = format!("aitex: cannot write {out_path}: {error}");
                eprintln!("{m}");
                log_tail_push(m);
                // Do not leave a previous successful artifact under the name
                // of this failed run. The writer has not exposed its partial
                // bytes, but an older DVI/PDF would be just as misleading.
                remove_failed_dvi_artifact(out_path);
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                return false;
            }
        };
        if out_path.ends_with(".pdf") {
            let byte_len = bytes.len();
            self.finished_dvi = Some(bytes);
            let m = format!(
                "Output prepared for {out_path} ({} page{}, {byte_len} private DVI bytes).",
                self.pages,
                if self.pages == 1 { "" } else { "s" },
            );
            cli_detail!("{m}");
            log_tail_push(m);
            return true;
        }
        match publish_output_artifact(out_path, &bytes) {
            // `tex.web` §642's own wording, which every TeX transcript ends
            // with: `Output written on trip.dvi (16 pages, 2920 bytes).`
            Ok(()) => {
                let m = format!(
                    "Output written on {out_path} ({} page{}, {} bytes).",
                    self.pages,
                    if self.pages == 1 { "" } else { "s" },
                    bytes.len()
                );
                cli_detail!("{m}");
                log_tail_push(m);
                true
            }
            Err(e) => {
                eprintln!("cannot write {out_path}: {e}");
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                false
            }
        }
    }

    fn take_finished_dvi(&mut self) -> Option<Vec<u8>> {
        self.finished_dvi.take()
    }
}

/// Which fonts a shipped page draws in.
fn mark_fonts(list: &[aitex_engine::node::Node], used: &mut [bool]) {
    use aitex_engine::node::Node;
    for n in list {
        match n {
            Node::Char { font, .. } => {
                if let Some(u) = used.get_mut(*font) {
                    *u = true;
                }
            }
            Node::Box(b) => mark_fonts(&b.list, used),
            Node::Leaders { boxed, .. } => mark_fonts(&boxed.list, used),
            _ => {}
        }
    }
}

fn write_dvi(f: &Fast, out_path: &str, input: &str) {
    use aitex_engine::dvi::{DviWriter, FontDef};
    use aitex_engine::scaled::Scaled;
    use aitex_engine::shipout::ship_out_box;

    if f.ts.pages.is_empty() {
        let m = String::from("No pages of output.");
        log_tail_push(m);
        return;
    }

    let mut dvi = DviWriter::new();
    let page_meta = f.retained_page_metadata();
    assert_eq!(
        page_meta.len(),
        f.ts.pages.len(),
        "every retained page must retain its shipout metadata"
    );
    dvi.preamble(
        &format!(" aiTeX fast-engine output from {input} "),
        page_meta[0].magnification,
    );
    // Define only the fonts the pages use: plain.tex loads sixteen and a
    // one-paragraph document sets text in one.
    let used = f.ts.used_fonts();
    let mut font_nums: Vec<usize> = vec![0; f.ts.fonts.len()];
    for (i, tf) in f.ts.fonts.iter().enumerate() {
        if !used.get(i).copied().unwrap_or(false) {
            continue;
        }
        font_nums[i] = dvi.define_font(FontDef {
            checksum: tf.checksum,
            at_size: tf.at_size,
            design_size: tf.design_size,
            name: f.ts.font_names.get(i).cloned().unwrap_or_default(),
        });
    }
    // DVI's reference point is *already* one inch in from the paper's top-left
    // corner, and `tex.web` §640 ships a page from (0,0) relative to it. Adding
    // 72pt here moved every page an inch down and right of where pdftex puts it
    // — invisible in a text dump, an inch wrong on paper. `\hoffset`/`\voffset`
    // are the only displacements, and both default to zero.
    let mut max_v = Scaled::ZERO;
    let mut max_h = Scaled::ZERO;
    for (page, meta) in f.ts.pages.iter().zip(page_meta) {
        dvi.begin_page(meta.counts);
        // `\shipout` takes a *box*, and its kind decides how it is drawn.
        let _ = ship_out_box(
            &mut dvi,
            page,
            meta.h_offset,
            meta.v_offset,
            &ShipFonts {
                nums: &font_nums,
                metrics: aitex_engine::fastset::FontTableRef(&f.ts.fonts),
            },
        );
        dvi.end_page();
        let (page_v, page_h) = meta.dvi_extent(page);
        max_v = max_v.max(page_v);
        max_h = max_h.max(page_h);
    }
    dvi.note_extent(max_v, max_h);
    let bytes = match dvi.finish() {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("aitex: cannot write {out_path}: {error}");
            remove_failed_dvi_artifact(out_path);
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    match publish_output_artifact(out_path, &bytes) {
        Ok(()) => {
            let m = format!(
                "Output written on {out_path} ({} page{}, {} bytes).",
                f.ts.pages.len(),
                if f.ts.pages.len() == 1 { "" } else { "s" },
                bytes.len()
            );
            cli_detail!("{m}");
            log_tail_push(m);
        }
        Err(e) => {
            eprintln!("cannot write {out_path}: {e}");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[cfg(all(test, unix))]
mod dvi_cleanup_tests {
    use super::{may_remove_failed_dvi_artifact, remove_failed_dvi_artifact};

    #[test]
    fn failed_dvi_cleanup_never_unlinks_the_null_sink() {
        assert!(!may_remove_failed_dvi_artifact("/dev/null"));
        assert!(!may_remove_failed_dvi_artifact("/dev/./null"));
    }

    #[test]
    fn failed_dvi_cleanup_removes_an_ordinary_stale_artifact() {
        let path = std::env::temp_dir().join(format!(
            "aitex-dvi-cleanup-policy-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"stale DVI").unwrap();
        let path_text = path.to_str().unwrap();
        assert!(may_remove_failed_dvi_artifact(path_text));
        remove_failed_dvi_artifact(path_text);
        assert!(!path.exists());
    }
}
