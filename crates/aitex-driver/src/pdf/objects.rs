struct Objects {
    header: Vec<u8>,
    objects: Vec<Option<StoredObject>>,
    options: PdfOptions,
}

struct StoredObject {
    contents: Vec<u8>,
    is_stream: bool,
}

#[derive(Clone, Copy)]
enum XrefLocation {
    Free,
    Direct(usize),
    Compressed { stream: usize, index: usize },
}

const OBJECT_STREAM_MAX_OBJECTS: usize = 100;
const OBJECT_STREAM_MAX_BYTES: usize = 64 * 1024;
const OBJECT_STREAM_MAX_ITEM_BYTES: usize = 16 * 1024;

const MIN_BYTES_PER_COMPRESSION_WORKER: usize = 64 * 1024;

struct CompressionPermitState {
    available: usize,
    next_ticket: u64,
    serving_ticket: u64,
}

/// A process-wide CPU budget for PDF compression.
///
/// Permits are granted atomically and in FIFO order. Thus two documents which
/// each request the default four workers take turns instead of each receiving
/// a fragmented subset. A future document scheduler can deliberately request
/// one or two workers per document to admit corresponding concurrency.
struct CompressionPermitPool {
    capacity: usize,
    state: std::sync::Mutex<CompressionPermitState>,
    ready: std::sync::Condvar,
}

impl CompressionPermitPool {
    fn new() -> Self {
        let capacity = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1)
            .min(DEFAULT_PDF_COMPRESSION_WORKERS);
        Self {
            capacity,
            state: std::sync::Mutex::new(CompressionPermitState {
                available: capacity,
                next_ticket: 0,
                serving_ticket: 0,
            }),
            ready: std::sync::Condvar::new(),
        }
    }

    fn acquire(&self, requested: usize) -> CompressionPermitLease<'_> {
        let requested = requested.max(1).min(self.capacity);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        while ticket != state.serving_ticket || state.available < requested {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        state.available -= requested;
        state.serving_ticket = state.serving_ticket.wrapping_add(1);
        self.ready.notify_all();
        CompressionPermitLease {
            pool: self,
            count: requested,
        }
    }
}

struct CompressionPermitLease<'a> {
    pool: &'a CompressionPermitPool,
    count: usize,
}

impl Drop for CompressionPermitLease<'_> {
    fn drop(&mut self) {
        let mut state = self
            .pool
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.available += self.count;
        self.pool.ready.notify_all();
    }
}

fn compression_permits() -> &'static CompressionPermitPool {
    static PERMITS: std::sync::OnceLock<CompressionPermitPool> = std::sync::OnceLock::new();
    PERMITS.get_or_init(CompressionPermitPool::new)
}

fn stream_object_with_options(
    options: PdfOptions,
    dictionary: &[u8],
    content: &[u8],
) -> Vec<u8> {
    use std::io::Write;

    let compressed = if options.compress_streams {
        let mut encoder = flate2::write::ZlibEncoder::new(
            Vec::new(),
            flate2::Compression::new(STREAM_COMPRESSION_LEVEL),
        );
        encoder
            .write_all(content)
            .ok()
            .and_then(|()| encoder.finish().ok())
    } else {
        None
    };
    let raw_attributes = format!(" /Length {}", content.len());
    let compressed_attributes = compressed
        .as_ref()
        .map(|bytes| format!(" /Filter /FlateDecode /Length {}", bytes.len()));
    let use_compressed = compressed
        .as_ref()
        .zip(compressed_attributes.as_ref())
        .is_some_and(|(bytes, attributes)| {
            bytes.len().saturating_add(attributes.len())
                < content.len().saturating_add(raw_attributes.len())
        });
    let (attributes, bytes) = if use_compressed {
        (
            compressed_attributes.as_deref().unwrap_or_default(),
            compressed.as_deref().unwrap_or_default(),
        )
    } else {
        (raw_attributes.as_str(), content)
    };
    let mut object = Vec::with_capacity(
        dictionary
            .len()
            .saturating_add(attributes.len())
            .saturating_add(bytes.len())
            .saturating_add(24),
    );
    object.extend_from_slice(dictionary);
    object.extend_from_slice(attributes.as_bytes());
    object.extend_from_slice(b" >>\nstream\n");
    object.extend_from_slice(bytes);
    object.extend_from_slice(b"\nendstream");
    object
}

impl Objects {
    fn new() -> Self {
        Self::with_options(PdfOptions::default())
    }
    fn with_options(options: PdfOptions) -> Self {
        Self::with_version(options, "1.4")
    }
    fn with_version(options: PdfOptions, version: &str) -> Self {
        let version = if options.compact_objects
            && version
                .split_once('.')
                .and_then(|(major, minor)| Some((major.parse::<u8>().ok()?, minor.parse::<u8>().ok()?)))
                .is_some_and(|version| version < (1, 5))
        {
            "1.5"
        } else {
            version
        };
        let mut header = format!("%PDF-{version}\n%").into_bytes();
        header.extend_from_slice(b"\xE2\xE3\xCF\xD3\n");
        Self {
            header,
            objects: Vec::new(),
            options,
        }
    }
    /// Reserve an object number without writing it yet — a page has to name its
    /// parent, and the parent has to list its pages.
    fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }
    fn write(&mut self, id: usize, contents: &[u8]) {
        self.write_owned(id, contents.to_vec());
    }
    fn write_owned(&mut self, id: usize, contents: Vec<u8>) {
        let is_stream = contents.windows(b"stream".len()).any(|bytes| bytes == b"stream");
        self.objects[id - 1] = Some(StoredObject {
            contents,
            is_stream,
        });
    }
    fn write_interned(
        &mut self,
        interned: &mut std::collections::BTreeMap<Vec<u8>, usize>,
        contents: Vec<u8>,
    ) -> usize {
        if let Some(id) = interned.get(&contents) {
            return *id;
        }
        let id = self.reserve();
        self.write(id, &contents);
        interned.insert(contents, id);
        id
    }
    fn write_interned_stream(
        &mut self,
        interned: &mut std::collections::BTreeMap<Vec<u8>, usize>,
        dictionary: &[u8],
        content: &[u8],
    ) -> usize {
        let object = self.stream_object(dictionary, content);
        self.write_interned(interned, object)
    }
    /// Construct an ordinary stream, applying deterministic zlib compression
    /// only when the complete encoded representation is smaller.
    ///
    /// `dictionary` begins with `<<` and deliberately omits `/Length` and the
    /// closing `>>`. Streams which already carry a filter use
    /// `write_encoded_form_stream` or an explicitly assembled object instead.
    fn stream_object(&self, dictionary: &[u8], content: &[u8]) -> Vec<u8> {
        let _permit = self
            .options
            .compress_streams
            .then(|| compression_permits().acquire(1));
        stream_object_with_options(self.options, dictionary, content)
    }
    /// Construct a batch of independent ordinary streams. Results retain input
    /// order regardless of which worker completes first.
    fn stream_objects(&self, dictionary: &[u8], contents: &[&[u8]]) -> Vec<Vec<u8>> {
        if !self.options.compress_streams || contents.len() < 2 {
            return contents
                .iter()
                .map(|content| self.stream_object(dictionary, content))
                .collect();
        }

        let total_bytes = contents
            .iter()
            .fold(0usize, |total, content| total.saturating_add(content.len()));
        let useful_workers = total_bytes
            .div_ceil(MIN_BYTES_PER_COMPRESSION_WORKER)
            .max(1);
        let requested = self
            .options
            .compression_workers
            .max(1)
            .min(contents.len())
            .min(useful_workers);
        let permit = compression_permits().acquire(requested);
        if permit.count == 1 {
            return contents
                .iter()
                .map(|content| stream_object_with_options(self.options, dictionary, content))
                .collect();
        }

        let next = std::sync::atomic::AtomicUsize::new(0);
        let results = (0..contents.len())
            .map(|_| std::sync::OnceLock::<Vec<u8>>::new())
            .collect::<Vec<_>>();
        std::thread::scope(|scope| {
            for _ in 0..permit.count {
                scope.spawn(|| loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(content) = contents.get(index) else {
                        break;
                    };
                    let object = stream_object_with_options(self.options, dictionary, content);
                    results[index]
                        .set(object)
                        .unwrap_or_else(|_| unreachable!("one result per stream"));
                });
            }
        });
        results
            .into_iter()
            .map(|result| result.into_inner().expect("compression worker result"))
            .collect()
    }
    fn write_stream(&mut self, id: usize, dictionary: &[u8], content: &[u8]) {
        let object = self.stream_object(dictionary, content);
        self.write_owned(id, object);
    }
    fn encoded_stream_object(
        &self,
        dictionary: &[u8],
        stream_attributes: &[u8],
        content: &[u8],
    ) -> Vec<u8> {
        let mut object = Vec::with_capacity(
            dictionary
                .len()
                .saturating_add(stream_attributes.len())
                .saturating_add(content.len())
                .saturating_add(32),
        );
        object.extend_from_slice(dictionary);
        object.extend_from_slice(stream_attributes);
        object.extend_from_slice(format!(" /Length {} >>\nstream\n", content.len()).as_bytes());
        object.extend_from_slice(content);
        object.extend_from_slice(b"\nendstream");
        object
    }
    fn write_form_stream(&mut self, id: usize, dictionary: &[u8], chunks: &[&[u8]]) {
        let length = 4usize.saturating_add(chunks.iter().map(|chunk| chunk.len() + 1).sum());
        let mut content = Vec::with_capacity(length);
        content.extend_from_slice(b"q\n");
        for chunk in chunks {
            content.extend_from_slice(chunk);
            content.push(b'\n');
        }
        content.extend_from_slice(b"Q\n");
        self.write_stream(id, dictionary, &content);
    }
    fn write_encoded_form_stream(
        &mut self,
        id: usize,
        dictionary: &[u8],
        stream_attributes: &[u8],
        content: &[u8],
    ) {
        let object = self.encoded_stream_object(dictionary, stream_attributes, content);
        self.write_owned(id, object);
    }
    fn append_indirect(body: &mut Vec<u8>, id: usize, contents: &[u8]) -> usize {
        let offset = body.len();
        body.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        body.extend_from_slice(contents);
        body.extend_from_slice(b"\nendobj\n");
        offset
    }

    #[cfg(test)]
    fn debug_body(&self) -> Vec<u8> {
        let mut body = self.header.clone();
        for (index, object) in self.objects.iter().enumerate() {
            if let Some(object) = object {
                Self::append_indirect(&mut body, index + 1, &object.contents);
            }
        }
        body
    }

    fn finish_classic(self, root: usize, info: Option<usize>) -> Vec<u8> {
        let mut body = self.header;
        let mut offsets = Vec::with_capacity(self.objects.len());
        for (index, object) in self.objects.iter().enumerate() {
            offsets.push(object.as_ref().map_or(0, |object| {
                Self::append_indirect(&mut body, index + 1, &object.contents)
            }));
        }
        let start = body.len();
        let n = offsets.len() + 1;
        body.extend_from_slice(format!("xref\n0 {n}\n0000000000 65535 f \n").as_bytes());
        for (object, offset) in self.objects.iter().zip(offsets) {
            if object.is_some() {
                body.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            } else {
                body.extend_from_slice(b"0000000000 00000 f \n");
            }
        }
        let info = info
            .map(|id| format!(" /Info {id} 0 R"))
            .unwrap_or_default();
        body.extend_from_slice(
            format!("trailer\n<< /Size {n} /Root {root} 0 R{info} >>\nstartxref\n{start}\n%%EOF\n")
                .as_bytes(),
        );
        body
    }

    fn finish_compact(self, root: usize, info: Option<usize>) -> Vec<u8> {
        let mut groups = Vec::<Vec<usize>>::new();
        let mut group = Vec::new();
        let mut group_bytes = 0usize;
        for (index, object) in self.objects.iter().enumerate() {
            let Some(object) = object else {
                continue;
            };
            if object.is_stream || object.contents.len() > OBJECT_STREAM_MAX_ITEM_BYTES {
                continue;
            }
            if !group.is_empty()
                && (group.len() >= OBJECT_STREAM_MAX_OBJECTS
                    || group_bytes.saturating_add(object.contents.len())
                        > OBJECT_STREAM_MAX_BYTES)
            {
                groups.push(std::mem::take(&mut group));
                group_bytes = 0;
            }
            group.push(index + 1);
            group_bytes = group_bytes.saturating_add(object.contents.len());
        }
        if !group.is_empty() {
            groups.push(group);
        }

        let original_count = self.objects.len();
        let mut packed = vec![false; original_count + 1];
        for id in groups.iter().flatten().copied() {
            packed[id] = true;
        }
        let xref_id = original_count + groups.len() + 1;
        let size = xref_id + 1;
        let mut locations = vec![XrefLocation::Free; size];
        let mut body = self.header;
        for (index, object) in self.objects.iter().enumerate() {
            let id = index + 1;
            let Some(object) = object else {
                continue;
            };
            if packed[id] {
                continue;
            }
            locations[id] = XrefLocation::Direct(Self::append_indirect(
                &mut body,
                id,
                &object.contents,
            ));
        }

        for (group_index, group) in groups.iter().enumerate() {
            let stream_id = original_count + group_index + 1;
            let mut index_bytes = Vec::new();
            let mut object_bytes = Vec::new();
            for (item_index, id) in group.iter().copied().enumerate() {
                index_bytes.extend_from_slice(format!("{id} {} ", object_bytes.len()).as_bytes());
                object_bytes.extend_from_slice(&self.objects[id - 1].as_ref().unwrap().contents);
                object_bytes.push(b'\n');
                locations[id] = XrefLocation::Compressed {
                    stream: stream_id,
                    index: item_index,
                };
            }
            let first = index_bytes.len();
            index_bytes.extend_from_slice(&object_bytes);
            let dictionary = format!("<< /Type /ObjStm /N {} /First {first}", group.len());
            let stream = stream_object_with_options(self.options, dictionary.as_bytes(), &index_bytes);
            locations[stream_id] = XrefLocation::Direct(Self::append_indirect(
                &mut body,
                stream_id,
                &stream,
            ));
        }

        let xref_offset = body.len();
        locations[xref_id] = XrefLocation::Direct(xref_offset);
        let mut xref = Vec::with_capacity(size * 13);
        for location in locations {
            let (kind, field1, field2) = match location {
                XrefLocation::Free => (0u8, 0u64, 65_535u32),
                XrefLocation::Direct(offset) => (1, offset as u64, 0),
                XrefLocation::Compressed { stream, index } => (2, stream as u64, index as u32),
            };
            xref.push(kind);
            xref.extend_from_slice(&field1.to_be_bytes());
            xref.extend_from_slice(&field2.to_be_bytes());
        }
        let info = info
            .map(|id| format!(" /Info {id} 0 R"))
            .unwrap_or_default();
        let dictionary = format!(
            "<< /Type /XRef /Size {size} /Root {root} 0 R{info} /W [1 8 4]"
        );
        let stream = stream_object_with_options(self.options, dictionary.as_bytes(), &xref);
        Self::append_indirect(&mut body, xref_id, &stream);
        body.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        body
    }

    fn finish(self, root: usize, info: Option<usize>) -> Vec<u8> {
        if self.options.compact_objects {
            self.finish_compact(root, info)
        } else {
            self.finish_classic(root, info)
        }
    }
}

#[cfg(test)]
mod object_serialization_tests {
    use super::*;

    fn small_document(options: PdfOptions) -> Vec<u8> {
        let mut objects = Objects::with_options(options);
        let catalog = objects.reserve();
        let pages = objects.reserve();
        let page = objects.reserve();
        let contents = objects.reserve();
        objects.write(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        objects.write(
            pages,
            format!("<< /Type /Pages /Count 1 /Kids [{page} 0 R] >>").as_bytes(),
        );
        objects.write(
            page,
            format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 10 10] /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        objects.write_stream(contents, b"<<", b"0 0 1 1 re f\n");
        objects.finish(catalog, None)
    }

    #[test]
    fn compact_mode_uses_deterministic_object_and_xref_streams() {
        let first = small_document(PdfOptions::default());
        let second = small_document(PdfOptions::default());
        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.5"));
        assert!(first.windows(13).any(|bytes| bytes == b"/Type /ObjStm"));
        assert!(first.windows(11).any(|bytes| bytes == b"/Type /XRef"));
        assert!(!first.windows(6).any(|bytes| bytes == b"\nxref\n"));
    }

    #[test]
    fn diagnostic_mode_keeps_pdf14_classic_xref() {
        let pdf = small_document(PdfOptions::uncompressed());
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.windows(6).any(|bytes| bytes == b"\nxref\n"));
        assert!(!pdf.windows(13).any(|bytes| bytes == b"/Type /ObjStm"));
    }
}

fn escape(s: &[u8], out: &mut Vec<u8>) {
    for &c in s {
        match c {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(c);
            }
            // A PDF string may hold any byte, but keeping it printable makes
            // the file readable and diffable, which matters more here than size.
            0x20..=0x7e => out.push(c),
            _ => out.extend_from_slice(format!("\\{c:03o}").as_bytes()),
        }
    }
}

fn pdf_string(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    out.push(b'(');
    escape(bytes, &mut out);
    out.push(b')');
    String::from_utf8(out).expect("PDF literal strings are ASCII")
}

fn pdf_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2 + 2);
    out.push('<');
    for b in bytes {
        out.push_str(&format!("{b:02X}"));
    }
    out.push('>');
    out
}

/// Escape one component of a PDF name object. PostScript glyph names normally
/// need no escaping, but `#XX` keeps map files with punctuation unambiguous.
fn pdf_name(name: &str) -> String {
    let mut out = String::new();
    for &byte in name.as_bytes() {
        if (b'!'..=b'~').contains(&byte)
            && !matches!(
                byte,
                b'#' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
            )
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("#{byte:02X}"));
        }
    }
    out
}

fn encoding_dictionary(glyphs: &[String]) -> String {
    let mut out = String::from(" /Encoding << /Type /Encoding /Differences [0");
    for code in 0..256 {
        let glyph = glyphs.get(code).map_or(".notdef", String::as_str);
        out.push_str(" /");
        out.push_str(&pdf_name(glyph));
    }
    out.push_str("] >>");
    out
}

fn to_unicode_cmap(values: &[Option<String>]) -> Option<Vec<u8>> {
    let pairs: Vec<(usize, &str)> = values
        .iter()
        .take(256)
        .enumerate()
        .filter_map(|(code, value)| {
            let value = value.as_deref()?;
            (!value.is_empty()
                && value.len() % 4 == 0
                && value.as_bytes().iter().all(u8::is_ascii_hexdigit))
            .then_some((code, value))
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    let mut stream = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for chunk in pairs.chunks(100) {
        stream.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for &(code, value) in chunk {
            stream.push_str(&format!("<{code:02X}> <{}>\n", value.to_ascii_uppercase()));
        }
        stream.push_str("endbfchar\n");
    }
    stream.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    Some(stream.into_bytes())
}
