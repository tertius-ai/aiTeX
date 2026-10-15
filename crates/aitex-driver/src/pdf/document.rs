#[derive(Debug)]
enum LinkTarget {
    Uri(Vec<u8>),
    Destination(Vec<u8>),
}

#[derive(Clone, Copy, Debug)]
struct Rotation {
    x: f64,
    y: f64,
    radians: f64,
}

#[derive(Debug)]
struct LinkSegment {
    baseline: i64,
    transform_depth: usize,
    rect: [f64; 4],
}

fn transformed_rect(mut rect: [f64; 4], rotations: &[Rotation]) -> [f64; 4] {
    for rotation in rotations {
        let (sin, cos) = rotation.radians.sin_cos();
        let corners = [
            (rect[0], rect[1]),
            (rect[0], rect[3]),
            (rect[2], rect[1]),
            (rect[2], rect[3]),
        ];
        let mut next = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for (x, y) in corners {
            let dx = x - rotation.x;
            let dy = y - rotation.y;
            let tx = rotation.x + cos * dx - sin * dy;
            let ty = rotation.y + sin * dx + cos * dy;
            next[0] = next[0].min(tx);
            next[1] = next[1].min(ty);
            next[2] = next[2].max(tx);
            next[3] = next[3].max(ty);
        }
        rect = next;
    }
    rect
}

fn add_link_segment(
    segments: &mut Vec<LinkSegment>,
    baseline: i64,
    transform_depth: usize,
    rect: [f64; 4],
) {
    if let Some(last) = segments.last_mut()
        && last.baseline == baseline
        && last.transform_depth == transform_depth
    {
        last.rect[0] = last.rect[0].min(rect[0]);
        last.rect[1] = last.rect[1].min(rect[1]);
        last.rect[2] = last.rect[2].max(rect[2]);
        last.rect[3] = last.rect[3].max(rect[3]);
    } else {
        segments.push(LinkSegment {
            baseline,
            transform_depth,
            rect,
        });
    }
}

#[derive(Clone, Debug)]
struct Destination {
    page: usize,
    x: f64,
    y: f64,
    view: DestinationView,
}

#[derive(Clone, Copy, Debug)]
enum DestinationView {
    Xyz,
    Fit,
    FitB,
    FitH,
    FitBH,
    FitV,
    FitBV,
}

#[derive(Debug)]
struct Outline {
    title: Vec<u8>,
    destination: Vec<u8>,
    descendants: i32,
}

struct PdfFont {
    id: usize,
    /// Exact DVI/TFM advances in scaled points. These decide whether two glyphs
    /// are genuinely separated by a space or kern.
    advances: Vec<i64>,
    /// Effective advances encoded by `/Widths`, before the formatted text-size
    /// and horizontal-scale operands are applied. Keeping this separately lets
    /// a real TJ displacement absorb decimal residual without manufacturing a
    /// correction between every pair of otherwise adjacent glyphs.
    encoded_advances: Vec<f64>,
    at: i64,
    extend: f64,
    slant: f64,
}

const PAGE_CACHE_BYTES: usize = 32 * 1024 * 1024;
const PAGE_TREE_FANOUT: usize = 256;

#[derive(Clone)]
struct CachedPageDestination {
    name: Vec<u8>,
    x: f64,
    y: f64,
    view: DestinationView,
}

struct CachedPage {
    stream: std::sync::Arc<[u8]>,
    raw_len: usize,
    destinations: Vec<CachedPageDestination>,
    retained: usize,
}

struct PageCache {
    // The map makes both hits and misses independent of the number of cached
    // pages. The queue holds shared keys solely for bounded FIFO eviction;
    // exact LRU is not useful for the sequential over-capacity scans which
    // dominate large TeX documents, and would require a more complex linked
    // index or another dependency.
    entries: std::collections::HashMap<std::sync::Arc<[u8]>, CachedPage>,
    insertion_order: std::collections::VecDeque<std::sync::Arc<[u8]>>,
    bytes: usize,
    max_bytes: usize,
}

struct PageTreeNode {
    id: usize,
    parent: usize,
    children: Vec<usize>,
    count: usize,
}

struct PageTreeLevelEntry {
    id: usize,
    node_index: usize,
    count: usize,
}

struct PageTreePlan {
    page_parents: Vec<usize>,
    nodes: Vec<PageTreeNode>,
    root_children: Vec<usize>,
    page_count: usize,
}

impl PageTreePlan {
    fn reserve(objects: &mut Objects, root: usize, page_ids: &[usize]) -> Self {
        if page_ids.len() <= PAGE_TREE_FANOUT {
            return Self {
                page_parents: vec![root; page_ids.len()],
                nodes: Vec::new(),
                root_children: page_ids.to_vec(),
                page_count: page_ids.len(),
            };
        }

        let mut page_parents = vec![root; page_ids.len()];
        let mut nodes = Vec::<PageTreeNode>::new();
        let mut level = Vec::<PageTreeLevelEntry>::new();
        for (chunk_index, children) in page_ids.chunks(PAGE_TREE_FANOUT).enumerate() {
            let id = objects.reserve();
            let start = chunk_index * PAGE_TREE_FANOUT;
            page_parents[start..start + children.len()].fill(id);
            let node_index = nodes.len();
            nodes.push(PageTreeNode {
                id,
                parent: root,
                children: children.to_vec(),
                count: children.len(),
            });
            level.push(PageTreeLevelEntry {
                id,
                node_index,
                count: children.len(),
            });
        }

        while level.len() > PAGE_TREE_FANOUT {
            let mut next = Vec::<PageTreeLevelEntry>::new();
            for children in level.chunks(PAGE_TREE_FANOUT) {
                let id = objects.reserve();
                let count = children.iter().map(|child| child.count).sum();
                for child in children {
                    nodes[child.node_index].parent = id;
                }
                let node_index = nodes.len();
                nodes.push(PageTreeNode {
                    id,
                    parent: root,
                    children: children.iter().map(|child| child.id).collect(),
                    count,
                });
                next.push(PageTreeLevelEntry {
                    id,
                    node_index,
                    count,
                });
            }
            level = next;
        }
        for child in &level {
            nodes[child.node_index].parent = root;
        }

        Self {
            page_parents,
            nodes,
            root_children: level.iter().map(|child| child.id).collect(),
            page_count: page_ids.len(),
        }
    }

    fn write(&self, objects: &mut Objects, root: usize, extra: &[u8]) {
        for node in &self.nodes {
            let children = indirect_references(&node.children);
            objects.write(
                node.id,
                format!(
                    "<< /Type /Pages /Parent {} 0 R /Count {} /Kids [{children}] >>",
                    node.parent, node.count
                )
                .as_bytes(),
            );
        }
        let children = indirect_references(&self.root_children);
        objects.write(
            root,
            format!(
                "<< /Type /Pages /Count {} /Kids [{children}]{} >>",
                self.page_count,
                String::from_utf8_lossy(extra),
            )
            .as_bytes(),
        );
    }
}

fn indirect_references(ids: &[usize]) -> String {
    ids.iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Default for PageCache {
    fn default() -> Self {
        Self::with_max_bytes(PAGE_CACHE_BYTES)
    }
}

impl PageCache {
    fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            insertion_order: std::collections::VecDeque::new(),
            bytes: 0,
            max_bytes,
        }
    }

    fn get(
        &self,
        key: &[u8],
    ) -> Option<(
        std::sync::Arc<[u8]>,
        usize,
        Vec<CachedPageDestination>,
    )> {
        let entry = self.entries.get(key)?;
        Some((
            std::sync::Arc::clone(&entry.stream),
            entry.raw_len,
            entry.destinations.clone(),
        ))
    }

    fn insert(
        &mut self,
        key: Vec<u8>,
        stream: Vec<u8>,
        raw_len: usize,
        destinations: Vec<CachedPageDestination>,
    ) {
        let retained = key.len()
            + stream.len()
            + destinations
                .iter()
                .map(|destination| {
                    std::mem::size_of::<CachedPageDestination>() + destination.name.len()
                })
                .sum::<usize>();
        if retained > self.max_bytes || self.entries.contains_key(key.as_slice()) {
            return;
        }
        while self.bytes.saturating_add(retained) > self.max_bytes {
            let Some(expired_key) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(expired) = self.entries.remove(expired_key.as_ref()) {
                self.bytes = self.bytes.saturating_sub(expired.retained);
            }
        }
        let key: std::sync::Arc<[u8]> = key.into();
        self.bytes += retained;
        self.insertion_order.push_back(std::sync::Arc::clone(&key));
        self.entries.insert(
            key,
            CachedPage {
                stream: stream.into(),
                raw_len,
                destinations,
                retained,
            },
        );
    }
}

#[cfg(test)]
mod page_index_tests {
    use super::*;

    fn cache_key(index: u32) -> Vec<u8> {
        let mut key = vec![b'x'; 128];
        key.extend_from_slice(&index.to_le_bytes());
        key
    }

    #[test]
    fn indexed_page_cache_handles_many_exact_keys() {
        let mut cache = PageCache::with_max_bytes(usize::MAX);
        for index in 0..10_000u32 {
            cache.insert(
                cache_key(index),
                vec![(index % 251) as u8],
                index as usize,
                Vec::new(),
            );
        }
        assert_eq!(cache.entries.len(), 10_000);
        assert_eq!(cache.insertion_order.len(), 10_000);
        for index in 0..10_000u32 {
            let (stream, raw_len, destinations) = cache.get(&cache_key(index)).unwrap();
            assert_eq!(stream.as_ref(), &[(index % 251) as u8]);
            assert_eq!(raw_len, index as usize);
            assert!(destinations.is_empty());
        }
    }

    #[test]
    fn page_cache_evicts_oldest_entries_within_its_byte_budget() {
        let mut cache = PageCache::with_max_bytes(16);
        cache.insert(vec![1; 4], vec![11; 4], 4, Vec::new());
        cache.insert(vec![2; 4], vec![22; 4], 4, Vec::new());
        assert_eq!(cache.bytes, 16);

        cache.insert(vec![3; 4], vec![33; 4], 4, Vec::new());
        assert!(cache.get(&[1; 4]).is_none());
        assert!(cache.get(&[2; 4]).is_some());
        assert!(cache.get(&[3; 4]).is_some());
        assert_eq!(cache.bytes, 16);
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.insertion_order.len(), 2);
    }

    fn assert_valid_page_tree(page_count: usize) {
        let mut objects = Objects::new();
        let root = objects.reserve();
        let page_ids = (0..page_count)
            .map(|_| objects.reserve())
            .collect::<Vec<_>>();
        let plan = PageTreePlan::reserve(&mut objects, root, &page_ids);
        let pages = page_ids.iter().copied().collect::<std::collections::HashSet<_>>();
        let nodes = plan
            .nodes
            .iter()
            .map(|node| (node.id, node))
            .collect::<std::collections::HashMap<_, _>>();

        fn collect_pages(
            id: usize,
            parent: usize,
            pages: &std::collections::HashSet<usize>,
            nodes: &std::collections::HashMap<usize, &PageTreeNode>,
            page_parents: &std::collections::HashMap<usize, usize>,
            output: &mut Vec<usize>,
        ) -> usize {
            if pages.contains(&id) {
                assert_eq!(page_parents.get(&id), Some(&parent));
                output.push(id);
                return 1;
            }
            let node = nodes.get(&id).expect("every non-page child is a page-tree node");
            assert_eq!(node.parent, parent);
            assert!(!node.children.is_empty());
            assert!(node.children.len() <= PAGE_TREE_FANOUT);
            let count = node
                .children
                .iter()
                .map(|child| {
                    collect_pages(*child, node.id, pages, nodes, page_parents, output)
                })
                .sum::<usize>();
            assert_eq!(node.count, count);
            count
        }

        assert_eq!(plan.page_count, page_count);
        assert!(plan.root_children.len() <= PAGE_TREE_FANOUT);
        let page_parents = page_ids
            .iter()
            .copied()
            .zip(plan.page_parents.iter().copied())
            .collect::<std::collections::HashMap<_, _>>();
        let mut collected = Vec::new();
        let count = plan
            .root_children
            .iter()
            .map(|child| {
                collect_pages(
                    *child,
                    root,
                    &pages,
                    &nodes,
                    &page_parents,
                    &mut collected,
                )
            })
            .sum::<usize>();
        assert_eq!(count, page_count);
        assert_eq!(collected, page_ids);
    }

    #[test]
    fn page_tree_is_balanced_at_every_scale_boundary() {
        for page_count in [0, 1, 256, 257, 10_000, 65_537] {
            assert_valid_page_tree(page_count);
        }
    }
}

thread_local! {
    static PAGE_CACHE: std::cell::RefCell<PageCache> =
        std::cell::RefCell::new(PageCache::default());
}

fn cacheable_page_special(text: &[u8]) -> bool {
    let text = String::from_utf8_lossy(text);
    let trimmed = text.trim();
    // A DVI color push or pop changes state that can survive a page break.
    // Replaying a cached content stream would skip that transition and give
    // the following page the wrong incoming color stack.
    special_color(trimmed).is_none()
        && trimmed != "color pop"
        && (special_background(trimmed).is_some()
        || (trimmed.starts_with("ps:SDict begin")
            && (trimmed.ends_with("H.S end")
                || trimmed.ends_with("H.R end")
                || trimmed.ends_with("H.A end")
                || trimmed.contains("/DEST pdfmark")
                || trimmed.starts_with("ps:SDict begin /product where")))
        || trimmed.starts_with("aitex:page-rotate "))
}

fn page_cache_key(
    page: &dvi::Page,
    page_width: f64,
    page_height: f64,
    fonts: &std::collections::BTreeMap<u32, PdfFont>,
    options: PdfOptions,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(page.items.len().saturating_mul(32));
    key.extend_from_slice(options.stream_format_tag().as_bytes());
    key.push(0);
    key.extend_from_slice(&page_width.to_bits().to_le_bytes());
    key.extend_from_slice(&page_height.to_bits().to_le_bytes());
    let used_fonts = page
        .items
        .iter()
        .filter_map(|item| match item {
            dvi::Item::ClassicGlyph { font, .. } => Some(font.0),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    for number in used_fonts {
        let Some(font) = fonts.get(&number) else {
            continue;
        };
        key.extend_from_slice(&number.to_le_bytes());
        key.extend_from_slice(&font.id.to_le_bytes());
        key.extend_from_slice(&font.at.to_le_bytes());
        key.extend_from_slice(&font.extend.to_bits().to_le_bytes());
        key.extend_from_slice(&font.slant.to_bits().to_le_bytes());
        key.extend_from_slice(&font.advances.len().to_le_bytes());
        for advance in &font.advances {
            key.extend_from_slice(&advance.to_le_bytes());
        }
    }
    for item in &page.items {
        match item {
            dvi::Item::ClassicGlyph {
                font,
                code,
                origin_x,
                origin_y,
                ..
            } => {
                key.push(0);
                key.extend_from_slice(&font.0.to_le_bytes());
                key.extend_from_slice(&code.to_le_bytes());
                key.extend_from_slice(&origin_x.0.to_le_bytes());
                key.extend_from_slice(&origin_y.0.to_le_bytes());
            }
            dvi::Item::Rule {
                x,
                baseline_y,
                width,
                height,
            } => {
                key.push(1);
                key.extend_from_slice(&x.0.to_le_bytes());
                key.extend_from_slice(&baseline_y.0.to_le_bytes());
                key.extend_from_slice(&width.0.to_le_bytes());
                key.extend_from_slice(&height.0.to_le_bytes());
            }
            dvi::Item::Special { x, y, bytes } => {
                key.push(2);
                key.extend_from_slice(&x.0.to_le_bytes());
                key.extend_from_slice(&y.0.to_le_bytes());
                key.extend_from_slice(&bytes.len().to_le_bytes());
                key.extend_from_slice(bytes);
            }
            dvi::Item::GlyphRun(_) => key.push(3),
        }
    }
    key
}

fn page_font_resources(
    page: &dvi::Page,
    fonts: &std::collections::BTreeMap<u32, PdfFont>,
) -> String {
    page.items
        .iter()
        .filter_map(|item| match item {
            dvi::Item::ClassicGlyph { font, .. } => {
                fonts.get(&font.0).map(|font| font.id)
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|id| format!("/F{id} {id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn type3_charproc(glyph: &crate::pk::Glyph, advance: f64) -> Vec<u8> {
    // Keep the PK glyph in pixel coordinates and let the Type 3 FontMatrix
    // scale it. pdfTeX does the same thing: one inline image mask per used
    // glyph. Turning every black pixel into a vector rectangle is equivalent
    // in ideal arithmetic, but PDF renderers anti-alias the rectangle edges
    // differently and the resulting files are tens of times larger.
    let left = -glyph.x_offset;
    // PK's y offset names the top scanline's baseline-relative row. The PDF
    // image occupies half-open device pixels, hence the +1 used by pdfTeX for
    // its lower edge. Its cached d1 box is also one unit conservative at the
    // right edge, while the image matrix itself remains exactly width pixels.
    let bottom = glyph.y_offset - glyph.height as i32 + 1;
    let right = left + glyph.width as i32 + 1;
    let top = bottom + glyph.height as i32;
    if glyph.width == 0 || glyph.height == 0 {
        return format!("{advance:.2} 0 {left} {bottom} {right} {top} d1\n").into_bytes();
    }
    let mut content = format!(
        "{advance:.2} 0 {left} {bottom} {right} {top} d1\n\
         q\n{} 0 0 {} {left} {bottom} cm\n\
         BI\n/W {}\n/H {}\n/IM true\n/BPC 1\n/D [1 0]\n/F /AHx\nID\n",
        glyph.width, glyph.height, glyph.width, glyph.height
    )
    .into_bytes();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in &glyph.bitmap {
        content.push(HEX[(byte >> 4) as usize]);
        content.push(HEX[(byte & 0x0f) as usize]);
    }
    content.extend_from_slice(b">\nEI\nQ\n");
    content
}
