struct RunMetrics {
    source_read: std::time::Duration,
    cache_validation: std::time::Duration,
    cache_load: std::time::Duration,
    snapshot_restore: std::time::Duration,
    format_restore: std::time::Duration,
    format_decode_count: u8,
    format_source: &'static str,
    setup: std::time::Duration,
    tex_preamble: std::time::Duration,
    tex_class: std::time::Duration,
    tex_body: std::time::Duration,
    snapshot_write: std::time::Duration,
    class_snapshot_write: std::time::Duration,
    snapshot_bytes: usize,
    cache_state: &'static str,
    class_cache_state: &'static str,
    output_finalize: std::time::Duration,
    shipout: std::time::Duration,
    pdf: PdfMetrics,
}

#[derive(Clone, Debug)]
struct RunOutcome {
    project_snapshot_expected: bool,
    project_snapshot_ready: bool,
    rerun_needed: bool,
    dependencies: Vec<CacheDependency>,
    written_files: Vec<String>,
    diagnostics: Vec<aitex_core::Diagnostic>,
}

fn control_sequence_name(diagnostic: &aitex_engine::fast::Diagnostic) -> String {
    [&diagnostic.context, &diagnostic.before, &diagnostic.after]
        .into_iter()
        .find_map(|text| {
            let slash = text.rfind('\\')?;
            let rest = &text[slash + 1..];
            let word: String = rest
                .chars()
                .take_while(|character| character.is_ascii_alphabetic())
                .collect();
            if word.is_empty() {
                rest.chars().next().map(|character| character.to_string())
            } else {
                Some(word)
            }
        })
        .unwrap_or_default()
}

fn structured_engine_diagnostic(
    diagnostic: &aitex_engine::fast::Diagnostic,
    root: &str,
) -> aitex_core::Diagnostic {
    use aitex_engine::fast::ErrorKind;
    let kind = match diagnostic.kind {
        ErrorKind::UndefinedControlSequence => aitex_core::Kind::UndefinedControlSequence {
            name: control_sequence_name(diagnostic),
        },
        ErrorKind::MissingBrace
        | ErrorKind::AlignMissingLeftBrace
        | ErrorKind::AlignMissingRightBrace => aitex_core::Kind::MissingBrace,
        ErrorKind::RunawayArgument | ErrorKind::Runaway | ErrorKind::FileEndedWithinRead => {
            aitex_core::Kind::RunawayArgument
        }
        ErrorKind::DisplayMathEnd | ErrorKind::MissingDollar | ErrorKind::MissingDoubleDollar => {
            aitex_core::Kind::MissingMathShift
        }
        ErrorKind::ExtraAlignTab | ErrorKind::MisplacedTab | ErrorKind::MisplacedCrSpan => {
            aitex_core::Kind::ExtraAlignmentTab
        }
        ErrorKind::BudgetExhausted => aitex_core::Kind::CapacityExceeded {
            resource: "command budget".into(),
        },
        _ => aitex_core::Kind::Other,
    };
    let severity = if diagnostic.kind == ErrorKind::BudgetExhausted {
        aitex_core::Severity::Fatal
    } else {
        aitex_core::Severity::Error
    };
    let mut result = aitex_core::Diagnostic::new(
        severity,
        kind,
        &diagnostic.message,
        aitex_core::Source::Engine,
    );
    if diagnostic.line > 0 {
        result = result.at(aitex_core::Span::new(
            if diagnostic.file.is_empty() {
                root
            } else {
                &diagnostic.file
            },
            diagnostic.line,
        ));
    }
    result.context = [
        diagnostic.context.as_str(),
        diagnostic.before.as_str(),
        diagnostic.after.as_str(),
    ]
    .into_iter()
    .filter(|line| !line.trim().is_empty())
    .map(str::to_string)
    .collect();
    result.provenance = diagnostic
        .provenance
        .iter()
        .map(|frame| {
            use aitex_engine::fast::DiagnosticProvenanceKind as EngineKind;
            let kind = match frame.kind {
                EngineKind::FailingToken => aitex_core::ProvenanceKind::FailingToken,
                EngineKind::MacroExpansion => aitex_core::ProvenanceKind::MacroExpansion,
                EngineKind::MacroArgument => aitex_core::ProvenanceKind::MacroArgument,
                EngineKind::MacroDefinition => aitex_core::ProvenanceKind::MacroDefinition,
                EngineKind::Include => aitex_core::ProvenanceKind::Include,
            };
            let mut provenance = aitex_core::ProvenanceFrame::new(kind, &frame.label);
            if frame.line > 0 {
                provenance = provenance.at(aitex_core::Span::new(
                    if frame.file.is_empty() {
                        root
                    } else {
                        &frame.file
                    },
                    frame.line,
                ));
            }
            if !frame.detail.trim().is_empty() {
                provenance = provenance.with_detail(&frame.detail);
            }
            provenance
        })
        .collect();
    result
}

struct ForkPreloadedEngine {
    key: String,
    payload: Rc<Vec<u8>>,
    engine: Fast,
    dependencies: Vec<CacheDependency>,
    clock: Option<WallClock>,
}

struct HotBaseFormat {
    identity: String,
    engine: Fast,
}

/// One callback-free live engine immediately after LaTeX's canonical
/// `\document` macro. Complete-build passes may clone it only while the full
/// source/configuration identity, every dependency observed before the
/// boundary, and the exact per-pass clock still match. The exact clock guard
/// is conservative: a body that did not read it on one pass may read it next.
struct HotDocumentBoundary {
    key: String,
    engine: Fast,
    dependencies: Vec<CacheDependency>,
    /// Exact job clock. Even if the preamble did not read it, the body still
    /// may; retaining the prior initialized registers across a minute would
    /// otherwise differ from a fresh TeX pass.
    clock: WallClock,
    /// The narrower semantic dependency observed before the boundary. This is
    /// carried into completed-output/cache accounting after a hot clone.
    clock_reads: u8,
    /// The resolver's only semantic callback-local state: the graphic whose
    /// extension disambiguates the next synthetic dvipdfmx `.xbb` request.
    recent_graphic_asset: Option<String>,
}

/// PGF eagerly seeds its default pseudo-random generator from `\time *
/// \year`. Deferring those two reads is sound only for package bodies whose
/// complete bytes we have reviewed. TeX deliberately allows a local file to
/// shadow an installed package, so neither the requested nor resolved path is
/// an authority. Unknown package revisions simply keep the conservative clock
/// dependency.
fn trusted_clock_memoizer(name: &str, source: &str) -> bool {
    if !name.ends_with("pgfmathfunctions.random.code.tex") {
        return false;
    }
    // `read_latin1` represents each source byte as the same-valued Unicode
    // scalar. Convert back before hashing so the identity is byte-exact even
    // if a future package comment contains a non-ASCII byte.
    let bytes: Vec<u8> = source.chars().map(|character| character as u32 as u8).collect();
    matches!(
        sha256_hex(&bytes).as_str(),
        // Official PGF 3.1.5 through 3.1.12 package bodies. This covers the
        // TeX Live/MacTeX generations aiTeX supports; unknown future bodies
        // retain the conservative dependency until reviewed.
        "57f9c58bd3bd8f9dbf0df178a228a4a4194f914939ce842f4f3ae96c25e87281"
            | "ba4db4589f51d34871948247d9e7638a21b428530eed2d7c013440b091c621e7"
            | "9308151b47cfe8144a60110976fa68acdede9c49fb5567a204271c7ac5d535b4"
            | "c10a26a167c06b16e2d80ee4b119f77818ba2357ece3e5bc3cf7470c5dfb09f9"
            | "968308d364b4f31f2a787adf32c3df7d87959a519c4eb5c8b47bedde4311ab95"
            | "5b9fa5872702d40199e58963f47a28d1120bf826bd782a1cffcbdd0368825b76"
    )
}

thread_local! {
    /// The fork-watch parent owns this pristine, callback-free engine. A child
    /// takes its private copy after `fork`; the parent's slot is unchanged by
    /// that mutation because the address space is copy-on-write.
    static FORK_PRELOADED_ENGINE: RefCell<Option<ForkPreloadedEngine>> =
        const { RefCell::new(None) };
    /// Complete builds and portable watch sessions retain one callback-free,
    /// decoded base format. Every pass receives an independent state clone;
    /// a content-identity change replaces the template before it can be used.
    static HOT_BASE_FORMAT: RefCell<Option<HotBaseFormat>> =
        const { RefCell::new(None) };
    /// Complete builds retain at most one engine-observed document boundary.
    /// This is intentionally process-local: it accelerates successive TeX
    /// passes without creating a new persistent-cache trust boundary.
    static HOT_DOCUMENT_BOUNDARY: RefCell<Option<HotDocumentBoundary>> =
        const { RefCell::new(None) };
}

static FORK_CHILD_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl Default for RunMetrics {
    fn default() -> Self {
        Self {
            source_read: std::time::Duration::ZERO,
            cache_validation: std::time::Duration::ZERO,
            cache_load: std::time::Duration::ZERO,
            snapshot_restore: std::time::Duration::ZERO,
            format_restore: std::time::Duration::ZERO,
            format_decode_count: 0,
            format_source: "none",
            setup: std::time::Duration::ZERO,
            tex_preamble: std::time::Duration::ZERO,
            tex_class: std::time::Duration::ZERO,
            tex_body: std::time::Duration::ZERO,
            snapshot_write: std::time::Duration::ZERO,
            class_snapshot_write: std::time::Duration::ZERO,
            snapshot_bytes: 0,
            cache_state: "disabled",
            class_cache_state: "disabled",
            output_finalize: std::time::Duration::ZERO,
            shipout: std::time::Duration::ZERO,
            pdf: PdfMetrics::default(),
        }
    }
}

/// A completed-output cache can have been populated by the one-pass front
/// door. Preserve the semantic control streams recorded in its exact manifest
/// so `aitex build` still validates their processors instead of mistaking the
/// restored PDF for proof that an old `.ind`/`.gls` is current.
fn cached_semantic_control_files(dependencies: &[CacheDependency]) -> Vec<String> {
    let mut files = dependencies
        .iter()
        .map(|dependency| dependency.request.clone())
        .filter(|path| {
            matches!(
                std::path::Path::new(path)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("idx" | "glo" | "acn" | "nlo")
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

fn metric_ms(value: std::time::Duration) -> f64 {
    value.as_secs_f64() * 1e3
}

fn emit_run_metrics(metrics: &RunMetrics, total: std::time::Duration) {
    let resolver = resolver_metrics();
    eprintln!(
        concat!(
            "AITEX_METRICS {{\"schema\":4,\"cache\":\"{}\",",
            "\"class_cache\":\"{}\",",
            "\"source_read_ms\":{:.3},\"cache_validation_ms\":{:.3},",
            "\"cache_load_ms\":{:.3},\"snapshot_restore_ms\":{:.3},",
            "\"format_restore_ms\":{:.3},\"format_decode_count\":{},",
            "\"format_source\":\"{}\",\"setup_ms\":{:.3},",
            "\"tex_class_ms\":{:.3},\"tex_preamble_ms\":{:.3},",
            "\"tex_body_ms\":{:.3},",
            "\"tex_total_ms\":{:.3},\"snapshot_write_ms\":{:.3},",
            "\"class_snapshot_write_ms\":{:.3},",
            "\"snapshot_bytes\":{},\"shipout_ms\":{:.3},",
            "\"output_finalize_ms\":{:.3},\"pdf_dvi_parse_ms\":{:.3},",
            "\"pdf_font_ms\":{:.3},\"pdf_image_ms\":{:.3},",
            "\"pdf_assembly_ms\":{:.3},\"pdf_write_ms\":{:.3},",
            "\"pdf_total_ms\":{:.3},",
            "\"resolver_local_queries\":{},\"resolver_local_hits\":{},",
            "\"resolver_helper_starts\":{},\"resolver_helper_start_failures\":{},",
            "\"resolver_helper_start_ms\":{:.3},",
            "\"resolver_helper_queries\":{},\"resolver_helper_hits\":{},",
            "\"resolver_helper_query_ms\":{:.3},",
            "\"resolver_fallback_processes\":{},\"resolver_fallback_ms\":{:.3},",
            "\"resolver_pk_processes\":{},\"resolver_generated_pk\":{},",
            "\"resolver_pk_ms\":{:.3},",
            "\"resolver_extractbb_processes\":{},\"resolver_extractbb_ms\":{:.3},",
            "\"resolver_pdfinfo_processes\":{},\"resolver_pdfinfo_ms\":{:.3},",
            "\"internal_total_ms\":{:.3}}}"
        ),
        metrics.cache_state,
        metrics.class_cache_state,
        metric_ms(metrics.source_read),
        metric_ms(metrics.cache_validation),
        metric_ms(metrics.cache_load),
        metric_ms(metrics.snapshot_restore),
        metric_ms(metrics.format_restore),
        metrics.format_decode_count,
        metrics.format_source,
        metric_ms(metrics.setup),
        metric_ms(metrics.tex_class),
        metric_ms(metrics.tex_preamble),
        metric_ms(metrics.tex_body),
        metric_ms(metrics.tex_preamble + metrics.tex_body),
        metric_ms(metrics.snapshot_write),
        metric_ms(metrics.class_snapshot_write),
        metrics.snapshot_bytes,
        metric_ms(metrics.shipout),
        metric_ms(metrics.output_finalize),
        metric_ms(metrics.pdf.dvi_parse),
        metric_ms(metrics.pdf.font_work),
        metric_ms(metrics.pdf.image_work),
        metric_ms(metrics.pdf.assembly),
        metric_ms(metrics.pdf.write),
        metric_ms(metrics.pdf.total),
        resolver.local_queries,
        resolver.local_hits,
        resolver.helper_starts,
        resolver.helper_start_failures,
        metric_ms(resolver.helper_start),
        resolver.helper_queries,
        resolver.helper_hits,
        metric_ms(resolver.helper_query),
        resolver.fallback_processes,
        metric_ms(resolver.fallback_process),
        resolver.pk_processes,
        resolver.generated_pk,
        metric_ms(resolver.pk_process),
        resolver.extractbb_processes,
        metric_ms(resolver.extractbb_process),
        resolver.pdfinfo_processes,
        metric_ms(resolver.pdfinfo_process),
        metric_ms(total),
    );
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct UnicodeOptions {
    family: Option<String>,
    language: Option<String>,
    features: Option<String>,
    variations: Option<String>,
    direction: Option<String>,
    font_directories: Vec<std::path::PathBuf>,
}

impl UnicodeOptions {
    fn identity(&self) -> String {
        let mut identity = String::new();
        for (name, value) in [
            ("font", self.family.as_deref()),
            ("language", self.language.as_deref()),
            ("features", self.features.as_deref()),
            ("variations", self.variations.as_deref()),
            ("direction", self.direction.as_deref()),
        ] {
            identity.push_str(name);
            identity.push('\0');
            identity.push_str(value.unwrap_or(""));
            identity.push('\0');
        }
        for directory in &self.font_directories {
            identity.push_str("font-path\0");
            identity.push_str(&directory.to_string_lossy());
            identity.push('\0');
        }
        identity
    }
}

fn unicode_config_path(input: &str) -> Option<std::path::PathBuf> {
    let input = std::path::Path::new(input);
    let mut directory = if input.is_absolute() {
        input.parent()?.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(input).parent()?.to_path_buf()
    };
    loop {
        let candidate = directory.join(".aitex.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if directory.join(".git").exists() || !directory.pop() {
            return None;
        }
    }
}

fn unicode_string(
    table: &toml::Table,
    key: &str,
    config: &std::path::Path,
) -> Result<Option<String>, String> {
    table
        .get(key)
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "{}: [unicode].{key} must be a string",
                    config.display()
                )
            })
        })
        .transpose()
}

fn unicode_font_paths(
    table: &toml::Table,
    config: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, String> {
    let value = table.get("font-paths").or_else(|| table.get("font-path"));
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = if let Some(value) = value.as_str() {
        vec![value]
    } else if let Some(values) = value.as_array() {
        values
            .iter()
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    format!(
                        "{}: every [unicode].font-paths entry must be a string",
                        config.display()
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        return Err(format!(
            "{}: [unicode].font-paths must be a string or array of strings",
            config.display()
        ));
    };
    let base = config.parent().unwrap_or_else(|| std::path::Path::new("."));
    Ok(values
        .into_iter()
        .map(std::path::PathBuf::from)
        .map(|path| if path.is_absolute() { path } else { base.join(path) })
        .collect())
}

fn cli_values(arguments: &[String], flag: &str) -> Vec<String> {
    arguments
        .windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect()
}

fn load_unicode_options(input: &str, arguments: &[String]) -> Result<UnicodeOptions, String> {
    let mut options = UnicodeOptions::default();
    if let Some(config) = unicode_config_path(input) {
        let text = std::fs::read_to_string(&config)
            .map_err(|error| format!("cannot read {}: {error}", config.display()))?;
        let document = text
            .parse::<toml::Table>()
            .map_err(|error| format!("cannot parse {}: {error}", config.display()))?;
        if let Some(value) = document.get("unicode") {
            let table = value.as_table().ok_or_else(|| {
                format!("{}: [unicode] must be a table", config.display())
            })?;
            const KEYS: &[&str] = &[
                "font",
                "font-path",
                "font-paths",
                "language",
                "features",
                "variations",
                "direction",
            ];
            if let Some(key) = table.keys().find(|key| !KEYS.contains(&key.as_str())) {
                return Err(format!(
                    "{}: unsupported [unicode] key {key:?}",
                    config.display()
                ));
            }
            if table.contains_key("font-path") && table.contains_key("font-paths") {
                return Err(format!(
                    "{}: use only one of [unicode].font-path or font-paths",
                    config.display()
                ));
            }
            options.family = unicode_string(table, "font", &config)?;
            options.language = unicode_string(table, "language", &config)?;
            options.features = unicode_string(table, "features", &config)?;
            options.variations = unicode_string(table, "variations", &config)?;
            options.direction = unicode_string(table, "direction", &config)?;
            options.font_directories = unicode_font_paths(table, &config)?;
        }
    }

    for (flag, target) in [
        ("--font", &mut options.family),
        ("--language", &mut options.language),
        ("--features", &mut options.features),
        ("--variations", &mut options.variations),
        ("--direction", &mut options.direction),
    ] {
        if let Some(value) = cli_values(arguments, flag).into_iter().next() {
            *target = Some(value);
        }
    }
    for value in cli_values(arguments, "--font-path") {
        options
            .font_directories
            .extend(std::env::split_paths(std::ffi::OsStr::new(&value)));
    }
    if let Some(paths) = std::env::var_os("AITEX_FONT_PATH") {
        options.font_directories.extend(std::env::split_paths(&paths));
    }
    for directory in &options.font_directories {
        if !directory.is_dir() {
            return Err(format!(
                "font path is not a directory: {}",
                directory.display()
            ));
        }
    }
    aitex_unicode::validate_text_settings(
        options.features.as_deref(),
        options.variations.as_deref(),
        options.direction.as_deref(),
    )?;
    Ok(options)
}

fn unicode_compat_file(name: &str) -> Option<&'static str> {
    const INPUTENC: &str = r#"\NeedsTeXFormat{LaTeX2e}
\ProvidesPackage{inputenc}[2026/09/07 aiTeX native UTF-8 compatibility]
\makeatletter
\DeclareOption{utf8}{}
\DeclareOption*{\PackageError{inputenc}{Legacy input encoding `\CurrentOption' is incompatible with aiTeX Unicode mode}{Remove inputenc or select utf8; --unicode decodes the complete source tree as UTF-8.}}
\ProcessOptions\relax
\def\inputencoding#1{\edef\aitex@encoding{#1}\def\aitex@utf{utf8}\ifx\aitex@encoding\aitex@utf\else\PackageError{inputenc}{Legacy input encoding `#1' is incompatible with aiTeX Unicode mode}{Remove inputenc or select utf8; --unicode decodes the complete source tree as UTF-8.}\fi}
\PackageInfo{inputenc}{Native UTF-8 input is provided by aiTeX}
\makeatother
\endinput
"#;
    const FONTSPEC: &str = r#"\NeedsTeXFormat{LaTeX2e}
\ProvidesPackage{fontspec}[2026/09/04 aiTeX native OpenType compatibility]
\makeatletter
\def\aitex@defaultfeatures{}
\def\aitex@mainfont{}
\def\aitex@mainfeatures{}
\def\aitex@sansfont{}
\def\aitex@sansfeatures{}
\def\aitex@monofont{}
\def\aitex@monofeatures{}
\def\aitex@activate#1#2{\aitexnativefont{#1}\aitexfeatures{\aitex@defaultfeatures,#2}}
\def\aitex@optional#1{\@ifnextchar[{#1}{#1[]}}
\def\setmainfont{\aitex@optional\aitex@setmainfont}
\def\aitex@setmainfont[#1]#2{\gdef\aitex@mainfont{#2}\gdef\aitex@mainfeatures{#1}\aitex@activate{#2}{#1}}
\def\setsansfont{\aitex@optional\aitex@setsansfont}
\def\aitex@setsansfont[#1]#2{\gdef\aitex@sansfont{#2}\gdef\aitex@sansfeatures{#1}}
\def\setmonofont{\aitex@optional\aitex@setmonofont}
\def\aitex@setmonofont[#1]#2{\gdef\aitex@monofont{#2}\gdef\aitex@monofeatures{#1}}
\let\aitex@rmfamily\rmfamily
\let\aitex@sffamily\sffamily
\let\aitex@ttfamily\ttfamily
\def\rmfamily{\aitex@rmfamily\aitex@activate{\aitex@mainfont}{\aitex@mainfeatures}}
\def\sffamily{\aitex@sffamily\aitex@activate{\aitex@sansfont}{\aitex@sansfeatures}}
\def\ttfamily{\aitex@ttfamily\aitex@activate{\aitex@monofont}{\aitex@monofeatures}}
\def\fontspec{\aitex@optional\aitex@fontspec}
\def\aitex@fontspec[#1]#2{\aitex@activate{#2}{#1}}
\def\newfontfamily#1{\@ifnextchar[{\aitex@newfontfamily{#1}}{\aitex@newfontfamily{#1}[]}}
\def\aitex@newfontfamily#1[#2]#3{\newcommand#1{\aitex@activate{#3}{#2}}}
\let\newfontface\newfontfamily
\def\defaultfontfeatures#1{\gdef\aitex@defaultfeatures{#1}}
\def\addfontfeatures#1{\aitexfeatures{#1}}
\PackageInfo{fontspec}{Using aiTeX's bounded native OpenType compatibility layer}
\makeatother
\endinput
"#;
    const POLYGLOSSIA: &str = r#"\NeedsTeXFormat{LaTeX2e}
\ProvidesPackage{polyglossia}[2026/09/04 aiTeX language compatibility]
\makeatletter
\def\aitex@langoptional#1{\@ifnextchar[{#1}{#1[]}}
\def\aitex@langoptions#1{\def\aitex@tmp{#1}\ifx\aitex@tmp\@empty\else\PackageError{polyglossia}{aiTeX does not support language option `#1'}{Use an explicit BCP 47 tag with \string\aitexlanguage.}\fi}
\def\setdefaultlanguage{\aitex@langoptional\aitex@setdefaultlanguage}
\def\aitex@setdefaultlanguage[#1]#2{\aitex@langoptions{#1}\aitexlanguage{#2}}
\def\setotherlanguage{\aitex@langoptional\aitex@setotherlanguage}
\def\aitex@setotherlanguage[#1]#2{\aitex@langoptions{#1}\expandafter\def\csname text#2\endcsname##1{{\aitexlanguage{#2}##1}}}
\def\textlang#1#2{{\aitexlanguage{#1}#2}}
\def\foreignlanguage#1#2{{\aitexlanguage{#1}#2}}
\PackageInfo{polyglossia}{Using aiTeX's bounded BCP 47 compatibility layer}
\makeatother
\endinput
"#;
    match name.trim_start_matches("./") {
        "inputenc.sty" => Some(INPUTENC),
        "fontspec.sty" => Some(FONTSPEC),
        "polyglossia.sty" => Some(POLYGLOSSIA),
        _ => None,
    }
}

fn source_for_run(
    path: &str,
    pdf_driver_mode: bool,
    unicode_options: Option<&UnicodeOptions>,
) -> std::io::Result<String> {
    let mut source = if unicode_flag() {
        // The main document is the opt-in contract boundary: unlike installed
        // legacy packages, malformed UTF-8 here is an actionable input error.
        std::fs::read_to_string(path)?
    } else {
        read_latin1(path)?
    };
    complete_file_source(&mut source);
    if let Some(options) = unicode_options {
        // Options live in host state, but their normalized value must also be
        // present in exact snapshot/output identity. A false conditional is a
        // state-free TeX no-op and, without a newline, preserves source lines.
        let identity = sha256_hex(options.identity().as_bytes());
        source.insert_str(0, &format!("\\ifnum0=1 {identity}\\fi"));
        // Keep babel's mature TeX hyphenation/locale behavior and additionally
        // pass its language names to HarfRust. The wrapper is installed only
        // after all packages have loaded, and remains inert without babel.
        const LANGUAGE_PRELUDE: &str = concat!(
            "\\makeatletter",
            "\\AtBeginDocument{",
            "\\@ifundefined{selectlanguage}{}{",
            "\\let\\aitex@selectlanguage\\selectlanguage",
            "\\def\\selectlanguage#1{\\aitex@selectlanguage{#1}\\aitexlanguage{#1}}",
            "}",
            "\\@ifundefined{foreignlanguage}{}{",
            "\\let\\aitex@foreignlanguage\\foreignlanguage",
            "\\def\\foreignlanguage#1#2{\\begingroup\\aitexlanguage{#1}\\aitex@foreignlanguage{#1}{#2}\\endgroup}",
            "}",
            "}",
            "\\makeatother",
        );
        source.insert_str(0, LANGUAGE_PRELUDE);
    }
    if pdf_driver_mode {
        let needs_epstopdf = project_may_need_epstopdf(std::path::Path::new(path), &source);
        // aiTeX always typesets through DVI, even when the requested final
        // artifact is PDF. Tell LaTeX's package ecosystem which DVI special
        // dialect the host will materialize. Keeping this on the first source
        // line preserves every user line number and naturally makes it part of
        // the preamble-snapshot key.
        const PDF_DRIVER_PRELUDE: &str = concat!(
            // Freeze the modern kernel's output/backend constants while the
            // engine still reports its real internal DVI mode. Afterwards,
            // advertise the requested final PDF to older packages whose
            // pdfTeX primitive interfaces aiTeX implements. This deliberately
            // supports both generations without asking either to contradict
            // its own engine/backend validation.
            "\\ExplSyntaxOn\\sys_load_backend:n{dvipdfmx}\\ExplSyntaxOff\\pdfoutput=1",
            "\\makeatletter",
            // latexrelease can roll the kernel back past the modern hook
            // implementation after this prelude has registered its package
            // and document hooks.  Keep the original RequirePackage call on
            // the expansion stack; once latexrelease returns, its historical
            // AtBeginDocument implementation is active and can retain a fresh
            // registration of the transport boundary defined below.
            "\\let\\aitex@saved@RequirePackage\\RequirePackage",
            "\\def\\aitex@latexrelease{latexrelease}",
            "\\def\\aitex@afterrequire#1{",
            "\\def\\aitex@requiredpackage{#1}",
            "\\ifx\\aitex@requiredpackage\\aitex@latexrelease",
            "\\AtBeginDocument{\\aitex@pdf@begin}",
            "\\fi",
            "}",
            "\\def\\aitex@RequirePackage@opt[#1]#2{\\@ifnextchar[{\\aitex@RequirePackage@optdate{#1}{#2}}{\\aitex@RequirePackage@optnodate{#1}{#2}}}",
            "\\def\\aitex@RequirePackage@optdate#1#2[#3]{\\aitex@saved@RequirePackage[#1]{#2}[#3]\\aitex@afterrequire{#2}}",
            "\\def\\aitex@RequirePackage@optnodate#1#2{\\aitex@saved@RequirePackage[#1]{#2}\\aitex@afterrequire{#2}}",
            "\\def\\aitex@RequirePackage@plain#1{\\@ifnextchar[{\\aitex@RequirePackage@plaindate{#1}}{\\aitex@RequirePackage@plainnodate{#1}}}",
            "\\def\\aitex@RequirePackage@plaindate#1[#2]{\\aitex@saved@RequirePackage{#1}[#2]\\aitex@afterrequire{#1}}",
            "\\def\\aitex@RequirePackage@plainnodate#1{\\aitex@saved@RequirePackage{#1}\\aitex@afterrequire{#1}}",
            "\\def\\RequirePackage{\\@ifnextchar[\\aitex@RequirePackage@opt\\aitex@RequirePackage@plain}",
            // The synthetic bitmap sidecar writes its physical dimensions in
            // both BoundingBox forms. Do not force `hiresbb` globally: many
            // ordinary EPS files legitimately provide only `%%BoundingBox`.
            // Do not inject package-local or global graphics/color options:
            // legacy color packages and catoptions correctly reject those as
            // an option clash. The after-hooks below install the transport
            // without altering author option lists.
            // `trimclip` has no dvipdfmx backend. Left to infer one from
            // `\\Gin@driver`, it falls back to PGF; that is both needlessly
            // expensive and observably incompatible with documents which
            // loaded the legacy `program` package first (that package makes
            // `|` active, while PGF declares its `||` operator during load).
            // Its pdfTeX backend uses only `\\pdfsave`, `\\pdfliteral`, and
            // `\\pdfrestore`, whose semantics aiTeX transports explicitly.
            // Selecting it therefore matches pdfLaTeX without changing the
            // dvipdfmx graphics/image boundary used by the host PDF writer.
            "\\PassOptionsToPackage{pdftex}{trimclip}",
            // pict2e performs its own driver selection and, when left to
            // `\pdfoutput`, can reload graphics-def's direct-pdf driver after
            // graphicx already selected dvipdfmx. That late reload changes
            // `\Gin@driver` globally and makes the next `\includegraphics`
            // call reach `\pdfximage` instead of our qualified special route.
            "\\PassOptionsToPackage{dvipdfmx}{pict2e}",
            // `pdfpages` otherwise keys only on the presence of
            // `\pdftexversion` and selects its direct-pdf backend. Its
            // dvipdfmx backend emits the same `pdf:epdf` graphics specials we
            // already materialize, including one selected source page at a
            // time.
            "\\PassOptionsToPackage{dvipdfmx}{pdfpages}",
            // PGF is pinned to the DVI-special transport. Hyperref may use its
            // supported legacy pdfTeX interface; LaTeX3 is already locked to
            // dvipdfmx above.
            "\\def\\pgfsysdriver{pgfsys-dvipdfmx.def}",
            // duckuments asks pdfTeX for the page count of its two bundled
            // 128-page example PDFs while the package is loading. It never
            // ships the resulting image object. Scope that tiny compatibility
            // surface to the package so unrelated code cannot mistake aiTeX
            // for a complete direct-pdf image backend.
            "\\AddToHook{package/duckuments/before}{",
            "\\let\\aitex@duck@saved@pdfximage\\pdfximage",
            "\\let\\aitex@duck@saved@pdflastximagepages\\pdflastximagepages",
            "\\def\\pdfximage#1{}",
            // duckuments reads this primitive with `\the`, so expose a real
            // TeX numeric command rather than a macro whose replacement text
            // merely happens to contain digits.
            "\\chardef\\pdflastximagepages=128 ",
            "}",
            "\\AddToHook{package/duckuments/after}{",
            "\\let\\pdfximage\\aitex@duck@saved@pdfximage",
            "\\let\\pdflastximagepages\\aitex@duck@saved@pdflastximagepages",
            "}",
            // dvipdfmx normally reads `stem.xbb` for every raster/PDF
            // extension. That aliases `plot.pdf` and `plot.png` inside
            // graphics.sty's bbox cache. Our synthetic sidecars can retain the
            // selected extension, making the private transport agree with
            // pdfLaTeX when both assets legitimately coexist.
            "\\makeatletter",
            // Old documents often pin [pdftex] explicitly. Hyperref can also
            // load color's pdftex.def after graphicx selected dvipdfmx. Both
            // overwrite the shared image macros with a direct-pdfximage route.
            // Reinstall the qualified transport after every such package and
            // once at the document boundary.
            "\\def\\aitex@dvipsdriver{dvips.def}",
            "\\def\\aitex@dvipdfmxdriver{dvipdfmx.def}",
            "\\def\\aitex@pdftexdriver{pdftex.def}",
            "\\newif\\ifaitex@explicitdvips",
            "\\newif\\ifaitex@explicitdvipdfmx",
            "\\newif\\ifaitex@pdftexoverride",
            "\\newif\\ifaitex@epstopdfdraft",
            "\\newif\\ifaitex@epstopdfneeded",
            // A latexrelease rollback can leave both the original modern hook
            // and the replacement AtBeginDocument registration live.  The
            // transport below wraps graphics-def macros by saving their
            // current meanings; running it twice therefore saves its own
            // wrapper and makes the first EPS inclusion recurse forever.
            // Treat the document boundary as the one-shot event it is.
            "\\newif\\ifaitex@pdfbegun",
            "\\def\\aitex@observegraphicsdriver{",
            "\\@ifundefined{Gin@driver}{}{",
            "\\ifx\\Gin@driver\\aitex@dvipdfmxdriver",
            "\\global\\aitex@explicitdvipdfmxtrue",
            "\\fi",
            "}",
            "}",
            "\\def\\aitex@ensuregraphicsdriver{",
            "\\@ifundefined{Gin@driver}{}{",
            // An explicit dvips choice under pdfTeX is observable: pdfTeX
            // accepts the resulting PSfile specials but deliberately ignores
            // them. Preserve that author-selected incompatibility instead of
            // silently turning its graphics into successful dvipdfmx imports.
            "\\ifx\\Gin@driver\\aitex@dvipsdriver",
            "\\global\\aitex@explicitdvipstrue",
            // This is an ordered state transition: a later explicit dvips
            // driver cancels an earlier pdftex.def observation, while a
            // subsequent pdftex.def below establishes the override again.
            "\\global\\aitex@pdftexoverridefalse",
            "\\else",
            // A document may explicitly select dvipdfmx for graphicx and then
            // load hyperref, whose pdfTeX backend replaces the shared graphics
            // macros with pdftex.def. Stock pdfLaTeX includes later images in
            // that state. We still reinstall our qualified dvipdfmx transport,
            // but remember that the author's initially incompatible driver no
            // longer controls image inclusion at the document boundary.
            "\\ifx\\Gin@driver\\aitex@pdftexdriver",
            "\\global\\aitex@pdftexoverridetrue",
            "\\fi",
            "\\ifx\\Gin@driver\\aitex@dvipdfmxdriver\\else",
            "\\let\\Gin@driver\\aitex@dvipdfmxdriver",
            "\\makeatletter\\input{dvipdfmx.def}\\makeatother",
            "\\fi",
            "\\fi",
            "}",
            "}",
            "\\AddToHook{package/graphics/after}{\\aitex@observegraphicsdriver\\aitex@ensuregraphicsdriver}",
            "\\AddToHook{package/color/after}{\\aitex@ensuregraphicsdriver}",
            "\\AddToHook{package/xcolor/after}{\\aitex@ensuregraphicsdriver}",
            // epstopdf's useful semantics are not limited to running a
            // converter: its `Gin@setfile` wrapper also selects an already
            // archived `name-eps-converted-to.pdf`. arXiv sources routinely
            // ship that PDF without the original EPS. Let the package install
            // the same naming wrapper it uses under pdfLaTeX, then restore our
            // dvipdfmx transport before any image is typeset. With shell escape
            // disabled, a genuinely absent conversion remains absent exactly
            // as it does in the qualified stock run.
            "\\def\\aitex@prepare@epstopdf{",
            "\\@ifundefined{Gin@driver}{}{",
            "\\let\\aitex@epstopdf@saved@driver\\Gin@driver",
            "\\def\\Gin@driver{pdftex.def}",
            "}",
            "}",
            "\\AddToHook{package/epstopdf-base/before}{",
            // Preserve an explicitly selected incompatible driver unless a
            // later package genuinely changed stock pdfTeX back to its own
            // backend.  epstopdf-base must then issue its normal unsupported-
            // driver warning and install no conversion rule.  Pretending that
            // dvips was pdftex here left a live repstopdf pipe which aiTeX
            // correctly refused when the image was eventually included.
            "\\ifaitex@explicitdvips",
            "\\ifaitex@pdftexoverride\\aitex@prepare@epstopdf\\fi",
            "\\else",
            "\\ifaitex@explicitdvipdfmx",
            "\\ifaitex@pdftexoverride\\aitex@prepare@epstopdf\\fi",
            "\\else\\aitex@prepare@epstopdf\\fi",
            "\\fi",
            "}",
            "\\AddToHook{package/epstopdf-base/after}{",
            "\\@ifundefined{aitex@epstopdf@saved@driver}{}{",
            "\\let\\Gin@driver\\aitex@epstopdf@saved@driver",
            "}",
            // epstopdf deliberately keeps the converted asset's ordinary
            // `.pdf` suffix in its private `GraphicsRead` argument. That is
            // exactly what pdftex.def wants, but dvipdfmx.def interprets the
            // argument as a text bounding-box sidecar and tries to parse the
            // PDF itself as `%%BoundingBox` records. Adapt only epstopdf's
            // saved setfile seam: the output name remains the real archived
            // `*-eps-converted-to.pdf`, while its read name reaches our
            // synthetic XBB resolver (including page-box selection).
            "\\@ifundefined{ETE@OrgGin@setfile}{}{",
            "\\let\\aitex@epstopdf@original@setfile\\ETE@OrgGin@setfile",
            "\\expandafter\\let\\expandafter\\aitex@epstopdf@epsrule\\csname Gin@rule@.eps\\endcsname",
            "\\def\\ETE@OrgGin@setfile#1#2#3{",
            "\\def\\aitex@epstopdf@type{#1}",
            "\\def\\aitex@epstopdf@pdf{pdf}",
            "\\ifx\\aitex@epstopdf@type\\aitex@epstopdf@pdf",
            "\\aitex@epstopdf@original@setfile{#1}{.pdf.aitex.\\Gin@page.\\Gin@pagebox.xbb}{#3}",
            "\\else",
            "\\aitex@epstopdf@original@setfile{#1}{#2}{#3}",
            "\\fi",
            "}",
            "}",
            "\\aitex@ensuregraphicsdriver",
            "}",
            // pdftex.def normally loads epstopdf-base at begin-document when
            // restricted shell escape is available. aiTeX deliberately
            // reports shell escape as disabled, but its host still provides a
            // bounded EPS normalizer and the hooks above prevent the package
            // from launching a command. Load the package for its other
            // observable behavior: in particular, selecting an archived,
            // up-to-date `name-eps-converted-to.pdf`, whose crop box and page
            // rotation may differ from a fresh conversion of the EPS.
            "\\AddToHook{begindocument/before}{",
            "\\ifaitex@epstopdfneeded",
            // A bare explicit dvips route leaves PostScript graphics ignored
            // by pdfTeX.  A later pdftex graphics/color driver changes that:
            // stock pdfLaTeX then uses epstopdf's restricted conversion path.
            // Mirror the effective driver at the document boundary, not only
            // the first option that graphics.sty observed.
            "\\def\\aitex@load@epstopdf{",
            "\\@ifpackageloaded{graphics}{",
            "\\@ifundefined{DoNotLoadEpstopdf}{",
            "\\@ifpackageloaded{pst-pdf}{}{",
            "\\@ifpackageloaded{pdftricks}{}{",
            "\\@ifpackageloaded{epstopdf-base}{}{\\RequirePackage{epstopdf-base}[2009/07/12]}",
            "}",
            "}",
            "}{}",
            "}{}",
            "}",
            "\\ifaitex@explicitdvips",
            "\\ifaitex@pdftexoverride\\aitex@load@epstopdf\\fi",
            "\\else",
            // An explicit dvipdfmx choice also suppresses pdftex.def's
            // begin-document hook.  It becomes active again only when a
            // later package (most commonly color or hyperref) selects the
            // pdfTeX driver, which `aitex@ensuregraphicsdriver` records as an
            // override before reinstalling our private transport.
            "\\ifaitex@explicitdvipdfmx",
            "\\ifaitex@pdftexoverride\\aitex@load@epstopdf\\fi",
            "\\else\\aitex@load@epstopdf\\fi",
            "\\fi",
            "\\fi",
            "}",
            // pinlabel predates graphics' portable PDF interfaces and calls
            // pdfTeX's image primitive solely to obtain four crop-box values.
            // Scope a compatibility spelling to its header scan and source
            // those values from the same qualified synthetic XBB used by
            // graphicx. This does not advertise a partial global pdfximage.
            "\\AddToHook{package/pinlabel/after}{",
            "\\let\\aitex@pinlabel@scan@header\\scan@header",
            "\\def\\scan@header{",
            "\\let\\aitex@saved@pdfximage\\pdfximage",
            "\\let\\aitex@saved@pdflastximage\\pdflastximage",
            "\\let\\aitex@saved@pdfximagebbox\\pdfximagebbox",
            "\\def\\pdfximage cropbox ##1{\\Gread@pdf{##1.xbb}}",
            "\\def\\pdflastximage{0}",
            "\\def\\pdfximagebbox##1##2{\\ifcase##2\\or\\Gin@llx bp\\or\\Gin@lly bp\\or\\Gin@urx bp\\or\\Gin@ury bp\\fi}",
            "\\aitex@pinlabel@scan@header",
            "\\let\\pdfximage\\aitex@saved@pdfximage",
            "\\let\\pdflastximage\\aitex@saved@pdflastximage",
            "\\let\\pdfximagebbox\\aitex@saved@pdfximagebbox",
            "}",
            "}",
            "\\def\\aitex@pdf@begin{",
            "\\ifaitex@pdfbegun\\else",
            "\\global\\aitex@pdfbeguntrue",
            // The private PDF host normally receives `\\pdfpagewidth`, but
            // aiTeX deliberately runs LaTeX's portable dvipdfmx backend to
            // collect graphics specials. With an author-selected dvipdfmx
            // graphics driver, that backend records A4/landscape only in a
            // DVI papersize special while stock pdfLaTeX synchronizes its PDF
            // media box directly from `\\paperwidth`. Carry the logical
            // LaTeX size separately so raw author specials remain ignorable.
            "\\@ifundefined{paperwidth}{}{\\AtBeginDvi{\\special{aitex:latex-page-size \\the\\paperwidth,\\the\\paperheight}}}",
            "\\aitex@ensuregraphicsdriver",
            "\\ifaitex@explicitdvips",
            "\\ifaitex@pdftexoverride",
            "\\let\\aitex@pdf@begin@selected\\aitex@pdf@begin@transport",
            "\\else",
            "\\let\\aitex@pdf@begin@selected\\aitex@pdf@begin@dvips",
            "\\fi",
            "\\else",
            "\\let\\aitex@pdf@begin@selected\\aitex@pdf@begin@transport",
            "\\fi",
            "\\aitex@pdf@begin@selected",
            "\\fi",
            "}",
            "\\def\\aitex@pdf@begin@dvips{",
            // A special emitted directly by AtBeginDocument can precede the
            // first shipped page and disappear from the DVI event stream.
            // AtBeginDvi retains the marker on page one, where the PDF host
            // can apply stock pdfTeX's document-wide PS-ignore semantics.
            "\\AtBeginDvi{\\special{aitex:pdftex-ignore-postscript}}",
            "}",
            "\\def\\aitex@pdf@begin@transport{",
            // epstopdf-base can be loaded before graphics. Stock pdfLaTeX
            // warns and installs no EPS conversion rule in that order, so a
            // later explicit .eps inclusion falls back to graphicx's framed
            // draft placeholder. Loading our private dvipdfmx transport must
            // not silently turn that placeholder into a rendered figure.
            "\\@ifpackageloaded{epstopdf-base}{",
            "\\@ifundefined{aitex@epstopdf@epsrule}{",
            "\\aitex@epstopdfdrafttrue",
            "\\let\\Ginclude@eps\\relax",
            "}{}",
            "}{}",
            "\\ifaitex@explicitdvipdfmx",
            "\\AtBeginDvi{\\special{aitex:pdftex-explicit-dvipdfmx}}",
            "\\ifaitex@pdftexoverride",
            "\\else",
            // Stock pdfTeX ignores EPS selected through an explicitly pinned
            // dvipdfmx graphics driver.  A later pdftex.def override enables
            // the ordinary epstopdf path instead, so suppress only the
            // still-unoverridden route.  The marker is consumed beside the
            // exact-size companion in the PDF host.
            "\\@ifundefined{Ginclude@eps}{}{",
            "\\let\\aitex@explicit@Ginclude@eps\\Ginclude@eps",
            "\\def\\Ginclude@eps##1{",
            "\\special{aitex:pdftex-ignore-next-driver-graphic}",
            "\\aitex@explicit@Ginclude@eps{##1}",
            "}",
            "}",
            "\\@ifundefined{Ginclude@dvipdfmx}{}{",
            "\\let\\aitex@explicit@Ginclude@dvipdfmx\\Ginclude@dvipdfmx",
            "\\def\\Ginclude@dvipdfmx##1##2{",
            "\\special{aitex:pdftex-ignore-next-driver-graphic}",
            "\\aitex@explicit@Ginclude@dvipdfmx{##1}{##2}",
            "}",
            "}",
            // pdfTeX's begin-document epstopdf hook still converts and embeds
            // EPS when graphicx was explicitly given `dvipdfmx`. Leave our
            // bounded EPS compatibility route active; only native dvipdfmx
            // PDF/raster and MetaPost specials remain ignored.
            "\\@ifundefined{Ginclude@mps}{}{",
            "\\let\\aitex@explicit@Ginclude@mps\\Ginclude@mps",
            "\\def\\Ginclude@mps##1{",
            "\\special{aitex:pdftex-ignore-next-driver-graphic}",
            "\\aitex@explicit@Ginclude@mps{##1}",
            "}",
            "}",
            // graphics-def's scale/rotation macros emit dvipdfmx-only
            // transform specials. pdfTeX ignores them when the author pins
            // that incompatible backend; suppressing the transport command
            // preserves the box dimensions computed by graphicx without
            // accidentally transforming all later page content.
            "\\@ifundefined{Gscale@start}{}{\\let\\Gscale@start\\relax}",
            "\\@ifundefined{Gscale@end}{}{\\let\\Gscale@end\\relax}",
            "\\@ifundefined{Grot@start}{}{\\let\\Grot@start\\relax}",
            "\\@ifundefined{Grot@end}{}{\\let\\Grot@end\\relax}",
            "\\fi",
            "\\fi",
            // The dvipdfmx transport records requested image dimensions, but
            // that is not enough to recover pdfTeX's outer graphicx matrix.
            // For `width=`, the requested dimension remains exact while the
            // matrix was rounded by `\\Gscale@div`; for `width=,height=`, the
            // requested dimensions have already absorbed that rounding.  A
            // `scale=` option is ambiguous with both. Carry the resolved pair
            // that pdftex.def would pass to `\\Gscale@box` so the host never
            // has to guess which path produced the ordinary image special.
            "\\@ifundefined{Ginclude@dvipdfmx}{}{",
            "\\let\\aitex@transport@Ginclude@dvipdfmx\\Ginclude@dvipdfmx",
            "\\def\\Ginclude@dvipdfmx##1##2{",
            "\\def\\aitex@graphic@bang{!}",
            "\\let\\aitex@graphic@scalex\\Gin@scalex",
            "\\let\\aitex@graphic@scaley\\Gin@scaley",
            "\\ifx\\aitex@graphic@scaley\\aitex@graphic@bang",
            "\\let\\aitex@graphic@scaley\\aitex@graphic@scalex",
            "\\else",
            "\\ifx\\aitex@graphic@scalex\\aitex@graphic@bang\\let\\aitex@graphic@scalex\\aitex@graphic@scaley\\fi",
            "\\fi",
            "\\ifx\\aitex@graphic@scalex\\aitex@graphic@bang\\def\\aitex@graphic@scalex{1}\\fi",
            "\\ifx\\aitex@graphic@scaley\\aitex@graphic@bang\\def\\aitex@graphic@scaley{1}\\fi",
            "\\special{aitex:pdftex-graphic-scale \\aitex@graphic@scalex,\\aitex@graphic@scaley}",
            "\\aitex@transport@Ginclude@dvipdfmx{##1}{##2}",
            "}",
            "}",
            // dvips/dvipdfmx round PSfile's rwi/rhi fields to tenths of a bp.
            // pdfTeX's epstopdf route retains the full TeX scaled dimension,
            // and the rounding is enough to move following text across the
            // raster oracle. Carry the exact requested box in a private
            // companion special immediately before the ordinary PSfile.
            "\\@ifundefined{Ginclude@eps}{}{",
            "\\let\\aitex@transport@Ginclude@eps\\Ginclude@eps",
            "\\def\\Ginclude@eps##1{",
            "\\special{aitex:eps-size \\the\\Gin@req@width,\\the\\Gin@req@height}",
            "\\aitex@transport@Ginclude@eps{##1}",
            "}",
            "}",
            // PGF's dvipdfmx backend substitutes a 1bp square when both image
            // dimensions are omitted. That is enough for a backend which can
            // discover and paint the natural image size later, but TeX has
            // already laid out the 1bp box by then. TikZ externalization uses
            // exactly this dimensionless `\\pgfimage` path for its cached PDF.
            // Delegate only natural-size, unmasked PGF images to graphicx: its
            // qualified dvipdfmx route reads our synthetic XBB, so TeX and the
            // PDF writer agree on the real box. Explicitly sized images and
            // image masks retain PGF's own special protocol unchanged.
            "\\@ifundefined{pgfsys@defineimage}{}{",
            "\\@ifundefined{includegraphics}{}{",
            "\\let\\aitex@pgfsys@defineimage\\pgfsys@defineimage",
            "\\def\\pgfsys@defineimage{",
            "\\ifx\\pgf@imagewidth\\@empty",
            "\\ifx\\pgf@imageheight\\@empty",
            "\\ifx\\pgf@imagemask\\@empty",
            "\\ifx\\pgf@imagepage\\@empty",
            "\\edef\\pgf@image{\\noexpand\\includegraphics{\\pgf@filename}}",
            "\\else",
            "\\edef\\pgf@image{\\noexpand\\includegraphics[page=\\pgf@imagepage]{\\pgf@filename}}",
            "\\fi",
            "\\else\\aitex@pgfsys@defineimage\\fi",
            "\\else\\aitex@pgfsys@defineimage\\fi",
            "\\else\\aitex@pgfsys@defineimage\\fi",
            "}",
            "}",
            "}",
            // For an EPS without an archived `*-eps-converted-to.pdf`, use a
            // private xbb sidecar. EPS that needs external normalization gets
            // its box from the actual restricted repstopdf result; the simple
            // internally translated subset keeps graphicx's ordinary DSC box.
            // DSC HiResBoundingBox is not authoritative:
            // Photoshop files in the compatibility corpus advertise boxes
            // that disagree with the page Ghostscript creates. The host
            // caches this normalization and reuses it for final PDF assembly.
            "\\@ifundefined{Gread@eps}{}{",
            "\\let\\aitex@saved@Gread@eps\\Gread@eps",
            "\\@ifundefined{ifaitex@epspdf}{\\newif\\ifaitex@epspdf}{}",
            "\\def\\aitex@transport@Gread@eps##1{",
            "\\filename@parse{##1}",
            "\\edef\\aitex@epspdf{\\filename@area\\filename@base-eps-converted-to.pdf}",
            "\\IfFileExists{\\aitex@epspdf}{\\aitex@epspdftrue}{\\aitex@epspdffalse}",
            "\\ifaitex@epspdf",
            "\\aitex@saved@Gread@eps{##1}",
            "\\else",
            "\\Gread@generic{##1.aitex-eps-normalized.xbb}\\Gread@eps@aux",
            "\\fi",
            "}",
            "\\def\\Gread@eps##1{",
            "\\ifaitex@epstopdfdraft",
            "\\expandafter\\aitex@saved@Gread@eps",
            "\\else",
            "\\expandafter\\aitex@transport@Gread@eps",
            "\\fi{##1}",
            "}",
            "}",
            // PGF's direct-pdf backend reserves resource objects and puts
            // their finalizers in `\pgfutil@everybye`. Its LaTeX adapter sees
            // that non-empty token register and clears the current page before
            // running those finalizers. The dvipdfmx backend needs no matching
            // resource writes, but skipping the clear is observably different:
            // RevTeX 4 sees a different current column at `enddocument` and
            // balances a last page that pdfLaTeX leaves unbalanced. A harmless
            // token gives PGF's existing end hook the same page boundary; the
            // token itself executes only after that clear.
            // Loading a PGF utility such as `pgffor` alone also creates the
            // token register, but pdfLaTeX leaves it empty because no system
            // driver was initialized.  Adding a token in that case inserts a
            // spurious final `\\clearpage`; RevTeX then ships its last column
            // before `close@column@grid` can balance it.  `pgfsys@invoke` is
            // installed by every real PGF system backend and is absent from
            // the utilities-only surface.
            "\\@ifundefined{pgfsys@invoke}{}{",
            "\\@ifundefined{pgfutil@everybye}{}{",
            "\\expandafter\\pgfutil@everybye\\expandafter{\\the\\pgfutil@everybye\\relax}",
            "}",
            "}",
            // A later graphics/color package may have made the driver guard
            // reload dvipdfmx.def after epstopdf loaded. That reload restores
            // the driver's raw EPS rule, losing epstopdf's archived-conversion
            // lookup. Reinstate the exact package-defined rule only when the
            // package actually installed one.
            "\\@ifundefined{aitex@epstopdf@epsrule}{}{",
            "\\expandafter\\let\\csname Gin@rule@.eps\\endcsname\\aitex@epstopdf@epsrule",
            "}",
            // epstopdf normally invokes restricted `repstopdf` through
            // \write18.  aiTeX never exposes a general shell, while its PDF
            // host already performs that same bounded conversion.  Preserve
            // epstopdf's archived `*-eps-converted-to.pdf` lookup, but when
            // only the original EPS exists route it through the driver's EPS
            // special so the host can normalize it safely.
            "\\@ifundefined{aitex@epstopdf@original@setfile}{}{",
            "\\let\\aitex@epstopdf@setfile\\Gin@setfile",
            "\\def\\Gin@setfile##1##2##3{",
            "\\ifnum\\pdf@strcmp{\\Gin@ext}{.eps}=0 ",
            "\\edef\\aitex@epspdf{\\Gin@base-eps-converted-to.pdf}",
            "\\IfFileExists{\\aitex@epspdf}{",
            "\\aitex@epstopdf@setfile{##1}{##2}{##3}",
            "}{",
            "\\IfFileExists{\\Gin@base\\Gin@ext}{",
            "\\aitex@epstopdf@original@setfile{eps}{.eps}{\\Gin@base\\Gin@ext}",
            "}{\\aitex@epstopdf@setfile{##1}{##2}{##3}}",
            "}",
            "\\else\\aitex@epstopdf@setfile{##1}{##2}{##3}\\fi",
            "}",
            "}",
            // Carry a requested source page and PDF page-box through the XBB
            // filename to the host resolver. The ordinary first-page cropbox
            // remains process-free; uncommon selections use extractbb with
            // the exact qualified arguments instead of silently measuring a
            // different page.
            "\\@namedef{Gin@rule@.pdf}##1{{pdf}{.pdf.aitex.\\Gin@page.\\Gin@pagebox.xbb}{##1}}",
            "\\@namedef{Gin@rule@.PDF}##1{{pdf}{.PDF.aitex.\\Gin@page.\\Gin@pagebox.xbb}{##1}}",
            // Illustrator files may be PDF containers with an `.ai` suffix.
            // dvipdfmx supports them as PDF, but an author-level rule such as
            // `{.ai}{pdf}{*}{}` makes its bbox reader open the binary asset
            // itself. Route both spellings through the same synthetic XBB
            // boundary as an ordinary PDF while preserving the real suffix.
            "\\@namedef{Gin@rule@.ai}##1{{pdf}{.ai.aitex.\\Gin@page.\\Gin@pagebox.xbb}{##1}}",
            "\\@namedef{Gin@rule@.AI}##1{{pdf}{.AI.aitex.\\Gin@page.\\Gin@pagebox.xbb}{##1}}",
            "\\@namedef{Gin@rule@.png}##1{{bmp}{.png.xbb}{##1}}",
            "\\@namedef{Gin@rule@.PNG}##1{{bmp}{.PNG.xbb}{##1}}",
            "\\@namedef{Gin@rule@.jpg}##1{{bmp}{.jpg.xbb}{##1}}",
            "\\@namedef{Gin@rule@.JPG}##1{{bmp}{.JPG.xbb}{##1}}",
            "\\@namedef{Gin@rule@.jpeg}##1{{bmp}{.jpeg.xbb}{##1}}",
            "\\@namedef{Gin@rule@.JPEG}##1{{bmp}{.JPEG.xbb}{##1}}",
            // pdftex.def gives a missing graphic a 72bp square when Gin's
            // global `draft` switch is active. dvipdfmx.def instead tries its
            // extractbb pipe even in draft mode; aiTeX intentionally does not
            // expose that shell escape. Preserve the pdfLaTeX behavior while
            // still measuring every graphic that actually exists.
            "\\let\\aitex@saved@Gread@pdf\\Gread@pdf",
            "\\let\\aitex@saved@Gread@bmp\\Gread@bmp",
            "\\def\\aitex@draftsafe@Gread##1##2{",
            "\\ifGin@draft",
            "\\IfFileExists{\\Gin@base\\Gin@ext}{##1{##2}}{",
            "\\def\\Gin@llx{0}\\let\\Gin@lly\\Gin@llx",
            "\\def\\Gin@urx{72}\\let\\Gin@ury\\Gin@urx",
            "}",
            "\\else##1{##2}\\fi",
            "}",
            "\\def\\Gread@pdf##1{\\aitex@draftsafe@Gread\\aitex@saved@Gread@pdf{##1}}",
            "\\def\\Gread@bmp##1{\\aitex@draftsafe@Gread\\aitex@saved@Gread@bmp{##1}}",
            "}",
            "\\AtBeginDocument{\\aitex@pdf@begin}",
            "\\makeatother",
        );
        if needs_epstopdf {
            // Execute after the prelude has declared the switch but before the
            // user's first source token. The begin-document hook above then
            // retains pdfTeX's epstopdf behavior only for projects which can
            // actually observe it.
            // PDF_DRIVER_PRELUDE restores `@` to its ordinary catcode before
            // the user's first token. Re-enter the private namespace for this
            // host-selected switch; otherwise TeX reads `\aitex` followed by
            // literal `@epstopdfneededtrue` and reports an undefined control
            // sequence only in the projects that need this compatibility path.
            source.insert_str(
                0,
                "\\makeatletter\\aitex@epstopdfneededtrue\\makeatother",
            );
        }
        source.insert_str(0, PDF_DRIVER_PRELUDE);
    }
    Ok(source)
}

/// Whether stock pdfTeX's automatic `epstopdf-base` load can affect this
/// project.
///
/// Loading the package expands roughly half a million tokens and used to tax
/// every PDF build, including projects containing only native PDF/PNG/JPEG
/// assets.  An EPS file (including an archived converted companion) or an
/// explicit use of the epstopdf API keeps the exact compatibility path.  The
/// walk is bounded and conservative: an unusually large or unreadable source
/// tree pays the package cost rather than risking different semantics.
fn project_may_need_epstopdf(path: &std::path::Path, root_source: &str) -> bool {
    const MAX_FILES: usize = 10_000;
    const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
    const SOURCE_EXTENSIONS: &[&str] = &["tex", "sty", "cls", "cfg", "def", "ltx"];

    fn mentions_epstopdf(bytes: &[u8]) -> bool {
        const NEEDLE: &[u8] = b"epstopdf";
        bytes.windows(NEEDLE.len()).any(|window| {
            window
                .iter()
                .zip(NEEDLE)
                .all(|(left, right)| left.to_ascii_lowercase() == *right)
        })
    }

    if mentions_epstopdf(root_source.as_bytes()) {
        return true;
    }
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut pending = vec![root.to_path_buf()];
    let mut files = 0usize;
    let mut source_bytes = root_source.len();
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => return true,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return true,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => return true,
            };
            // Do not follow symlinks outside the project boundary. Explicit
            // `.eps` spellings in source are still caught by the text scan.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            files += 1;
            if files > MAX_FILES {
                return true;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let lower_name = name.to_ascii_lowercase();
            if lower_name.ends_with(".eps") || lower_name.contains("-eps-converted-to.pdf") {
                return true;
            }
            let extension = entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase);
            if !extension
                .as_deref()
                .is_some_and(|extension| SOURCE_EXTENSIONS.contains(&extension))
            {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => return true,
            };
            let size = match usize::try_from(metadata.len()) {
                Ok(size) => size,
                Err(_) => return true,
            };
            source_bytes = match source_bytes.checked_add(size) {
                Some(total) if total <= MAX_SOURCE_BYTES => total,
                _ => return true,
            };
            let bytes = match std::fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(_) => return true,
            };
            if mentions_epstopdf(&bytes) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod epstopdf_project_tests {
    use super::{project_may_need_epstopdf, source_for_run};

    fn root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aitex-epstopdf-hint-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn native_graphics_project_skips_the_epstopdf_prelude() {
        let directory = root("native");
        let main = directory.join("main.tex");
        std::fs::write(&main, b"\\includegraphics{figure.pdf}").unwrap();
        std::fs::write(directory.join("figure.pdf"), b"%PDF-1.7\n").unwrap();
        assert!(!project_may_need_epstopdf(
            &main,
            "\\includegraphics{figure.pdf}"
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn eps_assets_and_explicit_epstopdf_apis_keep_compatibility() {
        let directory = root("needed");
        let main = directory.join("main.tex");
        std::fs::write(&main, b"\\includegraphics{figure}").unwrap();
        let nested = directory.join("figures");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("figure.EPS"), b"%!PS\n").unwrap();
        assert!(project_may_need_epstopdf(
            &main,
            "\\includegraphics{figure}"
        ));
        std::fs::remove_file(nested.join("figure.EPS")).unwrap();
        std::fs::write(
            nested.join("configuration.sty"),
            b"\\epstopdfsetup{update=false}",
        )
        .unwrap();
        assert!(project_may_need_epstopdf(
            &main,
            "\\includegraphics{figure.pdf}"
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn eps_switch_is_tokenized_inside_the_private_namespace() {
        let directory = root("switch-catcode");
        let main = directory.join("main.tex");
        std::fs::write(&main, b"\\documentclass{article}\\begin{document}x\\end{document}")
            .unwrap();
        std::fs::write(directory.join("figure.eps"), b"%!PS\n").unwrap();
        let source = source_for_run(main.to_str().unwrap(), true, None).unwrap();
        assert!(source.contains(
            "\\makeatother\\makeatletter\\aitex@epstopdfneededtrue\\makeatother\\documentclass"
        ));
        assert!(!source.contains("\\makeatother\\aitex@epstopdfneededtrue"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}

fn take_fork_preloaded_engine(
    key: &str,
    clock: WallClock,
) -> Option<ForkPreloadedEngine> {
    use std::sync::atomic::Ordering;
    if !FORK_CHILD_ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    FORK_PRELOADED_ENGINE.with(|slot| {
        let matches = slot
            .borrow()
            .as_ref()
            .is_some_and(|preloaded| {
                preloaded.key == key
                    && preloaded
                        .clock
                        .is_none_or(|expected| clock_matches(expected, clock))
            });
        matches.then(|| slot.borrow_mut().take()).flatten()
    })
}

fn restore_base_format(
    path: &str,
    retain_decoded: bool,
    known_identity: Option<&str>,
) -> Result<(Fast, bool, &'static str), String> {
    // Cache-key construction already authenticates the format for build and
    // watch paths. Reuse that digest instead of hashing the 18 MiB format a
    // second time. A cold one-shot pass neither retains nor keys the decoded
    // format, so it can skip the digest entirely.
    let identity = retain_decoded.then(|| {
        known_identity
            .map(str::to_owned)
            .unwrap_or_else(|| base_format_identity(Some(path)))
    });
    if retain_decoded
        && let Some(engine) = HOT_BASE_FORMAT.with(|slot| {
            slot.borrow()
                .as_ref()
                .filter(|format| Some(format.identity.as_str()) == identity.as_deref())
                .map(|format| format.engine.clone())
        })
    {
        return Ok((engine, false, "hot-clone"));
    }

    let bytes = std::fs::read(path).map_err(|error| format!("cannot read format {path}: {error}"))?;
    let engine = Fast::undump(&bytes).map_err(|error| format!("cannot use format {path}: {error}"))?;
    if retain_decoded {
        // Keep the pristine decoded value and hand this pass its independent
        // clone. This one bounded copy is substantially cheaper than parsing
        // the serialized format again, and prevents one pass from mutating the
        // template used by the next.
        let pass_engine = engine.clone();
        HOT_BASE_FORMAT.with(|slot| {
            *slot.borrow_mut() = Some(HotBaseFormat {
                identity: identity.expect("retained formats have an identity"),
                engine,
            });
        });
        Ok((pass_engine, true, "decoded+retained"))
    } else {
        Ok((engine, true, "decoded"))
    }
}

fn clone_hot_document_boundary(
    key: &str,
    clock: WallClock,
) -> Result<Option<(Fast, Vec<CacheDependency>, u8, Option<String>)>, String> {
    HOT_DOCUMENT_BOUNDARY.with(|slot| {
        let boundary = slot.borrow();
        let Some(boundary) = boundary.as_ref() else {
            return Ok(None);
        };
        if boundary.key != key {
            return Ok(None);
        }
        validate_hot_document_boundary(boundary, clock)?;
        Ok(Some((
            boundary.engine.clone(),
            boundary.dependencies.clone(),
            boundary.clock_reads,
            boundary.recent_graphic_asset.clone(),
        )))
    })
}

fn validate_hot_document_boundary(
    boundary: &HotDocumentBoundary,
    clock: WallClock,
) -> Result<(), String> {
    if boundary.clock != clock {
        return Err("clock dependency changed".to_string());
    }
    dependencies_hold(&boundary.dependencies)
}

fn retain_hot_document_boundary(
    key: String,
    engine: &Fast,
    dependencies: Vec<CacheDependency>,
    clock: WallClock,
    clock_reads: u8,
    recent_graphic_asset: Option<String>,
) {
    HOT_DOCUMENT_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some(HotDocumentBoundary {
            key,
            engine: engine.clone(),
            dependencies,
            clock,
            clock_reads,
            recent_graphic_asset,
        });
    });
}

#[cfg(test)]
mod document_boundary_tests {
    use super::{CacheDependency, Fast, HotDocumentBoundary, validate_hot_document_boundary};

    fn boundary(dependencies: Vec<CacheDependency>) -> HotDocumentBoundary {
        HotDocumentBoundary {
            key: "key".to_string(),
            engine: Fast::new(),
            dependencies,
            clock: (12 * 60, 9, 9, 2026, -7 * 60, 0),
            clock_reads: 0,
            recent_graphic_asset: None,
        }
    }

    #[test]
    fn every_semantic_tool_input_invalidates_a_dependent_document_boundary() {
        let root = std::env::temp_dir().join(format!(
            "aitex-document-boundary-dependencies-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        for extension in ["aux", "bbl", "ind", "gls", "acr", "nls"] {
            let path = root.join(format!("main.{extension}"));
            std::fs::write(&path, b"old").unwrap();
            let name = path.to_string_lossy().into_owned();
            let dependency = CacheDependency::from_resolution(&name, Some(&name));
            let boundary = boundary(vec![dependency]);
            assert!(
                validate_hot_document_boundary(&boundary, boundary.clock).is_ok(),
                "unchanged .{extension} dependency"
            );

            std::fs::write(&path, b"changed bytes").unwrap();
            assert!(
                validate_hot_document_boundary(&boundary, boundary.clock).is_err(),
                "changed .{extension} dependency must invalidate"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_appearing_auxiliary_or_changed_clock_invalidates_the_boundary() {
        let root = std::env::temp_dir().join(format!(
            "aitex-document-boundary-absence-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("main.aux");
        let name = path.to_string_lossy().into_owned();
        let missing = CacheDependency::from_resolution(&name, None);
        let boundary = boundary(vec![missing]);
        assert!(validate_hot_document_boundary(&boundary, boundary.clock).is_ok());

        std::fs::write(&path, b"now present").unwrap();
        assert!(validate_hot_document_boundary(&boundary, boundary.clock).is_err());
        let changed_clock = (
            boundary.clock.0 + 1,
            boundary.clock.1,
            boundary.clock.2,
            boundary.clock.3,
            boundary.clock.4,
            boundary.clock.5,
        );
        assert!(
            validate_hot_document_boundary(&boundary, changed_clock).is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(target_os = "linux")]
fn refresh_fork_preloaded_engine() -> bool {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if std::env::args().any(|argument| {
        matches!(argument.as_str(), "--no-cache" | "--no-project-cache")
    }) {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    }
    let Some(path) = input_argument(&arguments) else {
        return false;
    };
    let dvi_out = dvi_flag().or_else(|| std::env::var("AITEX_DVI").ok());
    let pdf_driver_mode = dvi_out.as_deref().is_some_and(|out| out.ends_with(".pdf"));
    let unicode_options = unicode_flag()
        .then(|| load_unicode_options(path, &arguments))
        .transpose()
        .ok()
        .flatten();
    if unicode_flag() && unicode_options.is_none() {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    }
    let Some(source) = source_for_run(path, pdf_driver_mode, unicode_options.as_ref()).ok() else {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    };
    let Some((preamble, _)) = split_preamble(&source) else {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    };
    let Some(root) = cache_root_argument().map(std::path::PathBuf::from) else {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    };
    let job_name = std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let max_print_line = std::env::var("max_print_line")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(79);
    let format_identity = base_format_identity(format_argument().as_deref());
    let key = snapshot_key(
        CacheKind::Project,
        preamble,
        Some(job_name),
        &format_identity,
        pdf_driver_mode,
        tex82_flag(),
        max_print_line,
    );
    let clock = wall_clock();
    let CacheLookup::Hit(hit) = load_cache(&root, CacheKind::Project, &key, clock) else {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    };
    let already_loaded = FORK_PRELOADED_ENGINE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|preloaded| {
            preloaded.key == key && Rc::ptr_eq(&preloaded.payload, &hit.bytes)
        })
    });
    if already_loaded {
        return true;
    }
    let Ok(engine) = Fast::undump(&hit.bytes) else {
        FORK_PRELOADED_ENGINE.with(|slot| *slot.borrow_mut() = None);
        return false;
    };
    FORK_PRELOADED_ENGINE.with(|slot| {
        *slot.borrow_mut() = Some(ForkPreloadedEngine {
            key,
            payload: hit.bytes,
            engine,
            dependencies: hit.dependencies,
            clock: hit.clock,
        });
    });
    true
}

fn run() -> RunOutcome {
    // `cache warm` and `watch` execute multiple complete jobs on this thread.
    // Closing transcript lines belong to one job only; retaining them would
    // make every later `.log` repeat the output records of earlier builds.
    LOG_TAIL.with(|tail| tail.borrow_mut().clear());
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let path = input_argument(&arguments)
        .expect("main validated the input argument")
        .to_string();
    let dvi_out = dvi_flag().or_else(|| std::env::var("AITEX_DVI").ok());
    let pdf_driver_mode = dvi_out.as_deref().is_some_and(|out| out.ends_with(".pdf"));
    let unicode_mode = unicode_flag();
    let has_unicode_option = [
        "--font",
        "--font-path",
        "--language",
        "--features",
        "--variations",
        "--direction",
    ]
    .iter()
    .any(|option| arguments.iter().any(|argument| argument == option));
    if !unicode_mode && has_unicode_option {
        eprintln!("aitex: font/language options require --unicode");
        std::process::exit(2);
    }
    let unicode_options = if unicode_mode {
        Some(load_unicode_options(&path, &arguments).unwrap_or_else(|error| {
            eprintln!("aitex: invalid Unicode configuration: {error}");
            std::process::exit(2);
        }))
    } else {
        None
    };
    // Qualification can force the former serialize/parse route. This is not a
    // user-facing mode: both paths must produce identical bytes, and keeping
    // it out of cache identity lets the differential test exercise that fact.
    let direct_pdf_mode =
        pdf_driver_mode && std::env::var_os("AITEX_PDF_DVI_COMPAT").is_none();
    if unicode_mode && !pdf_driver_mode {
        eprintln!("aitex: --unicode currently requires PDF output (-o FILE.pdf)");
        std::process::exit(2);
    }
    if unicode_mode && std::env::var_os("AITEX_VLIST").is_some() {
        eprintln!("aitex: --unicode does not yet support the AITEX_VLIST developer probe");
        std::process::exit(2);
    }
    let t0 = Instant::now();
    let mut metrics = RunMetrics::default();
    let metrics_enabled = std::env::var_os("AITEX_METRICS").is_some();
    reset_resolver_metrics(metrics_enabled);
    metrics.pdf.enabled = metrics_enabled;
    let source_started = Instant::now();
    let src = match source_for_run(&path, pdf_driver_mode, unicode_options.as_ref()) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("aitex: cannot read {path}: {e}");
            std::process::exit(1);
        }
    };
    metrics.source_read = source_started.elapsed();
    cli_phase("Read", &path, metrics.source_read);

    // Cache identity is conservative by design. Anything that can affect
    // prefix execution is part of the SHA-256 key; every file actually read is
    // re-resolved and rehashed from the manifest before a snapshot is trusted.
    let job_name = std::path::Path::new(&path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .to_string();
    let display_output = dvi_out
        .clone()
        .or_else(|| arg_after("--dump"))
        .unwrap_or_else(|| format!("{job_name}.log"));
    let clock = wall_clock();
    let max_print_line = std::env::var("max_print_line")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(79);
    let format_arg = format_argument();
    let cache_root = (!std::env::args().any(|arg| arg == "--no-cache"))
        .then(cache_root_argument)
        .flatten()
        .map(std::path::PathBuf::from);
    let front_door = classify_front_door(&arguments);
    let document_boundary_enabled = matches!(front_door, FrontDoor::Build);
    let track_dependencies = cache_root.is_some()
        || document_boundary_enabled
        || matches!(front_door, FrontDoor::Watch | FrontDoor::ForkWatch)
        || std::env::var_os("AITEX_FORMAT_DEPENDENCIES").is_some();
    let format_identity = (cache_root.is_some() || document_boundary_enabled)
        .then(|| base_format_identity(format_arg.as_deref()));
    let document_boundary_key = document_boundary_enabled.then(|| {
        // The whole source, not a host-guessed prefix, identifies the live
        // source frame retained by the engine. Dependencies read before the
        // observed boundary are validated separately immediately before use.
        snapshot_key(
            CacheKind::Project,
            &src,
            Some(job_name.as_str()),
            format_identity
                .as_deref()
                .expect("document boundaries have a format identity"),
            pdf_driver_mode,
            tex82_flag(),
            max_print_line,
        )
    });
    let project_cache_enabled = !std::env::args().any(|arg| arg == "--no-project-cache");
    let class_cache_enabled = !std::env::args().any(|arg| arg == "--no-class-cache");
    let output_cache_enabled = cache_root.is_some()
        && dvi_out.as_deref().is_some_and(|out| !is_output_sink(out))
        && matches!(
            front_door,
            FrontDoor::Run
                | FrontDoor::Build
                | FrontDoor::Pass
                | FrontDoor::CacheWarm
                | FrontDoor::Watch
                | FrontDoor::ForkWatch
        )
        && arg_after("--dump").is_none()
        && probe_argument(&arguments).is_none()
        && completed_output_reuse_allowed();
    // P3 remains an explicit watch-mode experiment until the exact DVI/PDF
    // differential gate below the host layer has qualified it. One-shot and
    // fork-watch jobs retain their bounded streaming-page behavior.
    let incremental_requested = arguments.first().map(String::as_str) == Some("watch")
        && std::env::var_os("AITEX_INCREMENTAL").is_some();
    let incremental_enabled = incremental_requested
        && !unicode_mode
        && dvi_out.is_some()
        && std::env::var_os("AITEX_VLIST").is_none()
        && split_preamble(&src)
            .is_some_and(|(_, body)| body.len() >= BODY_CHECKPOINT_MIN_SOURCE);
    if incremental_requested && !incremental_enabled {
        let reason = if dvi_out.is_none() {
            "no DVI/PDF output was requested"
        } else if std::env::var_os("AITEX_VLIST").is_some() {
            "AITEX_VLIST requires retained engine pages"
        } else {
            "body is below the 8 KiB admission floor"
        };
        cli_detail!("body checkpoints bypassed ({reason})");
        HOT_BODY_CHECKPOINTS.with(|slot| *slot.borrow_mut() = None);
    }
    // The release path no longer guesses a semantic preamble boundary. Cache
    // warming now targets the independently safe exact completed-output entry.
    let project_snapshot_expected = output_cache_enabled;
    let mut pending_project: Option<PendingCache> = None;
    let mut pending_output: Option<PendingCache> = None;
    let mut project_snapshot_key: Option<String> = None;
    let mut pending_class: Option<PendingCache> = None;
    let mut pending_job_class: Option<PendingCache> = None;
    let mut project_snapshot_hit = false;
    let mut project_snapshot_ready = false;
    let mut class_snapshot_hit = false;
    let mut restored_clock_reads = 0u8;
    let mut restored_engine: Option<Fast> = None;
    let mut deferred_project_snapshot: Option<Rc<Vec<u8>>> = None;
    let mut restored_dependencies = Vec::new();
    // Completed-output identity covers the entire source, observable request,
    // locked format, dependencies, clock reads, and side artifacts. It needs
    // no preamble split and is therefore safe even for generated or aliased
    // `\begin{document}` spellings.
    if let (Some(root), Some(output)) = (cache_root.as_ref(), dvi_out.as_deref())
        && output_cache_enabled
    {
        let format_identity = format_identity
            .as_deref()
            .expect("output caching has a format identity");
        let validation_started = Instant::now();
        metrics.cache_state = "miss";
        let output_key = output_cache_key(
            &src,
            &path,
            output,
            &job_name,
            format_identity,
            format_arg.as_deref(),
            pdf_driver_mode,
            tex82_flag(),
            max_print_line,
        );
        pending_output = Some(PendingCache {
            root: root.clone(),
            kind: CacheKind::Output,
            key: output_key.clone(),
        });
        match load_output_cache(root, &output_key, clock, Path::new(output)) {
            CacheLookup::Hit(hit) => {
                let written_files = cached_semantic_control_files(&hit.dependencies);
                metrics.cache_load += hit.load_time;
                metrics.cache_validation = validation_started
                    .elapsed()
                    .saturating_sub(metrics.cache_load);
                let write_started = Instant::now();
                let output_current = hit.output_current;
                let restored = if output_current {
                    Ok(())
                } else if filesystem_watch_mode() {
                    publish_output_artifact(output, &hit.bytes)
                } else {
                    atomic_write(Path::new(output), &hit.bytes)
                };
                match restored {
                    Ok(()) => {
                        metrics.output_finalize = write_started.elapsed();
                        metrics.cache_state = "output-hit";
                        cli_detail!(
                            "persistent output cache hit (source, dependencies, clock, and auxiliary artifacts exact)"
                        );
                        cli_detail!("converged: no rerun needed");
                        let detail = format!(
                            "{} · {} dependenc{}",
                            if output_current {
                                "output already current"
                            } else {
                                "restored output cache"
                            },
                            hit.dependencies.len(),
                            if hit.dependencies.len() == 1 { "y" } else { "ies" },
                        );
                        cli_phase(
                            if output_current { "Cache" } else { "Restore" },
                            detail,
                            metrics.cache_validation + metrics.cache_load + metrics.output_finalize,
                        );
                        cli_finish_pass(&front_door, Some(output), t0.elapsed(), false);
                        if metrics_enabled {
                            emit_run_metrics(&metrics, t0.elapsed());
                        }
                        return RunOutcome {
                            project_snapshot_expected,
                            project_snapshot_ready: true,
                            rerun_needed: false,
                            dependencies: hit.dependencies,
                            written_files,
                            diagnostics: Vec::new(),
                        };
                    }
                    Err(error) => {
                        cli_detail!("aitex: cannot restore persistent output cache: {error}");
                    }
                }
            }
            CacheLookup::Miss(reason) => {
                if reason != "absent" {
                    cli_detail!("aitex: persistent output cache miss ({reason})");
                }
            }
            CacheLookup::Invalid(reason) => {
                cli_detail!("aitex: ignoring persistent output cache ({reason})");
            }
        }
        metrics.cache_validation = validation_started
            .elapsed()
            .saturating_sub(metrics.cache_load);
    }
    if let (Some(root), Some((preamble, _))) = (cache_root.as_ref(), split_preamble(&src)) {
        let format_identity = format_identity
            .as_deref()
            .expect("project caching has a format identity");
        let validation_started = Instant::now();
        if project_cache_enabled {
            metrics.cache_state = "miss";
            let key = snapshot_key(
                CacheKind::Project,
                preamble,
                Some(job_name.as_str()),
                format_identity,
                pdf_driver_mode,
                tex82_flag(),
                max_print_line,
            );
            project_snapshot_key = Some(key.clone());
            if output_cache_enabled && let Some(output) = dvi_out.as_deref() {
                let output_key = output_cache_key(
                    &src,
                    &path,
                    output,
                    &job_name,
                    format_identity,
                    format_arg.as_deref(),
                    pdf_driver_mode,
                    tex82_flag(),
                    max_print_line,
                );
                pending_output = Some(PendingCache {
                    root: root.clone(),
                    kind: CacheKind::Output,
                    key: output_key.clone(),
                });
                match load_output_cache(root, &output_key, clock, Path::new(output)) {
                    CacheLookup::Hit(hit) => {
                        let written_files = cached_semantic_control_files(&hit.dependencies);
                        metrics.cache_load += hit.load_time;
                        metrics.cache_validation = validation_started
                            .elapsed()
                            .saturating_sub(metrics.cache_load);
                        let write_started = Instant::now();
                        let output_current = hit.output_current;
                        let restored = if output_current {
                            Ok(())
                        } else if filesystem_watch_mode() {
                            publish_output_artifact(output, &hit.bytes)
                        } else {
                            atomic_write(Path::new(output), &hit.bytes)
                        };
                        match restored {
                            Ok(()) => {
                                metrics.output_finalize = write_started.elapsed();
                                metrics.cache_state = "output-hit";
                                cli_detail!(
                                    "project snapshot hit (converged output artifact reused)"
                                );
                                cli_detail!(
                                    "persistent output cache hit (source, dependencies, clock, and auxiliary artifacts exact)"
                                );
                                cli_detail!("converged: no rerun needed");
                                let detail = format!(
                                    "{} · {} dependenc{}",
                                    if output_current {
                                        "output already current"
                                    } else {
                                        "restored output cache"
                                    },
                                    hit.dependencies.len(),
                                    if hit.dependencies.len() == 1 { "y" } else { "ies" },
                                );
                                cli_phase(
                                    if output_current { "Cache" } else { "Restore" },
                                    detail,
                                    metrics.cache_validation
                                        + metrics.cache_load
                                        + metrics.output_finalize,
                                );
                                cli_finish_pass(&front_door, Some(output), t0.elapsed(), false);
                                if metrics_enabled {
                                    emit_run_metrics(&metrics, t0.elapsed());
                                }
                                return RunOutcome {
                                    project_snapshot_expected,
                                    project_snapshot_ready: true,
                                    rerun_needed: false,
                                    dependencies: hit.dependencies,
                                    written_files,
                                    diagnostics: Vec::new(),
                                };
                            }
                            Err(error) => {
                                cli_detail!(
                                    "aitex: cannot restore persistent output cache: {error}"
                                );
                            }
                        }
                    }
                    CacheLookup::Miss(reason) => {
                        if reason != "absent" {
                            cli_detail!("aitex: persistent output cache miss ({reason})");
                        }
                    }
                    CacheLookup::Invalid(reason) => {
                        cli_detail!("aitex: ignoring persistent output cache ({reason})");
                    }
                }
            }
            let completed_dependencies = if incremental_enabled && completed_output_reuse_allowed()
            {
                match (split_preamble(&src), dvi_out.as_deref()) {
                    (Some((_, body)), Some(output)) => HOT_BODY_CHECKPOINTS.with(|slot| {
                        slot.borrow().as_ref().and_then(|index| {
                            completed_output_is_reusable(index, &key, body, clock, output)
                                .then(|| (index.dependencies.clone(), output))
                        })
                    }),
                    _ => None,
                }
            } else {
                None
            };
            if let Some((dependencies, output)) = completed_dependencies {
                metrics.cache_state = "hit";
                metrics.cache_validation = validation_started.elapsed();
                cli_detail!("project snapshot hit (final body artifact remains valid)");
                cli_detail!("body checkpoint hit (no source edit, final artifact reused)");
                cli_detail!("converged: no rerun needed");
                cli_phase("Restore", "hot body checkpoint", metrics.cache_validation);
                cli_finish_pass(&front_door, Some(output), t0.elapsed(), false);
                if metrics_enabled {
                    emit_run_metrics(&metrics, t0.elapsed());
                }
                return RunOutcome {
                    project_snapshot_expected,
                    project_snapshot_ready: true,
                    rerun_needed: false,
                    dependencies,
                    written_files: Vec::new(),
                    diagnostics: Vec::new(),
                };
            }
            pending_project = Some(PendingCache {
                root: root.clone(),
                kind: CacheKind::Project,
                key: key.clone(),
            });
            if let Some(preloaded) = take_fork_preloaded_engine(&key, clock) {
                cli_detail!(
                    "project snapshot fork hit ({:.1} MB)",
                    preloaded.payload.len() as f64 / 1e6
                );
                metrics.cache_state = "fork-hit";
                metrics.snapshot_bytes = preloaded.payload.len();
                project_snapshot_hit = true;
                project_snapshot_ready = true;
                pending_project = None;
                restored_clock_reads = clock_read_mask(preloaded.clock);
                restored_dependencies = preloaded.dependencies;
                restored_engine = Some(preloaded.engine);
            } else {
                match load_cache(root, CacheKind::Project, &key, clock) {
                    CacheLookup::Hit(hit) => {
                        metrics.cache_load += hit.load_time;
                        cli_detail!(
                            "project snapshot hit ({:.1} MB)",
                            hit.bytes.len() as f64 / 1e6
                        );
                        metrics.cache_state = "hit";
                        metrics.snapshot_bytes = hit.bytes.len();
                        project_snapshot_hit = true;
                        project_snapshot_ready = true;
                        restored_clock_reads = clock_read_mask(hit.clock);
                        restored_dependencies = hit.dependencies;
                        if incremental_enabled {
                            deferred_project_snapshot = Some(hit.bytes);
                        } else {
                            let restore_started = Instant::now();
                            match Fast::undump(&hit.bytes) {
                                Ok(engine) => {
                                    pending_project = None;
                                    restored_engine = Some(engine);
                                }
                                Err(error) => {
                                    project_snapshot_hit = false;
                                    project_snapshot_ready = false;
                                    restored_clock_reads = 0;
                                    restored_dependencies.clear();
                                    metrics.cache_state = "invalid";
                                    cli_detail!("aitex: ignoring project snapshot ({error})");
                                }
                            }
                            metrics.snapshot_restore += restore_started.elapsed();
                        }
                    }
                    CacheLookup::Miss(reason) => {
                        if reason != "absent" {
                            cli_detail!("aitex: project cache miss ({reason})");
                        }
                    }
                    CacheLookup::Invalid(reason) => {
                        metrics.cache_state = "invalid";
                        cli_detail!("aitex: ignoring project cache entry ({reason})");
                    }
                }
            }
        }
        if !project_snapshot_hit
            && class_cache_enabled
            && let Some((class_prefix, _)) = split_documentclass(preamble)
        {
            metrics.class_cache_state = "miss";
            let shared_key = snapshot_key(
                CacheKind::Class,
                class_prefix,
                None,
                &format_identity,
                pdf_driver_mode,
                tex82_flag(),
                max_print_line,
            );
            let job_key = snapshot_key(
                CacheKind::Class,
                class_prefix,
                Some(job_name.as_str()),
                &format_identity,
                pdf_driver_mode,
                tex82_flag(),
                max_print_line,
            );
            pending_class = Some(PendingCache {
                root: root.clone(),
                kind: CacheKind::Class,
                key: shared_key.clone(),
            });
            pending_job_class = Some(PendingCache {
                root: root.clone(),
                kind: CacheKind::Class,
                key: job_key.clone(),
            });
            // A class that observes `\jobname` has a job-specific object. Try
            // it first, then the genuinely shared object used by ordinary
            // classes. Prefix execution decides which key is published.
            let lookup = match load_cache(root, CacheKind::Class, &job_key, clock) {
                CacheLookup::Hit(hit) => CacheLookup::Hit(hit),
                CacheLookup::Miss(reason) => {
                    if reason != "absent" {
                        cli_detail!("aitex: job-specific class cache miss ({reason})");
                    }
                    load_cache(root, CacheKind::Class, &shared_key, clock)
                }
                CacheLookup::Invalid(reason) => {
                    cli_detail!("aitex: ignoring job-specific class cache entry ({reason})");
                    load_cache(root, CacheKind::Class, &shared_key, clock)
                }
            };
            match lookup {
                CacheLookup::Hit(hit) => {
                    metrics.cache_load += hit.load_time;
                    let restore_started = Instant::now();
                    match Fast::undump(&hit.bytes) {
                        Ok(engine) => {
                            cli_detail!(
                                "class snapshot hit ({:.1} MB)",
                                hit.bytes.len() as f64 / 1e6
                            );
                            metrics.class_cache_state = "hit";
                            metrics.snapshot_bytes = hit.bytes.len();
                            class_snapshot_hit = true;
                            restored_clock_reads = clock_read_mask(hit.clock);
                            pending_class = None;
                            pending_job_class = None;
                            restored_dependencies = hit.dependencies;
                            restored_engine = Some(engine);
                        }
                        Err(error) => {
                            metrics.class_cache_state = "invalid";
                            cli_detail!("aitex: ignoring class snapshot ({error})");
                        }
                    }
                    metrics.snapshot_restore += restore_started.elapsed();
                }
                CacheLookup::Miss(reason) => {
                    if reason != "absent" {
                        cli_detail!("aitex: class cache miss ({reason})");
                    }
                }
                CacheLookup::Invalid(reason) => {
                    metrics.class_cache_state = "invalid";
                    cli_detail!("aitex: ignoring class cache entry ({reason})");
                }
            }
        }
        metrics.cache_validation = validation_started
            .elapsed()
            .saturating_sub(metrics.cache_load)
            .saturating_sub(metrics.snapshot_restore);
    }

    let mut body_checkpoint_builder: Option<BodyCheckpointBuilder> = None;
    let mut body_resumed_at = None;
    let mut body_resumed_shipper = None;
    let mut body_capture_limit = None;
    if incremental_enabled
        && let (Some(key), Some((_, body))) =
            (project_snapshot_key.as_deref(), split_preamble(&src))
    {
        let prior = HOT_BODY_CHECKPOINTS.with(|slot| slot.borrow_mut().take());
        if project_snapshot_hit {
            match prior {
                Some(index) => {
                    let select_started = Instant::now();
                    let selected = select_body_checkpoint(
                        index,
                        key,
                        body,
                        clock,
                        &restored_dependencies,
                    );
                    metrics.cache_validation += select_started.elapsed();
                    match selected {
                    Ok(hit) => {
                        let BodyCheckpointHit {
                            site,
                            bytes,
                            prepared,
                            dependencies,
                            builder,
                            changed,
                            shipper,
                            kinds,
                            dependency_changed,
                        } = hit;
                        let restore_started = Instant::now();
                        let restored = match (prepared, bytes) {
                            (Some(engine), _) => {
                                engine.resume_prepared_checkpoint(site.clone(), body)
                            }
                            (None, Some(bytes)) => Fast::resume_checkpoint(&bytes, body),
                            (None, None) => unreachable!("checkpoint has bytes or a prepared engine"),
                        };
                        match restored {
                            Ok((engine, restored_site)) if restored_site == site => {
                                cli_detail!(
                                    "body checkpoint hit (byte {}, paragraph {}, boundary {}, {})",
                                    site.byte_offset,
                                    site.paragraph_serial,
                                    kinds.label(),
                                    changed.map_or_else(
                                        || if dependency_changed {
                                            "dependency suffix changed"
                                        } else {
                                            "no source edit"
                                        },
                                        |_| "source suffix changed",
                                    ),
                                );
                                restored_engine = Some(engine);
                                restored_dependencies = dependencies;
                                body_resumed_at = Some(site);
                                body_resumed_shipper = Some(shipper);
                                // Rebuilding a multi-megabyte checkpoint in the
                                // foreground made the edit that benefited from
                                // it pay the serialization bill. Keep the
                                // proven prefix now; a later cold/full pass can
                                // replenish downstream entries.
                                body_capture_limit = changed.map(|_| 0);
                                body_checkpoint_builder = Some(builder);
                            }
                            Ok(_) => {
                                cli_detail!("aitex: body checkpoint miss (provenance mismatch)");
                                body_checkpoint_builder = Some(BodyCheckpointBuilder::empty(
                                    key.to_string(),
                                    body.to_string(),
                                ));
                            }
                            Err(error) => {
                                cli_detail!("aitex: body checkpoint miss ({error})");
                                body_checkpoint_builder = Some(BodyCheckpointBuilder::empty(
                                    key.to_string(),
                                    body.to_string(),
                                ));
                            }
                        }
                        metrics.snapshot_restore += restore_started.elapsed();
                    }
                    Err((builder, reason)) => {
                        cli_detail!("body checkpoint miss ({reason})");
                        body_checkpoint_builder = Some(builder);
                    }
                    }
                }
                None => {
                    cli_detail!("body checkpoint cold");
                    body_checkpoint_builder = Some(BodyCheckpointBuilder::empty(
                        key.to_string(),
                        body.to_string(),
                    ));
                }
            }
        } else {
            if prior.is_some() {
                cli_detail!("body checkpoint miss (project prefix changed)");
            } else {
                cli_detail!("body checkpoint cold");
            }
            body_checkpoint_builder = Some(BodyCheckpointBuilder::empty(
                key.to_string(),
                body.to_string(),
            ));
        }
    }

    if restored_engine.is_none()
        && let Some(bytes) = deferred_project_snapshot.take()
    {
        let restore_started = Instant::now();
        match Fast::undump(&bytes) {
            Ok(engine) => restored_engine = Some(engine),
            Err(error) => {
                project_snapshot_hit = false;
                project_snapshot_ready = false;
                restored_clock_reads = 0;
                restored_dependencies.clear();
                metrics.cache_state = "invalid";
                cli_detail!("aitex: ignoring project snapshot ({error})");
            }
        }
        metrics.snapshot_restore += restore_started.elapsed();
    }
    if project_snapshot_hit && restored_engine.is_some() {
        pending_project = None;
    }

    let mut document_boundary_resumed = false;
    let mut document_boundary_recent_graphic_asset = None;
    if restored_engine.is_none()
        && let Some(key) = document_boundary_key.as_deref()
    {
        let restore_started = Instant::now();
        match clone_hot_document_boundary(key, clock) {
            Ok(Some((engine, dependencies, boundary_clock_reads, recent_graphic_asset))) => {
                restored_clock_reads = boundary_clock_reads;
                restored_dependencies = dependencies;
                restored_engine = Some(engine);
                document_boundary_resumed = true;
                document_boundary_recent_graphic_asset = recent_graphic_asset;
                cli_detail!(
                    "document boundary hit (engine-observed state and prefix dependencies exact)"
                );
            }
            Ok(None) => cli_detail!("document boundary cold"),
            Err(reason) => cli_detail!("document boundary miss ({reason})"),
        }
        metrics.snapshot_restore += restore_started.elapsed();
    }

    // A project or class snapshot supersedes its base format. Otherwise start
    // from the requested dumped format, or from INITEX state. Remember that
    // distinction after moving the restored engine: device defaults belong to
    // the fresh base-format path, while a snapshot may already contain an
    // intentional `\pdfpagewidth=0` assignment from the document preamble.
    let resumed_snapshot = restored_engine.is_some();
    let mut f = match restored_engine {
        Some(engine) => {
            metrics.format_source = if document_boundary_resumed {
                "document-hot-clone"
            } else {
                "snapshot"
            };
            engine
        }
        None => match format_arg.as_deref() {
            Some(p) => {
                let restore_started = Instant::now();
                let retain_decoded = matches!(
                    front_door,
                    FrontDoor::Build | FrontDoor::CacheWarm | FrontDoor::Watch
                );
                let (restored, decoded, source) = restore_base_format(
                    p,
                    retain_decoded,
                    format_identity.as_deref(),
                )
                .unwrap_or_else(|error| {
                    eprintln!("aitex: {error}");
                    std::process::exit(1);
                });
                metrics.format_restore = restore_started.elapsed();
                metrics.format_decode_count = u8::from(decoded);
                metrics.format_source = source;
                restored
            }
            None => {
                metrics.format_source = "initex";
                Fast::new()
            }
        },
    };
    if unicode_mode && !document_boundary_resumed {
        // The preloaded pdfLaTeX format leaves 128..255 active for byte-wise
        // `inputenc`. Native input has already been decoded as UTF-8 scalars,
        // so restore Unicode catcodes before the first source token is read.
        f.enable_native_utf8_input();
    }
    let unicode_host = unicode_options.as_ref().map(|options| {
        Rc::new(RefCell::new(aitex_unicode::UnicodeHost::system_with_dirs(
            options.font_directories.iter(),
        )))
    });
    if let Some(host) = unicode_host.as_ref() {
        let host = Rc::clone(host);
        f.set_native_text_shaper(Box::new(move |request| {
            host.borrow_mut().shape(request)
        }));
    }
    if !document_boundary_resumed
        && let Some(options) = unicode_options.as_ref()
    {
        f.set_native_text_defaults(
            options.family.clone(),
            options.language.clone(),
            options.features.clone(),
            options.variations.clone(),
            options.direction.clone(),
            options.family.is_some(),
        );
    }
    let setup_started = Instant::now();
    if tex82_flag() && !document_boundary_resumed {
        f.set_tex82_compat();
    }
    // `-file-line-error`, as every TeX spells it. `aitex-harness` already
    // passes it to pdfTeX (docs/history/PLAN.md §5); it gets the same shape back from here.
    f.file_line_error = file_line_error_flag();
    f.set_halt_on_error(halt_on_error_flag());
    // The final-artifact mode is host state and therefore is not serialized in
    // a project/document-boundary snapshot. Reattach it on every invocation.
    f.force_dvi_output_mode(pdf_driver_mode);
    if !resumed_snapshot {
        // Our standard format is dumped directly from latex.ltx. Reattach the
        // observable pdftexconfig.tex values that TeX Live carries in both its
        // latex and pdflatex formats before any document source runs.
        f.initialize_standard_latex_format_defaults();
    }
    if pdf_driver_mode && !resumed_snapshot {
        // TeX Live builds pdflatex.fmt after reading pdftexconfig.tex, which
        // contributes the ordinary Letter device dimensions. aiTeX shares its
        // DVI-built format between output modes, so supply those device-only
        // defaults once before the document can override them.
        f.initialize_pdf_output_defaults();
    }
    // §58's margin, read under kpathsea's own name. The engine needs it as
    // well as the printer: §1279 measures a `\message` against it to decide
    // whether the text joins the line already in progress or starts a new one.
    if !document_boundary_resumed {
        f.set_max_print_line(max_print_line);
    }
    let init = t0.elapsed();
    let t = Instant::now();
    // `\jobname` is the input file's base name, as in every TeX: LaTeX opens
    // `\jobname.aux` with it.
    if !document_boundary_resumed {
        f.set_job_name(&job_name);
        f.set_clock(clock.0, clock.1, clock.2, clock.3);
        f.set_pdf_creation_date(pdf_creation_date(clock));
    }
    let read = t.elapsed();
    if std::env::var("AITEX_TRACE_DEFS").is_ok() {
        f.trace_defs_on();
    }
    // AITEX_TRACE_PAGE=1: every exercise of the page builder — what it fired,
    // with which \outputpenalty, and what it still holds. A wrong page decision
    // costs a whole sheet of paper and shows up nowhere else.
    if std::env::var("AITEX_TRACE_PAGE").is_ok() {
        f.dbg_page_on = true;
    }
    // AITEX_TRACING_PAGES=1 — TeX's own `\tracingpages` lines, so the two
    // engines' page decisions can be diffed directly.
    if std::env::var("AITEX_TRACING_PAGES").is_ok() {
        f.ts.page.trace_forced = true;
        f.ts.page.trace_nodes = std::env::var("AITEX_TRACING_NODES").is_ok();
    }
    // AITEX_INS_DEBUG=1 — the quantities §1010 computes a split width from,
    // for when a `% split` line does not agree with TeX's. Independent of
    // `AITEX_TRACING_PAGES`, so a document's own `\tracingpages` still decides
    // *when* the trace runs and the two logs stay comparable.
    if std::env::var("AITEX_INS_DEBUG").is_ok() {
        f.ts.page.trace_ins_debug = true;
    }
    if std::env::var("AITEX_HOT_CS").is_ok() {
        f.trace_hot_on();
    }
    if let Ok(v) = std::env::var("AITEX_STEP_TRACE") {
        if let Some((lo, hi)) = v.split_once(',') {
            if let (Ok(lo), Ok(hi)) = (lo.parse::<u64>(), hi.parse::<u64>()) {
                f.dbg_trace_win = (lo, hi);
            }
        }
    }
    // AITEX_TRACE_CAP=N: how many trace entries to keep (default 4000). A whole
    // LaTeX document is a few hundred thousand.
    // AITEX_GROUP_TRACE=1: every group open/close, interleaved with the cs
    // trace, so a lost group can be read off against what was being expanded.
    f.dbg_sel = std::env::var("AITEX_DBG_SEL").is_ok();
    if std::env::var("AITEX_GROUP_TRACE").is_ok() {
        f.dbg_group = true;
    }
    // AITEX_ALIGN_TRACE=1: every tab mark and every \cr an open alignment sees,
    // with the align_state §342 judged it at. The near misses are the ones worth
    // having — a \cr at align_state=1 silently stops being a row boundary.
    if let Ok(v) = std::env::var("AITEX_ALIGN_TRACE") {
        f.dbg_align = true;
        f.dbg_align_braces = v == "braces";
    }
    if let Ok(v) = std::env::var("AITEX_TRACE_CAP") {
        if let Ok(n) = v.parse::<usize>() {
            f.dbg_trace_cap = n;
        }
    }
    // AITEX_RUNAWAY=N: snapshot the input stack once a cs has expanded N times in a
    // row — the only way to see who fed a self-expanding quark, since the spin never
    // leaves get_x_token and LTO inlines its caller out of any backtrace.
    if let Ok(v) = std::env::var("AITEX_RUNAWAY") {
        if let Ok(n) = v.parse::<u64>() {
            f.dbg_runaway_n = n;
        }
    }
    // AITEX_FIRST_OUT=N: the step at which each of the first N non-space tokens was
    // typeset. Loading a format must produce nothing, so each is a defect, and the
    // step is what AITEX_TOK_TRACE needs to be aimed at.
    if let Ok(v) = std::env::var("AITEX_FIRST_OUT") {
        if let Ok(n) = v.parse::<usize>() {
            f.set_first_out_cap(n);
        }
    }
    // AITEX_ARG_CAP=cap[,fromstep]
    if let Ok(v) = std::env::var("AITEX_TOK_TRACE") {
        if let Some((lo, hi)) = v.split_once(',') {
            if let (Ok(lo), Ok(hi)) = (lo.parse::<u64>(), hi.parse::<u64>()) {
                f.dbg_tok_win = (lo, hi);
            }
        }
    }
    if let Ok(v) = std::env::var("AITEX_CATCODE_WATCH") {
        if let Ok(n) = v.parse::<i64>() {
            f.dbg_catcode_watch = n;
        }
    }
    if let Ok(v) = std::env::var("AITEX_ARENA_WATCH") {
        if let Ok(n) = v.parse::<i64>() {
            f.dbg_arena_watch = n;
        }
    }
    if let Ok(v) = std::env::var("AITEX_STACK_AT") {
        if let Ok(n) = v.parse::<u64>() {
            f.dbg_stack_at = n;
        }
    }
    if let Ok(v) = std::env::var("AITEX_ARG_CAP") {
        let (cap, from) = match v.split_once(',') {
            Some((c, s)) => (c, s.parse::<u64>().unwrap_or(0)),
            None => (v.as_str(), 0),
        };
        if let Ok(n) = cap.parse::<u32>() {
            f.dbg_arg_cap = n;
            f.dbg_arg_from = from;
        }
    }
    if let Ok(v) = std::env::var("AITEX_MAX_COMMANDS") {
        if let Ok(n) = v.parse::<u64>() {
            f.max_commands = n;
        }
    }
    // Keep an expansion-only spin finite, but leave enough room for genuinely
    // long books. The old 200M probe-era default stopped a healthy 209-page
    // LaTeX document, and the later 2B ceiling stopped a healthy 1,965-page
    // handbook only 35 pages from its end. CPU and wall limits remain the
    // outer backstop for unattended corpus work.
    f.set_max_steps(
        std::env::var("AITEX_MAX_STEPS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(8_000_000_000),
    );
    // \input file loader: resolve names via kpsewhich (cached), read the file. This is
    // the seam that lets latex.ltx pull in expl3.ltx and the other kernel sub-files.
    // Enabled unless AITEX_NO_INPUT is set (keeps the isolation probes hermetic).
    // The cache is shared with the *writer* below, and a write drops the entry:
    // LaTeX writes `\jobname.aux`, closes it and reads it back **in the same
    // run** — that re-read is what turns `\@writefile` loose and fills the
    // `.toc`. A cached "no such file" from the read at `\begin{document}` made
    // every table of contents come out empty.
    // The resolved path travels with the contents: §537 echoes it.
    #[allow(clippy::type_complexity)]
    let cache: Rc<RefCell<HashMap<String, Option<(String, String)>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    // Every logical lookup, its resolved path (including absence), and the
    // content digest. A class hit seeds the project manifest with its own
    // transitive dependencies so the more specific project snapshot cannot
    // bypass class/package invalidation on the next run.
    let read_log: Rc<RefCell<Vec<CacheDependency>>> =
        Rc::new(RefCell::new(restored_dependencies));
    let recent_graphic_asset = Rc::new(RefCell::new(document_boundary_recent_graphic_asset));
    f.set_clock_dependency_tracking(track_dependencies);
    if std::env::var_os("AITEX_TRACE_CLOCK_DEPENDENCIES").is_some() {
        f.set_clock_dependency_trace(true);
    }
    if track_dependencies {
        f.set_clock_memoizer_validator(Box::new(trusted_clock_memoizer));
    }
    if std::env::var("AITEX_NO_INPUT").is_err() {
        // dvipdfmx asks for `stem.xbb` even when the author explicitly named
        // `stem.png`. If `stem.pdf` also exists, the sidecar name alone is
        // ambiguous. The immediately preceding graphics existence probe is
        // the selected asset; include it in the synthetic-sidecar cache key.
        let cache = Rc::clone(&cache);
        let rl = Rc::clone(&read_log);
        let recent = Rc::clone(&recent_graphic_asset);
        f.set_file_loader(Box::new(move |name: &str| -> Option<(String, String)> {
            if unicode_mode
                && let Some(body) = unicode_compat_file(name)
            {
                return Some((format!("./{}", name.trim_start_matches("./")), body.to_string()));
            }
            let is_xbb = name.ends_with(".xbb");
            let plain_key = name.to_string();
            if !is_xbb
                && has_graphic_extension(name)
                && let Some(Some((path, _))) = cache.borrow().get(&plain_key)
            {
                *recent.borrow_mut() = Some(path.trim_start_matches("./").to_string());
            }
            let cache_key = if is_xbb {
                format!(
                    "{name}\0{}",
                    recent.borrow().as_deref().unwrap_or("")
                )
            } else {
                plain_key
            };
            if let Some(hit) = cache.borrow().get(&cache_key) {
                return hit.clone();
            }
            let path = resolve_tex_path_hot(name, ".tex");
            if !is_xbb
                && has_graphic_extension(name)
                && let Some(path) = path.as_deref()
            {
                *recent.borrow_mut() = Some(path.to_string());
            }
            // A read that found nothing is part of the state too: LaTeX asks for
            // `\jobname.aux` at `\begin{document}` and it does not exist on the
            // first run. Recording the *absence* is what makes a later run —
            // where it does exist — invalidate the snapshot instead of reusing
            // one that read a file that was not there.
            let synthesized = path
                .is_none()
                .then(|| {
                    let selected = recent.borrow();
                    synthetic_xbb_hot(name, selected.as_deref())
                })
                .flatten();
            if track_dependencies {
                let dependency = match (&path, &synthesized) {
                    (Some(path), _) => CacheDependency::from_resolution(name, Some(path)),
                    // Synthetic xbb data is derived entirely from the selected
                    // asset, so that asset is the dependency to revalidate.
                    (None, Some((asset, _))) => {
                        CacheDependency::from_resolution(asset, Some(asset))
                    }
                    (None, None) => CacheDependency::from_resolution(name, None),
                };
                rl.borrow_mut().push(dependency);
            }
            // The engine needs the path as well as the contents: §537 echoes
            // it, and only the resolver knows what it was.
            let resolved = path
                .and_then(|p| {
                    read_tex_text(&p, unicode_mode).ok().map(|body| {
                        let shown = match std::path::Path::new(&p).is_absolute() {
                            true => p.clone(),
                            false => format!("./{}", p.trim_start_matches("./")),
                        };
                        (shown, body)
                    })
                })
                .or_else(|| {
                    synthesized.map(|(_, body)| {
                        let shown = match std::path::Path::new(name).is_absolute() {
                            true => name.to_string(),
                            false => format!("./{}", name.trim_start_matches("./")),
                        };
                        (shown, body)
                    })
                });
            cache.borrow_mut().insert(cache_key, resolved.clone());
            resolved
        }));
    }
    {
        // `\pdffilemoddate`. The date comes from `date -r`, for the same reason
        // §241's four parameters do: the standard library has no time zone, and
        // pdfTeX prints *local* time here — `D:20200304060607+01'00'` — unless
        // both `SOURCE_DATE_EPOCH` and `FORCE_SOURCE_DATE=1` are set, when it
        // prints UTC with a `Z`. Verified against `pdftex -ini` all four ways.
        let rl = Rc::clone(&read_log);
        f.set_file_moddate(Box::new(move |name: &str| -> Option<String> {
            let path = resolve_path_hot(name, "")?;
            if track_dependencies {
                rl.borrow_mut()
                    .push(CacheDependency::from_metadata_resolution(name, Some(&path)));
            }
            let mut cmd = std::process::Command::new("date");
            let forced = std::env::var("FORCE_SOURCE_DATE").as_deref() == Ok("1");
            if forced && std::env::var("SOURCE_DATE_EPOCH").is_ok() {
                cmd.arg("-u");
            }
            let out = cmd
                .arg("-r")
                .arg(&path)
                .arg("+%Y%m%d%H%M%S%z")
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let t = String::from_utf8_lossy(&out.stdout);
            let t = t.trim();
            // `date` writes the zone as `+0100`; PDF wants `+01'00'`, and `Z`
            // when there is no offset at all.
            let (stamp, zone) = t.split_at(t.len().checked_sub(5)?);
            let zone = match &zone[1..] {
                "0000" => String::from("Z"),
                z => format!("{}{}'{}'", &zone[..1], &z[..2], &z[2..]),
            };
            Some(format!("D:{stamp}{zone}"))
        }));
    }
    // `\openout`/`\write` reach the disk: LaTeX's `.aux`, `.toc` and friends. A
    // document with `\label` or `\tableofcontents` is only right on its second run
    // if the first one wrote these.
    // What LaTeX *wrote*, so it can be compared with what it read. A document
    // needs another pass exactly when a cross-reference file it consumed is not
    // the one it produced — that is the whole of the "Rerun to get
    // cross-references right" rule, and it is decidable here rather than
    // guessed. `latexmk` runs the whole job again to find out; we already know.
    let written: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let write_failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let cache = Rc::clone(&cache);
        let w = Rc::clone(&written);
        let failures = Rc::clone(&write_failures);
        f.set_file_writer(Box::new(move |name: &str, body: &str| {
            // What was on disk *before* this write is what the run consumed.
            // A resumed output prefix may write the same file more than once;
            // only the pre-run version decides whether the final version needs
            // another LaTeX pass.
            let mut written = w.borrow_mut();
            if !written.iter().any(|(written_name, _)| written_name == name) {
                let before = read_tex_text(name, unicode_mode).unwrap_or_default();
                written.push((name.to_string(), before));
            }
            drop(written);
            if let Err(error) = std::fs::write(name, tex_text_bytes(body, unicode_mode)) {
                let message = format!("cannot write {name}: {error}");
                eprintln!("aitex: {message}");
                failures.borrow_mut().push(message);
            }
            cache.borrow_mut().remove(name);
        }));
        if body_resumed_at.is_some() || document_boundary_resumed {
            f.replay_emitted_files();
        }
    }
    // Metrics are loaded *always*, because real TeX always has them.
    //
    // This used to be gated on `AITEX_DVI`/`-o`, on the theory that a format
    // load must not build nodes. The theory did not survive contact: a format
    // must be built with metrics too — `\textfont2=\tensy` records which font
    // family 2 is set in, and a font recorded without its TFM has no metrics to
    // restore, so every formula in every document built from that format came
    // out at the wrong size — and `--dump` was added to the condition. What was
    // left was a gate that no longer described anything.
    //
    // What it *did* do was silently break every probe run without `-o`. Fifty
    // `Font \tenrm=cmr10 not loadable` errors during plain.tex's preload, every
    // `\font` refused, every box fontless — so `\showbox` answered a question
    // nobody asked. That is a trap for this project's whole method, which is to
    // reduce a divergence to a minimal probe and diff it against `tex -ini`: the
    // probe silently measures a different engine, and the more carefully it is
    // minimized the more likely it is to drop the `-o` that kept it honest.
    // (It cost an hour here, chasing a `---` ligature that was never broken.)
    //
    // No gate is affected: every tool in `tools/` already passes `-o`,
    // `AITEX_DVI` or `--dump`, which is exactly why this survived so long.
    {
        use std::collections::HashMap;
        let mut cache: HashMap<String, Option<Vec<u8>>> = HashMap::new();
        let rl = Rc::clone(&read_log);
        f.set_tfm_loader(Box::new(move |name: &str| -> Option<Vec<u8>> {
            if let Some(hit) = cache.get(name) {
                return hit.clone();
            }
            let file = format!("{name}.tfm");
            let path = resolve_tfm_path_hot(name);
            if track_dependencies {
                rl.borrow_mut()
                    .push(CacheDependency::from_resolution(&file, path.as_deref()));
            }
            let resolved = path.and_then(|p| std::fs::read(p).ok());
            cache.insert(name.to_string(), resolved.clone());
            resolved
        }));
    }
    {
        use std::collections::HashMap;
        let mut cache: HashMap<String, Option<Vec<u8>>> = HashMap::new();
        let rl = Rc::clone(&read_log);
        f.set_vf_loader(Box::new(move |name: &str| -> Option<Vec<u8>> {
            if let Some(hit) = cache.get(name) {
                return hit.clone();
            }
            let file = format!("{name}.vf");
            let path = resolve_path_hot(&file, "");
            if track_dependencies {
                rl.borrow_mut()
                    .push(CacheDependency::from_resolution(&file, path.as_deref()));
            }
            let resolved = path.and_then(|path| std::fs::read(path).ok());
            cache.insert(name.to_string(), resolved.clone());
            resolved
        }));
    }
    // AITEX_TRACE_PAR=1 — what each line of each paragraph cost, in the shape
    // of TeX's own `\tracingparagraphs`: badness, the penalty at the break,
    // and the running demerits.
    if std::env::var("AITEX_TRACE_PAR").is_ok() {
        f.trace_paragraphs_on();
    }
    // Ship each page as the engine finishes it, so a long document costs one
    // page of memory rather than all of them. Off when `AITEX_VLIST` asks to see
    // the finished pages, since streaming does not keep them.
    let shipper = if dvi_out.is_some() && std::env::var("AITEX_VLIST").is_err() {
        let restored = body_resumed_shipper
            .take()
            .unwrap_or_else(|| {
                Shipper::new(
                    &path,
                    metrics_enabled,
                    direct_pdf_mode || unicode_mode,
                )
            });
        let sh = Rc::new(RefCell::new(restored));
        let s2 = Rc::clone(&sh);
        let sink = Box::new(move |page: &_, ts: &_, meta| {
            s2.borrow_mut().ship(page, ts, meta)
        });
        if body_checkpoint_builder.is_some() {
            f.set_checkpoint_page_sink(sink);
        } else {
            f.set_page_sink(sink);
        }
        Some(sh)
    } else {
        None
    };
    metrics.setup = setup_started.elapsed();
    let preparation = match (
        document_boundary_resumed,
        metrics.cache_state,
        metrics.class_cache_state,
    ) {
        (true, _, _) => "document boundary",
        (_, "hit" | "fork-hit", _) => "project snapshot",
        (_, _, "hit") => "class snapshot",
        (_, _, _) if format_arg.is_some() => "preloaded format",
        _ => "INITEX",
    };
    cli_phase(
        "Prepare",
        preparation,
        t0.elapsed().saturating_sub(metrics.source_read),
    );
    let sampler = start_sampler();
    let t = Instant::now();
    let mut body_reconverged = false;
    // The layered path is format -> class -> project preamble. Project hits
    // execute only the body. On a project miss, a class hit executes packages
    // and custom definitions before publishing the project snapshot. Every
    // dump happens between top-level run calls, never inside recursive TeX.
    if project_snapshot_hit {
        let (_, body) = split_preamble(&src).expect("project hit requires a preamble");
        let body_started = Instant::now();
        if let Some(builder) = body_checkpoint_builder.as_mut() {
            body_reconverged = run_body_with_checkpoints(
                &mut f,
                body,
                builder,
                body_resumed_at.take(),
                shipper.as_ref().expect("incremental output requires a shipper"),
                &read_log,
                body_capture_limit,
            );
        } else {
            f.run(body);
        }
        metrics.tex_body = body_started.elapsed();
    } else if cache_root.is_some()
        && let Some((preamble, body)) = split_preamble(&src)
    {
        let preamble_started = Instant::now();
        if class_cache_enabled
            && let Some((class_prefix, after_class)) = split_documentclass(preamble)
        {
            if !class_snapshot_hit {
                let class_started = Instant::now();
                // LaTeX's class loader finishes with a token lookahead. EOF is
                // not a token, so ending the synthetic prefix directly after
                // the closing brace strands that scanner. A top-level \relax
                // closes the lookahead and executes as a semantic no-op; the
                // zero-depth check below is still the authority for whether
                // this run reached a serializable boundary.
                let mut class_source = String::with_capacity(class_prefix.len() + 8);
                class_source.push_str(class_prefix);
                class_source.push_str("\\relax\n");
                f.run(&class_source);
                metrics.tex_class = class_started.elapsed();
                let class_cache_threshold = class_cache_minimum_duration();
                let selected_class_cache = if f.job_name_was_read() {
                    pending_job_class.as_ref()
                } else {
                    pending_class.as_ref()
                };
                if !f.halted_on_error()
                    && f.error_count() == 0
                    && f.depths() == (0, 0)
                    && write_failures.borrow().is_empty()
                    && let Some(pending) = selected_class_cache
                    && (force_class_cache_warm() || metrics.tex_class >= class_cache_threshold)
                {
                    let snapshot_started = Instant::now();
                    match f.dump() {
                        Ok(bytes) => {
                            metrics.snapshot_bytes = bytes.len();
                            match write_cache(
                                pending,
                                &bytes,
                                &read_log.borrow(),
                                clock_dependency(clock, f.clock_reads()),
                            ) {
                                Ok(true) => cli_detail!(
                                    "class snapshot written ({:.1} MB)",
                                    bytes.len() as f64 / 1e6
                                ),
                                Ok(false) => {
                                    cli_detail!("aitex: class snapshot writer already active")
                                }
                                Err(error) => {
                                    eprintln!("aitex: cannot cache class snapshot: {error}")
                                }
                            }
                        }
                        Err(error) => eprintln!("aitex: cannot dump class snapshot: {error}"),
                    }
                    metrics.class_snapshot_write = snapshot_started.elapsed();
                } else if selected_class_cache.is_some()
                    && !f.halted_on_error()
                    && f.error_count() == 0
                    && metrics.tex_class < class_cache_threshold
                {
                    cli_detail!(
                        "class snapshot skipped ({:.1} ms load is below {} ms admission threshold)",
                        metrics.tex_class.as_secs_f64() * 1e3,
                        class_cache_threshold.as_millis(),
                    );
                }
            }
            if !f.halted_on_error() {
                f.run(after_class);
            }
        } else {
            f.run(preamble);
        }
        metrics.tex_preamble = preamble_started.elapsed();
        // A failed preamble is not reusable and halt-on-error must keep the
        // document body unobserved.
        if !f.halted_on_error() {
            if f.error_count() == 0
                && f.depths() == (0, 0)
                && write_failures.borrow().is_empty()
                && let Some(pending) = pending_project.as_ref()
            {
                let snapshot_started = Instant::now();
                match f.dump() {
                    Ok(bytes) => {
                        metrics.snapshot_bytes = bytes.len();
                        match write_cache(
                            pending,
                            &bytes,
                            &read_log.borrow(),
                            clock_dependency(clock, restored_clock_reads | f.clock_reads()),
                        ) {
                            Ok(true) => {
                                project_snapshot_ready = true;
                                cli_detail!(
                                    "project snapshot written ({:.1} MB)",
                                    bytes.len() as f64 / 1e6
                                );
                            }
                            Ok(false) => {
                                cli_detail!("aitex: project snapshot writer already active")
                            }
                            Err(error) => {
                                eprintln!("aitex: cannot cache project snapshot: {error}")
                            }
                        }
                    }
                    Err(error) => eprintln!("aitex: cannot dump project snapshot: {error}"),
                }
                metrics.snapshot_write = snapshot_started.elapsed();
            }
            let body_started = Instant::now();
            if let Some(builder) = body_checkpoint_builder.as_mut() {
                body_reconverged = run_body_with_checkpoints(
                    &mut f,
                    body,
                    builder,
                    None,
                    shipper.as_ref().expect("incremental output requires a shipper"),
                    &read_log,
                    None,
                );
            } else {
                f.run(body);
            }
            metrics.tex_body = body_started.elapsed();
        }
    } else if document_boundary_resumed {
        let body_started = Instant::now();
        f.main_loop();
        metrics.tex_body = body_started.elapsed();
    } else if let Some(key) = document_boundary_key.as_ref() {
        let preamble_started = Instant::now();
        match f.run_until_document_boundary(&src) {
            Ok(Some(site)) => {
                metrics.tex_preamble = preamble_started.elapsed();
                if !f.halted_on_error()
                    && f.error_count() == 0
                    && write_failures.borrow().is_empty()
                {
                    let dependencies = normalized_dependencies(&read_log.borrow());
                    retain_hot_document_boundary(
                        key.clone(),
                        &f,
                        dependencies,
                        clock,
                        f.clock_reads(),
                        recent_graphic_asset.borrow().clone(),
                    );
                    cli_detail!(
                        "document boundary retained (source byte {}, {} prefix dependencies)",
                        site.byte_offset,
                        read_log.borrow().len(),
                    );
                } else {
                    HOT_DOCUMENT_BOUNDARY.with(|slot| *slot.borrow_mut() = None);
                    cli_detail!("document boundary discarded (preamble did not complete cleanly)");
                }
                let body_started = Instant::now();
                f.main_loop();
                metrics.tex_body = body_started.elapsed();
            }
            Ok(None) => {
                metrics.tex_body = preamble_started.elapsed();
                HOT_DOCUMENT_BOUNDARY.with(|slot| *slot.borrow_mut() = None);
                cli_detail!("document boundary unavailable (canonical macro was not observed safely)");
            }
            Err(error) => {
                HOT_DOCUMENT_BOUNDARY.with(|slot| *slot.borrow_mut() = None);
                cli_detail!("document boundary unavailable ({error}); using a full pass");
                let body_started = Instant::now();
                f.run(&src);
                metrics.tex_body = body_started.elapsed();
            }
        }
    } else {
        let body_started = Instant::now();
        f.run(&src);
        metrics.tex_body = body_started.elapsed();
    }
    if body_reconverged {
        let restored = body_checkpoint_builder
            .as_ref()
            .is_some_and(BodyCheckpointBuilder::restore_reconverged_outputs);
        if restored {
            let index = body_checkpoint_builder
                .take()
                .and_then(BodyCheckpointBuilder::finish_reconverged)
                .expect("a proven reconvergence retains its prior index");
            let entries = index.entries.len();
            let dependencies = index.dependencies.clone();
            HOT_BODY_CHECKPOINTS.with(|slot| *slot.borrow_mut() = Some(index));
            cli_detail!("semantic reconvergence hit (complete state, DVI prefix, and downstream inputs match)");
            cli_detail!("body checkpoints retained ({entries} proven prefix checkpoints)");
            cli_detail!("converged: no rerun needed");
            cli_phase("Typeset", "reconverged checkpoint", metrics.tex_body);
            cli_finish_pass(&front_door, dvi_out.as_deref(), t0.elapsed(), false);
            if metrics_enabled {
                emit_run_metrics(&metrics, t0.elapsed());
            }
            return RunOutcome {
                project_snapshot_expected,
                project_snapshot_ready: true,
                rerun_needed: false,
                dependencies,
                written_files: Vec::new(),
                diagnostics: Vec::new(),
            };
        }
        // A concurrent external write can invalidate the saved artifacts after
        // selection. The paused engine is still complete and resumes safely.
        f.main_loop();
    }
    let mut halted = f.halted_on_error();
    // `--dump <file>`: write the engine out, so the next run can skip what this
    // one just did. Written *before* `finish_page`, since a format is dumped at
    // the end of a run that typesets nothing.
    // §1327: a job that ended with `\dump` writes its format, named after the
    // job as TeX names it. This is how `trip.tex` is meant to be run — pass one
    // dumps `trip.fmt` and halts, pass two runs against it.
    if !halted && f.dump_requested() {
        if std::env::var_os("AITEX_BUILD_LATEX_FORMAT").is_some() {
            f.mark_standard_latex_format();
        }
        let name = format!("{}.fmt", f.job_name());
        match f.dump() {
            Ok(bytes) => match std::fs::write(&name, &bytes) {
                Ok(()) => {
                    f.report_format_dump(&name);
                    eprintln!("wrote {name} — {:.1} MB", bytes.len() as f64 / 1e6);
                    if let Some(path) = std::env::var_os("AITEX_FORMAT_DEPENDENCIES")
                        && let Err(error) = write_format_dependency_log(
                            std::path::Path::new(&path),
                            &read_log.borrow(),
                        )
                    {
                        let message = format!("cannot record format dependencies: {error}");
                        eprintln!("aitex: {message}");
                        write_failures.borrow_mut().push(message);
                    }
                }
                Err(error) => {
                    let message = format!("cannot write {name}: {error}");
                    eprintln!("aitex: {message}");
                    write_failures.borrow_mut().push(message);
                }
            },
            Err(error) => {
                let message = format!("cannot create requested format {name}: {error}");
                eprintln!("aitex: {message}");
                write_failures.borrow_mut().push(message);
            }
        }
    }
    if !halted && let Some(path) = arg_after("--dump") {
        let t = Instant::now();
        match f.dump() {
            Ok(bytes) => match std::fs::write(&path, &bytes) {
                Ok(()) => eprintln!(
                    "dumped {path} — {:.1} MB in {:.1}ms",
                    bytes.len() as f64 / 1e6,
                    t.elapsed().as_nanos() as f64 / 1e6
                ),
                Err(error) => {
                    let message = format!("cannot write {path}: {error}");
                    eprintln!("aitex: {message}");
                    write_failures.borrow_mut().push(message);
                }
            },
            Err(error) => {
                let message = format!("cannot create requested dump {path}: {error}");
                eprintln!("aitex: {message}");
                write_failures.borrow_mut().push(message);
            }
        }
    }
    // Errors, in TeX's shape: `! <message>.` then `l.<n> <context>`. A user who
    // knows TeX recognises it; a user who does not can at least see the file
    // and the line, which is the whole point and which this engine reported
    // nowhere until now.
    let mut failed = f.error_count() > 0 || !write_failures.borrow().is_empty();

    // A build tool has to be able to tell. Exiting 0 on a document full of
    // undefined control sequences is how a broken PDF reaches a publisher.
    if failed {
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
    }
    if !f.par_trace.is_empty() {
        eprintln!("--- paragraphs ---");
        for l in &f.par_trace {
            eprintln!("{l}");
        }
    }
    let output_started = Instant::now();
    let mut pending_pdf_conversion = None;
    if !halted && let Some(out) = &dvi_out {
        f.finish_page();
        // `finish_page` runs paragraph breaking and the output routine. Errors
        // and fatal budgets raised there are just as real as errors read from
        // the source: sample them before a partial artifact can be cached or a
        // successful status can escape to the caller.
        halted = f.halted_on_error();
        if f.error_count() > 0 || halted {
            failed = true;
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        }
        // `\tracingpages` AFTER `finish_page`, not before it. §1054's `\end`
        // contributes `\hbox to\hsize{} \vfill \penalty-'10000000000` and lets
        // the page builder run once more — which is where the last page's
        // breakpoints are weighed, and on a short document where *every* page's
        // are. Printing the log first threw all of those lines away: they are
        // appended to a buffer that has already been written out.
        //
        // This is the same fault as the transcript flush, which used to happen
        // straight after `f.run` and truncated whatever the closing `\output`
        // said. Anything the engine can still append to has to be printed after
        // the last thing that appends to it.
        if !f.ts.page.log.is_empty() {
            eprintln!("--- pages ---");
            for l in &f.ts.page.log {
                match l {
                    aitex_engine::page::PageLog::Text(t) => eprintln!("{t}"),
                    other => eprintln!("{other:?}"),
                }
            }
        }
        if std::env::var("AITEX_VLIST").is_ok() {
            eprintln!(
                "--- open boxes: {} (vlist {}, page {}) ---",
                f.ts.open.len(),
                f.ts.vlist.len(),
                f.ts.page.page.len()
            );
            for (i, b) in f.ts.open.iter().enumerate() {
                eprintln!(
                    "  open[{i}]: {:?} to={:?} nodes={} hlist={} in_par={} back_to={:?}",
                    b.kind,
                    b.to,
                    b.list.len(),
                    b.hlist.len(),
                    b.in_paragraph,
                    b.saved_mode
                );
            }
            use aitex_engine::node::Node;
            use aitex_engine::scaled::print_scaled as pt;
            eprintln!("--- pages ---");
            fn dump(list: &[Node], depth: usize, max_depth: usize) {
                use aitex_engine::scaled::print_scaled as pt;
                let pad = "  ".repeat(depth);
                for n in list {
                    match n {
                        Node::Glue(g) => eprintln!(
                            "{pad}glue w={} s={} sh={}",
                            pt(g.width),
                            pt(g.stretch),
                            pt(g.shrink)
                        ),
                        Node::Kern(k, sub) => eprintln!("{pad}kern {} sub={sub}", pt(*k)),
                        Node::Penalty(p) => eprintln!("{pad}penalty {p}"),
                        Node::Char { code, font, .. } => {
                            eprintln!("{pad}char {code} font={font}")
                        }
                        Node::Whatsit(text) => eprintln!("{pad}special {text:?}"),
                        Node::Box(b) => {
                            eprintln!(
                                "{pad}box {:?} w={} h={} d={} ({} nodes)",
                                b.kind,
                                pt(b.width),
                                pt(b.height),
                                pt(b.depth),
                                b.list.len()
                            );
                            if depth < max_depth {
                                dump(&b.list, depth + 1, max_depth);
                            }
                        }
                        other => eprintln!("{pad}{:?}", core::mem::discriminant(other)),
                    }
                }
            }
            if std::env::var("AITEX_VLIST_DEEP").is_ok() {
                for p in f.ts.pages.iter() {
                    dump(&p.list, 0, usize::MAX);
                }
            }
            for (i, p) in f.ts.pages.iter().enumerate() {
                eprintln!(
                    "page{i}: {}x({}+{})",
                    pt(p.width),
                    pt(p.height),
                    pt(p.depth)
                );
                for n in &p.list {
                    match n {
                        Node::Box(b) => eprintln!(
                            "  box w={} h={} d={} n={} {:?} set={}/{}",
                            pt(b.width),
                            pt(b.height),
                            pt(b.depth),
                            b.list.len(),
                            b.glue_sign,
                            b.set_num,
                            b.set_den
                        ),
                        Node::Glue(g) => eprintln!("  glue {}", pt(g.width)),
                        other => eprintln!("  {other:?}"),
                    }
                }
            }
        }
        if !halted {
            match &shipper {
                Some(sh) => {
                let (wrote, page_rotations, positioned, direct_document, private_dvi) = {
                    let mut shipper = sh.borrow_mut();
                    let wrote = shipper.finish(out);
                    let page_document = (wrote && (direct_pdf_mode || unicode_mode)).then(|| {
                        shipper.take_page_document(
                            &f.ts,
                            f.dvi_magnification().unwrap_or(1000),
                        )
                    });
                    // The legacy hybrid entry point needs only native runs.
                    // Clone those few objects while classic page events move
                    // zero-copy into the direct document below.
                    let positioned = unicode_mode.then(|| aitex_page::Document {
                        pages: page_document
                            .as_ref()
                            .map(|document| {
                                document
                                    .pages
                                    .iter()
                                    .map(|page| aitex_page::Page {
                                        width_millibp: page.width_millibp,
                                        height_millibp: page.height_millibp,
                                        counts: page.counts,
                                        items: page
                                            .items
                                            .iter()
                                            .filter(|item| {
                                                matches!(item, aitex_page::Item::GlyphRun(_))
                                            })
                                            .cloned()
                                            .collect(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                    // A successful TeX run is allowed to ship no pages (for
                    // example a rebuttal source containing only declarations).
                    // In that case `finish` correctly reports no artifact and
                    // there is no direct document to move into the PDF writer.
                    let direct_document = direct_pdf_mode.then_some(page_document).flatten();
                    (
                        wrote,
                        shipper.page_rotations.clone(),
                        positioned,
                        direct_document,
                        shipper.take_finished_dvi(),
                    )
                };
                // PDF and DVI share the same shipout walk. The direct route
                // moves its canonical page events into the PDF driver here,
                // once the font table is complete. Private DVI remains only as
                // an in-memory compatibility and differential oracle.
                if wrote {
                    // Keep an opt-in copy of the in-memory oracle for
                    // diagnostics. A value of `1` chooses `<output>.dvi`; any
                    // other non-empty value is the destination path.
                    if let Ok(keep) = std::env::var("AITEX_KEEP_DVI")
                        && !keep.is_empty()
                    {
                        let keep = if keep == "1" {
                            format!("{out}.dvi")
                        } else {
                            keep
                        };
                        let result = private_dvi.as_ref().map_or_else(
                            || std::fs::copy(out, &keep).map(|_| ()),
                            |bytes| std::fs::write(&keep, bytes),
                        );
                        if let Err(e) = result {
                            eprintln!("aitex: cannot preserve private DVI as {keep}: {e}");
                        }
                    }
                    pending_pdf_conversion = out
                        .ends_with(".pdf")
                        .then(|| {
                            (
                                out.clone(),
                                page_rotations,
                                positioned,
                                direct_document,
                                private_dvi,
                            )
                        });
                }
            }
                None => {
                    write_dvi(&f, out, &path);
                    if out.ends_with(".pdf") && !f.ts.pages.is_empty() {
                        if let Ok(keep) = std::env::var("AITEX_KEEP_DVI")
                            && !keep.is_empty()
                        {
                            let keep = if keep == "1" {
                                format!("{out}.dvi")
                            } else {
                                keep
                            };
                            if let Err(error) = std::fs::copy(out, &keep) {
                                eprintln!(
                                    "aitex: cannot preserve private DVI as {keep}: {error}"
                                );
                            }
                        }
                        let page_rotations = f
                            .retained_page_metadata()
                            .iter()
                            .map(|meta| meta.pdf_page_rotation)
                            .collect();
                        pending_pdf_conversion =
                            Some((out.clone(), page_rotations, None, None, None));
                    }
                }
            }
        }
    }
    metrics.output_finalize = output_started.elapsed();
    let output_page_count = shipper
        .as_ref()
        .map(|shipper| shipper.borrow().pages)
        .unwrap_or(f.ts.pages.len());
    if !failed {
        let pages = format!(
            "{} page{}",
            output_page_count,
            if output_page_count == 1 { "" } else { "s" }
        );
        cli_phase(
            "Typeset",
            pages,
            metrics.tex_preamble + metrics.tex_body,
        );
        if dvi_out.as_deref().is_some_and(|output| output.ends_with(".dvi")) {
            cli_phase("Write", "DVI", metrics.output_finalize);
        }
    }
    // `tex.web` §1333 closes streams only after the final page has shipped.
    // A controlled-EOF job can leave deferred `\write` whatsits on the page
    // that the host's `finish_page` ships, so flushing before it loses those
    // writes or sends them to the terminal instead of their open stream.
    f.flush_write_streams();
    // Include final-page and close failures before admitting body or completed
    // output caches.
    if !write_failures.borrow().is_empty() {
        failed = true;
        EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
    }
    // Does this document need another pass? Only files that feed the *next*
    // run count — `.aux`, `.toc`, `.lof`, `.lot` — not the log or DVI. This
    // comparison must follow final-page shipout because non-immediate `\write`
    // whatsits execute there.
    let stale: Vec<String> = written
        .borrow()
        .iter()
        .filter(|(name, before)| {
            std::path::Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(is_fixpoint_extension)
                && read_tex_text(name, unicode_mode).unwrap_or_default() != *before
        })
        .map(|(name, _)| name.clone())
        .collect();
    let rerun_needed = !stale.is_empty();
    if !halted && write_failures.borrow().is_empty() {
        if stale.is_empty() {
            cli_detail!("converged: no rerun needed");
        } else {
            cli_detail!("rerun needed: {} changed", stale.join(", "));
        }
    }
    let pending_body_checkpoint = if let Some(builder) = body_checkpoint_builder.take() {
        if !halted && !failed && builder.entry_count() > 0 {
            let entries = builder.entry_count();
            let retained_bytes = builder.retained_bytes();
            let dependencies = normalized_dependencies(&read_log.borrow());
            let clock_dependency =
                clock_dependency(clock, restored_clock_reads | f.clock_reads());
            Some((
                builder,
                dependencies,
                clock_dependency,
                entries,
                retained_bytes,
            ))
        } else {
            HOT_BODY_CHECKPOINTS.with(|slot| *slot.borrow_mut() = None);
            if !halted && !failed {
                cli_detail!(
                    "body checkpoints unavailable ({})",
                    f.checkpoint_refusal_reason().unwrap_or("no safe boundary"),
                );
            }
            None
        }
    } else {
        None
    };
    // AITEX_MEM=1: where the engine's memory is at the end of the run. The
    // question this answers is "what grows with the document". Capture it
    // before releasing the engine for PDF conversion, then print the complete
    // process high-water mark afterward.
    let memory_report = std::env::var("AITEX_MEM")
        .is_ok()
        .then(|| f.memory_report());
    // The transcript: `\message`, the `\tracing*` narration, and — since
    // §82/§311 moved into the engine — the errors, all in the order they
    // happened. It is written last because the run keeps appending to it right
    // up to the final page: flushing it straight after `f.run` truncated
    // whatever the closing `\output` said, which cost a `\showlists` block.
    //
    // Everything of *ours* goes after it, on stderr. That ordering is the point:
    // our `(N errors)` summary used to flush before the errors it counts.
    let mut transcript_diagnostics = Vec::new();
    {
        use std::io::Write;
        // `tex.web` §58: every character printed advances `file_offset`, and
        // when it reaches `max_print_line` the line is broken then and there.
        //
        //     if file_offset=max_print_line then
        //       begin wlog_cr; file_offset:=0; end;
        //
        // So a transcript line is never wider than that, and — this is the part
        // that is easy to miss — a line whose length is *exactly* the limit has
        // already been ended by the wrap, so the `print_ln` that was going to
        // end it writes a blank line instead. §90's ending does that after every
        // error whose context happens to fill the line, which §317 makes common:
        // it pads the second line of a context to exactly `error_line`, which is
        // 79, which is also the default `max_print_line`.
        //
        // Read from the environment under kpathsea's own name, so a run set up
        // with `max_print_line=1000` — which every differential tool here does,
        // to keep TeX from wrapping lines it is being compared on — turns it off
        // for both engines at once.
        let mpl: usize = std::env::var("max_print_line")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n >= 20)
            .unwrap_or(79);
        // An entry is a *logical* line, ended the way §57's `print_nl` ends one
        // — only if there is anything on it. A `\n` inside an entry is §57's
        // `print_ln`, which ends the line whether or not it is empty, and an
        // empty entry is one of those on its own. That distinction is the whole
        // reason this is not a plain `writeln!` per entry: §90 ends every error
        // with two unconditional `print_ln`s, so a context line that the wrap
        // has already ended is followed by *two* blank lines and not one.
        let lines = render_transcript(&f.messages, mpl);
        let tail = LOG_TAIL.with(|t| t.borrow().clone());
        if !cli_compact_output() {
            let out = std::io::stdout();
            let mut out = out.lock();
            for l in &lines {
                let _ = writeln!(out, "{l}");
            }
            // A page-less job reaches §1333 only after §1327 has finished
            // writing the format inventory. Keep its terminal copy in the
            // traditional transcript, but not in the compact presentation.
            for l in tail.iter().filter(|l| l.as_str() == "No pages of output.") {
                let _ = writeln!(out, "{l}");
            }
            let _ = out.flush();
        }
        // §534/§1333: the same stream, plus the banner that opens a TeX log and
        // the `Output written on …` that closes one.
        for l in &f.dbg_sel_log {
            eprintln!("DBGSEL {l}");
        }
        let transcript_path = format!("{}.log", f.job_name());
        if let Err(error) = write_log(&f.job_name(), &path, &lines, &tail, unicode_mode) {
            eprintln!("aitex: cannot write {}.log: {error}", f.job_name());
            failed = true;
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        } else if f.error_count() == 0
            && let Some(report) =
                tex_log_error_report_at(std::path::Path::new(&transcript_path), &path)
        {
            // LaTeX macros can emit a conventional `! LaTeX Error` and then
            // recover without incrementing TeX's primitive error counter.
            // Detect it before deciding whether any checkpoint or completed
            // output is safe to persist.
            if let Some(diagnostic) = report.first_error() {
                let presentation = diagnostic.presentation();
                let location = diagnostic
                    .location()
                    .map(|span| format!(" · {span}"))
                    .unwrap_or_default();
                report_build_problem("TeX", format_args!("{}{location}", presentation.error));
                show_diagnostic_details(diagnostic);
            }
            transcript_diagnostics = report.diagnostics;
            failed = true;
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        }
        cli_show_diagnostics(
            f.errors(),
            f.error_count(),
            &format!("{}.log", f.job_name()),
            &path,
        );
    }
    {
        let n = f.error_count();
        let shown = f.errors().len() as u64;
        if n > shown {
            cli_detail!("({n} errors, first {shown} shown)");
        } else if n > 0 {
            cli_detail!("({n} error{})", if n == 1 { "" } else { "s" });
        }
    }
    if let Some(s) = &sampler {
        s.report();
    }
    if std::env::var("AITEX_TRACE_DEFS").is_ok() {
        eprintln!("--- last 30 definitions ---");
        for name in f.def_log_tail(30) {
            eprintln!("  def \\{name}");
        }
    }
    // AITEX_DEF_GREP=substr: every definition whose name contains `substr`, with its
    // ordinal. Answers "did this family ever get defined, and in what order" — which
    // `AITEX_TRACE_DEFS`' last-30 tail cannot. Needs AITEX_TRACE_DEFS to record.
    if let Ok(pat) = std::env::var("AITEX_DEF_GREP") {
        let all = f.def_log_tail(usize::MAX);
        eprintln!(
            "--- definitions matching {pat:?} ({} total defs) ---",
            all.len()
        );
        for (i, name) in all.iter().enumerate() {
            if name.contains(&pat) {
                eprintln!("  [{i}] def \\{name}");
            }
        }
    }
    if std::env::var("AITEX_HOT_CS").is_ok() {
        eprintln!("--- top 20 expanded control sequences ---");
        for (name, c) in f.hot_cs_top(20) {
            eprintln!("  {c:>10}  \\{name}");
        }
        eprintln!("--- top 20 \\csname names built ---");
        for (name, c) in f.csn_top(20) {
            eprintln!("  {c:>10}  [{name}]");
        }
    }
    if std::env::var("AITEX_STATE").is_ok() {
        eprintln!(
            "dbg: group_depth when _ became a letter = {}",
            f.dbg_us_depth
        );
        for (name, gd, cd) in &f.dbg_inputs {
            eprintln!("dbg: \\input {name} at group_depth={gd} cond_depth={cd}");
        }
    }
    if std::env::var("AITEX_STATE").is_ok() {
        eprintln!("--- engine state ---");
        eprintln!(
            "  escapechar={} endlinechar={}",
            f.escapechar_val(),
            f.named_of("endlinechar")
        );
        for (label, c) in [
            ("{", '{'),
            ("}", '}'),
            ("#", '#'),
            ("space", ' '),
            ("_", '_'),
            (":", ':'),
        ] {
            eprintln!("  catcode({label})={}", f.catcode_of(c));
        }
    }
    if std::env::var("AITEX_STEP_TRACE").is_ok() {
        eprintln!(
            "--- step-window cs trace ({} entries) ---",
            f.dbg_trace.len()
        );
        for (step, kind, name) in &f.dbg_trace {
            eprintln!("  [{step}] {kind} \\{name}");
        }
    }
    // AITEX_OUT=1: the produced token stream, rendered. Small isolation probes are read
    // by their output, so without this they can only be judged by "did it not hang".
    if std::env::var("AITEX_OUT").is_ok() {
        eprintln!(
            "--- out ({} tokens, first {} kept) ---\n{}",
            f.out_total(),
            f.out.len(),
            f.out_str()
        );
    }
    if std::env::var("AITEX_TOK_TRACE").is_ok() {
        eprintln!("--- token trace ({} tokens) ---", f.dbg_tok.len());
        for (step, t) in &f.dbg_tok {
            eprintln!("  [{step}] {t}");
        }
    }
    if std::env::var("AITEX_FILE_PROGRESS").is_ok() {
        eprintln!("--- file progress (pos/len) ---");
        for (i, pos, len) in f.file_progress() {
            let pct = if len > 0 {
                100.0 * pos as f64 / len as f64
            } else {
                100.0
            };
            let mark = if pos >= len { "" } else { "   <-- ABANDONED" };
            eprintln!("  file{i}: {pos}/{len} ({pct:.1}%){mark}");
        }
    }
    // AITEX_STATS=1: what the run cost the engine, in the units it allocates in.
    if std::env::var("AITEX_STATS").is_ok() {
        eprintln!("--- engine stats ---\n  {}", f.stats_report());
    }
    // The group stack and the typesetter's mode should agree about whether we
    // are in math; see the check that fills this. Silence is the expected
    // result, and the whole point of printing it is that it stops being so.
    if !f.dbg_drift.is_empty() {
        eprintln!("--- math group / mode drift ---");
        for l in &f.dbg_drift {
            eprintln!("  {l}");
        }
    }
    if !f.dbg_page.is_empty() {
        eprintln!("--- page builder ---");
        for l in &f.dbg_page {
            eprintln!("  {l}");
        }
    }
    for l in &f.dbg_catcode_log {
        eprintln!("catcode: {l}");
    }
    for l in &f.dbg_align_log {
        eprintln!("{l}");
    }
    for r in &f.dbg_runaway {
        eprintln!("--- runaway snapshot ---\n{r}");
    }
    if !f.dbg_first_out.is_empty() {
        eprintln!("--- first typeset tokens (token @ step) ---");
        for (d, step) in &f.dbg_first_out {
            eprintln!("  {step:>9}  {d}");
        }
    }
    if std::env::var("AITEX_FIRST_UNDEF").is_ok() {
        eprintln!("--- first undefined-cs invocations (name @ step) ---");
        for (name, step) in &f.dbg_first_undef {
            eprintln!("  {step:>9}  \\{name}");
        }
    }
    // The build loop's to-do list: top unimplemented primitives by hit count.
    for (name, c) in f.frontier(25) {
        cli_detail!("  {c:>7}  \\{name}");
    }
    // Definedness report: for latex.ltx, confirms the kernel actually installed its
    // macros (a clean undef=0 is necessary but not sufficient — it could also mean a
    // conditional skipped everything). Pass names as a 2nd arg, comma-separated.
    if let Ok(nm) = std::env::var("AITEX_DUMP_BODY") {
        for n in nm.split(',').filter(|s| !s.is_empty()) {
            eprintln!("BODY \\{n} = {}", f.dump_body(n));
        }
    }
    // Explicit developer probe. Keeping this as a named option means stray
    // positional arguments are usage errors instead of silently changing the
    // compiler's diagnostics and cacheability.
    if let Some(names) = probe_argument(&arguments) {
        for name in names.split(',').filter(|s| !s.is_empty()) {
            eprintln!(
                "  {} {} {:14}  \\{name}",
                if f.is_defined(name) { "DEF  " } else { "undef" },
                if f.is_protected_name(name) {
                    "PROT"
                } else {
                    "    "
                },
                f.meaning_kind(name),
            );
        }
    }

    // The engine and its input caches are intentionally gone before the PDF
    // object graph is assembled. Once the page stream and private oracle are
    // complete, the driver needs only font metadata and `\pdfglyphtounicode`;
    // retaining the macro arena, stacks, registers, and package source cache
    // made the two independent phases' peaks add together on image-heavy papers.
    let (gd, cd) = f.depths();
    let summary = (
        f.count_reg(0),
        f.out_total(),
        f.undefined_total(),
        f.frontier(usize::MAX).len(),
        f.inputs_loaded,
        f.steps(),
        f.hit_cap,
    );
    if std::env::var_os("AITEX_TRACE_CLOCK_DEPENDENCIES").is_some() {
        eprintln!(
            "aitex: clock dependency trace {:?}",
            f.clock_dependency_trace()
        );
    }
    let pdf_conversion = pending_pdf_conversion.map(
        |(out, page_rotations, positioned, direct_document, private_dvi)| {
            let glyph_to_unicode = f.glyph_to_unicode().clone();
            let pdf_map_files = f.pdf_map_files().to_vec();
            let pdf_map_lines = f.pdf_map_lines().to_vec();
            let fonts = core::mem::take(&mut f.ts.fonts);
        let font_names = core::mem::take(&mut f.ts.font_names);
        (
            out,
            page_rotations,
            fonts,
                font_names,
                glyph_to_unicode,
                pdf_map_files,
                pdf_map_lines,
                positioned,
            direct_document,
            private_dvi,
        )
    });
    let completed_output_paths = if pending_body_checkpoint.is_some()
        && !rerun_needed
        && completed_output_reuse_allowed()
    {
        dvi_out.as_ref().map(|primary| {
            let mut paths: Vec<_> = written
                .borrow()
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            paths.push(format!("{}.log", f.job_name()));
            paths.push(primary.clone());
            (primary.clone(), paths)
        })
    } else {
        None
    };
    // Persist only the primary DVI/PDF bytes. Auxiliary files remain ordinary
    // build artifacts and are exact manifest dependencies: a missing or edited
    // `.aux`, `.toc`, custom write stream, or transcript forces a real run that
    // recreates it. The primary is deliberately excluded so a cache hit can
    // restore it after `clean` without weakening the proof for everything else.
    let output_cache_candidate = if !halted
        && !failed
        && !rerun_needed
        && completed_output_reuse_allowed()
    {
        pending_output.take().and_then(|pending| {
            dvi_out.as_ref().map(|primary| {
                let mut side_artifacts: Vec<_> = written
                    .borrow()
                    .iter()
                    .map(|(name, _)| name.clone())
                    .filter(|name| name != primary)
                    .collect();
                side_artifacts.push(format!("{}.log", f.job_name()));
                side_artifacts.sort();
                side_artifacts.dedup();
                (
                    pending,
                    normalized_dependencies(&read_log.borrow()),
                    clock_dependency(clock, restored_clock_reads | f.clock_reads()),
                    primary.clone(),
                    side_artifacts,
                )
            })
        })
    } else {
        None
    };
    metrics.shipout = shipper
        .as_ref()
        .map(|value| value.borrow().elapsed)
        .unwrap_or_default();
    let written_files = written
        .borrow()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let mut diagnostics: Vec<_> = f
        .errors()
        .iter()
        .map(|diagnostic| structured_engine_diagnostic(diagnostic, &path))
        .collect();
    diagnostics.extend(transcript_diagnostics);
    let observe_dependencies = filesystem_watch_mode();
    let mut observed_dependencies = if observe_dependencies {
        normalized_dependencies(&read_log.borrow())
    } else {
        Vec::new()
    };
    drop(f);
    drop(shipper);
    drop(cache);
    drop(read_log);
    drop(written);
    drop(src);
    if let Some((
        out,
        page_rotations,
        fonts,
        font_names,
        glyph_to_unicode,
        pdf_map_files,
        pdf_map_lines,
        positioned,
        direct_document,
        private_dvi,
    )) =
        pdf_conversion
    {
        let pdf_started = Instant::now();
        let converted = convert_to_pdf(
            &out,
            &fonts,
            &font_names,
            &glyph_to_unicode,
            &pdf_map_files,
            &pdf_map_lines,
            &page_rotations,
            track_dependencies,
            &mut metrics.pdf,
            positioned.as_ref(),
            unicode_host.as_ref(),
            direct_document,
            private_dvi,
        );
        metrics.pdf.total = pdf_started.elapsed();
        if !converted {
            // Do not leave a previous successful PDF behind under the name of
            // this failed build.
            if !filesystem_watch_mode() {
                let _ = std::fs::remove_file(&out);
            }
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        } else if EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            let pages = format!(
                "PDF · {} page{}",
                output_page_count,
                if output_page_count == 1 { "" } else { "s" }
            );
            cli_phase("Render", pages, metrics.pdf.total);
        }
    }
    // A DVI run has no driver dependencies. PDF conversion records every font
    // map, encoding, font program, bitmap, and included asset it resolved; a
    // `None` result means that the driver could not prove the output reusable.
    let pdf_dependencies = if pdf_driver_mode {
        take_last_pdf_dependencies()
    } else {
        Some(Vec::new())
    };
    if observe_dependencies && let Some(dependencies) = &pdf_dependencies {
        observed_dependencies.extend(dependencies.iter().cloned());
        observed_dependencies = normalized_dependencies(&observed_dependencies);
    }
    if let Some((pending, mut dependencies, clock_dependency, primary, side_artifacts)) =
        output_cache_candidate
        && EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed) == 0
        && let Some(driver_dependencies) = &pdf_dependencies
    {
        dependencies.extend(driver_dependencies.iter().cloned());
        let mut side_artifacts_valid = true;
        for path in side_artifacts {
            if std::fs::File::open(&path).is_err() {
                eprintln!("aitex: persistent output cache skipped (artifact unreadable: {path})");
                side_artifacts_valid = false;
                break;
            }
            dependencies.push(CacheDependency::from_resolution(&path, Some(&path)));
        }
        if side_artifacts_valid {
            match read_output_artifact(&primary) {
                Ok(bytes) => match write_cache(
                    &pending,
                    &bytes,
                    &normalized_dependencies(&dependencies),
                    clock_dependency,
                ) {
                    Ok(true) => {
                        project_snapshot_ready = true;
                        cli_detail!(
                            "persistent output cache written ({:.1} MB)",
                            bytes.len() as f64 / 1e6
                        )
                    }
                    Ok(false) => {
                        cli_detail!("aitex: persistent output cache writer already active")
                    }
                    Err(error) => eprintln!("aitex: cannot cache completed output: {error}"),
                },
                Err(error) => eprintln!(
                    "aitex: persistent output cache skipped (cannot read {primary}: {error})"
                ),
            }
        }
    }
    if let Some((builder, mut dependencies, clock_dependency, entries, retained_bytes)) =
        pending_body_checkpoint
    {
        if EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            if let Some(pdf_dependencies) = &pdf_dependencies {
                dependencies.extend(pdf_dependencies.iter().cloned());
            }
            dependencies = normalized_dependencies(&dependencies);
            let completed_outputs = pdf_dependencies.as_ref().and_then(|_| {
                completed_output_paths
                    .and_then(|(primary, paths)| CompletedOutputs::capture(primary, paths))
            });
            HOT_BODY_CHECKPOINTS.with(|slot| {
                *slot.borrow_mut() = Some(builder.finish(
                    dependencies,
                    clock_dependency,
                    completed_outputs,
                ));
            });
            cli_detail!(
                "body checkpoints retained ({entries} checkpoints, {:.1} MB)",
                retained_bytes as f64 / 1e6,
            );
        } else {
            HOT_BODY_CHECKPOINTS.with(|slot| *slot.borrow_mut() = None);
        }
    }
    let run = t.elapsed();
    if let Some(report) = memory_report {
        use std::sync::atomic::Ordering::Relaxed;
        let total: usize = report.iter().map(|&(_, n)| n).sum();
        eprintln!(
            "--- memory: peak heap {:.1} MB, live at exit {:.1} MB, {:.1} MB in tracked engine buffers ---",
            PEAK.load(Relaxed) as f64 / 1e6,
            LIVE.load(Relaxed) as f64 / 1e6,
            total as f64 / 1e6,
        );
        for (name, n) in report {
            eprintln!("  {:>9.2} MB  {name}", n as f64 / 1e6);
        }
    }
    cli_detail!(
        "init={:.1}ms read={:.1}ms run={:.1}ms | count0={} out={} undef={} ({} distinct) group_depth={gd} cond_depth={cd} inputs={} steps={} cap={}",
        init.as_secs_f64() * 1e3,
        read.as_secs_f64() * 1e3,
        run.as_secs_f64() * 1e3,
        summary.0,
        summary.1,
        summary.2,
        summary.3,
        summary.4,
        summary.5,
        summary.6,
    );
    if EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        cli_finish_pass(
            &front_door,
            Some(&display_output),
            t0.elapsed(),
            rerun_needed,
        );
    } else {
        cli_fail_pass(&front_door, t0.elapsed());
    }
    if metrics_enabled {
        emit_run_metrics(&metrics, t0.elapsed());
    }
    RunOutcome {
        project_snapshot_expected,
        project_snapshot_ready,
        rerun_needed,
        dependencies: observed_dependencies,
        written_files,
        diagnostics,
    }
}
