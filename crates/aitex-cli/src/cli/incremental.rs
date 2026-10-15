const BODY_CHECKPOINT_LIMIT: usize = 12;
const BODY_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
const BODY_CHECKPOINT_MIN_SOURCE: usize = 8 * 1024;
const BODY_CHECKPOINT_DENOMINATOR: usize = 32;
const BODY_CHECKPOINT_FRACTIONS: [usize; 11] = [0, 4, 8, 12, 16, 20, 24, 28, 29, 30, 31];
const BODY_PREPARED_NUMERATOR: usize = 29;
const CHUNK_MIN: usize = 16 * 1024;
const CHUNK_MASK: u64 = (1 << 16) - 1;
const CHUNK_MAX: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceDiff {
    unchanged_prefix: usize,
    old_changed_end: usize,
    new_changed_end: usize,
}

/// One contiguous edit after removing the longest equal prefix and suffix.
/// Offsets are always UTF-8 boundaries even when the first differing byte is
/// inside a multi-byte scalar.
fn source_diff(old: &str, new: &str) -> Option<SourceDiff> {
    if old == new {
        return None;
    }
    let old_bytes = old.as_bytes();
    let new_bytes = new.as_bytes();
    let mut prefix = old_bytes
        .iter()
        .zip(new_bytes)
        .take_while(|(left, right)| left == right)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }

    let mut suffix = old_bytes[prefix..]
        .iter()
        .rev()
        .zip(new_bytes[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix)
            || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }
    Some(SourceDiff {
        unchanged_prefix: prefix,
        old_changed_end: old.len() - suffix,
        new_changed_end: new.len() - suffix,
    })
}

fn source_diff_is_trivia(old: &str, new: &str, diff: SourceDiff) -> bool {
    fn whitespace(value: &str) -> bool {
        value.chars().all(char::is_whitespace)
    }
    fn inside_comment(source: &str, start: usize, end: usize) -> bool {
        let line_start = source[..start]
            .rfind(['\n', '\r'])
            .map_or(0, |offset| offset + 1);
        let line_end = source[end..]
            .find(['\n', '\r'])
            .map_or(source.len(), |offset| end + offset);
        let prefix = &source[line_start..start];
        let mut escaped = false;
        let comment = prefix.char_indices().find_map(|(offset, character)| {
            if character == '%' && !escaped {
                return Some(line_start + offset);
            }
            escaped = character == '\\' && !escaped;
            if character != '\\' {
                escaped = false;
            }
            None
        });
        comment.is_some_and(|comment| comment <= start && end <= line_end)
    }

    let old_changed = &old[diff.unchanged_prefix..diff.old_changed_end];
    let new_changed = &new[diff.unchanged_prefix..diff.new_changed_end];
    (whitespace(old_changed) && whitespace(new_changed))
        || (inside_comment(old, diff.unchanged_prefix, diff.old_changed_end)
            && inside_comment(new, diff.unchanged_prefix, diff.new_changed_end))
}

const fn gear_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < table.len() {
        let mut value = (i as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        table[i] = value ^ (value >> 31);
        i += 1;
    }
    table
}

const GEAR: [u64; 256] = gear_table();

fn chunk_ranges(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        let mut at = start;
        let mut hash = 0u64;
        while at < bytes.len() {
            hash = hash.rotate_left(1).wrapping_add(GEAR[bytes[at] as usize]);
            at += 1;
            let len = at - start;
            if len >= CHUNK_MIN && ((hash & CHUNK_MASK) == 0 || len >= CHUNK_MAX) {
                break;
            }
        }
        ranges.push(start..at);
        start = at;
    }
    ranges
}

#[derive(Clone)]
struct CheckpointChunk {
    digest: [u8; 32],
    bytes: Rc<[u8]>,
}

#[derive(Clone)]
struct ChunkedCheckpoint {
    len: usize,
    digest: [u8; 32],
    chunks: Vec<CheckpointChunk>,
    /// The most useful checkpoint remains decoded. Repeated edits should pay
    /// for postcard restore, not LZ4 reconstruction, while the global 64 MiB
    /// bound still accounts for this copy.
    hot: Option<Rc<[u8]>>,
}

impl ChunkedCheckpoint {
    fn materialize(&self) -> Option<Rc<[u8]>> {
        // `hot` is an immutable `Rc` installed only from bytes whose digest
        // was computed at capture/materialization time. Re-hashing several
        // megabytes on every edit adds no new proof: no caller can mutate the
        // allocation after publication.
        if let Some(bytes) = &self.hot && bytes.len() == self.len {
            return Some(Rc::clone(bytes));
        }
        let mut bytes = Vec::with_capacity(self.len);
        for chunk in &self.chunks {
            let decoded = lz4_flex::block::decompress_size_prepended(&chunk.bytes).ok()?;
            if sha256(&decoded) != chunk.digest
                || bytes.len().checked_add(decoded.len())? > self.len
            {
                return None;
            }
            bytes.extend_from_slice(&decoded);
        }
        (bytes.len() == self.len && sha256(&bytes) == self.digest)
            .then(|| Rc::from(bytes))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BoundaryKinds(u8);

impl BoundaryKinds {
    const PARAGRAPH: u8 = 1;
    const SECTION: u8 = 2;
    const INCLUDE: u8 = 4;
    const PAGE: u8 = 8;

    fn paragraph() -> Self {
        Self(Self::PARAGRAPH)
    }

    fn insert(&mut self, kind: u8) {
        self.0 |= kind;
    }

    fn label(self) -> String {
        let mut labels = vec!["paragraph"];
        if self.0 & Self::SECTION != 0 {
            labels.push("section");
        }
        if self.0 & Self::INCLUDE != 0 {
            labels.push("include");
        }
        if self.0 & Self::PAGE != 0 {
            labels.push("page");
        }
        labels.join("+")
    }
}

struct BodyCheckpointEntry {
    site: aitex_engine::fast::CheckpointSite,
    payload: ChunkedCheckpoint,
    shipper: Shipper,
    /// One tail checkpoint is decoded between requests. Keeping exactly one
    /// avoids multiplying the live engine footprint by the checkpoint count.
    prepared: Option<Fast>,
    /// Only inputs observed before this boundary constrain its validity. A
    /// dependency first read later may change without invalidating this entry;
    /// resumption naturally reads the changed subtree again.
    dependencies: Vec<CacheDependency>,
    kinds: BoundaryKinds,
    semantic_digest: Option<[u8; 32]>,
    output_digest: [u8; 32],
}

#[derive(Clone)]
struct CompletedFile {
    path: String,
    len: usize,
    digest: [u8; 32],
}

impl CompletedFile {
    fn capture(path: String, primary: &str) -> Option<Self> {
        let bytes = if path == primary {
            read_output_artifact(&path).ok()?
        } else {
            std::fs::read(&path).ok()?
        };
        Some(Self {
            path,
            len: bytes.len(),
            digest: sha256(&bytes),
        })
    }

    fn matches(&self, primary: &str) -> bool {
        let bytes = if self.path == primary {
            read_output_artifact(&self.path)
        } else {
            std::fs::read(&self.path)
        };
        let Ok(bytes) = bytes else {
            return false;
        };
        bytes.len() == self.len && sha256(&bytes) == self.digest
    }
}

#[derive(Clone)]
struct CompletedOutputs {
    primary: String,
    files: Vec<CompletedFile>,
}

impl CompletedOutputs {
    fn capture(primary: String, mut paths: Vec<String>) -> Option<Self> {
        paths.sort();
        paths.dedup();
        let files = paths
            .into_iter()
            .map(|path| CompletedFile::capture(path, &primary))
            .collect::<Option<Vec<_>>>()?;
        Some(Self { primary, files })
    }

    fn matches(&self, primary: &str) -> bool {
        self.primary == primary
            && self
                .files
                .iter()
                .all(|file| file.matches(&self.primary))
    }

    fn snapshot(&self) -> Option<RestorableOutputs> {
        let files = self
            .files
            .iter()
            .map(|file| {
                let bytes = if file.path == self.primary {
                    read_output_artifact(&file.path).ok()?
                } else {
                    std::fs::read(&file.path).ok()?
                };
                (bytes.len() == file.len && sha256(&bytes) == file.digest)
                    .then(|| (file.path.clone(), Rc::<[u8]>::from(bytes)))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(RestorableOutputs {
            primary: self.primary.clone(),
            files,
        })
    }
}

struct RestorableOutputs {
    primary: String,
    files: Vec<(String, Rc<[u8]>)>,
}

impl RestorableOutputs {
    fn restore(&self) -> bool {
        self.files
            .iter()
            .all(|(path, bytes)| {
                if path == &self.primary {
                    publish_output_artifact(path, bytes.as_ref()).is_ok()
                } else {
                    std::fs::write(path, bytes.as_ref()).is_ok()
                }
            })
    }
}

struct BodyCheckpointIndex {
    key: String,
    source: String,
    dependencies: Vec<CacheDependency>,
    clock: Option<WallClock>,
    entries: Vec<BodyCheckpointEntry>,
    completed_outputs: Option<CompletedOutputs>,
}

struct BodyCheckpointBuilder {
    key: String,
    source: String,
    entries: Vec<BodyCheckpointEntry>,
    chunks: HashMap<[u8; 32], Rc<[u8]>>,
    unique_bytes: usize,
    output_bytes: usize,
    reconvergence: Vec<ReconvergenceCandidate>,
    prior_dependencies: Option<Vec<CacheDependency>>,
    prior_clock: Option<WallClock>,
    prior_outputs: Option<CompletedOutputs>,
    restorable_outputs: Option<RestorableOutputs>,
}

struct ReconvergenceCandidate {
    new_offset: usize,
    semantic_digest: [u8; 32],
    output_digest: [u8; 32],
}

impl BodyCheckpointBuilder {
    fn empty(key: String, source: String) -> Self {
        Self {
            key,
            source,
            entries: Vec::new(),
            chunks: HashMap::new(),
            unique_bytes: 0,
            output_bytes: 0,
            reconvergence: Vec::new(),
            prior_dependencies: None,
            prior_clock: None,
            prior_outputs: None,
            restorable_outputs: None,
        }
    }

    fn from_prefix(index: BodyCheckpointIndex, source: String, through: usize) -> Self {
        let diff = source_diff(&index.source, &source);
        let reconvergence_possible = diff
            .is_some_and(|diff| source_diff_is_trivia(&index.source, &source, diff));
        let restorable_outputs = reconvergence_possible
            .then(|| {
                index
                    .completed_outputs
                    .as_ref()
                    .filter(|outputs| {
                        dependencies_hold(&index.dependencies).is_ok()
                            && outputs.matches(&outputs.primary)
                    })
                    .and_then(CompletedOutputs::snapshot)
            })
            .flatten();
        let reconvergence = match (diff, restorable_outputs.as_ref()) {
            (Some(diff), Some(_)) => index
                .entries
                .iter()
                .filter(|entry| entry.site.byte_offset >= diff.old_changed_end)
                .filter_map(|entry| {
                    let semantic_digest = entry.semantic_digest?;
                    let suffix_offset = entry.site.byte_offset.checked_sub(diff.old_changed_end)?;
                    let new_offset = diff.new_changed_end.checked_add(suffix_offset)?;
                    Some(ReconvergenceCandidate {
                        new_offset,
                        semantic_digest,
                        output_digest: entry.output_digest,
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let entries: Vec<_> = index
            .entries
            .into_iter()
            .filter(|entry| entry.site.byte_offset <= through)
            .collect();
        let mut builder = Self::empty(index.key, source);
        builder.entries = entries;
        builder.reconvergence = reconvergence;
        builder.prior_dependencies = Some(index.dependencies);
        builder.prior_clock = index.clock;
        builder.prior_outputs = index.completed_outputs;
        builder.restorable_outputs = restorable_outputs;
        builder.rebuild_chunks();
        builder
    }

    fn rebuild_chunks(&mut self) {
        self.chunks.clear();
        for entry in &self.entries {
            for chunk in &entry.payload.chunks {
                self.chunks
                    .entry(chunk.digest)
                    .or_insert_with(|| Rc::clone(&chunk.bytes));
            }
        }
        self.unique_bytes = self.chunks.values().map(|chunk| chunk.len()).sum();
        self.output_bytes = self
            .entries
            .iter()
            .map(|entry| {
                entry.shipper.retained_bytes()
                    + entry.prepared.as_ref().map_or(0, |_| entry.payload.len)
            })
            .sum();
    }

    fn capture(
        &mut self,
        site: aitex_engine::fast::CheckpointSite,
        bytes: &[u8],
        shipper: &Shipper,
        dependencies: Vec<CacheDependency>,
        mut kinds: BoundaryKinds,
        semantic_digest: Option<[u8; 32]>,
        prepare: bool,
    ) -> bool {
        if self
            .entries
            .last()
            .is_some_and(|entry| entry.site.byte_offset >= site.byte_offset)
        {
            return false;
        }
        let hot = self
            .entries
            .iter()
            .filter(|entry| entry.payload.hot.is_some())
            .count();
        if hot >= 3
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.payload.hot.is_some())
        {
            entry.payload.hot = None;
        }
        if self
            .entries
            .last()
            .is_some_and(|entry| entry.site.pages_shipped < site.pages_shipped)
            || self.entries.is_empty() && site.pages_shipped > 0
        {
            kinds.insert(BoundaryKinds::PAGE);
        }
        let chunks = chunk_ranges(bytes)
            .into_iter()
            .map(|range| {
                let body = &bytes[range];
                let digest = sha256(body);
                let bytes = if let Some(chunk) = self.chunks.get(&digest) {
                    Rc::clone(chunk)
                } else {
                    let chunk: Rc<[u8]> =
                        Rc::from(lz4_flex::block::compress_prepend_size(body));
                    self.unique_bytes += chunk.len();
                    self.chunks.insert(digest, Rc::clone(&chunk));
                    chunk
                };
                CheckpointChunk { digest, bytes }
            })
            .collect();
        let prepared = prepare
            .then(|| Fast::resume_checkpoint(bytes, &self.source).ok())
            .flatten()
            .and_then(|(engine, restored)| (restored == site).then_some(engine));
        self.entries.push(BodyCheckpointEntry {
            site,
            payload: ChunkedCheckpoint {
                len: bytes.len(),
                digest: sha256(bytes),
                chunks,
                hot: Some(Rc::from(bytes)),
            },
            shipper: shipper.clone(),
            prepared,
            dependencies,
            kinds,
            semantic_digest,
            output_digest: shipper.checkpoint_identity(),
        });
        self.output_bytes += shipper.retained_bytes()
            + self
                .entries
                .last()
                .and_then(|entry| entry.prepared.as_ref().map(|_| entry.payload.len))
                .unwrap_or(0);

        while self.entries.len() > BODY_CHECKPOINT_LIMIT
            || self.retained_bytes() > BODY_CHECKPOINT_BYTES
        {
            // Preserve both ends and remove the interior point whose neighbors
            // are already closest. Ties go to the later point, which keeps an
            // initially uniform stream distributed instead of clustering all
            // retained checkpoints at the end.
            let remove = self.eviction_index();
            self.entries.remove(remove);
            self.rebuild_chunks();
            if self.entries.len() == 1 && self.retained_bytes() > BODY_CHECKPOINT_BYTES {
                self.entries.clear();
                self.rebuild_chunks();
                return false;
            }
        }
        true
    }

    fn eviction_index(&self) -> usize {
        if self.entries.len() <= 2 {
            return self.entries.len().saturating_sub(1);
        }
        let mut best = 1usize;
        let mut best_span = usize::MAX;
        for index in 1..self.entries.len() - 1 {
            let span = self.entries[index + 1]
                .site
                .byte_offset
                .saturating_sub(self.entries[index - 1].site.byte_offset);
            if span <= best_span {
                best = index;
                best_span = span;
            }
        }
        best
    }

    fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn retained_bytes(&self) -> usize {
        self.unique_bytes
            + self.output_bytes
            + self
                .entries
                .iter()
                .filter_map(|entry| entry.payload.hot.as_ref())
                .map(|bytes| bytes.len())
                .sum::<usize>()
    }

    fn finish(
        self,
        dependencies: Vec<CacheDependency>,
        clock: Option<WallClock>,
        completed_outputs: Option<CompletedOutputs>,
    ) -> BodyCheckpointIndex {
        BodyCheckpointIndex {
            key: self.key,
            source: self.source,
            dependencies,
            clock,
            entries: self.entries,
            completed_outputs,
        }
    }

    fn reconvergence_targets(&self) -> Vec<CheckpointTarget> {
        self.reconvergence
            .iter()
            .map(|candidate| CheckpointTarget {
                offset: candidate.new_offset,
                kinds: BoundaryKinds::paragraph(),
                prepare: false,
            })
            .collect()
    }

    fn reconverged(
        &self,
        site: &aitex_engine::fast::CheckpointSite,
        semantic_digest: Option<[u8; 32]>,
        shipper: &Shipper,
    ) -> bool {
        let Some(semantic_digest) = semantic_digest else {
            cli_detail!("semantic reconvergence miss (state digest unavailable)");
            return false;
        };
        let output_digest = shipper.checkpoint_identity();
        let Some(candidate) = self
            .reconvergence
            .iter()
            .find(|candidate| candidate.new_offset == site.byte_offset)
        else {
            if let Some(candidate) = self.reconvergence.first() {
                cli_detail!(
                    "semantic reconvergence miss (boundary {} reached, expected {})",
                    site.byte_offset, candidate.new_offset
                );
            }
            return false;
        };
        if candidate.semantic_digest != semantic_digest {
            cli_detail!("semantic reconvergence miss (complete state differs)");
            return false;
        }
        if candidate.output_digest != output_digest {
            cli_detail!("semantic reconvergence miss (DVI prefix differs)");
            return false;
        }
        true
    }

    fn restore_reconverged_outputs(&self) -> bool {
        self.restorable_outputs
            .as_ref()
            .is_some_and(RestorableOutputs::restore)
    }

    fn finish_reconverged(self) -> Option<BodyCheckpointIndex> {
        Some(BodyCheckpointIndex {
            key: self.key,
            source: self.source,
            dependencies: self.prior_dependencies?,
            clock: self.prior_clock,
            entries: self.entries,
            completed_outputs: self.prior_outputs,
        })
    }
}

fn completed_output_is_reusable(
    index: &BodyCheckpointIndex,
    key: &str,
    source: &str,
    clock: WallClock,
    output: &str,
) -> bool {
    index.key == key
        && index.source == source
        && index
            .clock
            .is_none_or(|saved| clock_matches(saved, clock))
        && dependencies_hold(&index.dependencies).is_ok()
        && index
            .completed_outputs
            .as_ref()
            .is_some_and(|artifact| artifact.matches(output))
}

fn completed_output_reuse_allowed() -> bool {
    [
        "AITEX_ALIGN_TRACE",
        "AITEX_ARENA_WATCH",
        "AITEX_CATCODE_WATCH",
        "AITEX_DBG_SEL",
        "AITEX_DEF_GREP",
        "AITEX_DUMP_BODY",
        "AITEX_FILE_PROGRESS",
        "AITEX_FIRST_OUT",
        "AITEX_FIRST_UNDEF",
        "AITEX_GROUP_TRACE",
        "AITEX_HOT_CS",
        "AITEX_INS_DEBUG",
        "AITEX_KEEP_DVI",
        "AITEX_MEM",
        "AITEX_NO_INPUT",
        "AITEX_OUT",
        "AITEX_PROFILE",
        "AITEX_RUNAWAY",
        "AITEX_STACK_AT",
        "AITEX_STATE",
        "AITEX_STATS",
        "AITEX_STEP_TRACE",
        "AITEX_TOK_TRACE",
        "AITEX_TRACE_DEFS",
        "AITEX_TRACE_PAGE",
        "AITEX_TRACE_PAR",
        "AITEX_TRACING_NODES",
        "AITEX_TRACING_PAGES",
        "AITEX_VLIST",
        "AITEX_VLIST_DEEP",
    ]
    .iter()
    .all(|name| std::env::var_os(name).is_none())
}

/// Files whose exact before/after identity decides whether another TeX pass or
/// an external bibliography/index/glossary tool can observe new information.
/// This is deliberately an allow-list: arbitrary `\write` output is a build
/// product, not evidence that rerunning the document will reach a fixpoint.
fn is_fixpoint_extension(extension: &str) -> bool {
    matches!(
        extension,
        "aux"
            | "toc"
            | "lof"
            | "lot"
            | "out"
            | "brf"
            | "bcf"
            | "idx"
            | "glo"
            | "glsdefs"
            | "acn"
            | "nlo"
            | "nav"
            | "snm"
    )
}

struct BodyCheckpointHit {
    site: aitex_engine::fast::CheckpointSite,
    bytes: Option<Rc<[u8]>>,
    prepared: Option<Fast>,
    dependencies: Vec<CacheDependency>,
    builder: BodyCheckpointBuilder,
    changed: Option<SourceDiff>,
    shipper: Shipper,
    kinds: BoundaryKinds,
    dependency_changed: bool,
}

fn select_body_checkpoint(
    mut index: BodyCheckpointIndex,
    key: &str,
    source: &str,
    clock: WallClock,
    proven_dependencies: &[CacheDependency],
) -> Result<BodyCheckpointHit, (BodyCheckpointBuilder, &'static str)> {
    if index.key != key {
        return Err((
            BodyCheckpointBuilder::empty(key.to_string(), source.to_string()),
            "identity changed",
        ));
    }
    if index
        .clock
        .is_some_and(|saved| !clock_matches(saved, clock))
    {
        return Err((
            BodyCheckpointBuilder::empty(key.to_string(), source.to_string()),
            "clock changed",
        ));
    }
    let changed = source_diff(&index.source, source);
    // A checkpoint contains the dependencies observed before its boundary.
    // Validate the union once, rather than re-resolving the same TeX-tree
    // inputs for every candidate while walking backward through the index.
    let mut dependency_validity: BTreeMap<CacheDependency, bool> = proven_dependencies
        .iter()
        .cloned()
        .map(|dependency| (dependency, true))
        .collect();
    for dependency in index.entries.iter().flat_map(|entry| &entry.dependencies) {
        dependency_validity
            .entry(dependency.clone())
            .or_insert_with(|| dependency_holds(dependency).is_ok());
    }
    let dependency_changed = index
        .entries
        .iter()
        .flat_map(|entry| &entry.dependencies)
        .any(|dependency| !dependency_validity[dependency]);
    let through = changed
        .map(|diff| diff.unchanged_prefix)
        .unwrap_or(source.len());
    let selected_index = index
        .entries
        .iter()
        .rposition(|entry| {
            entry.site.byte_offset <= through
                && entry
                    .dependencies
                    .iter()
                    .all(|dependency| dependency_validity[dependency])
        });
    let Some(selected_index) = selected_index else {
        return Err((
            BodyCheckpointBuilder::empty(key.to_string(), source.to_string()),
            if index.entries.iter().any(|entry| entry.site.byte_offset <= through) {
                "dependency changed"
            } else {
                "edit precedes first checkpoint"
            },
        ));
    };
    if index.source[..index.entries[selected_index].site.byte_offset]
        != source[..index.entries[selected_index].site.byte_offset]
    {
        return Err((
            BodyCheckpointBuilder::empty(key.to_string(), source.to_string()),
            "source prefix changed",
        ));
    }
    // The resumed writer owns a potentially multi-megabyte DVI prefix. Move
    // the selected entry out of the old index instead of cloning that prefix
    // on the latency-critical path. Earlier distributed checkpoints remain
    // available; a later full pass replenishes the consumed boundary.
    let selected = index.entries.remove(selected_index);
    let site = selected.site.clone();
    let prepared = selected.prepared;
    let bytes = if prepared.is_some() {
        None
    } else {
        match selected.payload.materialize() {
            Some(bytes) => Some(bytes),
            None => {
                return Err((
                    BodyCheckpointBuilder::empty(key.to_string(), source.to_string()),
                    "corrupt compressed payload",
                ));
            }
        }
    };
    let shipper = selected.shipper;
    let kinds = selected.kinds;
    let dependencies = selected.dependencies.clone();
    let mut builder = BodyCheckpointBuilder::from_prefix(
        index,
        source.to_string(),
        site.byte_offset,
    );
    if let (Some(entry), Some(bytes)) = (builder.entries.last_mut(), bytes.as_ref()) {
        entry.payload.hot = Some(Rc::clone(bytes));
    }
    Ok(BodyCheckpointHit {
        site,
        bytes,
        prepared,
        dependencies,
        builder,
        changed,
        shipper,
        kinds,
        dependency_changed,
    })
}

thread_local! {
    static HOT_BODY_CHECKPOINTS: RefCell<Option<BodyCheckpointIndex>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug)]
struct CheckpointTarget {
    offset: usize,
    kinds: BoundaryKinds,
    prepare: bool,
}

fn body_checkpoint_targets(source: &str) -> Vec<CheckpointTarget> {
    let mut targets = Vec::with_capacity(BODY_CHECKPOINT_FRACTIONS.len());
    for numerator in BODY_CHECKPOINT_FRACTIONS {
        let mut offset = source.len().saturating_mul(numerator) / BODY_CHECKPOINT_DENOMINATOR;
        while !source.is_char_boundary(offset) {
            offset -= 1;
        }
        targets.push(CheckpointTarget {
            offset,
            kinds: BoundaryKinds::paragraph(),
            prepare: numerator == BODY_PREPARED_NUMERATOR,
        });
    }
    // Structural targets are merely requests. The engine remains the authority
    // and pauses only after a complete paragraph at a safe outer-loop point.
    // These hints put retained entries near the constructs humans edit most.
    for (needle, kind) in [
        ("\\section", BoundaryKinds::SECTION),
        ("\\chapter", BoundaryKinds::SECTION),
        ("\\part", BoundaryKinds::SECTION),
        ("\\input", BoundaryKinds::INCLUDE),
        ("\\include", BoundaryKinds::INCLUDE),
    ] {
        for (offset, _) in source.match_indices(needle) {
            targets.push(CheckpointTarget {
                offset,
                kinds: BoundaryKinds(BoundaryKinds::PARAGRAPH | kind),
                prepare: false,
            });
        }
    }
    targets.sort_by_key(|target| target.offset);
    let mut merged: Vec<CheckpointTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        if let Some(last) = merged.last_mut()
            && last.offset == target.offset
        {
            last.kinds.0 |= target.kinds.0;
            last.prepare |= target.prepare;
        } else {
            merged.push(target);
        }
    }
    merged
}

/// Run the remaining body while collecting a bounded set of paragraph-safe
/// checkpoints. A checkpoint refusal is an optimization miss: execution still
/// proceeds to ordinary EOF from either the project snapshot or a resumed
/// checkpoint.
fn run_body_with_checkpoints(
    engine: &mut Fast,
    source: &str,
    builder: &mut BodyCheckpointBuilder,
    resumed_at: Option<aitex_engine::fast::CheckpointSite>,
    shipper: &Rc<RefCell<Shipper>>,
    read_log: &Rc<RefCell<Vec<CacheDependency>>>,
    capture_limit: Option<usize>,
) -> bool {
    let resumed = resumed_at.is_some();
    let mut started = resumed;
    let mut completed = false;
    let mut through = resumed_at.as_ref().map_or(0, |site| site.byte_offset);
    let mut captured = 0usize;

    let targets = if capture_limit == Some(0) && !builder.reconvergence.is_empty() {
        builder.reconvergence_targets()
    } else {
        body_checkpoint_targets(source)
    };
    for target in targets {
        if capture_limit.is_some_and(|limit| captured >= limit)
            && builder.reconvergence.is_empty()
        {
            engine.main_loop();
            completed = true;
            break;
        }
        if target.offset <= through && started {
            continue;
        }
        let result = if started {
            engine.continue_until_checkpoint(target.offset)
        } else {
            started = true;
            engine.run_until_checkpoint(source, target.offset)
        };
        match result {
            Ok(Some(site)) => {
                through = site.byte_offset;
                let semantic_digest = engine
                    .reconvergence_state()
                    .ok()
                    .map(|bytes| sha256(&bytes));
                if builder.reconverged(&site, semantic_digest, &shipper.borrow()) {
                    return true;
                }
                if !capture_limit.is_some_and(|limit| captured >= limit)
                    && let Ok(bytes) = engine.checkpoint()
                {
                    if builder.capture(
                        site,
                        &bytes,
                        &shipper.borrow(),
                        normalized_dependencies(&read_log.borrow()),
                        target.kinds,
                        semantic_digest,
                        target.prepare,
                    ) {
                        captured += 1;
                    }
                }
            }
            Ok(None) => {
                completed = true;
                break;
            }
            Err(_) if resumed => {
                engine.main_loop();
                completed = true;
                break;
            }
            Err(_) => {
                engine.run(source);
                completed = true;
                break;
            }
        }
    }
    if !completed {
        if started {
            engine.main_loop();
        } else {
            engine.run(source);
        }
    }
    false
}

#[cfg(test)]
mod incremental_tests {
    use super::*;

    fn site(offset: usize) -> aitex_engine::fast::CheckpointSite {
        aitex_engine::fast::CheckpointSite {
            source_name: String::new(),
            file_index: 0,
            char_offset: offset,
            byte_offset: offset,
            line: 1,
            paragraph_serial: offset as u64,
            pages_shipped: 0,
        }
    }

    fn shipper() -> Shipper {
        Shipper::new("test.tex", false, false)
    }

    #[test]
    fn source_diff_preserves_utf8_boundaries() {
        assert_eq!(
            source_diff("aéz", "aêz"),
            Some(SourceDiff {
                unchanged_prefix: 1,
                old_changed_end: 3,
                new_changed_end: 3,
            })
        );
        assert_eq!(source_diff("same", "same"), None);
    }

    #[test]
    fn reconvergence_prefilter_accepts_only_whitespace_or_comment_edits() {
        let old = "text\n% BASELINE\nmore\n";
        let comment = "text\n% MUTATION\nmore\n";
        let prose = "next\n% BASELINE\nmore\n";
        let spacing = "text\n% BASELINE\n\nmore\n";
        let comment_diff = source_diff(old, comment).unwrap();
        assert!(source_diff_is_trivia(old, comment, comment_diff));
        let prose_diff = source_diff(old, prose).unwrap();
        assert!(!source_diff_is_trivia(old, prose, prose_diff));
        let spacing_diff = source_diff(old, spacing).unwrap();
        assert!(source_diff_is_trivia(old, spacing, spacing_diff));
    }

    #[test]
    fn checkpoint_chunks_round_trip_and_deduplicate_shifted_payloads() {
        let mut first = vec![b'a'; CHUNK_MAX * 2];
        first.extend(vec![b'b'; CHUNK_MAX * 2]);
        let mut second = vec![b'a'; CHUNK_MAX * 2 + 37];
        second.extend(vec![b'b'; CHUNK_MAX * 2]);
        let mut builder = BodyCheckpointBuilder::empty("key".into(), "source".into());
        assert!(builder.capture(site(1), &first, &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        assert!(builder.capture(site(2), &second, &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        assert_eq!(builder.entries[0].payload.materialize().unwrap().as_ref(), first);
        assert_eq!(builder.entries[1].payload.materialize().unwrap().as_ref(), second);
        assert!(builder.unique_bytes < first.len() + second.len());
    }

    #[test]
    fn checkpoint_chunks_reject_corrupt_compressed_payloads() {
        let mut builder = BodyCheckpointBuilder::empty("key".into(), "source".into());
        assert!(builder.capture(site(1), b"checkpoint payload", &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        builder.entries[0].payload.hot = None;
        builder.entries[0].payload.chunks[0].digest[0] ^= 1;
        assert!(builder.entries[0].payload.materialize().is_none());
    }

    #[test]
    fn completed_output_reuse_checks_every_artifact_digest() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "aitex-completed-output-{}-{id}",
            std::process::id()
        ));
        let primary = base.with_extension("dvi");
        let side = base.with_extension("aux");
        std::fs::write(&primary, b"final dvi").unwrap();
        std::fs::write(&side, b"stable aux").unwrap();
        let primary_name = primary.to_string_lossy().into_owned();
        let outputs = CompletedOutputs::capture(
            primary_name.clone(),
            vec![primary_name.clone(), side.to_string_lossy().into_owned()],
        )
        .unwrap();
        assert!(outputs.matches(&primary_name));
        std::fs::write(&side, b"mutant aux").unwrap();
        assert!(!outputs.matches(&primary_name));
        let _ = std::fs::remove_file(primary);
        let _ = std::fs::remove_file(side);
    }

    #[test]
    fn a_diff_selects_the_nearest_strictly_preceding_checkpoint() {
        let old = "0123456789abcdefghij";
        let new = "0123456789abcXefghij";
        let mut builder = BodyCheckpointBuilder::empty("key".into(), old.into());
        assert!(builder.capture(site(5), b"five", &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        assert!(builder.capture(site(10), b"ten", &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        assert!(builder.capture(site(15), b"fifteen", &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        let index = builder.finish(Vec::new(), None, None);
        let Ok(hit) = select_body_checkpoint(index, "key", new, (0, 0, 0, 0, 0, 0), &[]) else {
            panic!("checkpoint hit");
        };
        assert_eq!(hit.site.byte_offset, 10);
        assert_eq!(hit.bytes.as_deref().unwrap(), b"ten");
        assert_eq!(hit.changed.unwrap().unchanged_prefix, 13);
        assert_eq!(hit.builder.entries.len(), 1);
    }

    #[test]
    fn checkpoint_targets_are_utf8_boundaries() {
        let source = "é".repeat(BODY_CHECKPOINT_DENOMINATOR * 3);
        let targets = body_checkpoint_targets(&source);
        assert!(targets.iter().all(|target| source.is_char_boundary(target.offset)));
        assert!(targets.windows(2).all(|pair| pair[0].offset < pair[1].offset));
    }

    #[test]
    fn bounded_index_keeps_checkpoints_distributed() {
        let mut builder = BodyCheckpointBuilder::empty("key".into(), "source".into());
        for offset in 0..BODY_CHECKPOINT_LIMIT + 8 {
            assert!(builder.capture(site(offset), &[offset as u8], &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        }
        let offsets = builder
            .entries
            .iter()
            .map(|entry| entry.site.byte_offset)
            .collect::<Vec<_>>();
        assert_eq!(offsets.len(), BODY_CHECKPOINT_LIMIT);
        assert_eq!(offsets.first(), Some(&0));
        assert_eq!(offsets.last(), Some(&(BODY_CHECKPOINT_LIMIT + 7)));
        assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn clock_sensitive_index_is_invalidated() {
        let mut builder = BodyCheckpointBuilder::empty("key".into(), "source".into());
        assert!(builder.capture(site(1), b"checkpoint", &shipper(), Vec::new(), BoundaryKinds::paragraph(), None, false));
        let index = builder.finish(Vec::new(), Some((1, 2, 3, 4, 0, 0)), None);
        let Err((_, reason)) =
            select_body_checkpoint(index, "key", "source", (5, 2, 3, 4, 0, 0), &[])
        else {
            panic!("clock change must miss");
        };
        assert_eq!(reason, "clock changed");
    }

    #[test]
    fn fixpoint_inputs_cover_tex_bibliography_index_and_glossary_protocols() {
        for extension in [
            "aux", "toc", "lof", "lot", "out", "brf", "bcf", "idx", "glo", "glsdefs",
            "acn", "nlo", "nav", "snm",
        ] {
            assert!(is_fixpoint_extension(extension), "{extension}");
        }
        for extension in ["log", "dvi", "pdf", "synctex", "tmp"] {
            assert!(!is_fixpoint_extension(extension), "{extension}");
        }
    }

    #[test]
    fn a_changed_late_dependency_selects_an_entry_before_it_was_read() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aitex-incremental-dependency-{}-{id}.tex",
            std::process::id()
        ));
        std::fs::write(&path, b"first").unwrap();
        let name = path.to_string_lossy().into_owned();
        let dependency = CacheDependency::from_resolution(&name, Some(&name));
        let mut builder = BodyCheckpointBuilder::empty("key".into(), "source".into());
        assert!(builder.capture(
            site(1),
            b"before",
            &shipper(),
            Vec::new(),
            BoundaryKinds::paragraph(),
            None,
            false,
        ));
        assert!(builder.capture(
            site(2),
            b"after",
            &shipper(),
            vec![dependency],
            BoundaryKinds(BoundaryKinds::PARAGRAPH | BoundaryKinds::INCLUDE),
            None,
            false,
        ));
        std::fs::write(&path, b"second").unwrap();
        let Ok(hit) = select_body_checkpoint(
            builder.finish(Vec::new(), None, None),
            "key",
            "source",
            (0, 0, 0, 0, 0, 0),
            &[],
        ) else {
            panic!("the checkpoint before the dependency read must remain valid");
        };
        assert_eq!(hit.site.byte_offset, 1);
        let _ = std::fs::remove_file(path);
    }
}
