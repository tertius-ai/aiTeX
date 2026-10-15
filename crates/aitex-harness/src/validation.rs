//! Isolated complete-build validation shared by `aitex fix` and the repair lab.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationLimits {
    pub max_files: usize,
    pub max_bytes: u64,
    pub timeout: Duration,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_bytes: 256 * 1024 * 1024,
            timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationArtifact {
    Pdf,
    Dvi,
}

impl ValidationArtifact {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pdf => "PDF",
            Self::Dvi => "DVI",
        }
    }

    fn filename(self) -> &'static str {
        match self {
            Self::Pdf => "aitex-repair-validation.pdf",
            Self::Dvi => "aitex-repair-validation.dvi",
        }
    }
}

#[derive(Debug)]
pub struct ValidationRequest<'a> {
    pub executable: &'a Path,
    pub project_root: &'a Path,
    pub input: &'a Path,
    pub target: &'a Path,
    pub after: &'a [u8],
    pub artifact: ValidationArtifact,
    pub format: Option<&'a Path>,
    pub tex82: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ValidationResult {
    pub elapsed: Duration,
    pub artifact_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceBuild {
    pub succeeded: bool,
    pub elapsed: Duration,
    pub artifact_bytes: Option<u64>,
    pub diagnostic: Option<aitex_core::Diagnostic>,
    pub detail: String,
    /// Successful compact build phases emitted by the isolated aiTeX child.
    /// Raw transcripts and diagnostics deliberately remain private.
    pub phases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

struct ScratchProject {
    path: PathBuf,
}

/// A private project tree used for iterative repair. Forking it before each
/// proposal isolates generated auxiliaries as well as author source, so a
/// rejected candidate cannot poison the next route.
pub struct RepairWorkspace {
    scratch: ScratchProject,
    limits: ValidationLimits,
}

impl Drop for ScratchProject {
    fn drop(&mut self) {
        let temp = std::env::temp_dir();
        if self.path.starts_with(&temp)
            && self
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("aitex-repair-validate-"))
        {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn scratch_project() -> Result<ScratchProject, ValidationError> {
    let base = std::env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..16u8 {
        let path = base.join(format!(
            "aitex-repair-validate-{}-{nonce}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| {
                            ValidationError(format!("cannot protect validation directory: {error}"))
                        })?;
                }
                return Ok(ScratchProject { path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ValidationError(format!(
                    "cannot create validation directory: {error}"
                )));
            }
        }
    }
    Err(ValidationError(
        "cannot allocate a unique validation directory".into(),
    ))
}

fn copy_project(
    source: &Path,
    destination: &Path,
    scratch_root: &Path,
    files: &mut usize,
    bytes: &mut u64,
    limits: ValidationLimits,
    preserve_private_cache: bool,
) -> Result<(), ValidationError> {
    let entries = std::fs::read_dir(source).map_err(|error| {
        ValidationError(format!(
            "cannot read project directory {}: {error}",
            source.display()
        ))
    })?;
    for entry in entries {
        let entry = entry
            .map_err(|error| ValidationError(format!("cannot read project entry: {error}")))?;
        if scratch_root.starts_with(entry.path()) {
            continue;
        }
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some(".git" | ".aitex" | ".aitex-cache" | "target")
        ) || (name.to_string_lossy().starts_with(".aitex-repair-")
            && !(preserve_private_cache && name == ".aitex-repair-cache"))
        {
            continue;
        }
        let kind = entry.file_type().map_err(|error| {
            ValidationError(format!(
                "cannot inspect {}: {error}",
                entry.path().display()
            ))
        })?;
        let target = destination.join(&name);
        if kind.is_dir() {
            std::fs::create_dir(&target).map_err(|error| {
                ValidationError(format!("cannot create validation directory: {error}"))
            })?;
            copy_project(
                &entry.path(),
                &target,
                scratch_root,
                files,
                bytes,
                limits,
                preserve_private_cache,
            )?;
        } else if kind.is_file() {
            let size = entry
                .metadata()
                .map_err(|error| {
                    ValidationError(format!(
                        "cannot inspect {}: {error}",
                        entry.path().display()
                    ))
                })?
                .len();
            *files += 1;
            *bytes = bytes.saturating_add(size);
            if *files > limits.max_files || *bytes > limits.max_bytes {
                return Err(ValidationError(format!(
                    "project exceeds the isolated repair limit ({} files / {} bytes)",
                    limits.max_files, limits.max_bytes
                )));
            }
            std::fs::copy(entry.path(), &target).map_err(|error| {
                ValidationError(format!("cannot copy {}: {error}", entry.path().display()))
            })?;
        }
        // Symlinks and special files deliberately do not cross the boundary.
    }
    Ok(())
}

fn collect_tool_logs(root: &Path, logs: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_tool_logs(&path, logs);
        } else if kind.is_file()
            && matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("blg" | "ilg" | "glg" | "alg" | "nlg")
            )
        {
            logs.push(path);
        }
    }
}

fn enforce_tree_limits(
    root: &Path,
    files: &mut usize,
    bytes: &mut u64,
    limits: ValidationLimits,
) -> Result<(), ValidationError> {
    for entry in std::fs::read_dir(root).map_err(|error| {
        ValidationError(format!(
            "cannot inspect isolated project {}: {error}",
            root.display()
        ))
    })? {
        let entry = entry
            .map_err(|error| ValidationError(format!("cannot inspect project entry: {error}")))?;
        let kind = entry.file_type().map_err(|error| {
            ValidationError(format!(
                "cannot inspect {}: {error}",
                entry.path().display()
            ))
        })?;
        if kind.is_dir() {
            enforce_tree_limits(&entry.path(), files, bytes, limits)?;
        } else if kind.is_file() {
            *files = files.saturating_add(1);
            *bytes = bytes.saturating_add(
                entry
                    .metadata()
                    .map_err(|error| {
                        ValidationError(format!(
                            "cannot inspect {}: {error}",
                            entry.path().display()
                        ))
                    })?
                    .len(),
            );
            if *files > limits.max_files || *bytes > limits.max_bytes {
                return Err(ValidationError(format!(
                    "isolated build exceeded the repair workspace limit ({} files / {} bytes)",
                    limits.max_files, limits.max_bytes
                )));
            }
        }
    }
    Ok(())
}

fn first_workspace_diagnostic(
    root: &Path,
    input: &Path,
    stderr: &str,
) -> Option<aitex_core::Diagnostic> {
    let stem = input.file_stem()?.to_string_lossy();
    let tex_log = root.join(format!("{stem}.log"));
    if let Some(text) = crate::read_log(&tex_log) {
        let mut report = crate::parse_tex_log(&text, input);
        enrich_report_diagnostics(root, input, &mut report);
        report.cluster();
        if let Some(diagnostic) = report.first_error() {
            let mut diagnostic = diagnostic.clone();
            let primary_signature = diagnostic.signature();
            let primary_line = diagnostic.location().map(|span| span.line).unwrap_or(0);
            let mut related = report
                .diagnostics
                .iter()
                .filter(|related| {
                    matches!(
                        related.kind,
                        aitex_core::Kind::UndefinedCitation { .. }
                            | aitex_core::Kind::UndefinedReference { .. }
                    ) && related.signature() != primary_signature
                })
                .collect::<Vec<_>>();
            if !related.is_empty() {
                let definitions = literal_unresolved_definitions(root);
                related
                    .retain(|related| !literal_unresolved_definition_exists(&definitions, related));
            }
            let warning_line = |related: &aitex_core::Diagnostic| {
                related
                    .location()
                    .map(|span| span.line)
                    .filter(|line| *line > 0)
                    .or_else(|| {
                        related
                            .message
                            .split("input line ")
                            .nth(1)
                            .and_then(|tail| {
                                tail.chars()
                                    .take_while(char::is_ascii_digit)
                                    .collect::<String>()
                                    .parse::<u32>()
                                    .ok()
                            })
                    })
                    .unwrap_or(0)
            };
            related.sort_by_key(|related| {
                let line = warning_line(related);
                (
                    if primary_line > 0 && line > 0 {
                        primary_line.abs_diff(line)
                    } else {
                        u32::MAX
                    },
                    line,
                )
            });
            for related in related.into_iter().take(4) {
                let mut frame = aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::CompilationPass,
                    "additional unresolved diagnostic · no literal definition found",
                )
                .with_detail(related.message.clone());
                let line = warning_line(related);
                if line > 0 {
                    let file = related
                        .location()
                        .map(|location| location.file.clone())
                        .unwrap_or_else(|| input.to_path_buf());
                    frame = frame.at(aitex_core::Span::new(file, line));
                }
                diagnostic.provenance.push(frame);
            }
            return Some(diagnostic);
        }
    }

    let mut logs = Vec::new();
    collect_tool_logs(root, &mut logs);
    logs.sort();
    for path in logs {
        let Some(text) = crate::read_log(&path) else {
            continue;
        };
        let tool = match path.extension().and_then(|value| value.to_str()) {
            Some("blg") if text.contains("This is Biber") || text.contains("Biber") => {
                aitex_core::Tool::Biber
            }
            Some("blg") => aitex_core::Tool::BibTex,
            Some("ilg") => aitex_core::Tool::MakeIndex,
            Some("glg" | "alg") => aitex_core::Tool::MakeGlossaries,
            Some("nlg") => aitex_core::Tool::Nomenclature,
            _ => continue,
        };
        let mut report = crate::parse_tool_log(&text, tool);
        enrich_report_diagnostics(root, input, &mut report);
        report.cluster();
        if let Some(diagnostic) = report.first_error() {
            return Some(diagnostic.clone());
        }
    }

    let message = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("isolated build failed without a structured diagnostic")
        .trim();
    let mut diagnostic = aitex_core::Diagnostic::new(
        aitex_core::Severity::Error,
        aitex_core::Kind::Other,
        message,
        aitex_core::Source::Engine,
    );
    if let Some((file, line)) = fallback_message_location(root, input, message) {
        diagnostic = diagnostic.at(aitex_core::Span::new(file, line));
    }
    enrich_diagnostic_origin(root, input, &mut diagnostic);
    Some(diagnostic)
}

fn fallback_message_location(root: &Path, input: &Path, message: &str) -> Option<(PathBuf, u32)> {
    if let Some(line) = message.rsplit("line ").next().and_then(|tail| {
        tail.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .ok()
    }) {
        return Some((input.to_path_buf(), line));
    }

    // Compact human diagnostics use `file:line`. A repair child captures that
    // display on stderr, so its final bounded provenance line may be the only
    // location left when neither the TeX nor semantic-tool log was parseable.
    // Accept only an author file inside this project boundary.
    message.split_whitespace().rev().find_map(|word| {
        let word = word.trim_matches(|character: char| {
            matches!(character, ',' | ';' | ')' | ']' | '}' | '`' | '\'' | '"')
        });
        let (file, line) = word.rsplit_once(':')?;
        let line = line.parse::<u32>().ok().filter(|line| *line > 0)?;
        let file = PathBuf::from(file);
        let relative = project_relative_path(root, &file)?;
        (crate::author_source(&root.join(&relative)) && root.join(&relative).is_file())
            .then_some((file, line))
    })
}

/// Add bounded source-level causal evidence to every diagnostic in a report.
///
/// This is the shared boundary used by interactive repair, ordinary builds,
/// watch mode, and read-only explanations. It deliberately declines ambiguous
/// mappings: a reported recovery point is preferable to a confident-looking
/// but incorrect author location.
pub fn enrich_report_diagnostics(root: &Path, input: &Path, report: &mut aitex_core::Report) {
    for diagnostic in &mut report.diagnostics {
        enrich_diagnostic_origin(root, input, diagnostic);
    }
}

fn project_author_files(root: &Path) -> Vec<PathBuf> {
    fn collect(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
        if paths.len() >= 512 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            if paths.len() >= 512 {
                break;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with('.') || name == "target")
                {
                    continue;
                }
                collect(root, &path, paths);
            } else if kind.is_file()
                && crate::author_source(&path)
                && let Ok(relative) = path.strip_prefix(root)
            {
                paths.push(relative.to_path_buf());
            }
        }
    }
    let mut paths = Vec::new();
    collect(root, root, &mut paths);
    paths.sort();
    paths
}

fn source_sites(root: &Path, needles: &[&str]) -> Vec<(PathBuf, u32, String)> {
    let mut sites = Vec::new();
    for relative in project_author_files(root) {
        let Ok(bytes) = std::fs::read(root.join(&relative)) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes);
        for (index, line) in source.lines().enumerate() {
            let compact = line
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>();
            for needle in needles {
                let mut rest = compact.as_str();
                while let Some(at) = rest.find(needle) {
                    sites.push((relative.clone(), index as u32 + 1, line.trim().to_string()));
                    rest = &rest[at + needle.len()..];
                }
            }
        }
    }
    sites
}

fn braced_arguments(source: &str, command: &str, output: &mut BTreeSet<String>) {
    let mut rest = source;
    while let Some(at) = rest.find(command) {
        let tail = &rest[at + command.len()..];
        if let Some(end) = tail.find('}') {
            let value = &tail[..end];
            if !value.is_empty() {
                output.insert(value.to_string());
            }
            rest = &tail[end + 1..];
        } else {
            break;
        }
    }
}

fn literal_unresolved_definitions(root: &Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut references = BTreeSet::new();
    let mut citations = BTreeSet::new();
    for relative in project_author_files(root) {
        let Ok(bytes) = std::fs::read(root.join(relative)) else {
            continue;
        };
        let compact = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        braced_arguments(&compact, "\\label{", &mut references);
        braced_arguments(&compact, "\\bibitem{", &mut citations);
        let mut rest = compact.as_str();
        while let Some(at) = rest.find('@') {
            let tail = &rest[at + 1..];
            let Some(open) = tail.find(['{', '(']) else {
                break;
            };
            let key = &tail[open + 1..];
            let Some(comma) = key.find(',') else {
                rest = &tail[open + 1..];
                continue;
            };
            let key = &key[..comma];
            if !key.is_empty() {
                citations.insert(key.to_string());
            }
            rest = &tail[open + comma + 2..];
        }
    }
    (references, citations)
}

fn literal_unresolved_definition_exists(
    definitions: &(BTreeSet<String>, BTreeSet<String>),
    diagnostic: &aitex_core::Diagnostic,
) -> bool {
    match &diagnostic.kind {
        aitex_core::Kind::UndefinedCitation { key } => definitions.1.contains(key),
        aitex_core::Kind::UndefinedReference { label } => definitions.0.contains(label),
        _ => false,
    }
}

fn project_relative_path(root: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return path.strip_prefix(root).ok().map(Path::to_path_buf);
    }
    (!path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir)))
    .then(|| path.to_path_buf())
}

fn generated_record_line(root: &Path, input: &Path, reported: &aitex_core::Span) -> Option<String> {
    let reported_file = project_relative_path(root, &reported.file)?;
    let input = project_relative_path(root, input)?;
    let index = usize::try_from(reported.line.checked_sub(1)?).ok()?;
    let beside_input = input.parent().map(|parent| parent.join(&reported_file));
    [Some(reported_file), beside_input]
        .into_iter()
        .flatten()
        .find_map(|relative| {
            std::fs::read_to_string(root.join(relative))
                .ok()?
                .lines()
                .nth(index)
                .map(str::to_string)
        })
}

fn braced_value_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let rest = text.split_once(marker)?.1;
    Some(rest.split_once('}')?.0)
}

fn glossary_record_key(record: &str) -> Option<&str> {
    ["\\glsentry{", "\\glossentry{"]
        .into_iter()
        .find_map(|marker| braced_value_after(record, marker))
}

fn odd_inline_math_delimiters(line: &str) -> bool {
    let mut escaped = false;
    let mut delimiters = 0usize;
    for character in line.chars() {
        if character == '%' && !escaped {
            break;
        }
        if character == '$' && !escaped {
            delimiters += 1;
        }
        if character == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    delimiters % 2 == 1
}

fn nearby_math_cause(
    root: &Path,
    reported: &aitex_core::Span,
) -> Option<(aitex_core::Span, String)> {
    if reported.line == 0 {
        return None;
    }
    let relative = project_relative_path(root, &reported.file)?;
    let source = std::fs::read_to_string(root.join(relative)).ok()?;
    let lines = source.lines().collect::<Vec<_>>();
    let reported_index = usize::try_from(reported.line - 1).ok()?;
    if let Some(line) = lines.get(reported_index).copied() {
        if odd_inline_math_delimiters(line) {
            return Some((
                aitex_core::Span::new(&reported.file, u32::try_from(reported_index + 1).ok()?),
                line.trim().to_string(),
            ));
        }
    }

    // TeX commonly reports the macro invocation where it finally noticed the
    // wrong math state (`\maketitle`, `\end{...}`, ...), not the delimiter.
    // Search only the current paragraph, cap the look-behind, and require one
    // unambiguous odd-dollar line.  This deliberately avoids the unsafe global
    // unmatched-dollar guess that can be confused by unrelated definitions.
    let lower_bound = reported_index.saturating_sub(80);
    let paragraph_start = (lower_bound..reported_index)
        .rev()
        .find(|index| lines.get(*index).is_some_and(|line| line.trim().is_empty()))
        .map_or(lower_bound, |index| index + 1);
    let candidates = (paragraph_start..=reported_index)
        .filter(|index| {
            lines
                .get(*index)
                .is_some_and(|line| odd_inline_math_delimiters(line))
        })
        .collect::<Vec<_>>();
    if let [cause] = candidates.as_slice() {
        let line = lines[*cause];
        return Some((
            aitex_core::Span::new(&reported.file, u32::try_from(*cause + 1).ok()?),
            line.trim().to_string(),
        ));
    }

    // Preamble material is commonly accumulated across blank lines and only
    // executed by `\maketitle` or at the document boundary.  For an early
    // recovery point, make one wider but still bounded pass and accept it only
    // when exactly one source line has odd inline-dollar parity.
    if reported_index < 256 {
        let preamble_start = reported_index.saturating_sub(160);
        let candidates = (preamble_start..=reported_index)
            .filter(|index| {
                lines
                    .get(*index)
                    .is_some_and(|line| odd_inline_math_delimiters(line))
            })
            .collect::<Vec<_>>();
        if let [cause] = candidates.as_slice() {
            let line = lines[*cause];
            return Some((
                aitex_core::Span::new(&reported.file, u32::try_from(*cause + 1).ok()?),
                line.trim().to_string(),
            ));
        }
    }
    None
}

fn unique_literal_site(root: &Path, literal: &str) -> Option<(PathBuf, u32, String)> {
    let sites = source_sites(root, &[literal]);
    let [site] = sites.as_slice() else {
        return None;
    };
    Some(site.clone())
}

fn unique_control_sequence_site(root: &Path, name: &str) -> Option<(PathBuf, u32, String)> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|character| character.is_ascii_alphabetic() || character == '@')
    {
        return None;
    }
    let needle = format!("\\{name}");
    let mut match_site = None;
    for relative in project_author_files(root) {
        let Ok(bytes) = std::fs::read(root.join(&relative)) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes);
        for (index, line) in source.lines().enumerate() {
            for (offset, _) in line.match_indices(&needle) {
                let after = &line[offset + needle.len()..];
                if !after
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic() || character == '@')
                {
                    let site = (
                        relative.clone(),
                        u32::try_from(index + 1).unwrap_or(u32::MAX),
                        line.trim().to_string(),
                    );
                    if match_site.is_some() {
                        return None;
                    }
                    match_site = Some(site);
                }
            }
        }
    }
    match_site
}

fn missing_path_spelling_candidate(name: &str) -> Option<&str> {
    const EXTENSIONS: &[&str] = &[".pdf", ".png", ".jpg", ".jpeg", ".eps", ".svg"];
    EXTENSIONS.iter().find_map(|extension| {
        let end = name.find(extension)? + extension.len();
        (end < name.len()).then_some(&name[..end])
    })
}

fn glossary_definition_site(root: &Path, key: &str) -> Option<(PathBuf, u32, String)> {
    if key.is_empty() || key.len() > 256 {
        return None;
    }
    let markers = [
        format!("\\newglossaryentry{{{key}}}"),
        format!("\\newacronym{{{key}}}"),
        format!("\\newabbreviation{{{key}}}"),
    ];
    for relative in project_author_files(root) {
        let Ok(bytes) = std::fs::read(root.join(&relative)) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes);
        let lines = source.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            if !markers.iter().any(|marker| line.contains(marker)) {
                continue;
            }
            let end = (index + 12).min(lines.len().saturating_sub(1));
            let cause = (index..=end)
                .find(|candidate| {
                    lines[*candidate]
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .collect::<String>()
                        .contains("={}")
                })
                .unwrap_or(index);
            return Some((
                relative,
                u32::try_from(cause + 1).unwrap_or(u32::MAX),
                lines[cause].trim().to_string(),
            ));
        }
    }
    None
}

fn biber_entry_field_site(root: &Path, message: &str) -> Option<(PathBuf, u32, String)> {
    let tail = message.rsplit_once("entry ")?.1;
    let key = tail
        .trim_start_matches(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '\\' | '\'' | '`' | '"')
        })
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '\\' | '\'' | '`' | '"')
        })
        .next()?;
    let field = message.contains("Name \"").then_some("author")?;
    let mut sites = Vec::new();
    for relative in project_author_files(root) {
        if relative
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("bib")
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(&relative)) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes);
        let lines = source.lines().collect::<Vec<_>>();
        let Some(start) = lines.iter().position(|line| {
            line.trim_start().starts_with('@') && line.contains(&format!("{{{key},"))
        }) else {
            continue;
        };
        let end = lines[start + 1..]
            .iter()
            .position(|line| line.trim_start().starts_with('@'))
            .map_or(lines.len(), |offset| start + 1 + offset);
        for (offset, line) in lines[start + 1..end].iter().enumerate() {
            let compact = line
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            if compact.starts_with(&format!("{field}=")) {
                sites.push((
                    relative.clone(),
                    u32::try_from(start + offset + 2).unwrap_or(u32::MAX),
                    line.trim().to_string(),
                ));
            }
        }
    }
    let [site] = sites.as_slice() else {
        return None;
    };
    Some(site.clone())
}

/// Add the causal author location that a generated `.idx`/`.glo` record or a
/// missing include obscures. This is bounded heuristic evidence, not a claimed
/// exact source map, and is therefore retained as provenance as well as in the
/// separate `cause` field.
pub fn enrich_diagnostic_origin(
    root: &Path,
    input: &Path,
    diagnostic: &mut aitex_core::Diagnostic,
) {
    if diagnostic.cause.is_some() {
        return;
    }
    let input_relative = project_relative_path(root, input).unwrap_or_else(|| input.to_path_buf());
    if let aitex_core::Kind::MismatchedEnvironment { ended, .. } = &diagnostic.kind
        && !ended.is_empty()
        && let Some((file, line, source)) = unique_literal_site(root, &format!("\\end{{{ended}}}"))
    {
        let span = aitex_core::Span::new(file, line);
        diagnostic.cause = Some(span.clone());
        diagnostic.provenance.push(
            aitex_core::ProvenanceFrame::new(
                aitex_core::ProvenanceKind::FailingToken,
                format!("unique mismatched \\end{{{ended}}}"),
            )
            .at(span)
            .with_detail(source),
        );
        return;
    }
    if let aitex_core::Kind::UndefinedControlSequence { name } = &diagnostic.kind
        && let Some((file, line, source)) = unique_control_sequence_site(root, name)
    {
        let span = aitex_core::Span::new(file, line);
        diagnostic.cause = Some(span.clone());
        diagnostic.provenance.push(
            aitex_core::ProvenanceFrame::new(
                aitex_core::ProvenanceKind::FailingToken,
                format!("unique undefined control sequence `\\{name}`"),
            )
            .at(span)
            .with_detail(source),
        );
        return;
    }
    if (matches!(diagnostic.kind, aitex_core::Kind::MissingMathShift)
        || diagnostic.message.starts_with("Extra }, or forgotten $"))
        && let Some(reported) = diagnostic.reported.as_ref()
        && let Some((span, source)) = nearby_math_cause(root, reported)
    {
        diagnostic.kind = aitex_core::Kind::MissingMathShift;
        diagnostic.cause = Some(span.clone());
        diagnostic.provenance.push(
            aitex_core::ProvenanceFrame::new(
                aitex_core::ProvenanceKind::FailingToken,
                "odd inline math delimiter count",
            )
            .at(span)
            .with_detail(source),
        );
        return;
    }
    if matches!(
        diagnostic.source,
        aitex_core::Source::ToolLog(aitex_core::Tool::Biber | aitex_core::Tool::BibTex)
    ) {
        let normalized = diagnostic.reported.as_ref().and_then(|reported| {
            (reported
                .file
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("utf8"))
            .then(|| (reported.file.clone(), reported.line))
        });
        let databases = project_author_files(root)
            .into_iter()
            .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("bib"))
            .collect::<Vec<_>>();
        if let (Some((generated, line)), [database]) = (normalized, databases.as_slice()) {
            let span = aitex_core::Span::new(database, line);
            diagnostic.reported = Some(span.clone());
            diagnostic.provenance.push(
                aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::GeneratedOrigin,
                    "Biber normalized database",
                )
                .at(span)
                .with_detail(format!(
                    "temporary parser input {} maps to the project's unique bibliography database",
                    generated.display()
                )),
            );
        }
        if diagnostic.message.contains("expected end of entry")
            && let Some(reported) = diagnostic.reported.as_ref()
            && reported.line > 1
        {
            let span = aitex_core::Span::new(&reported.file, reported.line - 1);
            diagnostic.cause = Some(span.clone());
            diagnostic.provenance.push(
                aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::FailingToken,
                    "field before Biber parser recovery",
                )
                .at(span)
                .with_detail("Biber found the next field where it expected the entry to end"),
            );
            return;
        }
        if let Some((file, line, source)) = biber_entry_field_site(root, &diagnostic.message) {
            let span = aitex_core::Span::new(file, line);
            diagnostic.cause = Some(span.clone());
            diagnostic.provenance.push(
                aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::FailingToken,
                    "Biber entry field",
                )
                .at(span)
                .with_detail(source),
            );
            return;
        }
    }
    if let aitex_core::Source::ToolLog(tool) = diagnostic.source
        && let Some(reported) = diagnostic.reported.as_ref()
    {
        let record_line = reported.line;
        let extension = reported.file.extension().and_then(|value| value.to_str());
        if tool == aitex_core::Tool::MakeGlossaries
            && matches!(extension, Some("glo" | "acn"))
            && let Some(record) = generated_record_line(root, &input_relative, reported)
            && let Some(key) = glossary_record_key(&record)
            && let Some((file, line, source)) = glossary_definition_site(root, key)
        {
            let span = aitex_core::Span::new(file, line);
            diagnostic.cause = Some(span.clone());
            diagnostic.provenance.push(
                aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::GeneratedOrigin,
                    format!("glossary key `{key}`"),
                )
                .at(span)
                .with_detail(format!(
                    "generated record {record_line} names `{key}`; source: {source}"
                )),
            );
            return;
        }
        let commands: &[&str] = match (tool, extension) {
            (aitex_core::Tool::MakeIndex, _) => &["\\index{"],
            (aitex_core::Tool::MakeGlossaries, Some("acn")) => &["\\newacronym{"],
            (aitex_core::Tool::MakeGlossaries, _) => &["\\newglossaryentry{"],
            (aitex_core::Tool::Nomenclature, _) => &["\\nomenclature{"],
            (aitex_core::Tool::BibTex | aitex_core::Tool::Biber, _) => &[],
        };
        if !commands.is_empty()
            && let Some(index) = record_line.checked_sub(1)
        {
            let sites = source_sites(root, commands);
            let preferred = sites
                .iter()
                .filter(|(file, _, _)| file == &input_relative)
                .cloned()
                .collect::<Vec<_>>();
            // An index artifact normally belongs to the root whose stem it
            // shares. Prefer that root's command order when it contains the
            // requested record; only fall back to project-wide order for
            // glossary definitions and other commands living in includes.
            let Some((file, line, source)) = preferred
                .get(index as usize)
                .or_else(|| sites.get(index as usize))
                .cloned()
            else {
                return;
            };
            let span = aitex_core::Span::new(file, line);
            diagnostic.cause = Some(span.clone());
            diagnostic.provenance.push(
                aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::GeneratedOrigin,
                    "author command",
                )
                .at(span)
                .with_detail(format!(
                    "generated record {} maps by command order to {}",
                    record_line, source
                )),
            );
            return;
        }
    }

    // Package diagnostics often report a value produced by a user macro, not
    // the template that produced it. For example `\Gls{#1-missing}` reports
    // the concrete key `CVQKD-missing`, which does not occur anywhere in the
    // source. Preserve that non-local causal hop for both people and models.
    if let aitex_core::Kind::PackageError { package } = &diagnostic.kind
        && package.eq_ignore_ascii_case("glossaries")
        && let Some(key) = diagnostic
            .message
            .split("Glossary entry `")
            .nth(1)
            .and_then(|tail| tail.split('\'').next())
        && let Some((_, suffix)) = key.split_once('-')
    {
        let template = format!("#1-{suffix}");
        if let Some((file, line, source)) = source_sites(root, &[&template]).into_iter().next() {
            let span = aitex_core::Span::new(file, line);
            diagnostic.cause = Some(span.clone());
            diagnostic.provenance.push(
                aitex_core::ProvenanceFrame::new(
                    aitex_core::ProvenanceKind::MacroDefinition,
                    "glossary key template",
                )
                .at(span)
                .with_detail(format!("{template} expanded to {key}; source: {source}")),
            );
            return;
        }
    }

    let (name, package) = match &diagnostic.kind {
        aitex_core::Kind::MissingFile { name } => (name.as_str(), false),
        aitex_core::Kind::MissingPackage { name } => (name.as_str(), true),
        _ => return,
    };
    let without_extension = Path::new(name)
        .with_extension("")
        .to_string_lossy()
        .into_owned();
    let candidates = [name, without_extension.as_str()];
    let mut paths = project_author_files(root);
    if let Some(at) = paths.iter().position(|path| path == input) {
        paths.swap(0, at);
    }
    for relative in paths {
        let Ok(bytes) = std::fs::read(root.join(&relative)) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes);
        for (index, line) in source.lines().enumerate() {
            if candidates
                .iter()
                .any(|needle| !needle.is_empty() && line.contains(needle))
            {
                let span = aitex_core::Span::new(relative, index as u32 + 1);
                diagnostic.cause = Some(span.clone());
                diagnostic.provenance.push(
                    aitex_core::ProvenanceFrame::new(
                        aitex_core::ProvenanceKind::Include,
                        if package {
                            "package request"
                        } else {
                            "missing input"
                        },
                    )
                    .at(span.clone())
                    .with_detail(line.trim()),
                );
                if package {
                    let requested = without_extension.as_str();
                    if let Some((distance, candidate)) =
                        crate::fixes::nearest_known_package(requested)
                    {
                        diagnostic.provenance.push(
                            aitex_core::ProvenanceFrame::new(
                                aitex_core::ProvenanceKind::FailingToken,
                                "known package spelling candidate",
                            )
                            .at(span.clone())
                            .with_detail(format!(
                                "`{requested}` is {distance} edit from known package `{candidate}`; verify and test the smallest correction"
                            )),
                        );
                    }
                } else if let Some(candidate) = missing_path_spelling_candidate(name) {
                    diagnostic.provenance.push(
                        aitex_core::ProvenanceFrame::new(
                            aitex_core::ProvenanceKind::FailingToken,
                            "path spelling candidate",
                        )
                        .at(span.clone())
                        .with_detail(format!(
                            "requested `{name}` has a suffix after a recognized extension; verify candidate `{candidate}`"
                        )),
                    );
                }
                return;
            }
        }
    }
}

fn artifact_size(path: &Path, artifact: ValidationArtifact) -> Result<u64, ValidationError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ValidationError(format!(
            "isolated build did not produce readable {}: {error}",
            artifact.label()
        ))
    })?;
    let structurally_valid = match artifact {
        ValidationArtifact::Pdf => {
            bytes.starts_with(b"%PDF-")
                && bytes[bytes.len().saturating_sub(2_048)..]
                    .windows(5)
                    .any(|window| window == b"%%EOF")
        }
        ValidationArtifact::Dvi => {
            bytes.first() == Some(&247) && bytes.iter().rev().take(64).any(|byte| *byte == 249)
        }
    };
    if bytes.len() < 16 || !structurally_valid {
        return Err(ValidationError(format!(
            "isolated build produced a malformed {} artifact",
            artifact.label()
        )));
    }
    Ok(bytes.len() as u64)
}

impl RepairWorkspace {
    pub fn new(source: &Path, limits: ValidationLimits) -> Result<Self, ValidationError> {
        Self::copy_from(source, limits, false)
    }

    fn copy_from(
        source: &Path,
        limits: ValidationLimits,
        preserve_private_cache: bool,
    ) -> Result<Self, ValidationError> {
        if !source.is_dir() {
            return Err(ValidationError(format!(
                "project directory does not exist: {}",
                source.display()
            )));
        }
        let scratch = scratch_project()?;
        let mut files = 0usize;
        let mut bytes = 0u64;
        copy_project(
            source,
            &scratch.path,
            &scratch.path,
            &mut files,
            &mut bytes,
            limits,
            preserve_private_cache,
        )?;
        Ok(Self { scratch, limits })
    }

    pub fn fork(&self) -> Result<Self, ValidationError> {
        Self::copy_from(&self.scratch.path, self.limits, true)
    }

    pub fn root(&self) -> &Path {
        &self.scratch.path
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.limits.timeout = timeout;
    }

    pub fn read(&self, relative: &Path) -> Result<Vec<u8>, ValidationError> {
        if !safe_relative(relative) {
            return Err(ValidationError("repair target escapes the project".into()));
        }
        std::fs::read(self.scratch.path.join(relative)).map_err(|error| {
            ValidationError(format!(
                "cannot read staged {}: {error}",
                relative.display()
            ))
        })
    }

    pub fn write(&self, relative: &Path, bytes: &[u8]) -> Result<(), ValidationError> {
        if !safe_relative(relative) {
            return Err(ValidationError("repair target escapes the project".into()));
        }
        let target = self.scratch.path.join(relative);
        if !target.is_file() || !crate::author_source(&target) {
            return Err(ValidationError(format!(
                "{} is not an existing author-source file",
                relative.display()
            )));
        }
        std::fs::write(target, bytes)
            .map_err(|error| ValidationError(format!("cannot stage source patch: {error}")))
    }

    pub fn build(
        &self,
        executable: &Path,
        input: &Path,
        artifact: ValidationArtifact,
        format: Option<&Path>,
        tex82: bool,
    ) -> Result<WorkspaceBuild, ValidationError> {
        if !executable.is_file() {
            return Err(ValidationError(format!(
                "aiTeX executable does not exist: {}",
                executable.display()
            )));
        }
        if !safe_relative(input) {
            return Err(ValidationError(
                "validation input must stay inside the project".into(),
            ));
        }
        let output = artifact.filename();
        let artifact_path = self.scratch.path.join(output);
        let _ = std::fs::remove_file(&artifact_path);
        let stdout_path = self.scratch.path.join(".aitex-repair-stdout");
        let stderr_path = self.scratch.path.join(".aitex-repair-stderr");
        let stdout = std::fs::File::create(&stdout_path).map_err(|error| {
            ValidationError(format!("cannot capture validation output: {error}"))
        })?;
        let stderr = std::fs::File::create(&stderr_path).map_err(|error| {
            ValidationError(format!("cannot capture validation error: {error}"))
        })?;

        let mut command = std::process::Command::new(executable);
        command
            .current_dir(&self.scratch.path)
            .arg("build")
            .arg(input)
            .arg("-o")
            .arg(output)
            .arg("--cache")
            .arg(".aitex-repair-cache")
            .arg("--no-banner")
            .arg("-halt-on-error")
            .arg("-no-shell-escape")
            .stdout(stdout)
            .stderr(stderr)
            .env("AITEX_NO_BANNER", "1")
            .env("AITEX_REPAIR_CHILD", "1")
            .env_remove("AITEX_DVI")
            .env_remove("AITEX_CACHE")
            .env_remove("AITEX_INCREMENTAL");
        if tex82 {
            command.arg("--tex82");
        }
        if let Some(format) = format {
            command.arg("--fmt").arg(format);
        }

        let started = Instant::now();
        let mut child = command.spawn().map_err(|error| {
            ValidationError(format!("cannot start isolated aiTeX validation: {error}"))
        })?;
        let status = loop {
            match child.try_wait().map_err(|error| {
                ValidationError(format!("cannot monitor isolated validation: {error}"))
            })? {
                Some(status) => break status,
                None if started.elapsed() < self.limits.timeout => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ValidationError(format!(
                        "isolated full build exceeded the {} second repair limit",
                        self.limits.timeout.as_secs()
                    )));
                }
            }
        };
        let elapsed = started.elapsed();
        let mut files = 0usize;
        let mut bytes = 0u64;
        enforce_tree_limits(&self.scratch.path, &mut files, &mut bytes, self.limits)?;
        let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
        let phases = stderr
            .lines()
            .filter(|line| line.starts_with("  ✓ "))
            .map(str::to_owned)
            .collect();
        if status.success() {
            let artifact_bytes = artifact_size(&artifact_path, artifact)?;
            Ok(WorkspaceBuild {
                succeeded: true,
                elapsed,
                artifact_bytes: Some(artifact_bytes),
                diagnostic: None,
                detail: format!("complete isolated {} build", artifact.label()),
                phases,
            })
        } else {
            let diagnostic = first_workspace_diagnostic(&self.scratch.path, input, &stderr);
            let detail = diagnostic.as_ref().map_or_else(
                || format!("isolated full build rejected the patch ({status})"),
                |diagnostic| diagnostic.message.clone(),
            );
            Ok(WorkspaceBuild {
                succeeded: false,
                elapsed,
                artifact_bytes: None,
                diagnostic,
                detail,
                phases,
            })
        }
    }
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

pub fn validate_edit(request: &ValidationRequest<'_>) -> Result<ValidationResult, ValidationError> {
    if !request.executable.is_file() {
        return Err(ValidationError(format!(
            "aiTeX executable does not exist: {}",
            request.executable.display()
        )));
    }
    if !request.project_root.is_dir() {
        return Err(ValidationError(format!(
            "project directory does not exist: {}",
            request.project_root.display()
        )));
    }
    if !safe_relative(request.input) || !safe_relative(request.target) {
        return Err(ValidationError(
            "validation input and patch target must stay inside the project".into(),
        ));
    }

    let workspace = RepairWorkspace::new(request.project_root, ValidationLimits::default())?;
    workspace.write(request.target, request.after)?;
    let build = workspace.build(
        request.executable,
        request.input,
        request.artifact,
        request.format,
        request.tex82,
    )?;
    if !build.succeeded {
        return Err(ValidationError(format!(
            "isolated full build rejected the patch ({})",
            build.detail
        )));
    }
    Ok(ValidationResult {
        elapsed: build.elapsed,
        artifact_bytes: build.artifact_bytes.unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        RepairWorkspace, ValidationArtifact, ValidationLimits, artifact_size, enforce_tree_limits,
        enrich_diagnostic_origin, fallback_message_location, first_workspace_diagnostic,
        literal_unresolved_definitions, safe_relative,
    };
    use std::path::Path;

    #[test]
    fn validation_paths_stay_below_the_project() {
        assert!(safe_relative(Path::new("main.tex")));
        assert!(safe_relative(Path::new("chapters/one.tex")));
        assert!(!safe_relative(Path::new("../main.tex")));
        assert!(!safe_relative(Path::new("/tmp/main.tex")));
    }

    #[test]
    fn artifact_gate_rejects_a_named_but_malformed_pdf() {
        let path =
            std::env::temp_dir().join(format!("aitex-malformed-repair-pdf-{}", std::process::id()));
        std::fs::write(&path, b"not actually a PDF despite the extension").unwrap();
        assert!(artifact_size(&path, ValidationArtifact::Pdf).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn workspace_limit_counts_files_created_during_validation() {
        let root =
            std::env::temp_dir().join(format!("aitex-repair-tree-limit-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.tex"), b"source").unwrap();
        std::fs::write(root.join("generated.aux"), b"generated").unwrap();
        let mut files = 0;
        let mut bytes = 0;
        let limits = ValidationLimits {
            max_files: 1,
            ..ValidationLimits::default()
        };
        assert!(enforce_tree_limits(&root, &mut files, &mut bytes, limits).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_inputs_are_mapped_back_to_the_author_command() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-missing-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"\\documentclass{article}\n\\input{chapters/missing}\n",
        )
        .unwrap();
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingFile {
                name: "chapters/missing.tex".into(),
            },
            "LaTeX Error: File `chapters/missing.tex' not found.",
            aitex_core::Source::TexLog,
        );
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut diagnostic);
        assert_eq!(diagnostic.cause, Some(aitex_core::Span::new("main.tex", 2)));
        assert_eq!(
            diagnostic.provenance[0].kind,
            aitex_core::ProvenanceKind::Include
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_glossary_records_are_mapped_by_command_order() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-glossary-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("preamble")).unwrap();
        std::fs::write(root.join("main.tex"), b"\\input{preamble/glossary}\n").unwrap();
        std::fs::write(
            root.join("preamble/glossary.tex"),
            b"\\newacronym{one}{ONE}{First}\n\\newacronym{two}{}{Second}\n",
        )
        .unwrap();
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Glossary,
            "Illegal null field",
            aitex_core::Source::ToolLog(aitex_core::Tool::MakeGlossaries),
        )
        .at(aitex_core::Span::new("main.acn", 2));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut diagnostic);
        assert_eq!(
            diagnostic.cause,
            Some(aitex_core::Span::new("preamble/glossary.tex", 2))
        );
        assert_eq!(
            diagnostic.provenance[0].kind,
            aitex_core::ProvenanceKind::GeneratedOrigin
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_glossary_key_maps_to_its_empty_author_field() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-glossary-key-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("paper/preamble")).unwrap();
        std::fs::write(root.join("paper/main.tex"), b"\\input{preamble/glossary}\n").unwrap();
        std::fs::write(
            root.join("paper/preamble/glossary.tex"),
            b"\\newglossaryentry{first}{name={First},description={One}}\n\\newglossaryentry{broken}{\n  name={},\n  description={Broken}\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("paper/main.glo"),
            b"\\glossaryentry{broken?\\glossentry{broken}|setentrycounter[]{page}\\glsnumberformat}{1}\n",
        )
        .unwrap();
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Glossary,
            "Illegal null field",
            aitex_core::Source::ToolLog(aitex_core::Tool::MakeGlossaries),
        )
        .at(aitex_core::Span::new("main.glo", 1));
        enrich_diagnostic_origin(&root, Path::new("paper/main.tex"), &mut diagnostic);
        assert_eq!(
            diagnostic.cause,
            Some(aitex_core::Span::new("paper/preamble/glossary.tex", 3))
        );
        assert_eq!(diagnostic.provenance[0].label, "glossary key `broken`");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn math_recovery_uses_an_odd_reported_or_immediately_preceding_line() {
        let root =
            std::env::temp_dir().join(format!("aitex-repair-math-origin-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"balanced $x$\nmissing $close\nrecovery $y$\nsame-line $broken\n",
        )
        .unwrap();
        let mut prior = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingMathShift,
            "Missing $ inserted.",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("main.tex", 3));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut prior);
        assert_eq!(prior.cause, Some(aitex_core::Span::new("main.tex", 2)));
        let mut same = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingMathShift,
            "Missing $ inserted.",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("main.tex", 4));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut same);
        assert_eq!(same.cause, Some(aitex_core::Span::new("main.tex", 4)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn math_recovery_searches_one_bounded_paragraph_without_global_guessing() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-math-paragraph-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"heading\n\n\\author{Alice $1\nBob $2$}\nmore $x$\n\\maketitle\n\nvalid $math\nacross lines$\n\\section{Next}\n",
        )
        .unwrap();
        let mut unique = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingMathShift,
            "Extra }, or forgotten $.",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("main.tex", 6));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut unique);
        assert_eq!(unique.cause, Some(aitex_core::Span::new("main.tex", 3)));

        let mut ambiguous = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingMathShift,
            "Missing $ inserted.",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("main.tex", 10));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut ambiguous);
        assert_eq!(ambiguous.cause, None, "multiline math must not be guessed");

        std::fs::write(
            root.join("preamble.tex"),
            b"\\title{Example}\n\\author{Name $^1}\n\nmetadata $x$\n\n\\maketitle\n",
        )
        .unwrap();
        let mut delayed = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Other,
            "Extra }, or forgotten $.",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("preamble.tex", 6));
        enrich_diagnostic_origin(&root, Path::new("preamble.tex"), &mut delayed);
        assert_eq!(
            delayed.cause,
            Some(aitex_core::Span::new("preamble.tex", 2))
        );
        assert_eq!(delayed.kind, aitex_core::Kind::MissingMathShift);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unique_failing_tokens_relocate_recovery_diagnostics() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-unique-token-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("chapters")).unwrap();
        std::fs::write(root.join("main.tex"), b"\\input{chapters/body}\n").unwrap();
        std::fs::write(
            root.join("chapters/body.tex"),
            b"\\begin{itemize}\n\\item one\n\\end{itemizex}\n\\textbxf{two}\n",
        )
        .unwrap();

        let mut environment = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MismatchedEnvironment {
                began: "itemize".into(),
                ended: "itemizex".into(),
            },
            "begin itemize ended by end itemizex",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("main.tex", 20));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut environment);
        assert_eq!(
            environment.cause,
            Some(aitex_core::Span::new("chapters/body.tex", 3))
        );

        let mut control_sequence = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::UndefinedControlSequence {
                name: "textbxf".into(),
            },
            "Undefined control sequence.",
            aitex_core::Source::TexLog,
        )
        .at(aitex_core::Span::new("main.tex", 20));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut control_sequence);
        assert_eq!(
            control_sequence.cause,
            Some(aitex_core::Span::new("chapters/body.tex", 4))
        );
        assert!(control_sequence.provenance[0].label.contains("`\\textbxf`"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn suspicious_missing_names_get_conservative_spelling_evidence() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-spelling-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"\\usepackage{bmx}\n\\includegraphics{figure.png-missing}\n",
        )
        .unwrap();
        let mut package = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingPackage {
                name: "bmx.sty".into(),
            },
            "missing package",
            aitex_core::Source::TexLog,
        );
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut package);
        assert!(package.provenance.iter().any(|frame| {
            frame.label == "known package spelling candidate"
                && frame
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("`bm`"))
        }));
        let mut graphic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::MissingFile {
                name: "figure.png-missing".into(),
            },
            "missing graphic",
            aitex_core::Source::TexLog,
        );
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut graphic);
        assert!(graphic.provenance.iter().any(|frame| {
            frame.label == "path spelling candidate"
                && frame
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("`figure.png`"))
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn biber_temporary_locations_and_named_entry_fields_map_to_author_source() {
        let root =
            std::env::temp_dir().join(format!("aitex-repair-biber-origin-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.tex"), b"\\bibliography{references}\n").unwrap();
        std::fs::write(
            root.join("references.bib"),
            b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n  title = {Missing comma}\n  volume = {2}\n@article{Stein,\n  author = {L. C. Steinhauer,W. D. Kimura,\n  doi = {10.1/example},\n}\n",
        )
        .unwrap();

        let mut recovery = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Bibliography,
            "ERROR - BibTeX subsystem: /tmp/biber/a.utf8, line 12, syntax error: found \"volume\", expected end of entry",
            aitex_core::Source::ToolLog(aitex_core::Tool::Biber),
        )
        .at(aitex_core::Span::new("/tmp/biber/a.utf8", 12));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut recovery);
        assert_eq!(
            recovery.reported,
            Some(aitex_core::Span::new("references.bib", 12))
        );
        assert_eq!(
            recovery.cause,
            Some(aitex_core::Span::new("references.bib", 11))
        );

        let mut named = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Bibliography,
            "ERROR - Name \"L. C. Steinhauer,W. D. Kimura\" has too many commas, skipping entry \\'Stein\\'",
            aitex_core::Source::ToolLog(aitex_core::Tool::Biber),
        );
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut named);
        assert_eq!(
            named.cause,
            Some(aitex_core::Span::new("references.bib", 14))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_index_order_prefers_the_producing_root_over_a_sibling() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-index-root-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("a-sibling.tex"),
            b"\\index{sibling-one}\n\\index{sibling-two}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"\\index{root-one}\n\\index{@root-two}\n",
        )
        .unwrap();
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::Index,
            "Extra `@' in first argument",
            aitex_core::Source::ToolLog(aitex_core::Tool::MakeIndex),
        )
        .at(aitex_core::Span::new("main.idx", 2));
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut diagnostic);
        assert_eq!(diagnostic.cause, Some(aitex_core::Span::new("main.tex", 2)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn expanded_glossary_keys_map_back_to_the_macro_template() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-glossary-template-origin-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.tex"), b"\\input{acronyms}\n").unwrap();
        std::fs::write(
            root.join("acronyms.tex"),
            b"\\newcommand{\\Normal}[1]{\\Gls{#1-missing}}\n",
        )
        .unwrap();
        let mut diagnostic = aitex_core::Diagnostic::new(
            aitex_core::Severity::Error,
            aitex_core::Kind::PackageError {
                package: "glossaries".into(),
            },
            "Package glossaries Error: Glossary entry `CVQKD-missing' has not been defined.",
            aitex_core::Source::TexLog,
        );
        enrich_diagnostic_origin(&root, Path::new("main.tex"), &mut diagnostic);
        assert_eq!(
            diagnostic.cause,
            Some(aitex_core::Span::new("acronyms.tex", 1))
        );
        assert_eq!(
            diagnostic.provenance[0].kind,
            aitex_core::ProvenanceKind::MacroDefinition
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_builds_retain_additional_unresolved_reference_evidence() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-unresolved-reference-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"\\label{old-a}\n\\label{old-b}\n\\label{old-c}\n\\label{old-d}\nSee \\ref{target}.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.log"),
            b"! Undefined control sequence.\nl.20 \\fraxc{1}{2}\n\n\
              LaTeX Warning: Reference `old-a' on page 1 undefined on input line 1.\n\
              LaTeX Warning: Reference `old-b' on page 1 undefined on input line 2.\n\
              LaTeX Warning: Reference `old-c' on page 1 undefined on input line 3.\n\
              LaTeX Warning: Reference `old-d' on page 1 undefined on input line 4.\n\
              LaTeX Warning: Reference `target-missing' on page 1 undefined on input line 5.\n",
        )
        .unwrap();
        let diagnostic = first_workspace_diagnostic(&root, Path::new("main.tex"), "")
            .expect("the hard error should be diagnosed");
        let related = diagnostic
            .provenance
            .iter()
            .find(|frame| frame.label.starts_with("additional unresolved diagnostic"))
            .expect("one related warning should be retained");
        assert!(
            related
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("target-missing"))
        );
        assert_eq!(related.span.as_ref().map(|span| span.line), Some(5));
        assert!(related.label.contains("no literal definition found"));
        assert_eq!(
            diagnostic
                .provenance
                .iter()
                .filter(|frame| frame.label.starts_with("additional unresolved diagnostic"))
                .count(),
            1,
            "first-pass warnings with literal label definitions are noise"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_incidental_math_warning_does_not_displace_a_later_build_error() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-warning-before-error-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"text\n\\includegraphics{plot.pdf-missing}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.log"),
            b"LaTeX Warning: Command \\textendash invalid in math mode on input line 1.\n\
              ! LaTeX Error: File `plot.pdf-missing' not found.\n\
              l.2 \\includegraphics{plot.pdf-missing}\n",
        )
        .unwrap();
        let diagnostic = first_workspace_diagnostic(&root, Path::new("main.tex"), "")
            .expect("the real missing-file error should be diagnosed");
        assert!(matches!(
            diagnostic.kind,
            aitex_core::Kind::MissingFile { ref name } if name == "plot.pdf-missing"
        ));
        assert_eq!(diagnostic.location().map(|span| span.line), Some(2));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn captured_compact_trace_retains_its_file_line_location() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-compact-trace-location-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.tex"), b"broken\n").unwrap();
        assert_eq!(
            fallback_message_location(
                &root,
                Path::new("main.tex"),
                "↳ include · root input · main.tex:3569",
            ),
            Some(("main.tex".into(), 3569))
        );
        assert_eq!(
            fallback_message_location(
                &root,
                Path::new("main.tex"),
                "↳ include · root input · ../outside.tex:9",
            ),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_warning_ranking_uses_author_definitions_not_key_spelling() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-unresolved-ranking-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"\\label{looks-missing}\nSee \\ref{looks-missing} and \\ref{ordinary}.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.log"),
            b"! Undefined control sequence.\nl.1 \\fraxc{1}{2}\n\n\
              LaTeX Warning: Reference `looks-missing' on page 1 undefined on input line 2.\n\
              LaTeX Warning: Reference `ordinary' on page 1 undefined on input line 2.\n",
        )
        .unwrap();
        let diagnostic = first_workspace_diagnostic(&root, Path::new("main.tex"), "")
            .expect("the hard error should be diagnosed");
        let related = diagnostic
            .provenance
            .iter()
            .find(|frame| frame.label.contains("no literal definition found"))
            .expect("the warning without a definition should rank first");
        assert!(
            related
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("ordinary"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_definition_index_scans_each_author_tree_once() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-definition-index-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tex"),
            b"\\label {forward}\n\\bibitem {inline-key} Entry\n",
        )
        .unwrap();
        std::fs::write(
            root.join("references.bib"),
            b"@article{\n  database-key,\n  title = {Example}\n}\n",
        )
        .unwrap();
        let definitions = literal_unresolved_definitions(&root);
        assert!(definitions.0.contains("forward"));
        assert!(definitions.1.contains("inline-key"));
        assert!(definitions.1.contains("database-key"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn private_cache_is_clean_initially_and_carried_only_across_workspace_forks() {
        let root = std::env::temp_dir().join(format!(
            "aitex-repair-private-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let supplied_cache = root.join(".aitex-repair-cache");
        std::fs::create_dir_all(&supplied_cache).unwrap();
        std::fs::write(root.join("main.tex"), b"source").unwrap();
        std::fs::write(supplied_cache.join("poison"), b"untrusted").unwrap();

        let workspace = RepairWorkspace::new(&root, ValidationLimits::default()).unwrap();
        assert!(!workspace.root().join(".aitex-repair-cache").exists());
        let cache = workspace.root().join(".aitex-repair-cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("trusted"), b"compiler-created").unwrap();
        let fork = workspace.fork().unwrap();
        assert_eq!(
            std::fs::read(fork.root().join(".aitex-repair-cache/trusted")).unwrap(),
            b"compiler-created"
        );
        assert!(!fork.root().join(".aitex-repair-cache/poison").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
