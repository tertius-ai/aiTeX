use std::io::Write as _;
use std::path::{Path, PathBuf};

const CACHE_SCHEMA: u32 = 7;
const CACHE_MANIFEST_MAGIC: &str = "AITEX-CACHE-MANIFEST";
const MAX_CACHE_OBJECT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CACHE_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_CLASS_CACHE_MIN_MS: u64 = 25;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FileSignature {
    bytes: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanos: i64,
}

fn file_signature(metadata: &std::fs::Metadata) -> FileSignature {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    FileSignature {
        bytes: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        changed_seconds: metadata.ctime(),
        #[cfg(unix)]
        changed_nanos: metadata.ctime_nsec(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheKind {
    Format,
    Class,
    Project,
    Output,
}

impl CacheKind {
    fn label(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Class => "class",
            Self::Project => "project",
            Self::Output => "output",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "format" => Some(Self::Format),
            "class" => Some(Self::Class),
            "project" => Some(Self::Project),
            "output" => Some(Self::Output),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CacheDependency {
    request: String,
    resolved: Option<String>,
    sha256: Option<String>,
    bytes: Option<u64>,
    modified_unix_nanos: Option<u128>,
    changed_unix_nanos: Option<i128>,
    metadata_sensitive: bool,
}

impl CacheDependency {
    fn from_resolution(request: &str, resolved: Option<&str>) -> Self {
        let sha256 = resolved.and_then(|path| file_sha256(Path::new(path)).ok());
        let metadata = resolved.and_then(|path| std::fs::metadata(path).ok());
        Self {
            request: request.to_string(),
            resolved: resolved.map(str::to_string),
            sha256,
            bytes: metadata.as_ref().map(std::fs::Metadata::len),
            modified_unix_nanos: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos()),
            changed_unix_nanos: metadata.as_ref().and_then(metadata_change_nanos),
            metadata_sensitive: false,
        }
    }

    fn from_metadata_resolution(request: &str, resolved: Option<&str>) -> Self {
        let mut dependency = Self::from_resolution(request, resolved);
        dependency.metadata_sensitive = true;
        dependency
    }
}

fn metadata_change_nanos(metadata: &std::fs::Metadata) -> Option<i128> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some(metadata.ctime() as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

#[derive(Debug, Eq, PartialEq)]
struct CacheManifest {
    kind: CacheKind,
    key: String,
    payload_bytes: u64,
    payload_sha256: String,
    created_unix: u64,
    clock: Option<WallClock>,
    dependencies: Vec<CacheDependency>,
}

#[derive(Debug)]
struct PendingCache {
    root: PathBuf,
    kind: CacheKind,
    key: String,
}

#[derive(Debug)]
struct CacheHit {
    bytes: Rc<Vec<u8>>,
    output_current: bool,
    dependencies: Vec<CacheDependency>,
    clock: Option<WallClock>,
    load_time: std::time::Duration,
}

#[derive(Debug)]
enum CacheLookup {
    Hit(CacheHit),
    Miss(String),
    Invalid(String),
}

const CLOCK_WILDCARD: i64 = -1;

fn clock_dependency(clock: WallClock, reads: u8) -> Option<WallClock> {
    (reads != 0).then(|| {
        let values = [clock.0, clock.1, clock.2, clock.3, clock.4, clock.5];
        // Seconds and UTC offset are both observable only through
        // `\pdfcreationdate`, represented by the fifth dependency bit.
        let masks = [1, 2, 4, 8, 16, 16];
        let selected: [i64; 6] = core::array::from_fn(|index| {
            if reads & masks[index] != 0 {
                values[index]
            } else {
                CLOCK_WILDCARD
            }
        });
        (
            selected[0],
            selected[1],
            selected[2],
            selected[3],
            selected[4],
            selected[5],
        )
    })
}

fn clock_read_mask(clock: Option<WallClock>) -> u8 {
    let Some(clock) = clock else { return 0 };
    [clock.0, clock.1, clock.2, clock.3, clock.4, clock.5]
        .into_iter()
        .enumerate()
        .fold(0, |mask, (index, value)| {
            let component = [1, 2, 4, 8, 16, 16][index];
            mask | (u8::from(value != CLOCK_WILDCARD) * component)
        })
}

fn clock_matches(expected: WallClock, actual: WallClock) -> bool {
    [
        (expected.0, actual.0),
        (expected.1, actual.1),
        (expected.2, actual.2),
        (expected.3, actual.3),
        (expected.4, actual.4),
        (expected.5, actual.5),
    ]
    .into_iter()
    .all(|(expected, actual)| expected == CLOCK_WILDCARD || expected == actual)
}

fn cache_paths(root: &Path, kind: CacheKind, key: &str) -> (PathBuf, PathBuf, PathBuf) {
    let stem = format!("{}-{key}", kind.label());
    (
        root.join(format!("{stem}.asnap")),
        root.join(format!("{stem}.manifest")),
        root.join(format!("{stem}.lock")),
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

fn unhex(value: &str) -> Option<Vec<u8>> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut result = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        result.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(result)
}

fn encode_string(value: &str) -> String {
    hex(value.as_bytes())
}

fn decode_string(value: &str) -> Option<String> {
    String::from_utf8(unhex(value)?).ok()
}
/// Hardware-accelerated SHA-256 for cache identity and corruption detection.
/// Cache collisions are correctness bugs, not merely slow misses. Snapshot
/// payloads are tens of megabytes, so a scalar round loop also makes every hit
/// slower than simply executing a small class; `ring` selects the platform's
/// optimized implementation.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut result = [0u8; 32];
    result.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref());
    result
}
fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha256(bytes))
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_CACHE_OBJECT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds cache object limit",
        ));
    }
    std::fs::read(path).map(|bytes| sha256_hex(&bytes))
}

thread_local! {
    /// Verified immutable payloads retained by `aitex watch`. A one-shot CLI
    /// still drops this map with its process, while a watch rebuild avoids the
    /// disk read and checksum pass after the first verified hit.
    static HOT_PAYLOADS: std::cell::RefCell<
        std::collections::HashMap<(PathBuf, String), (FileSignature, Rc<Vec<u8>>)>
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    /// The base format participates in every key. Rehash it after a metadata
    /// change, but not for every request handled by one watch process.
    static HOT_FORMAT_IDENTITIES: std::cell::RefCell<
        std::collections::HashMap<PathBuf, (FileSignature, String)>
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn key_field(buffer: &mut Vec<u8>, name: &str, value: &[u8]) {
    buffer.extend_from_slice(name.as_bytes());
    buffer.push(0);
    buffer.extend_from_slice(&value.len().to_le_bytes());
    buffer.extend_from_slice(value);
}

fn os_str_cache_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        value
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}

fn relevant_environment() -> Vec<(String, Vec<u8>)> {
    // `std::env::vars` panics when *any* inherited value is not Unicode. A
    // perfectly valid Unix working directory can put such bytes in `PWD`, and
    // cache-key construction must not make the compiler unusable there. Keep
    // the selected ASCII variable names, but hash their platform-native bytes.
    let mut values: Vec<_> = std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            let relevant =
            name.starts_with("TEX")
                || name.starts_with("KPSE")
                || matches!(
                    name.as_str(),
                    "BIBINPUTS"
                        | "BSTINPUTS"
                        | "TFMFONTS"
                        | "VARTEXFONTS"
                        | "SOURCE_DATE_EPOCH"
                        | "FORCE_SOURCE_DATE"
                        | "TZ"
                        | "LC_ALL"
                        | "LANG"
                        | "AITEX_FONT_PATH"
                );
            relevant.then(|| (name, os_str_cache_bytes(&value)))
        })
        .collect();
    values.sort();
    values
}

#[allow(clippy::too_many_arguments)]
fn snapshot_key(
    kind: CacheKind,
    prefix: &str,
    job_name: Option<&str>,
    format_identity: &str,
    pdf_driver: bool,
    tex82: bool,
    max_print_line: usize,
) -> String {
    let mut input = Vec::with_capacity(prefix.len() + 512);
    key_field(&mut input, "cache-schema", &CACHE_SCHEMA.to_le_bytes());
    key_field(&mut input, "kind", kind.label().as_bytes());
    key_field(
        &mut input,
        "compiler-version",
        env!("CARGO_PKG_VERSION").as_bytes(),
    );
    key_field(
        &mut input,
        "compiler-features",
        if cfg!(feature = "counters") {
            b"counters".as_slice()
        } else {
            b"default".as_slice()
        },
    );
    key_field(&mut input, "format", format_identity.as_bytes());
    key_field(&mut input, "source-prefix", prefix.as_bytes());
    key_field(
        &mut input,
        "job-sensitive",
        &[u8::from(job_name.is_some())],
    );
    key_field(
        &mut input,
        "job-name",
        job_name.unwrap_or("").as_bytes(),
    );
    key_field(&mut input, "pdf-driver", &[pdf_driver as u8]);
    if pdf_driver {
        key_field(
            &mut input,
            "pdf-stream-format",
            pdf_stream_format_tag(pdf_options_from_environment()).as_bytes(),
        );
    }
    key_field(&mut input, "tex82", &[tex82 as u8]);
    key_field(&mut input, "unicode-mode", &[u8::from(unicode_flag())]);
    key_field(
        &mut input,
        "max-print-line",
        &max_print_line.to_le_bytes(),
    );
    for (name, value) in relevant_environment() {
        key_field(&mut input, &format!("env:{name}"), &value);
    }
    sha256_hex(&input)
}

#[allow(clippy::too_many_arguments)]
fn output_cache_key(
    source: &str,
    input_request: &str,
    output: &str,
    job_name: &str,
    format_identity: &str,
    format_request: Option<&str>,
    pdf_driver: bool,
    tex82: bool,
    max_print_line: usize,
) -> String {
    let base = snapshot_key(
        CacheKind::Output,
        source,
        Some(job_name),
        format_identity,
        pdf_driver,
        tex82,
        max_print_line,
    );
    let mut identity = Vec::with_capacity(base.len() + input_request.len() + output.len() + 128);
    key_field(&mut identity, "base", base.as_bytes());
    // These spellings affect observable artifacts even when their content
    // identities agree: the transcript records the input and format request,
    // while the requested primary path decides what is restored.
    key_field(&mut identity, "input-request", input_request.as_bytes());
    key_field(&mut identity, "output-request", output.as_bytes());
    key_field(
        &mut identity,
        "format-request",
        format_request.unwrap_or("").as_bytes(),
    );
    sha256_hex(&identity)
}

fn base_format_identity(path: Option<&str>) -> String {
    match path {
        None => "none".to_string(),
        Some(path) => {
            let path = PathBuf::from(path);
            let metadata = std::fs::metadata(&path).ok();
            let signature = metadata.as_ref().map(file_signature);
            if let Some(signature) = signature.as_ref()
                && let Some(identity) = HOT_FORMAT_IDENTITIES.with(|cache| {
                    cache
                        .borrow()
                        .get(&path)
                        .filter(|(old_signature, _)| old_signature == signature)
                        .map(|(_, identity)| identity.clone())
                })
            {
                return identity;
            }
            let identity = match file_sha256(&path) {
                Ok(digest) => format!("sha256:{digest}"),
                Err(_) => format!("unreadable:{}", encode_string(path.to_string_lossy().as_ref())),
            };
            if let Some(signature) = signature {
                HOT_FORMAT_IDENTITIES.with(|cache| {
                    cache
                        .borrow_mut()
                        .insert(path, (signature, identity.clone()));
                });
            }
            identity
        }
    }
}

fn manifest_text(manifest: &CacheManifest) -> String {
    let mut result = format!(
        "{CACHE_MANIFEST_MAGIC} {CACHE_SCHEMA}\nkind {}\nkey {}\npayload-bytes {}\npayload-sha256 {}\ncreated-unix {}\nclock {}\n",
        manifest.kind.label(),
        manifest.key,
        manifest.payload_bytes,
        manifest.payload_sha256,
        manifest.created_unix,
        manifest.clock.map_or_else(
            || "-".to_string(),
            |(time, day, month, year, zone, second)| {
                format!("{time},{day},{month},{year},{zone},{second}")
            },
        ),
    );
    for dependency in &manifest.dependencies {
        result.push_str("dep ");
        result.push_str(&encode_string(&dependency.request));
        result.push(' ');
        result.push_str(
            &dependency
                .resolved
                .as_deref()
                .map(encode_string)
                .unwrap_or_else(|| "-".to_string()),
        );
        result.push(' ');
        result.push_str(dependency.sha256.as_deref().unwrap_or("absent"));
        result.push(' ');
        result.push_str(
            &dependency
                .bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        result.push(' ');
        result.push_str(
            &dependency
                .modified_unix_nanos
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        result.push(' ');
        result.push_str(
            &dependency
                .changed_unix_nanos
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        result.push(' ');
        result.push_str(if dependency.metadata_sensitive {
            "metadata"
        } else {
            "content"
        });
        result.push('\n');
    }
    result
}

fn parse_manifest(text: &str) -> Result<CacheManifest, String> {
    let mut lines = text.lines();
    let header = lines.next().ok_or("empty manifest")?;
    if header != format!("{CACHE_MANIFEST_MAGIC} {CACHE_SCHEMA}") {
        return Err("unknown manifest schema".to_string());
    }
    let mut kind = None;
    let mut key = None;
    let mut payload_bytes = None;
    let mut payload_sha256 = None;
    let mut created_unix = None;
    let mut clock = None;
    let mut clock_seen = false;
    let mut dependencies = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["kind", value] => kind = CacheKind::parse(value),
            ["key", value] if value.len() == 64 => key = Some((*value).to_string()),
            ["payload-bytes", value] => payload_bytes = value.parse().ok(),
            ["payload-sha256", value] if value.len() == 64 => {
                payload_sha256 = Some((*value).to_string());
            }
            ["created-unix", value] => created_unix = value.parse().ok(),
            ["clock", value] => {
                clock_seen = true;
                if *value != "-" {
                    let values: Vec<_> = value.split(',').collect();
                    if let [time, day, month, year, zone, second] = values.as_slice() {
                        clock = Some((
                            time.parse().map_err(|_| "invalid clock")?,
                            day.parse().map_err(|_| "invalid clock")?,
                            month.parse().map_err(|_| "invalid clock")?,
                            year.parse().map_err(|_| "invalid clock")?,
                            zone.parse().map_err(|_| "invalid clock")?,
                            second.parse().map_err(|_| "invalid clock")?,
                        ));
                    } else {
                        return Err("invalid clock".to_string());
                    }
                }
            }
            ["dep", request, resolved, digest, bytes, modified, changed, sensitivity] => {
                let request = decode_string(request).ok_or("invalid dependency request")?;
                let resolved = if *resolved == "-" {
                    None
                } else {
                    Some(decode_string(resolved).ok_or("invalid dependency path")?)
                };
                let sha256 = if *digest == "absent" {
                    None
                } else if digest.len() == 64 {
                    Some((*digest).to_string())
                } else {
                    return Err("invalid dependency digest".to_string());
                };
                if resolved.is_some() != sha256.is_some() {
                    return Err("inconsistent dependency state".to_string());
                }
                let bytes = if *bytes == "-" {
                    None
                } else {
                    Some(bytes.parse().map_err(|_| "invalid dependency size")?)
                };
                let modified_unix_nanos = if *modified == "-" {
                    None
                } else {
                    Some(
                        modified
                            .parse()
                            .map_err(|_| "invalid dependency modification time")?,
                    )
                };
                if modified_unix_nanos.is_some() && resolved.is_none() {
                    return Err("absent dependency has modification time".to_string());
                }
                let changed_unix_nanos = if *changed == "-" {
                    None
                } else {
                    Some(
                        changed
                            .parse()
                            .map_err(|_| "invalid dependency change time")?,
                    )
                };
                let metadata_sensitive = match *sensitivity {
                    "content" => false,
                    "metadata" => true,
                    _ => return Err("invalid dependency sensitivity".to_string()),
                };
                if resolved.is_none()
                    && (bytes.is_some()
                        || changed_unix_nanos.is_some()
                        || metadata_sensitive)
                {
                    return Err("absent dependency has file metadata".to_string());
                }
                dependencies.push(CacheDependency {
                    request,
                    resolved,
                    sha256,
                    bytes,
                    modified_unix_nanos,
                    changed_unix_nanos,
                    metadata_sensitive,
                });
            }
            _ => return Err(format!("invalid manifest line: {line}")),
        }
    }
    let manifest = CacheManifest {
        kind: kind.ok_or("manifest has no kind")?,
        key: key.ok_or("manifest has no key")?,
        payload_bytes: payload_bytes.ok_or("manifest has no payload size")?,
        payload_sha256: payload_sha256.ok_or("manifest has no payload checksum")?,
        created_unix: created_unix.ok_or("manifest has no creation time")?,
        clock: if clock_seen {
            clock
        } else {
            return Err("manifest has no clock dependency".to_string());
        },
        dependencies,
    };
    if manifest.payload_bytes > MAX_CACHE_OBJECT_BYTES {
        return Err("payload exceeds cache object limit".to_string());
    }
    Ok(manifest)
}

fn dependency_holds(dependency: &CacheDependency) -> Result<(), String> {
    let now = cached_resolution_candidate(dependency)
        .unwrap_or_else(|| resolve_cache_dependency(&dependency.request));
    dependency_holds_at(dependency, now)
}

/// Resolve the cases that can change during ordinary document editing without
/// asking kpathsea to rediscover an already-known TeX Live path.
///
/// A local candidate is always checked first, so adding a project-local
/// package or document class immediately shadows the stored system path. An
/// existing external absolute path remains selected while the resolution
/// environment (part of the cache key) is unchanged; edits, replacement, and
/// deletion are still caught by [`dependency_holds_at`]'s metadata and digest
/// checks. This makes the common "nothing changed" path proportional to cheap
/// `stat` calls instead of hundreds of interactive kpathsea round trips.
///
/// Relative stored paths and generated PK fonts remain ambiguous and return
/// `None` so the caller performs the full resolver lookup.
fn cached_resolution_candidate(dependency: &CacheDependency) -> Option<Option<String>> {
    if dependency.request.starts_with("@pk:") {
        return None;
    }
    if let Some(local) = resolve_local_path(&dependency.request, ".tex") {
        return Some(Some(local));
    }
    match dependency.resolved.as_deref() {
        Some(path) if Path::new(path).is_absolute() => Some(Some(path.to_string())),
        None => Some(None),
        Some(_) => None,
    }
}

fn dependency_holds_at(
    dependency: &CacheDependency,
    now: Option<String>,
) -> Result<(), String> {
    if now != dependency.resolved {
        return Err(format!("resolution changed for {}", dependency.request));
    }
    match (&now, &dependency.sha256) {
        (None, None) => Ok(()),
        (Some(path), Some(expected_digest)) => {
            let metadata = std::fs::metadata(path)
                .map_err(|_| format!("dependency unreadable: {path}"))?;
            let bytes = Some(metadata.len());
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_nanos());
            let changed = metadata_change_nanos(&metadata);
            if dependency.metadata_sensitive && modified != dependency.modified_unix_nanos {
                return Err(format!("dependency metadata changed: {path}"));
            }
            let signature_unchanged = bytes == dependency.bytes
                && modified == dependency.modified_unix_nanos
                && changed == dependency.changed_unix_nanos;
            if !signature_unchanged {
                let actual = file_sha256(Path::new(path))
                    .map_err(|_| format!("dependency unreadable: {path}"))?;
                if &actual != expected_digest {
                    return Err(format!("dependency changed: {path}"));
                }
            }
            Ok(())
        }
        _ => Err(format!(
            "dependency state changed: {}",
            dependency.request
        )),
    }
}

fn resolve_cache_dependency(request: &str) -> Option<String> {
    let Some(qualified) = request.strip_prefix("@pk:") else {
        return resolve_path_hot(request, ".tex");
    };
    let (dpi, name) = qualified.split_once(':')?;
    if dpi.is_empty()
        || !dpi.bytes().all(|byte| byte.is_ascii_digit())
        || name.is_empty()
        || name.contains(['\n', '\r', '\0'])
    {
        return None;
    }
    resolve_pk_path_hot(dpi, name)
}

fn dependencies_hold(dependencies: &[CacheDependency]) -> Result<(), String> {
    let unresolved = dependencies
        .iter()
        .enumerate()
        .filter_map(|(index, dependency)| {
            cached_resolution_candidate(dependency)
                .is_none()
                .then_some((index, dependency.request.as_str()))
        })
        .collect::<Vec<_>>();
    let requests = unresolved
        .iter()
        .filter_map(|(index, request)| {
            (!dependencies[*index].request.starts_with("@pk:")).then_some(*request)
        })
        .collect::<Vec<_>>();
    let normal_resolutions = resolve_paths_hot(&requests, ".tex");
    let mut normal_resolutions = normal_resolutions.into_iter();
    let mut resolved = vec![None; dependencies.len()];
    for (index, _) in unresolved {
        let dependency = &dependencies[index];
        resolved[index] = Some(if dependency.request.starts_with("@pk:") {
            resolve_cache_dependency(&dependency.request)
        } else {
            normal_resolutions.next().flatten()
        });
    }
    for (index, dependency) in dependencies.iter().enumerate() {
        let now = cached_resolution_candidate(dependency)
            .unwrap_or_else(|| resolved[index].take().flatten());
        dependency_holds_at(dependency, now)?;
    }
    Ok(())
}

fn load_cache(
    root: &Path,
    kind: CacheKind,
    key: &str,
    clock: WallClock,
) -> CacheLookup {
    load_cache_impl(root, kind, key, clock, None)
}

fn load_output_cache(
    root: &Path,
    key: &str,
    clock: WallClock,
    output: &Path,
) -> CacheLookup {
    load_cache_impl(root, CacheKind::Output, key, clock, Some(output))
}

fn load_cache_impl(
    root: &Path,
    kind: CacheKind,
    key: &str,
    clock: WallClock,
    output: Option<&Path>,
) -> CacheLookup {
    let (payload_path, manifest_path, _) = cache_paths(root, kind, key);
    let manifest_metadata = match std::fs::metadata(&manifest_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CacheLookup::Miss("absent".to_string());
        }
        Err(error) => return CacheLookup::Invalid(format!("cannot stat manifest: {error}")),
    };
    if manifest_metadata.len() > MAX_CACHE_MANIFEST_BYTES {
        return CacheLookup::Invalid("manifest exceeds size limit".to_string());
    }
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) => return CacheLookup::Invalid(format!("cannot read manifest: {error}")),
    };
    let manifest = match parse_manifest(&text) {
        Ok(manifest) => manifest,
        Err(error) => return CacheLookup::Invalid(error),
    };
    if manifest.kind != kind || manifest.key != key {
        return CacheLookup::Invalid("manifest identity does not match its name".to_string());
    }
    if manifest
        .clock
        .is_some_and(|expected| !clock_matches(expected, clock))
    {
        return CacheLookup::Miss("clock changed".to_string());
    }
    let payload_metadata = match std::fs::metadata(&payload_path) {
        Ok(metadata) => metadata,
        Err(error) => return CacheLookup::Invalid(format!("payload is absent: {error}")),
    };
    if payload_metadata.len() != manifest.payload_bytes {
        return CacheLookup::Invalid("payload size does not match manifest".to_string());
    }
    if let Err(reason) = dependencies_hold(&manifest.dependencies) {
        return CacheLookup::Miss(reason);
    }
    let load_started = std::time::Instant::now();
    let output_current = output.is_some_and(|output| {
        std::fs::metadata(output).is_ok_and(|metadata| {
            metadata.len() == manifest.payload_bytes
                && file_sha256(output)
                    .is_ok_and(|digest| digest == manifest.payload_sha256)
        })
    });
    let bytes = if output_current {
        // The requested destination already contains the exact artifact named
        // by the manifest. The cache payload was checked for presence and
        // size above; avoid reading it merely to rewrite identical bytes.
        Rc::new(Vec::new())
    } else {
        let hot_key = (payload_path.clone(), manifest.payload_sha256.clone());
        let payload_signature = file_signature(&payload_metadata);
        let bytes = HOT_PAYLOADS.with(|cache| {
            cache
                .borrow()
                .get(&hot_key)
                .filter(|(signature, _)| signature == &payload_signature)
                .map(|(_, bytes)| Rc::clone(bytes))
        });
        match bytes {
            Some(bytes) => bytes,
            None => {
                let bytes = match std::fs::read(&payload_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return CacheLookup::Invalid(format!("cannot read payload: {error}"));
                    }
                };
                if sha256_hex(&bytes) != manifest.payload_sha256 {
                    return CacheLookup::Invalid("payload checksum mismatch".to_string());
                }
                let bytes = Rc::new(bytes);
                HOT_PAYLOADS.with(|cache| {
                    cache
                        .borrow_mut()
                        .insert(hot_key, (payload_signature, Rc::clone(&bytes)));
                });
                bytes
            }
        }
    };
    let load_time = load_started.elapsed();
    CacheLookup::Hit(CacheHit {
        bytes,
        output_current,
        dependencies: manifest.dependencies,
        clock: manifest.clock,
        load_time,
    })
}

struct CacheWriteLock {
    path: PathBuf,
}

impl Drop for CacheWriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn modified_age(path: &Path) -> Option<std::time::Duration> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

fn acquire_cache_lock(path: &Path) -> std::io::Result<Option<CacheWriteLock>> {
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                writeln!(file, "pid={} schema={CACHE_SCHEMA}", std::process::id())?;
                file.sync_all()?;
                return Ok(Some(CacheWriteLock {
                    path: path.to_path_buf(),
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt == 0
                    && modified_age(path).is_some_and(|age| age.as_secs() > 300)
                {
                    let _ = std::fs::remove_file(path);
                    continue;
                }
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("cache");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_file_name(format!(
        ".{name}.tmp-{}-{nonce}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalized_dependencies(log: &[CacheDependency]) -> Vec<CacheDependency> {
    let mut dependencies = log.to_vec();
    dependencies.sort_by(|left, right| {
        (
            &left.request,
            &left.resolved,
            &left.sha256,
            &left.bytes,
            &left.modified_unix_nanos,
            &left.changed_unix_nanos,
            &left.metadata_sensitive,
        )
            .cmp(&(
                &right.request,
                &right.resolved,
                &right.sha256,
                &right.bytes,
                &right.modified_unix_nanos,
                &right.changed_unix_nanos,
                &right.metadata_sensitive,
            ))
    });
    dependencies.dedup();
    dependencies
}

fn write_cache(
    pending: &PendingCache,
    bytes: &[u8],
    dependencies: &[CacheDependency],
    clock: Option<WallClock>,
) -> Result<bool, String> {
    if bytes.len() as u64 > MAX_CACHE_OBJECT_BYTES {
        return Err("snapshot exceeds cache object limit".to_string());
    }
    std::fs::create_dir_all(&pending.root)
        .map_err(|error| format!("cannot create cache directory: {error}"))?;
    let (payload_path, manifest_path, lock_path) =
        cache_paths(&pending.root, pending.kind, &pending.key);
    let Some(lock) = acquire_cache_lock(&lock_path)
        .map_err(|error| format!("cannot lock cache entry: {error}"))?
    else {
        return Ok(false);
    };
    let manifest = CacheManifest {
        kind: pending.kind,
        key: pending.key.clone(),
        payload_bytes: bytes.len() as u64,
        payload_sha256: sha256_hex(bytes),
        created_unix: now_unix(),
        clock,
        dependencies: normalized_dependencies(dependencies),
    };
    // Publish the manifest last. A racing reader sees an old consistent pair
    // or a checksum miss, never a trusted partial write.
    atomic_write(&payload_path, bytes)
        .map_err(|error| format!("cannot publish cache payload: {error}"))?;
    atomic_write(&manifest_path, manifest_text(&manifest).as_bytes())
        .map_err(|error| format!("cannot publish cache manifest: {error}"))?;
    drop(lock);
    let _ = enforce_cache_limit(&pending.root, cache_limit_bytes());
    Ok(true)
}

fn cache_limit_bytes() -> u64 {
    std::env::var("AITEX_CACHE_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CACHE_BYTES)
}

fn class_cache_minimum_duration() -> std::time::Duration {
    std::time::Duration::from_millis(
        std::env::var("AITEX_CLASS_CACHE_MIN_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_CLASS_CACHE_MIN_MS),
    )
}

fn force_class_cache_warm() -> bool {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    matches!(classify_front_door(&arguments), FrontDoor::CacheWarm)
}

fn remove_cache_pair(root: &Path, kind: CacheKind, key: &str) -> u64 {
    let (payload, manifest, _) = cache_paths(root, kind, key);
    let bytes = std::fs::metadata(&payload).map(|m| m.len()).unwrap_or(0)
        + std::fs::metadata(&manifest).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_file(payload);
    let _ = std::fs::remove_file(manifest);
    bytes
}

fn cache_entries(root: &Path) -> Vec<(CacheKind, String, PathBuf)> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("manifest") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some((kind, key)) = stem.split_once('-') else {
            continue;
        };
        if let Some(kind) = CacheKind::parse(kind)
            && key.len() == 64
            && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            result.push((kind, key.to_string(), path));
        }
    }
    result.sort_by(|left, right| left.2.cmp(&right.2));
    result
}

fn enforce_cache_limit(root: &Path, limit: u64) -> (usize, u64) {
    let mut entries = Vec::new();
    let mut total = 0u64;
    for (kind, key, manifest) in cache_entries(root) {
        let (payload, _, _) = cache_paths(root, kind, &key);
        let bytes = std::fs::metadata(&payload).map(|m| m.len()).unwrap_or(0)
            + std::fs::metadata(&manifest).map(|m| m.len()).unwrap_or(0);
        let modified = std::fs::metadata(&manifest)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        total = total.saturating_add(bytes);
        entries.push((modified, kind, key, bytes));
    }
    entries.sort_by_key(|entry| entry.0);
    let mut removed = 0usize;
    let mut reclaimed = 0u64;
    for (_, kind, key, bytes) in entries {
        if total <= limit {
            break;
        }
        remove_cache_pair(root, kind, &key);
        total = total.saturating_sub(bytes);
        removed += 1;
        reclaimed = reclaimed.saturating_add(bytes);
    }
    (removed, reclaimed)
}

fn inspect_cache(root: &Path) -> i32 {
    let entries = cache_entries(root);
    let mut valid = 0usize;
    let mut bytes = 0u64;
    for (kind, key, _) in &entries {
        match load_cache(root, *kind, key, wall_clock()) {
            CacheLookup::Hit(hit) => {
                valid += 1;
                bytes += hit.bytes.len() as u64;
                println!(
                    "{} {} valid bytes={} dependencies={}",
                    kind.label(),
                    key,
                    hit.bytes.len(),
                    hit.dependencies.len()
                );
            }
            CacheLookup::Miss(reason) => {
                println!("{} {} stale reason={reason}", kind.label(), key);
            }
            CacheLookup::Invalid(reason) => {
                println!("{} {} corrupt reason={reason}", kind.label(), key);
            }
        }
    }
    println!(
        "cache {}: entries={} valid={} payload-bytes={bytes}",
        root.display(),
        entries.len(),
        valid
    );
    0
}

fn prune_cache(root: &Path, limit: u64) -> i32 {
    let mut removed = 0usize;
    let mut reclaimed = 0u64;
    for (kind, key, _) in cache_entries(root) {
        if !matches!(
            load_cache(root, kind, &key, wall_clock()),
            CacheLookup::Hit(_)
        ) {
            reclaimed += remove_cache_pair(root, kind, &key);
            removed += 1;
        }
    }
    let (evicted, evicted_bytes) = enforce_cache_limit(root, limit);
    removed += evicted;
    reclaimed = reclaimed.saturating_add(evicted_bytes);
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
            if (name.contains(".tmp-") || name.ends_with(".lock"))
                && modified_age(&path).is_some_and(|age| age.as_secs() > 300)
            {
                reclaimed += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(path).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    println!(
        "cache {}: removed={} reclaimed-bytes={} limit={limit}",
        root.display(),
        removed,
        reclaimed
    );
    0
}

/// Where a document's preamble ends, through begin-document inclusive.
///
/// A byte lexer cannot prove this semantic boundary: a delimited argument can
/// swallow the sole literal marker while an alias or expansion executes the
/// real one later. The v0.2 RC therefore never splits a live source at a host-
/// guessed preamble boundary. Exact completed-output reuse remains available;
/// project/class/body snapshots return when the engine can publish an observed
/// `\document` checkpoint and a validated prefix pointer.
fn split_preamble(_src: &str) -> Option<(&str, &str)> {
    None
}

/// Find the safe shared-class boundary after a literal top-level
/// documentclass command. Unusual catcode-driven spellings do not receive a
/// class cache and fall back to the project snapshot.
fn split_documentclass(src: &str) -> Option<(&str, &str)> {
    fn skip_space_and_comments(bytes: &[u8], mut at: usize) -> usize {
        loop {
            while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            if bytes.get(at) == Some(&b'%') {
                while bytes.get(at).is_some_and(|byte| *byte != b'\n') {
                    at += 1;
                }
                continue;
            }
            return at;
        }
    }
    fn balanced(bytes: &[u8], at: usize, open: u8, close: u8) -> Option<usize> {
        if bytes.get(at) != Some(&open) {
            return None;
        }
        let mut depth = 0usize;
        let mut escaped = false;
        let mut comment = false;
        for (offset, byte) in bytes[at..].iter().copied().enumerate() {
            if comment {
                if byte == b'\n' {
                    comment = false;
                }
                continue;
            }
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'%' {
                comment = true;
            } else if byte == open {
                depth += 1;
            } else if byte == close {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at + offset + 1);
                }
            }
        }
        None
    }
    let bytes = src.as_bytes();
    let command = b"\\documentclass";
    let mut at = 0usize;
    let command_end = loop {
        if at + command.len() <= bytes.len()
            && &bytes[at..at + command.len()] == command
            && !bytes
                .get(at + command.len())
                .is_some_and(u8::is_ascii_alphabetic)
        {
            break at + command.len();
        }
        match bytes.get(at) {
            None => return None,
            Some(b'%') => {
                while bytes.get(at).is_some_and(|byte| *byte != b'\n') {
                    at += 1;
                }
            }
            Some(_) => at += 1,
        }
    };
    at = skip_space_and_comments(bytes, command_end);
    if bytes.get(at) == Some(&b'[') {
        at = balanced(bytes, at, b'[', b']')?;
        at = skip_space_and_comments(bytes, at);
    }
    at = balanced(bytes, at, b'{', b'}')?;
    at = skip_space_and_comments(bytes, at);
    Some((&src[..at], &src[at..]))
}

#[cfg(test)]
mod cache_tests {
    use super::{
        CacheDependency, CacheKind, CacheLookup, CacheManifest, PendingCache, cache_paths,
        cached_resolution_candidate, clock_dependency, load_cache, load_output_cache,
        manifest_text, os_str_cache_bytes, output_cache_key, parse_manifest, prune_cache, sha256_hex,
        snapshot_key, split_documentclass, split_preamble, write_cache,
    };

    fn temporary_cache(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aitex-cache-test-{label}-{}-{}",
            std::process::id(),
            super::now_unix()
        ))
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_identity_preserves_non_unicode_environment_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let value = std::ffi::OsString::from_vec(vec![b'/', 0xa6, 0xc2]);
        assert_eq!(os_str_cache_bytes(&value), vec![b'/', 0xa6, 0xc2]);
    }

    #[test]
    fn cache_manifest_round_trips_paths_with_spaces() {
        let manifest = CacheManifest {
            kind: CacheKind::Class,
            key: "a".repeat(64),
            payload_bytes: 123,
            payload_sha256: "b".repeat(64),
            created_unix: 7,
            clock: Some((61, 2, 3, 2026, -480, 17)),
            dependencies: vec![CacheDependency {
                request: "local config.tex".to_string(),
                resolved: Some("./local config.tex".to_string()),
                sha256: Some("c".repeat(64)),
                bytes: Some(123),
                modified_unix_nanos: Some(42),
                changed_unix_nanos: Some(43),
                metadata_sensitive: true,
            }],
        };
        assert_eq!(parse_manifest(&manifest_text(&manifest)).unwrap(), manifest);
    }

    #[test]
    fn stable_external_resolutions_do_not_require_kpathsea() {
        let dependency = CacheDependency {
            request: "__aitex_nonlocal_package_probe__.sty".to_string(),
            resolved: Some("/opt/texlive/texmf-dist/package.sty".to_string()),
            sha256: Some("a".repeat(64)),
            bytes: Some(1),
            modified_unix_nanos: Some(2),
            changed_unix_nanos: Some(3),
            metadata_sensitive: false,
        };
        assert_eq!(
            cached_resolution_candidate(&dependency),
            Some(Some("/opt/texlive/texmf-dist/package.sty".to_string()))
        );

        let absent = CacheDependency {
            resolved: None,
            sha256: None,
            bytes: None,
            modified_unix_nanos: None,
            changed_unix_nanos: None,
            ..dependency.clone()
        };
        assert_eq!(cached_resolution_candidate(&absent), Some(None));

        let relative = CacheDependency {
            resolved: Some("elsewhere/package.sty".to_string()),
            ..dependency.clone()
        };
        assert_eq!(cached_resolution_candidate(&relative), None);

        let generated = CacheDependency {
            request: "@pk:600:cmr10.pk".to_string(),
            ..dependency
        };
        assert_eq!(cached_resolution_candidate(&generated), None);
    }

    #[test]
    fn current_output_does_not_reload_identical_cache_payload() {
        let root = temporary_cache("current-output");
        let pending = PendingCache {
            root: root.clone(),
            kind: CacheKind::Output,
            key: "9".repeat(64),
        };
        let payload = b"cached artifact";
        let output = root.join("paper.pdf");
        assert_eq!(write_cache(&pending, payload, &[], None), Ok(true));
        std::fs::write(&output, payload).unwrap();

        let CacheLookup::Hit(hit) =
            load_output_cache(&root, &pending.key, (0, 0, 0, 0, 0, 0), &output)
        else {
            panic!("identical output should be a cache hit");
        };
        assert!(hit.output_current);
        assert!(hit.bytes.is_empty());

        std::fs::write(&output, b"different").unwrap();
        let CacheLookup::Hit(hit) =
            load_output_cache(&root, &pending.key, (0, 0, 0, 0, 0, 0), &output)
        else {
            panic!("changed output should still be restorable");
        };
        assert!(!hit.output_current);
        assert_eq!(hit.bytes.as_ref(), payload);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn documentclass_split_handles_options_comments_and_groups() {
        let source = "% lead\n\\documentclass [a={b,c}] {article} % tail\n\\usepackage{x}";
        let (class, rest) = split_documentclass(source).unwrap();
        assert!(class.ends_with("% tail\n"));
        assert_eq!(rest, "\\usepackage{x}");
        assert!(split_documentclass("% \\documentclass{fake}\nplain").is_none());
    }

    #[test]
    fn preamble_split_never_guesses_a_semantic_tex_boundary() {
        assert!(
            split_preamble("\\documentclass{article}\n\\begin{document}Body").is_none()
        );
    }

    #[test]
    fn preamble_split_rejects_comments_macros_and_delimited_arguments() {
        let source = concat!(
            "% \\begin{document} is only documentation\n",
            "\\def\\fake{\\begin{document}}\n",
            "\\begin{document}Body",
        );
        assert!(split_preamble(source).is_none());
        let delimited = concat!(
            "\\long\\def\\capture#1END{}\n",
            "\\capture \\begin{document} END\n",
            "\\begin {document}Real body",
        );
        assert!(split_preamble(delimited).is_none());
        let expanded = concat!(
            "\\let\\realbegin\\begin\n",
            "\\long\\def\\capture#1END{}\n",
            "\\capture \\begin{document} END\n",
            "\\realbegin{document}Real body",
        );
        assert!(split_preamble(expanded).is_none());
    }

    #[test]
    fn preamble_split_rejects_ambiguous_catcode_changes() {
        assert!(split_preamble("\\catcode`\\%=12 \\begin{document}Body").is_none());
        assert!(split_preamble("% \\begin{document}\nplain text").is_none());
    }

    #[test]
    fn snapshot_identity_includes_job_and_exact_class_options() {
        let key = |kind, prefix: &str, job: Option<&str>| {
            snapshot_key(
                kind,
                prefix,
                job,
                "sha256:format",
                false,
                false,
                79,
            )
        };
        let prefix = "\\documentclass[11pt]{article}";
        let shared = key(CacheKind::Class, prefix, None);
        assert_eq!(shared, key(CacheKind::Class, prefix, None));
        assert_ne!(shared, key(CacheKind::Class, prefix, Some("paper")));
        assert_ne!(
            shared,
            key(
                CacheKind::Class,
                "\\documentclass[12pt]{article}",
                None,
            )
        );
        assert_ne!(
            key(CacheKind::Project, prefix, Some("paper")),
            key(CacheKind::Project, prefix, Some("appendix")),
        );
    }

    #[test]
    fn output_identity_includes_every_observable_request() {
        let key = |source: &str, input: &str, output: &str, format: Option<&str>| {
            output_cache_key(
                source,
                input,
                output,
                "paper",
                "sha256:format",
                format,
                false,
                false,
                79,
            )
        };
        let baseline = key("source", "paper.tex", "paper.dvi", Some("latex.afmt"));
        assert_eq!(
            baseline,
            key("source", "paper.tex", "paper.dvi", Some("latex.afmt"))
        );
        assert_ne!(
            baseline,
            key("changed source", "paper.tex", "paper.dvi", Some("latex.afmt"))
        );
        assert_ne!(
            baseline,
            key("source", "./paper.tex", "paper.dvi", Some("latex.afmt"))
        );
        assert_ne!(
            baseline,
            key("source", "paper.tex", "other.dvi", Some("latex.afmt"))
        );
        assert_ne!(
            baseline,
            key("source", "paper.tex", "paper.dvi", Some("./latex.afmt"))
        );
    }

    #[test]
    fn corrupt_payload_is_a_miss_and_prune_removes_it() {
        let root = temporary_cache("corrupt");
        let pending = PendingCache {
            root: root.clone(),
            kind: CacheKind::Project,
            key: "d".repeat(64),
        };
        assert_eq!(write_cache(&pending, b"snapshot", &[], None), Ok(true));
        assert!(matches!(
            load_cache(
                &root,
                CacheKind::Project,
                &pending.key,
                (0, 0, 0, 0, 0, 0),
            ),
            CacheLookup::Hit(_)
        ));
        let (payload, _, _) = cache_paths(&root, CacheKind::Project, &pending.key);
        std::fs::write(payload, b"snapshox").unwrap();
        // Model a fresh process. A watch process may continue using the
        // already-verified immutable bytes it owns even if its cache file is
        // damaged; any later reader must reject the disk entry.
        super::HOT_PAYLOADS.with(|cache| cache.borrow_mut().clear());
        assert!(matches!(
            load_cache(
                &root,
                CacheKind::Project,
                &pending.key,
                (0, 0, 0, 0, 0, 0),
            ),
            CacheLookup::Invalid(_)
        ));
        assert_eq!(prune_cache(&root, u64::MAX), 0);
        assert!(super::cache_entries(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_existing_writer_lock_never_exposes_a_partial_entry() {
        let root = temporary_cache("lock");
        std::fs::create_dir_all(&root).unwrap();
        let pending = PendingCache {
            root: root.clone(),
            kind: CacheKind::Class,
            key: "e".repeat(64),
        };
        let (_, _, lock) = cache_paths(&root, CacheKind::Class, &pending.key);
        std::fs::write(lock, b"active").unwrap();
        assert_eq!(
            write_cache(&pending, b"snapshot", &[], None),
            Ok(false)
        );
        assert!(matches!(
            load_cache(
                &root,
                CacheKind::Class,
                &pending.key,
                (0, 0, 0, 0, 0, 0),
            ),
            CacheLookup::Miss(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clock_sensitive_entries_expire_but_ordinary_entries_do_not() {
        let root = temporary_cache("clock");
        let ordinary = PendingCache {
            root: root.clone(),
            kind: CacheKind::Class,
            key: "f".repeat(64),
        };
        let sensitive = PendingCache {
            root: root.clone(),
            kind: CacheKind::Project,
            key: "0".repeat(64),
        };
        let creation_date = PendingCache {
            root: root.clone(),
            kind: CacheKind::Output,
            key: "1".repeat(64),
        };
        let old = (60, 1, 2, 2026, -480, 7);
        let new = (61, 1, 2, 2026, -480, 7);
        let next_year = (61, 1, 2, 2027, -480, 7);
        let new_zone = (60, 1, 2, 2026, -420, 7);
        let new_second = (60, 1, 2, 2026, -480, 8);
        assert_eq!(write_cache(&ordinary, b"ordinary", &[], None), Ok(true));
        assert_eq!(
            write_cache(
                &sensitive,
                b"sensitive",
                &[],
                clock_dependency(old, 0b1000),
            ),
            Ok(true)
        );
        assert_eq!(
            write_cache(
                &creation_date,
                b"creation date",
                &[],
                clock_dependency(old, 0b1_1111),
            ),
            Ok(true)
        );
        assert!(matches!(
            load_cache(&root, CacheKind::Class, &ordinary.key, new),
            CacheLookup::Hit(_)
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Project, &sensitive.key, new),
            CacheLookup::Hit(_)
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Project, &sensitive.key, next_year),
            CacheLookup::Miss(reason) if reason == "clock changed"
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Output, &creation_date.key, new_zone),
            CacheLookup::Miss(reason) if reason == "clock changed"
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Output, &creation_date.key, new_second),
            CacheLookup::Miss(reason) if reason == "clock changed"
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn class_config_package_upgrade_and_new_files_invalidate_entries() {
        let root = temporary_cache("dependencies");
        let class_config = root.join("local-class.cfg");
        let package = root.join("package.sty");
        let absent = root.join("absent.tex");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&class_config, b"old class configuration").unwrap();
        std::fs::write(&package, b"old package").unwrap();
        let class_config_name = class_config.to_string_lossy();
        let package_name = package.to_string_lossy();
        let absent_name = absent.to_string_lossy();

        let changed_config = PendingCache {
            root: root.clone(),
            kind: CacheKind::Class,
            key: "1".repeat(64),
        };
        let upgraded_package = PendingCache {
            root: root.clone(),
            kind: CacheKind::Project,
            key: "3".repeat(64),
        };
        let appeared = PendingCache {
            root: root.clone(),
            kind: CacheKind::Project,
            key: "2".repeat(64),
        };
        assert_eq!(
            write_cache(
                &changed_config,
                b"class config",
                &[CacheDependency::from_resolution(
                    class_config_name.as_ref(),
                    Some(class_config_name.as_ref()),
                )],
                None,
            ),
            Ok(true)
        );
        assert_eq!(
            write_cache(
                &upgraded_package,
                b"package",
                &[CacheDependency::from_resolution(
                    package_name.as_ref(),
                    Some(package_name.as_ref()),
                )],
                None,
            ),
            Ok(true)
        );
        assert_eq!(
            write_cache(
                &appeared,
                b"appeared",
                &[CacheDependency::from_resolution(absent_name.as_ref(), None)],
                None,
            ),
            Ok(true)
        );
        let clock = (0, 0, 0, 0, 0, 0);
        assert!(matches!(
            load_cache(&root, CacheKind::Class, &changed_config.key, clock),
            CacheLookup::Hit(_)
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Project, &upgraded_package.key, clock),
            CacheLookup::Hit(_)
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Project, &appeared.key, clock),
            CacheLookup::Hit(_)
        ));

        std::fs::write(&class_config, b"new class configuration").unwrap();
        std::fs::write(&package, b"upgraded package with changed bytes").unwrap();
        std::fs::write(&absent, b"now here").unwrap();
        assert!(matches!(
            load_cache(&root, CacheKind::Class, &changed_config.key, clock),
            CacheLookup::Miss(reason) if reason.contains("dependency changed")
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Project, &upgraded_package.key, clock),
            CacheLookup::Miss(reason) if reason.contains("dependency changed")
        ));
        assert!(matches!(
            load_cache(&root, CacheKind::Project, &appeared.key, clock),
            CacheLookup::Miss(reason) if reason.contains("resolution changed")
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
