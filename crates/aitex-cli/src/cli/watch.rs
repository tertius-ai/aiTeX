// Native dependency-aware watch service. Filesystem polling is intentional:
// one small portable implementation gives atomic-save correctness on macOS
// and Linux, has no native callback queue to overflow while TeX is busy, and
// lets every rebuild establish a fresh baseline after its own generated writes.
// Metadata is the cheap wake-up index; content identity decides whether an
// ordinary input actually changed.

const WATCH_POLL: std::time::Duration = std::time::Duration::from_millis(40);
const WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(80);

struct ObservedContent {
    sha256: String,
    bytes: Option<u64>,
    modified_unix_nanos: Option<u128>,
    changed_unix_nanos: Option<i128>,
}

#[derive(Default)]
struct WatchedFile {
    signature: Option<FileSignature>,
    sha256: Option<String>,
    metadata_sensitive: bool,
    observed: Vec<ObservedContent>,
}

#[derive(Default)]
struct WatchSet {
    files: std::collections::BTreeMap<std::path::PathBuf, WatchedFile>,
}

impl WatchSet {
    fn insert(&mut self, path: std::path::PathBuf, metadata_sensitive: bool) {
        self.files.entry(path).or_default().metadata_sensitive |= metadata_sensitive;
    }

    fn insert_dependency(&mut self, path: std::path::PathBuf, dependency: &CacheDependency) {
        let watched = self.files.entry(path).or_default();
        watched.metadata_sensitive |= dependency.metadata_sensitive;
        if let Some(sha256) = &dependency.sha256 {
            watched.observed.push(ObservedContent {
                sha256: sha256.clone(),
                bytes: dependency.bytes,
                modified_unix_nanos: dependency.modified_unix_nanos,
                changed_unix_nanos: dependency.changed_unix_nanos,
            });
        }
    }

    fn extend(&mut self, other: Self) {
        for (path, watched) in other.files {
            self.insert(path, watched.metadata_sensitive);
        }
    }

    fn extend_missing(&mut self, other: &Self) {
        // Keep a bounded retirement set so deletion followed by recreation is
        // observable even when the failed/missing-input TeX run no longer
        // reports the former resolved path.
        for (path, watched) in other
            .files
            .iter()
            .filter(|(path, _)| !path.exists())
            .take(1024)
        {
            self.insert(path.clone(), watched.metadata_sensitive);
        }
    }

    fn baseline(&mut self) {
        for (path, watched) in &mut self.files {
            let metadata = std::fs::metadata(path).ok();
            watched.signature = metadata.as_ref().map(file_signature);
            watched.sha256 = metadata.as_ref().and_then(|metadata| {
                if watched.metadata_sensitive {
                    return None;
                }
                let modified_unix_nanos = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos());
                watched
                    .observed
                    .iter()
                    .find(|observed| {
                        observed.bytes == Some(metadata.len())
                            && observed.modified_unix_nanos == modified_unix_nanos
                            && observed.changed_unix_nanos == metadata_change_nanos(metadata)
                    })
                    .map(|observed| observed.sha256.clone())
                    .or_else(|| file_sha256(path).ok())
            });
            watched.observed.clear();
        }
    }

    fn snapshot(&self) -> Vec<Option<FileSignature>> {
        self.files
            .keys()
            .map(|path| std::fs::metadata(path).ok().map(|metadata| file_signature(&metadata)))
            .collect()
    }

    fn content_changed(&mut self, current: Vec<Option<FileSignature>>) -> bool {
        for ((path, watched), current) in self.files.iter_mut().zip(current) {
            if watched.signature == current {
                continue;
            }
            if watched.metadata_sensitive {
                return true;
            }
            match (&watched.sha256, file_sha256(path)) {
                (Some(expected), Ok(actual)) if expected == &actual => {
                    // Editors can rewrite or atomically replace an unchanged
                    // file. Adopt the new cheap signature and keep waiting.
                    watched.signature = current;
                }
                _ => return true,
            }
        }
        false
    }

    fn wait_for_change(&mut self) {
        loop {
            std::thread::sleep(WATCH_POLL);
            let current = self.snapshot();
            if self
                .files
                .values()
                .map(|watched| &watched.signature)
                .ne(current.iter())
            {
                let mut last = current;
                let mut quiet_since = std::time::Instant::now();
                loop {
                    std::thread::sleep(WATCH_POLL);
                    let current = self.snapshot();
                    if current != last {
                        last = current;
                        quiet_since = std::time::Instant::now();
                    } else if quiet_since.elapsed() >= WATCH_DEBOUNCE {
                        if self.content_changed(last) {
                            return;
                        }
                        break;
                    }
                }
            }
        }
    }
}

fn lexical_absolute(root: &std::path::Path, path: &std::path::Path) -> std::path::PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        return canonical;
    }
    let mut normalized = std::path::PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn project_watch_set(
    root: &std::path::Path,
    input: &str,
    observation: &BuildObservation,
) -> WatchSet {
    let mut result = WatchSet::default();
    result.insert(lexical_absolute(root, std::path::Path::new(input)), false);
    for dependency in &observation.dependencies {
        let candidate = dependency.resolved.as_deref().unwrap_or(&dependency.request);
        if candidate.is_empty() || candidate.contains('\0') {
            continue;
        }
        let path = lexical_absolute(root, std::path::Path::new(candidate));
        if path.starts_with(root) {
            result.insert_dependency(path, dependency);
        }
    }
    result
}

fn project_dependency_changed(root: &std::path::Path, dependency: &CacheDependency) -> bool {
    let candidate = dependency.resolved.as_deref().unwrap_or(&dependency.request);
    if candidate.is_empty() || candidate.contains('\0') {
        return false;
    }
    let path = lexical_absolute(root, std::path::Path::new(candidate));
    if !path.starts_with(root) {
        return false;
    }
    if dependency.metadata_sensitive {
        let Ok(metadata) = std::fs::metadata(&path) else {
            return dependency.resolved.is_some();
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_nanos());
        return dependency.bytes != Some(metadata.len())
            || dependency.modified_unix_nanos != modified
            || dependency.changed_unix_nanos != metadata_change_nanos(&metadata);
    }
    match (&dependency.sha256, file_sha256(&path)) {
        (Some(expected), Ok(current)) if expected != &current => true,
        (Some(_), Err(_)) | (None, Ok(_)) => true,
        _ => false,
    }
}

fn build_observation_is_stale(root: &std::path::Path, observation: &BuildObservation) -> bool {
    // A dependency can legitimately be read in more than one state during a
    // converging build (notably .aux). It is current if any observed state for
    // that path matches; otherwise an edit landed after its final useful read.
    let mut states: std::collections::BTreeMap<std::path::PathBuf, Vec<&CacheDependency>> =
        std::collections::BTreeMap::new();
    let written = observation
        .written_files
        .iter()
        .map(|path| lexical_absolute(root, std::path::Path::new(path)))
        .collect::<std::collections::BTreeSet<_>>();
    for dependency in &observation.dependencies {
        let candidate = dependency.resolved.as_deref().unwrap_or(&dependency.request);
        if candidate.is_empty() || candidate.contains('\0') {
            continue;
        }
        let path = lexical_absolute(root, std::path::Path::new(candidate));
        // Generated fixpoint inputs can be read and then rewritten during a
        // healthy pass. The scheduler has already converged them; only
        // externally owned inputs participate in the post-build race check.
        if path.starts_with(root) && !written.contains(&path) {
            states.entry(path).or_default().push(dependency);
        }
    }
    for (path, dependencies) in states {
        if dependencies
            .iter()
            .all(|dependency| project_dependency_changed(root, dependency))
        {
            cli_detail!(
                "aitex watch: post-build dependency changed: {}",
                path.display()
            );
            return true;
        }
    }
    false
}

fn report_watch_ready(sequence: u64, started: std::time::Instant) {
    let code = EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    cli_detail!(
        "AITEX_WATCH ready sequence={} code={} duration_ms={:.3}",
        sequence,
        code,
        started.elapsed().as_secs_f64() * 1e3,
    );
}

fn watch_run_text(sequence: u64, detail: &str, style: CliStyle) -> String {
    let rail = style.cyan("━━━━━━━━");
    let run = style.bold(&format!("Run {sequence}"));
    let detail = style.dim(&format!("· {detail}"));
    format!("  {rail} {run} {detail} {rail}")
}

fn report_watch_run(sequence: u64, detail: &str) {
    if cli_present_output() {
        eprintln!("{}", watch_run_text(sequence, detail, cli_style()));
    }
}

/// Preserve the pre-0.3 editor/benchmark protocol behind an explicit flag:
/// initial one-pass compile, one additional pass per line, `quit` to stop.
fn run_stdin_watch() {
    use std::io::BufRead as _;
    let mut sequence = 0u64;
    let build = |sequence: &mut u64, detail: &str| {
        *sequence += 1;
        report_watch_run(*sequence, detail);
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        let started = std::time::Instant::now();
        let _ = run();
        report_watch_ready(*sequence, started);
    };
    build(&mut sequence, "initial build");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        match line {
            Ok(line) if line.trim() == "quit" => break,
            Ok(_) => build(&mut sequence, "rebuild requested"),
            Err(error) => {
                eprintln!("aitex watch: stdin failed: {error}");
                EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
    }
}

/// Bare `aitex watch`: full fixed-point builds driven by the exact dependency
/// graph observed by the engine and semantic build tools.
fn run_filesystem_watch() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let input = input_argument(&arguments).expect("watch arguments were validated");
    let output = output_argument(&arguments).expect("watch has a validated output path");
    let root = std::fs::canonicalize(std::env::current_dir().unwrap_or_default())
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
    let mut watched = WatchSet::default();
    watched.insert(lexical_absolute(&root, std::path::Path::new(input)), false);
    watched.baseline();
    discard_watch_output(&output);
    let mut sequence = 0u64;
    let mut detail = "initial build";

    loop {
        sequence += 1;
        report_watch_run(sequence, detail);
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        let started = std::time::Instant::now();
        let (failure, observation) = run_native_build_observed();
        let mut failed = failure.is_some();
        if !failed && let Err(error) = promote_watch_output(&output) {
            eprintln!("aitex watch: cannot publish {output}: {error}");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
            failed = true;
        }
        if failed {
            discard_watch_output(&output);
        }

        let mut next = project_watch_set(&root, input, &observation);
        if failed {
            // A syntax error may stop before later includes are reached. Keep
            // the last complete graph so edits anywhere in it can recover.
            next.extend(watched);
            next.baseline();
        } else {
            next.extend_missing(&watched);
            next.baseline();
        }
        let raced = build_observation_is_stale(&root, &observation);
        watched = next;
        report_watch_ready(sequence, started);
        if raced {
            cli_detail!("aitex watch: input changed during build; rebuilding again");
            detail = "input changed during build";
            continue;
        }
        watched.wait_for_change();
        detail = "change detected";
    }
}

fn run_watch() {
    if std::env::args().any(|argument| argument == "--stdin") {
        run_stdin_watch();
    } else {
        run_filesystem_watch();
    }
}

#[cfg(test)]
mod watch_tests {
    use super::{
        BuildObservation, CacheDependency, CliStyle, WatchSet, lexical_absolute,
        project_watch_set, watch_run_text,
    };

    fn rewrite_with_new_mtime(path: &std::path::Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
        )
        .unwrap();
    }

    #[test]
    fn watch_runs_have_a_clear_compact_boundary() {
        let text = watch_run_text(2, "change detected", CliStyle { color: false });
        assert_eq!(
            text,
            "  ━━━━━━━━ Run 2 · change detected ━━━━━━━━"
        );
        assert!(!text.contains("\x1b["));
    }

    #[test]
    fn missing_watch_targets_are_retained_for_later_creation() {
        let root = std::env::temp_dir().join(format!("aitex-watch-path-{}", std::process::id()));
        let path = lexical_absolute(&root, std::path::Path::new("chapter.tex"));
        let mut watched = WatchSet::default();
        watched.insert(path.clone(), false);
        watched.baseline();
        assert!(watched.files.contains_key(&path));
        assert_eq!(watched.files[&path].signature, None);
    }

    #[test]
    fn content_identity_ignores_an_unchanged_rewrite() {
        let root = std::env::temp_dir().join(format!(
            "aitex-watch-content-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("chapter.tex");
        std::fs::write(&path, b"same bytes\n").unwrap();
        let mut watched = WatchSet::default();
        watched.insert(path.clone(), false);
        watched.baseline();
        rewrite_with_new_mtime(&path, b"same bytes\n");
        let current = watched.snapshot();
        assert!(!watched.content_changed(current));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_identity_detects_same_size_edits() {
        let root = std::env::temp_dir().join(format!(
            "aitex-watch-edit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("chapter.tex");
        std::fs::write(&path, b"version one\n").unwrap();
        let mut watched = WatchSet::default();
        watched.insert(path.clone(), false);
        watched.baseline();
        rewrite_with_new_mtime(&path, b"version two\n");
        let current = watched.snapshot();
        assert!(watched.content_changed(current));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_sensitive_inputs_still_observe_unchanged_rewrites() {
        let root = std::env::temp_dir().join(format!(
            "aitex-watch-metadata-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("chapter.tex");
        std::fs::write(&path, b"same bytes\n").unwrap();
        let mut watched = WatchSet::default();
        watched.insert(path.clone(), true);
        watched.baseline();
        rewrite_with_new_mtime(&path, b"same bytes\n");
        let current = watched.snapshot();
        assert!(watched.content_changed(current));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_dependencies_preserve_metadata_sensitivity() {
        let root = std::env::temp_dir().join(format!(
            "aitex-watch-project-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let path = root.join("chapter.tex");
        std::fs::write(&path, b"chapter\n").unwrap();
        let path_text = path.to_string_lossy();
        let observation = BuildObservation {
            dependencies: vec![CacheDependency::from_metadata_resolution(
                "chapter.tex",
                Some(&path_text),
            )],
            written_files: Vec::new(),
        };
        let watched = project_watch_set(&root, "main.tex", &observation);
        assert!(watched.files[&path].metadata_sensitive);
        std::fs::remove_dir_all(root).unwrap();
    }
}
