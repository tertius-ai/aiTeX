//! Strict, byte-preserving repair patches shared by the CLI and evaluator.

use crate::Action;
use crate::session::SafetyRecord;
use aitex_core::Diagnostic;
use std::fmt;
use std::path::{Component, Path, PathBuf};

const MAX_PATCH_BYTES: usize = 64 * 1024;
const MAX_HUNKS: usize = 16;
const MAX_STRUCTURED_EDITS: usize = 8;
const MAX_STRUCTURED_EDIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchError(pub String);

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PatchError {}

impl From<String> for PatchError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// A model diff after conservative transport repair and strict application.
/// The original response remains the benchmark artifact; this records exactly
/// what aiTeX had to normalize before it could interpret that response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPatch {
    pub patch: String,
    pub after: Vec<u8>,
    pub repaired_hunk_counts: bool,
    pub repaired_hunk_locations: bool,
    pub stripped_excerpt_labels: bool,
    pub stripped_marker_padding: bool,
    pub decoded_backslashes: bool,
}

/// A source-grounded replacement requested by a model.  This is deliberately
/// less expressive than a unified diff: the old text must occur exactly once
/// in one existing author file, so line-number and hunk-format mistakes cannot
/// obscure whether the model found the right repair.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StructuredEdit {
    pub path: PathBuf,
    pub old: String,
    pub new: String,
}

impl PreparedPatch {
    pub fn normalization(&self) -> Option<String> {
        let mut repairs = Vec::new();
        if self.repaired_hunk_counts {
            repairs.push("hunk-counts");
        }
        if self.repaired_hunk_locations {
            repairs.push("hunk-locations");
        }
        if self.stripped_excerpt_labels {
            repairs.push("excerpt-labels");
        }
        if self.stripped_marker_padding {
            repairs.push("diff-marker-padding");
        }
        if self.decoded_backslashes {
            repairs.push("json-backslashes");
        }
        (!repairs.is_empty()).then(|| repairs.join(","))
    }
}

#[derive(Debug, Clone)]
struct PatchLine {
    operation: u8,
    text: String,
}

#[derive(Debug, Clone)]
struct PatchHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<PatchLine>,
}

#[derive(Clone)]
struct SourceLine {
    text: Vec<u8>,
    ending: Vec<u8>,
}

pub fn author_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("tex" | "ltx" | "sty" | "cls" | "cfg" | "bib" | "bst" | "bbx" | "cbx")
    )
}

fn parse_range(value: &str, prefix: char) -> Result<(usize, usize), PatchError> {
    let value = value
        .strip_prefix(prefix)
        .ok_or_else(|| PatchError("malformed unified-diff hunk range".into()))?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start = start
        .parse::<usize>()
        .map_err(|_| PatchError("malformed unified-diff line number".into()))?;
    let count = count
        .parse::<usize>()
        .map_err(|_| PatchError("malformed unified-diff line count".into()))?;
    Ok((start, count))
}

fn parse_hunk_header(line: &str) -> Result<(usize, usize, usize, usize), PatchError> {
    let rest = line
        .strip_prefix("@@ ")
        .and_then(|value| value.split_once(" @@").map(|pair| pair.0))
        .ok_or_else(|| PatchError("malformed unified-diff hunk header".into()))?;
    let mut ranges = rest.split_whitespace();
    let old = ranges
        .next()
        .ok_or_else(|| PatchError("missing old hunk range".into()))?;
    let new = ranges
        .next()
        .ok_or_else(|| PatchError("missing new hunk range".into()))?;
    if ranges.next().is_some() {
        return Err(PatchError("malformed unified-diff hunk ranges".into()));
    }
    let (old_start, old_count) = parse_range(old, '-')?;
    let (new_start, new_count) = parse_range(new, '+')?;
    Ok((old_start, old_count, new_start, new_count))
}

fn author_path(raw: &str) -> Result<PathBuf, PatchError> {
    let raw = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    let path = PathBuf::from(raw);
    if raw.is_empty()
        || raw == "/dev/null"
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        || !author_source(&path)
    {
        return Err(PatchError(
            "model patch names an unsafe author-source path".into(),
        ));
    }
    Ok(path)
}

fn parse_path(header: &str, marker: &str) -> Result<PathBuf, PatchError> {
    let raw = header
        .strip_prefix(marker)
        .ok_or_else(|| PatchError("model patch is not a unified diff".into()))?
        .split('\t')
        .next()
        .unwrap_or_default()
        .trim();
    author_path(raw)
}

fn parse(patch: &str) -> Result<(PathBuf, Vec<PatchHunk>), PatchError> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err(PatchError(
            "model patch exceeds the 64 KiB repair limit".into(),
        ));
    }
    if patch.contains('\r') {
        return Err(PatchError(
            "model patch must use canonical LF diff lines".into(),
        ));
    }
    let mut lines = patch.lines();
    let old = lines
        .next()
        .ok_or_else(|| PatchError("model returned an empty patch".into()))?;
    let new = lines
        .next()
        .ok_or_else(|| PatchError("model patch has no new-file header".into()))?;
    let old_path = parse_path(old, "--- ")?;
    let new_path = parse_path(new, "+++ ")?;
    if old_path != new_path {
        return Err(PatchError(
            "model patch must modify exactly one existing file".into(),
        ));
    }
    let mut hunks = Vec::new();
    let mut current: Option<PatchHunk> = None;
    for line in lines {
        if line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            let (old_start, old_count, new_start, new_count) = parse_hunk_header(line)?;
            current = Some(PatchHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
            continue;
        }
        let hunk = current
            .as_mut()
            .ok_or_else(|| PatchError("model patch contains text outside a hunk".into()))?;
        let Some(operation) = line.as_bytes().first().copied() else {
            return Err(PatchError(
                "every unified-diff hunk line needs an operation prefix".into(),
            ));
        };
        if !matches!(operation, b' ' | b'+' | b'-') {
            return Err(PatchError(
                "model patch uses an unsupported unified-diff operation".into(),
            ));
        }
        hunk.lines.push(PatchLine {
            operation,
            text: line[1..].to_string(),
        });
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    if hunks.is_empty() || hunks.len() > MAX_HUNKS {
        return Err(PatchError(
            "model patch must contain between one and sixteen hunks".into(),
        ));
    }
    Ok((old_path, hunks))
}

/// Recompute hunk counts from their operation lines while retaining line
/// positions and optional function context. Models frequently count the
/// visible excerpt rather than the emitted hunk; counts are redundant metadata
/// and repairing only them cannot alter an edit.
fn repair_hunk_counts(patch: &str) -> Result<(String, bool), PatchError> {
    if patch.len() > MAX_PATCH_BYTES || patch.contains('\r') {
        // Preserve the strict parser's precise error.
        parse(patch)?;
    }
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.len() < 3 {
        parse(patch)?;
    }
    let mut result = String::new();
    let mut repaired = false;
    let mut at = 0usize;
    while at < lines.len() {
        let line = lines[at];
        if !line.starts_with("@@ ") {
            result.push_str(line);
            result.push('\n');
            at += 1;
            continue;
        }
        let (old_start, old_count, new_start, new_count) = parse_hunk_header(line)?;
        let tail = line
            .strip_prefix("@@ ")
            .and_then(|value| value.split_once(" @@").map(|pair| pair.1))
            .ok_or_else(|| PatchError("malformed unified-diff hunk header".into()))?;
        let mut old_actual = 0usize;
        let mut new_actual = 0usize;
        let mut end = at + 1;
        while end < lines.len() && !lines[end].starts_with("@@ ") {
            match lines[end].as_bytes().first() {
                Some(b' ') => {
                    old_actual += 1;
                    new_actual += 1;
                }
                Some(b'-') => old_actual += 1,
                Some(b'+') => new_actual += 1,
                _ => {
                    // Let the strict parser produce the canonical error.
                    parse(patch)?;
                }
            }
            end += 1;
        }
        if old_actual == old_count && new_actual == new_count {
            result.push_str(line);
        } else {
            repaired = true;
            use fmt::Write as _;
            let _ = write!(
                result,
                "@@ -{old_start},{old_actual} +{new_start},{new_actual} @@{tail}"
            );
        }
        result.push('\n');
        for body in &lines[at + 1..end] {
            result.push_str(body);
            result.push('\n');
        }
        at = end;
    }
    if !patch.ends_with('\n') {
        result.pop();
    }
    Ok((result, repaired))
}

fn decode_backslashes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while at < bytes.len() {
        decoded.push(bytes[at]);
        if bytes[at] == b'\\' && bytes.get(at + 1) == Some(&b'\\') {
            at += 2;
        } else {
            at += 1;
        }
    }
    String::from_utf8(decoded).expect("removing ASCII bytes preserves UTF-8")
}

/// Does the source itself prove that hunk payloads were copied with one extra
/// JSON-escape layer? Every old/context line must match either already or after
/// decoding, and at least one must require decoding. This prevents a blind
/// global rewrite of intentional TeX `\\` line breaks.
fn source_proves_extra_backslashes(patch: &str, source: &[u8]) -> bool {
    if std::str::from_utf8(source).is_err() {
        return false;
    }
    let Ok((_, hunks)) = parse(patch) else {
        return false;
    };
    let source = source_lines(source);
    let mut evidence = false;
    for hunk in hunks {
        let mut at = hunk.old_start.saturating_sub(1);
        for line in hunk.lines {
            if line.operation == b'+' {
                continue;
            }
            let Some(actual) = source.get(at).map(|line| line.text.as_slice()) else {
                return false;
            };
            let raw = line.text.as_bytes();
            if raw == actual && line.text.contains("\\\\") {
                return false;
            }
            if raw != actual {
                let decoded = decode_backslashes(&line.text);
                if decoded.as_bytes() != actual || decoded == line.text {
                    return false;
                }
                evidence = true;
            }
            at += 1;
        }
    }
    evidence
}

fn decode_hunk_backslashes(patch: &str) -> String {
    let mut in_hunk = false;
    let mut result = String::new();
    for line in patch.lines() {
        if line.starts_with("@@ ") {
            in_hunk = true;
            result.push_str(line);
        } else if in_hunk && matches!(line.as_bytes().first(), Some(b' ' | b'+' | b'-')) {
            result.push(char::from(line.as_bytes()[0]));
            result.push_str(&decode_backslashes(&line[1..]));
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    result
}

/// Models occasionally render a unified diff as `- old` / `+ new`, treating
/// the space after the operation as a visual separator. Accept that transport
/// mistake only when every changed line uses it and removing exactly one
/// space makes every removal match its declared source line byte-for-byte.
fn source_proves_marker_padding(patch: &str, source: &[u8]) -> bool {
    let Ok((_, hunks)) = parse(patch) else {
        return false;
    };
    let source = source_lines(source);
    let mut saw_removal = false;
    for hunk in hunks {
        let mut at = hunk.old_start.saturating_sub(1);
        for line in hunk.lines {
            if matches!(line.operation, b'+' | b'-') && !line.text.starts_with(' ') {
                return false;
            }
            if line.operation == b'+' {
                continue;
            }
            let Some(actual) = source.get(at).map(|line| line.text.as_slice()) else {
                return false;
            };
            let expected = if line.operation == b'-' {
                saw_removal = true;
                line.text.as_bytes().get(1..).unwrap_or_default()
            } else {
                line.text.as_bytes()
            };
            if expected != actual {
                return false;
            }
            at += 1;
        }
    }
    saw_removal
}

fn strip_marker_padding(patch: &str) -> Result<String, PatchError> {
    let (path, mut hunks) = parse(patch)?;
    for hunk in &mut hunks {
        for line in &mut hunk.lines {
            if matches!(line.operation, b'+' | b'-') {
                line.text.remove(0);
            }
        }
    }
    Ok(render_patch(&path, &hunks))
}

fn excerpt_label(text: &str) -> Option<(usize, &str)> {
    let (prefix, content) = text.split_once('|')?;
    let number = prefix.trim_end();
    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || !prefix[number.len()..]
            .bytes()
            .all(|byte| byte == b' ' || byte == b'\t')
    {
        return None;
    }
    Some((number.parse().ok()?, content))
}

fn render_patch(path: &Path, hunks: &[PatchHunk]) -> String {
    use fmt::Write as _;

    let path = path.to_string_lossy();
    let mut result = format!("--- a/{path}\n+++ b/{path}\n");
    for hunk in hunks {
        let _ = writeln!(
            result,
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        );
        for line in &hunk.lines {
            result.push(char::from(line.operation));
            result.push_str(&line.text);
            result.push('\n');
        }
    }
    result
}

/// Re-anchor hunks only when their complete old side occurs exactly once in
/// the source. Optionally remove the `N|` labels used by aiTeX's source
/// excerpts, but only when every hunk line has such a label and every old-side
/// label agrees with the exact source line it identifies.
fn source_grounded_hunks(
    patch: &str,
    source: &[u8],
    strip_labels: bool,
) -> Result<Option<(String, bool)>, PatchError> {
    if std::str::from_utf8(source).is_err() {
        return Ok(None);
    }
    let (path, mut hunks) = parse(patch)?;
    let source = source_lines(source);
    let mut previous_end = 0usize;
    let mut line_delta = 0isize;
    let mut repaired_location = false;

    for hunk in &mut hunks {
        let labels = if strip_labels {
            let mut labels = Vec::with_capacity(hunk.lines.len());
            for line in &mut hunk.lines {
                let Some((number, content)) = excerpt_label(&line.text) else {
                    return Ok(None);
                };
                labels.push(number);
                line.text = content.to_string();
            }
            Some(labels)
        } else {
            None
        };

        let old_side = hunk
            .lines
            .iter()
            .filter(|line| line.operation != b'+')
            .map(|line| line.text.as_bytes())
            .collect::<Vec<_>>();
        if old_side.is_empty() || old_side.len() > source.len() {
            return Ok(None);
        }
        let candidates = source
            .windows(old_side.len())
            .enumerate()
            .filter(|(_, window)| {
                window
                    .iter()
                    .zip(&old_side)
                    .all(|(actual, expected)| actual.text == *expected)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [anchor] = candidates.as_slice() else {
            return Ok(None);
        };
        if *anchor < previous_end {
            return Ok(None);
        }

        if let Some(labels) = labels {
            let removal_labels = hunk
                .lines
                .iter()
                .zip(&labels)
                .filter(|(line, _)| line.operation == b'-')
                .map(|(_, label)| *label)
                .collect::<Vec<_>>();
            let mut old_offset = 0usize;
            for (line, label) in hunk.lines.iter().zip(labels) {
                if line.operation == b'+' {
                    if !removal_labels.contains(&label) {
                        return Ok(None);
                    }
                    continue;
                }
                if label != anchor + old_offset + 1 {
                    return Ok(None);
                }
                old_offset += 1;
            }
        }

        let old_start = anchor + 1;
        let new_index = isize::try_from(*anchor)
            .ok()
            .and_then(|anchor| anchor.checked_add(line_delta))
            .and_then(|index| usize::try_from(index).ok());
        let Some(new_index) = new_index else {
            return Ok(None);
        };
        repaired_location |= hunk.old_start != old_start || hunk.new_start != new_index + 1;
        hunk.old_start = old_start;
        hunk.new_start = new_index + 1;
        previous_end = anchor + old_side.len();
        let new_count = isize::try_from(hunk.new_count)
            .map_err(|_| PatchError("model patch new-line count overflows".into()))?;
        let old_count = isize::try_from(hunk.old_count)
            .map_err(|_| PatchError("model patch old-line count overflows".into()))?;
        line_delta = line_delta
            .checked_add(new_count - old_count)
            .ok_or_else(|| PatchError("model patch line-count delta overflows".into()))?;
    }

    if !strip_labels && !repaired_location {
        return Ok(None);
    }
    Ok(Some((render_patch(&path, &hunks), repaired_location)))
}

fn finish_prepared_patch(
    patch: String,
    source: &[u8],
    repaired_hunk_counts: bool,
    repaired_hunk_locations: bool,
    stripped_excerpt_labels: bool,
    stripped_marker_padding: bool,
) -> Option<PreparedPatch> {
    if let Ok(after) = apply_unified_patch(&patch, source) {
        return Some(PreparedPatch {
            patch,
            after,
            repaired_hunk_counts,
            repaired_hunk_locations,
            stripped_excerpt_labels,
            stripped_marker_padding,
            decoded_backslashes: false,
        });
    }
    if source_proves_extra_backslashes(&patch, source) {
        let decoded = decode_hunk_backslashes(&patch);
        if let Ok(after) = apply_unified_patch(&decoded, source) {
            return Some(PreparedPatch {
                patch: decoded,
                after,
                repaired_hunk_counts,
                repaired_hunk_locations,
                stripped_excerpt_labels,
                stripped_marker_padding,
                decoded_backslashes: true,
            });
        }
    }
    None
}

/// Prepare an untrusted model diff without guessing at its semantic edit.
///
/// Redundant diff metadata may be repaired without guessing at an edit: hunk
/// counts come from operation lines; hunk positions come from a unique exact
/// match of the complete old side; and copied source-excerpt labels are
/// removed only when they identify those exact source lines. A visual space
/// after `+`/`-` and one extra backslash-escape layer are removed only with the
/// same source evidence. The resulting patch still passes the full strict
/// parser and isolated build gate.
pub fn prepare_model_patch(patch: &str, source: &[u8]) -> Result<PreparedPatch, PatchError> {
    let (canonical, repaired_hunk_counts) = repair_hunk_counts(patch)?;
    let original_error = match apply_unified_patch(&canonical, source) {
        Ok(after) => {
            return Ok(PreparedPatch {
                patch: canonical,
                after,
                repaired_hunk_counts,
                repaired_hunk_locations: false,
                stripped_excerpt_labels: false,
                stripped_marker_padding: false,
                decoded_backslashes: false,
            });
        }
        Err(error) => error,
    };

    if let Some(prepared) = finish_prepared_patch(
        canonical.clone(),
        source,
        repaired_hunk_counts,
        false,
        false,
        false,
    ) {
        return Ok(prepared);
    }
    if let Some((labeled, repaired_location)) = source_grounded_hunks(&canonical, source, true)?
        && let Some(prepared) = finish_prepared_patch(
            labeled,
            source,
            repaired_hunk_counts,
            repaired_location,
            true,
            false,
        )
    {
        return Ok(prepared);
    }
    if let Some((reanchored, _)) = source_grounded_hunks(&canonical, source, false)?
        && let Some(prepared) =
            finish_prepared_patch(reanchored, source, repaired_hunk_counts, true, false, false)
    {
        return Ok(prepared);
    }
    if source_proves_marker_padding(&canonical, source) {
        let unpadded = strip_marker_padding(&canonical)?;
        if let Some(prepared) =
            finish_prepared_patch(unpadded, source, repaired_hunk_counts, false, false, true)
        {
            return Ok(prepared);
        }
    }
    Err(original_error)
}

/// Resolve the single author file named by a structured proposal.
pub fn structured_edit_target(edits: &[StructuredEdit]) -> Result<PathBuf, PatchError> {
    if edits.is_empty() || edits.len() > MAX_STRUCTURED_EDITS {
        return Err(PatchError(
            "model response must contain between one and eight source edits".into(),
        ));
    }
    let first = author_path(&edits[0].path.to_string_lossy())?;
    for edit in &edits[1..] {
        let path = author_path(&edit.path.to_string_lossy())?;
        if path != first {
            return Err(PatchError(
                "one repair proposal must modify exactly one existing file".into(),
            ));
        }
    }
    Ok(first)
}

/// Apply exact search/replace edits and render the result as the canonical
/// patch consumed by the existing safety, validation, and session machinery.
///
/// Edits are evaluated in order.  Each `old` value must be non-empty and occur
/// exactly once in the current bytes; no fuzzy matching or whitespace repair
/// is attempted.  This makes the conversion deterministic and auditable while
/// allowing source files that do not end in a newline.
pub fn prepare_structured_edits(
    edits: &[StructuredEdit],
    source: &[u8],
) -> Result<PreparedPatch, PatchError> {
    let path = structured_edit_target(edits)?;
    let total = edits.iter().try_fold(0usize, |total, edit| {
        total
            .checked_add(edit.old.len())
            .and_then(|value| value.checked_add(edit.new.len()))
            .ok_or_else(|| PatchError("structured repair size overflows".into()))
    })?;
    if total > MAX_STRUCTURED_EDIT_BYTES {
        return Err(PatchError(
            "structured repair exceeds the 64 KiB repair limit".into(),
        ));
    }

    let mut after = source.to_vec();
    let utf8 = std::str::from_utf8(source).is_ok();
    let edit_bytes = |text: &str| -> Result<Vec<u8>, PatchError> {
        if utf8 {
            Ok(text.as_bytes().to_vec())
        } else {
            text.chars()
                .map(|character| {
                    u8::try_from(u32::from(character)).map_err(|_| {
                        PatchError(
                            "structured repair contains text outside the source's byte encoding"
                                .into(),
                        )
                    })
                })
                .collect()
        }
    };
    for edit in edits {
        if edit.old.is_empty() {
            return Err(PatchError(
                "structured repair old text must not be empty".into(),
            ));
        }
        if edit.old == edit.new {
            return Err(PatchError(
                "structured repair replacement makes no source change".into(),
            ));
        }
        let old = edit_bytes(&edit.old)?;
        let new = edit_bytes(&edit.new)?;
        let positions = after
            .windows(old.len())
            .enumerate()
            .filter_map(|(at, window)| (window == old).then_some(at))
            .take(2)
            .collect::<Vec<_>>();
        let [at] = positions.as_slice() else {
            return Err(PatchError(if positions.is_empty() {
                "structured repair old text does not match the current source".into()
            } else {
                "structured repair old text is ambiguous in the current source".into()
            }));
        };
        let mut next = Vec::with_capacity(
            after
                .len()
                .saturating_sub(old.len())
                .saturating_add(new.len()),
        );
        next.extend_from_slice(&after[..*at]);
        next.extend_from_slice(&new);
        next.extend_from_slice(&after[*at + old.len()..]);
        after = next;
    }
    if after == source {
        return Err(PatchError(
            "structured repair makes no source change".into(),
        ));
    }
    let patch = unified_diff(&path, source, &after);
    Ok(PreparedPatch {
        patch,
        after,
        repaired_hunk_counts: false,
        repaired_hunk_locations: false,
        stripped_excerpt_labels: false,
        stripped_marker_padding: false,
        decoded_backslashes: false,
    })
}

/// Reject source edits that can make a failed build appear healthy by hiding
/// errors, truncating the document, or deleting a substantial part of it.
/// This is deliberately conservative: a declined proposal can be edited by a
/// human and validated again, while an over-permissive gate destroys trust.
pub fn inspect_patch_safety(
    patch: &str,
    before: &[u8],
    after: &[u8],
    max_changed_lines: usize,
    max_deleted_lines: usize,
) -> Result<SafetyRecord, PatchError> {
    let (_, hunks) = parse(patch)?;
    let mut added = Vec::new();
    let mut deleted_lines = 0usize;
    for hunk in hunks {
        for line in hunk.lines {
            match line.operation {
                b'+' => added.push(line.text),
                b'-' => deleted_lines = deleted_lines.saturating_add(1),
                _ => {}
            }
        }
    }
    let added_lines = added.len();
    let changed_lines = added_lines.saturating_add(deleted_lines);
    if changed_lines == 0 {
        return Err(PatchError("repair patch makes no source change".into()));
    }
    if changed_lines > max_changed_lines {
        return Err(PatchError(format!(
            "repair changes {changed_lines} lines, above the {max_changed_lines}-line safety limit"
        )));
    }
    if deleted_lines > max_deleted_lines {
        return Err(PatchError(format!(
            "repair deletes {deleted_lines} lines, above the {max_deleted_lines}-line safety limit"
        )));
    }
    let before_lines = before.split(|byte| *byte == b'\n').count().max(1);
    if deleted_lines > 5 && deleted_lines.saturating_mul(4) > before_lines {
        return Err(PatchError(
            "repair deletes more than one quarter of the source".into(),
        ));
    }
    if before.len() > 1024 && after.len().saturating_mul(4) < before.len().saturating_mul(3) {
        return Err(PatchError(
            "repair removes more than one quarter of the source bytes".into(),
        ));
    }

    let additions = added.join("\n");
    let forbidden = [
        "\\write18",
        "\\input|\"",
        "\\openout",
        "\\batchmode",
        "\\scrollmode",
        "\\nonstopmode",
        "\\errorstopmode",
        "\\nofiles",
    ];
    if let Some(command) = forbidden
        .iter()
        .find(|command| additions.contains(**command))
    {
        return Err(PatchError(format!(
            "repair introduces forbidden control sequence {command}"
        )));
    }
    let count = |bytes: &[u8], needle: &[u8]| {
        bytes
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    };
    if count(after, b"\\end{document}") > count(before, b"\\end{document}")
        && count(before, b"\\end{document}") > 0
    {
        return Err(PatchError(
            "repair introduces an additional \\end{document}".into(),
        ));
    }
    for required in [
        b"\\begin{document}".as_slice(),
        b"\\end{document}".as_slice(),
    ] {
        if count(before, required) > 0 && count(after, required) == 0 {
            return Err(PatchError(format!(
                "repair removes required document marker {}",
                String::from_utf8_lossy(required)
            )));
        }
    }

    Ok(SafetyRecord {
        before_bytes: before.len(),
        after_bytes: after.len(),
        added_lines,
        deleted_lines,
    })
}

pub fn patch_target(patch: &str) -> Result<PathBuf, PatchError> {
    parse(patch).map(|(path, _)| path)
}

fn source_lines(source: &[u8]) -> Vec<SourceLine> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < source.len() {
        if let Some(offset) = source[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + offset;
            if end > start && source[end - 1] == b'\r' {
                lines.push(SourceLine {
                    text: source[start..end - 1].to_vec(),
                    ending: b"\r\n".to_vec(),
                });
            } else {
                lines.push(SourceLine {
                    text: source[start..end].to_vec(),
                    ending: b"\n".to_vec(),
                });
            }
            start = end + 1;
        } else {
            lines.push(SourceLine {
                text: source[start..].to_vec(),
                ending: Vec::new(),
            });
            break;
        }
    }
    lines
}

pub fn apply_unified_patch(patch: &str, source: &[u8]) -> Result<Vec<u8>, PatchError> {
    let (_, hunks) = parse(patch)?;
    let lines = source_lines(source);
    let source_has_final_newline = lines.last().is_none_or(|line| !line.ending.is_empty());
    let utf8 = std::str::from_utf8(source).is_ok();
    let patch_bytes = |text: &str| -> Result<Vec<u8>, PatchError> {
        if utf8 {
            Ok(text.as_bytes().to_vec())
        } else {
            text.chars()
                .map(|character| {
                    u8::try_from(u32::from(character)).map_err(|_| {
                        PatchError("patch contains text outside the source's byte encoding".into())
                    })
                })
                .collect()
        }
    };
    let ending = lines
        .iter()
        .find(|line| !line.ending.is_empty())
        .map_or_else(|| b"\n".to_vec(), |line| line.ending.clone());
    let mut old_at = 0usize;
    let mut result: Vec<SourceLine> = Vec::new();
    for hunk in &hunks {
        let wanted = if hunk.old_start == 0 {
            if hunk.old_count != 0 {
                return Err(PatchError(
                    "only an empty insertion may start at old line zero".into(),
                ));
            }
            0
        } else {
            hunk.old_start - 1
        };
        if wanted < old_at || wanted > lines.len() {
            return Err(PatchError(
                "model patch hunks overlap or lie outside the source".into(),
            ));
        }
        result.extend_from_slice(&lines[old_at..wanted]);
        if hunk.new_start.saturating_sub(1) != result.len() {
            return Err(PatchError(
                "model patch new-file line numbers are inconsistent".into(),
            ));
        }
        old_at = wanted;
        let mut old_seen = 0usize;
        let mut new_seen = 0usize;
        for line in &hunk.lines {
            let patch_line = patch_bytes(&line.text)?;
            match line.operation {
                b' ' => {
                    let source_line = lines.get(old_at).ok_or_else(|| {
                        PatchError("patch context extends beyond the source".into())
                    })?;
                    if source_line.text != patch_line {
                        return Err(PatchError(
                            "patch context does not match the current source".into(),
                        ));
                    }
                    result.push(source_line.clone());
                    old_at += 1;
                    old_seen += 1;
                    new_seen += 1;
                }
                b'-' => {
                    let source_line = lines.get(old_at).ok_or_else(|| {
                        PatchError("patch removal extends beyond the source".into())
                    })?;
                    if source_line.text != patch_line {
                        return Err(PatchError(
                            "patch removal does not match the current source".into(),
                        ));
                    }
                    old_at += 1;
                    old_seen += 1;
                }
                b'+' => {
                    result.push(SourceLine {
                        text: patch_line,
                        ending: ending.clone(),
                    });
                    new_seen += 1;
                }
                _ => unreachable!("parser validates patch operations"),
            }
        }
        if old_seen != hunk.old_count || new_seen != hunk.new_count {
            return Err(PatchError(
                "patch hunk counts do not match its header".into(),
            ));
        }
    }
    result.extend_from_slice(&lines[old_at..]);
    // Unified diffs produced by models almost never include the optional
    // "No newline" marker. Preserve the source file's existing EOF convention
    // instead of rejecting an otherwise exact edit or silently adding a byte.
    if !source_has_final_newline && let Some(last) = result.last_mut() {
        last.ending.clear();
    }
    let mut bytes = Vec::new();
    for line in result {
        bytes.extend(line.text);
        bytes.extend(line.ending);
    }
    if bytes == source {
        return Err(PatchError("model patch makes no source change".into()));
    }
    Ok(bytes)
}

fn line_range(bytes: &[u8], line: u32) -> Option<std::ops::Range<usize>> {
    if line == 0 {
        return None;
    }
    let mut current = 1u32;
    let mut start = 0usize;
    while current < line {
        let next = bytes[start..].iter().position(|byte| *byte == b'\n')?;
        start += next + 1;
        current += 1;
    }
    let end = bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| start + offset);
    Some(start..end)
}

pub fn replace_once_on_line(
    source: &[u8],
    line: u32,
    from: &[u8],
    to: &[u8],
) -> Result<Vec<u8>, PatchError> {
    let range = line_range(source, line)
        .ok_or_else(|| PatchError(format!("diagnostic line {line} is outside the source")))?;
    let positions: Vec<usize> = source[range.clone()]
        .windows(from.len())
        .enumerate()
        .filter_map(|(offset, value)| (value == from).then_some(range.start + offset))
        .collect();
    let [at] = positions.as_slice() else {
        return Err(PatchError(format!(
            "the expected source token does not occur exactly once on line {line}"
        )));
    };
    let mut result = Vec::with_capacity(source.len() + to.len().saturating_sub(from.len()));
    result.extend_from_slice(&source[..*at]);
    result.extend_from_slice(to);
    result.extend_from_slice(&source[*at + from.len()..]);
    Ok(result)
}

pub fn append_to_line(source: &[u8], line: u32, text: &str) -> Result<Vec<u8>, PatchError> {
    if text.is_empty() || text.len() > 32 || text.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
    {
        return Err(PatchError(
            "line append is empty or exceeds its safety bound".into(),
        ));
    }
    let range = line_range(source, line)
        .ok_or_else(|| PatchError(format!("diagnostic line {line} is outside the source")))?;
    let insertion = (range.start..range.end)
        .rev()
        .find(|at| !matches!(source[*at], b' ' | b'\t' | b'\r'))
        .map_or(range.start, |at| at + 1);
    let mut result = Vec::with_capacity(source.len() + text.len());
    result.extend_from_slice(&source[..insertion]);
    result.extend_from_slice(text.as_bytes());
    result.extend_from_slice(&source[insertion..]);
    Ok(result)
}

fn comment_start(line: &[u8]) -> usize {
    for (at, byte) in line.iter().enumerate() {
        if *byte != b'%' {
            continue;
        }
        let slashes = line[..at]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        if slashes % 2 == 0 {
            return at;
        }
    }
    line.len()
}

fn find_uncommented(source: &[u8], needle: &[u8]) -> Option<usize> {
    let mut start = 0usize;
    while start <= source.len() {
        let end = source[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(source.len(), |offset| start + offset);
        let line = &source[start..end];
        let live = &line[..comment_start(line)];
        if let Some(offset) = live.windows(needle.len()).position(|value| value == needle) {
            return Some(start + offset);
        }
        if end == source.len() {
            break;
        }
        start = end + 1;
    }
    None
}

pub fn add_package(source: &[u8], package: &str) -> Result<Vec<u8>, PatchError> {
    if package.is_empty()
        || !package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PatchError(
            "the proposed package name is not safe to insert".into(),
        ));
    }
    let documentclass = find_uncommented(source, b"\\documentclass")
        .ok_or_else(|| PatchError("no unambiguous LaTeX document class was found".into()))?;
    let begin = find_uncommented(source, b"\\begin{document}")
        .ok_or_else(|| PatchError("no unambiguous document preamble boundary was found".into()))?;
    if documentclass >= begin {
        return Err(PatchError(
            "the document class does not precede the document body".into(),
        ));
    }
    let already = format!("\\usepackage{{{package}}}");
    if find_uncommented(source, already.as_bytes()).is_some() {
        return Err(PatchError(format!("{package} is already loaded")));
    }
    let insertion = source[..begin]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    if insertion <= documentclass {
        return Err(PatchError(
            "the document class and body share a line; no safe insertion point".into(),
        ));
    }
    let ending = if source.windows(2).any(|pair| pair == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let addition = format!("\\usepackage{{{package}}}{ending}");
    let mut result = Vec::with_capacity(source.len() + addition.len());
    result.extend_from_slice(&source[..insertion]);
    result.extend_from_slice(addition.as_bytes());
    result.extend_from_slice(&source[insertion..]);
    Ok(result)
}

pub fn apply_action(
    action: &Action,
    diagnostic: &Diagnostic,
    source: &[u8],
) -> Result<Option<Vec<u8>>, PatchError> {
    let edited = match action {
        Action::AddPackage { package } => add_package(source, package)?,
        Action::ReplaceControlSequence { from, to } => {
            let line = diagnostic
                .location()
                .ok_or_else(|| PatchError("spelling repair has no source location".into()))?
                .line;
            replace_once_on_line(
                source,
                line,
                format!("\\{from}").as_bytes(),
                format!("\\{to}").as_bytes(),
            )?
        }
        Action::AppendToLine { line, text } => append_to_line(source, *line, text)?,
        Action::FixEnvironmentDelimiter { from, to } => {
            let line = diagnostic
                .location()
                .ok_or_else(|| PatchError("environment repair has no source location".into()))?
                .line;
            replace_once_on_line(
                source,
                line,
                format!("\\end{{{from}}}").as_bytes(),
                format!("\\end{{{to}}}").as_bytes(),
            )?
        }
        _ => return Ok(None),
    };
    (edited != source)
        .then_some(Some(edited))
        .ok_or_else(|| PatchError("the proposed edit makes no source change".into()))
}

fn display_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8(bytes.to_vec())
        .unwrap_or_else(|_| bytes.iter().map(|byte| char::from(*byte)).collect())
        .lines()
        .map(str::to_string)
        .collect()
}

pub fn unified_diff(path: &Path, before: &[u8], after: &[u8]) -> String {
    let before = display_lines(before);
    let after = display_lines(after);
    let mut prefix = 0usize;
    while prefix < before.len() && prefix < after.len() && before[prefix] == after[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < before.len().saturating_sub(prefix)
        && suffix < after.len().saturating_sub(prefix)
        && before[before.len() - 1 - suffix] == after[after.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let first = prefix.saturating_sub(2);
    let old_last = (before.len() - suffix + 2).min(before.len());
    let new_last = (after.len() - suffix + 2).min(after.len());
    let name = path.to_string_lossy();
    let mut output = format!(
        "--- a/{name}\n+++ b/{name}\n@@ -{},{} +{},{} @@\n",
        first + 1,
        old_last - first,
        first + 1,
        new_last - first
    );
    for line in &before[first..prefix] {
        output.push(' ');
        output.push_str(line);
        output.push('\n');
    }

    // Preserve unchanged lines between distinct edits as context. The old
    // prefix/suffix-only implementation represented every such line as a
    // deletion plus addition, which made a narrow two-point repair look like
    // destructive bulk replacement to the safety gate.
    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    let mut old_at = prefix;
    let mut new_at = prefix;
    while old_at < old_end || new_at < new_end {
        if old_at < old_end && new_at < new_end && before[old_at] == after[new_at] {
            output.push(' ');
            output.push_str(&before[old_at]);
            output.push('\n');
            old_at += 1;
            new_at += 1;
            continue;
        }

        const ALIGNMENT_WINDOW: usize = 128;
        let mut alignment = None;
        'distance: for distance in 1..=ALIGNMENT_WINDOW {
            for old_skip in 0..=distance {
                let new_skip = distance - old_skip;
                if old_at + old_skip < old_end
                    && new_at + new_skip < new_end
                    && before[old_at + old_skip] == after[new_at + new_skip]
                {
                    alignment = Some((old_skip, new_skip));
                    break 'distance;
                }
            }
        }
        let (old_skip, new_skip) = alignment.unwrap_or((old_end - old_at, new_end - new_at));
        for line in &before[old_at..old_at + old_skip] {
            output.push('-');
            output.push_str(line);
            output.push('\n');
        }
        for line in &after[new_at..new_at + new_skip] {
            output.push('+');
            output.push_str(line);
            output.push('\n');
        }
        old_at += old_skip;
        new_at += new_skip;
    }
    for line in &after[after.len() - suffix..new_last] {
        output.push(' ');
        output.push_str(line);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_unified_diff_is_byte_preserving() {
        let patch = "--- a/main.tex\n+++ b/main.tex\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n";
        assert_eq!(patch_target(patch).unwrap(), Path::new("main.tex"));
        assert_eq!(
            apply_unified_patch(patch, b"one\ntwo\nthree\n").unwrap(),
            b"one\nTWO\nthree\n"
        );
        assert!(patch_target("--- /dev/null\n+++ b/main.tex\n@@ -0,0 +1,1 @@\n+x\n").is_err());
        assert!(
            patch_target("--- a/../secret.tex\n+++ b/../secret.tex\n@@ -1 +1 @@\n-a\n+b\n")
                .is_err()
        );
        let no_final_newline =
            "--- a/main.tex\n+++ b/main.tex\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n";
        assert_eq!(
            apply_unified_patch(no_final_newline, b"one\ntwo").unwrap(),
            b"one\nTWO"
        );
    }

    #[test]
    fn structured_edits_are_exact_and_do_not_require_a_final_newline() {
        let edits = [StructuredEdit {
            path: PathBuf::from("main.tex"),
            old: "\\widgitize{x}".into(),
            new: "\\widgetize{x}".into(),
        }];
        let prepared =
            prepare_structured_edits(&edits, b"\\documentclass{article}\n\\widgitize{x}").unwrap();
        assert_eq!(prepared.after, b"\\documentclass{article}\n\\widgetize{x}");
        assert_eq!(
            patch_target(&prepared.patch).unwrap(),
            Path::new("main.tex")
        );
    }

    #[test]
    fn structured_edits_reject_ambiguous_or_unsafe_targets() {
        let ambiguous = [StructuredEdit {
            path: PathBuf::from("main.tex"),
            old: "same".into(),
            new: "changed".into(),
        }];
        assert!(prepare_structured_edits(&ambiguous, b"same\nsame\n").is_err());
        let unsafe_path = [StructuredEdit {
            path: PathBuf::from("../main.tex"),
            old: "same".into(),
            new: "changed".into(),
        }];
        assert!(prepare_structured_edits(&unsafe_path, b"same\n").is_err());
    }

    #[test]
    fn structured_edits_preserve_legacy_single_byte_encodings() {
        let edits = [StructuredEdit {
            path: PathBuf::from("main.tex"),
            old: "café".into(),
            new: "CAFÉ".into(),
        }];
        let prepared = prepare_structured_edits(&edits, b"caf\xe9\n").unwrap();
        assert_eq!(prepared.after, b"CAF\xc9\n");
    }

    #[test]
    fn generated_diff_preserves_context_between_distinct_edits() {
        let before = b"zero\none\ntwo\nthree\nfour\nfive\n";
        let after = b"added\nzero\none\ntwo\nTHREE\nfour\nfive\n";
        let patch = unified_diff(Path::new("main.tex"), before, after);
        assert_eq!(apply_unified_patch(&patch, before).unwrap(), after);
        let safety = inspect_patch_safety(&patch, before, after, 20, 10).unwrap();
        assert_eq!(safety.added_lines, 2);
        assert_eq!(safety.deleted_lines, 1);
        assert!(patch.contains(" two\n-three\n+THREE\n four"));
    }

    #[test]
    fn patches_preserve_utf8_and_legacy_encodings() {
        let patch = "--- a/main.tex\n+++ b/main.tex\n@@ -1 +1 @@\n-café\n+cafés\n";
        assert_eq!(
            apply_unified_patch(patch, "café\n".as_bytes()).unwrap(),
            "cafés\n".as_bytes()
        );
        assert_eq!(
            apply_unified_patch(patch, &[b'c', b'a', b'f', 0xe9, b'\n']).unwrap(),
            &[b'c', b'a', b'f', 0xe9, b's', b'\n']
        );
    }

    #[test]
    fn model_patch_recomputes_only_redundant_hunk_counts() {
        let source = b"\\documentclass{article}\n\\begin{document}\nold\n\\end{document}\n";
        let patch = "--- a/main.tex\n+++ b/main.tex\n@@ -2,7 +2,7 @@\n \\begin{document}\n-old\n+new\n \\end{document}\n";
        assert!(apply_unified_patch(patch, source).is_err());
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(prepared.repaired_hunk_counts);
        assert!(!prepared.repaired_hunk_locations);
        assert!(!prepared.stripped_excerpt_labels);
        assert!(!prepared.decoded_backslashes);
        assert_eq!(
            prepared.after,
            b"\\documentclass{article}\n\\begin{document}\nnew\n\\end{document}\n"
        );
        assert!(prepared.patch.contains("@@ -2,3 +2,3 @@"));
    }

    #[test]
    fn model_patch_decodes_backslashes_only_with_exact_source_evidence() {
        let source = b"\\documentclass{article}\n\\foo{x}\n";
        let patch = r#"--- a/main.tex
+++ b/main.tex
@@ -1,2 +1,2 @@
 \\documentclass{article}
-\\foo{x}
+\\bar{x}
"#;
        assert!(apply_unified_patch(patch, source).is_err());
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(!prepared.repaired_hunk_counts);
        assert!(!prepared.repaired_hunk_locations);
        assert!(!prepared.stripped_excerpt_labels);
        assert!(prepared.decoded_backslashes);
        assert_eq!(prepared.after, b"\\documentclass{article}\n\\bar{x}\n");
    }

    #[test]
    fn model_patch_does_not_decode_an_intentional_tex_line_break() {
        let source = b"before\\\\\nold\n";
        let patch = r#"--- a/main.tex
+++ b/main.tex
@@ -1,2 +1,2 @@
 before\\
-old
+new
"#;
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(!prepared.decoded_backslashes);
        assert_eq!(prepared.after, b"before\\\\\nnew\n");
    }

    #[test]
    fn model_patch_reanchors_a_unique_exact_old_side() {
        let source = b"\\documentclass{article}\n\\usepackage{glossaries}\n\\makeglossaries\n\\newglossaryentry{alpha}{name={alpha},description={first}}\n\\newglossaryentry{beta}{name={},description={second}}\n\\begin{document}\n";
        let patch = r#"--- a/main.tex
+++ b/main.tex
@@ -4,4 +4,4 @@
 \usepackage{glossaries}
 \makeglossaries
 \newglossaryentry{alpha}{name={alpha},description={first}}
-\newglossaryentry{beta}{name={},description={second}}
+\newglossaryentry{beta}{name={beta},description={second}}
"#;
        assert!(apply_unified_patch(patch, source).is_err());
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(prepared.repaired_hunk_locations);
        assert!(!prepared.stripped_excerpt_labels);
        assert!(prepared.patch.contains("@@ -2,4 +2,4 @@"));
        assert!(
            String::from_utf8(prepared.after)
                .unwrap()
                .contains("name={beta}")
        );
    }

    #[test]
    fn model_patch_strips_source_excerpt_labels_with_exact_evidence() {
        let source = b"zero\none\ntwo\nBeta\\index{} needs a key.\nfour\n";
        let patch = r#"--- a/main.tex
+++ b/main.tex
@@ -4,1 +4,1 @@
-4|Beta\index{} needs a key.
+4|Beta\index{beta} needs a key.
"#;
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(!prepared.repaired_hunk_locations);
        assert!(prepared.stripped_excerpt_labels);
        assert_eq!(
            prepared.after,
            b"zero\none\ntwo\nBeta\\index{beta} needs a key.\nfour\n"
        );
    }

    #[test]
    fn model_patch_strips_source_proven_diff_marker_padding() {
        let source = b"zero\nBeta\\index{} needs a key.\n";
        let patch = r#"--- a/main.tex
+++ b/main.tex
@@ -2,1 +2,1 @@
- Beta\index{} needs a key.
+ Beta\index{beta} needs a key.
"#;
        assert!(apply_unified_patch(patch, source).is_err());
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(prepared.stripped_marker_padding);
        assert_eq!(
            prepared.normalization().as_deref(),
            Some("diff-marker-padding")
        );
        assert_eq!(prepared.after, b"zero\nBeta\\index{beta} needs a key.\n");
        assert!(prepared.patch.contains("-Beta\\index{}"));
        assert!(prepared.patch.contains("+Beta\\index{beta}"));
    }

    #[test]
    fn model_patch_keeps_real_source_indentation() {
        let source = b" before\n";
        let patch = "--- a/main.tex\n+++ b/main.tex\n@@ -1 +1 @@\n- before\n+ after\n";
        let prepared = prepare_model_patch(patch, source).unwrap();
        assert!(!prepared.stripped_marker_padding);
        assert_eq!(prepared.after, b" after\n");
    }

    #[test]
    fn model_patch_does_not_guess_ambiguous_or_false_excerpt_labels() {
        let repeated = b"same\nsame\n";
        let ambiguous = "--- a/main.tex\n+++ b/main.tex\n@@ -9 +9 @@\n-same\n+new\n";
        assert!(prepare_model_patch(ambiguous, repeated).is_err());

        let source = b"zero\none\ntwo\nBeta\\index{} needs a key.\n";
        let false_label = "--- a/main.tex\n+++ b/main.tex\n@@ -4 +4 @@\n-3|Beta\\index{} needs a key.\n+3|Beta\\index{beta} needs a key.\n";
        assert!(prepare_model_patch(false_label, source).is_err());

        let false_addition_label = "--- a/main.tex\n+++ b/main.tex\n@@ -4 +4 @@\n-4|Beta\\index{} needs a key.\n+3|Beta\\index{beta} needs a key.\n";
        assert!(prepare_model_patch(false_addition_label, source).is_err());
    }

    #[test]
    fn source_safety_rejects_error_hiding_and_large_deletions() {
        let before =
            b"\\begin{document}\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n\\end{document}\n";
        let hiding = "--- a/main.tex\n+++ b/main.tex\n@@ -2,1 +2,2 @@\n one\n+\\batchmode\n";
        let hidden = apply_unified_patch(hiding, before).unwrap();
        assert!(inspect_patch_safety(hiding, before, &hidden, 200, 100).is_err());

        let truncating = "--- a/main.tex\n+++ b/main.tex\n@@ -2,6 +2,0 @@\n-one\n-two\n-three\n-four\n-five\n-six\n";
        let truncated = apply_unified_patch(truncating, before).unwrap();
        assert!(inspect_patch_safety(truncating, before, &truncated, 200, 100).is_err());
    }

    #[test]
    fn source_safety_accepts_a_small_grounded_repair() {
        let before = b"\\begin{document}\n\\textbf{open\n\\end{document}\n";
        let patch =
            "--- a/main.tex\n+++ b/main.tex\n@@ -2,1 +2,1 @@\n-\\textbf{open\n+\\textbf{open}\n";
        let after = apply_unified_patch(patch, before).unwrap();
        let report = inspect_patch_safety(patch, before, &after, 20, 10).unwrap();
        assert_eq!(report.added_lines, 1);
        assert_eq!(report.deleted_lines, 1);
    }

    #[test]
    fn mechanical_edits_decline_ambiguity() {
        assert!(replace_once_on_line(b"one\n\\fo \\fo\n", 2, b"\\fo", b"\\foo").is_err());
        assert_eq!(
            append_to_line(b"field = {value}  \r\n", 1, ",").unwrap(),
            b"field = {value},  \r\n"
        );
    }
}
