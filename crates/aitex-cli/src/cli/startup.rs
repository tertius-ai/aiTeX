use std::io::IsTerminal as _;

#[derive(Clone, Copy)]
struct CliStyle {
    color: bool,
}

impl CliStyle {
    fn paint(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn bold(self, text: &str) -> String {
        self.paint("1", text)
    }

    fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }

    fn cyan(self, text: &str) -> String {
        self.paint("1;36", text)
    }

    fn violet(self, text: &str) -> String {
        self.paint("1;35", text)
    }

    fn green(self, text: &str) -> String {
        self.paint("1;32", text)
    }

    fn yellow(self, text: &str) -> String {
        self.paint("1;33", text)
    }

    fn red(self, text: &str) -> String {
        self.paint("1;31", text)
    }
}

fn cli_verbose() -> bool {
    std::env::var_os("AITEX_VERBOSE").is_some()
        || std::env::args().any(|argument| matches!(argument.as_str(), "-v" | "--verbose"))
}

/// The polished presentation is deliberately terminal-only. Captured output
/// remains the byte-for-byte TeX transcript used by the differential suite and
/// by tools that already consume aiTeX's stdout/stderr.
fn cli_interactive_output() -> bool {
    std::io::stdout().is_terminal() && std::io::stderr().is_terminal()
}

fn cli_compact_output() -> bool {
    (cli_interactive_output() && !cli_verbose())
        || std::env::var_os("AITEX_REPAIR_CHILD").is_some()
        || std::env::args().any(|argument| argument == "--json")
}

fn cli_present_output() -> bool {
    cli_interactive_output()
}

fn cli_style() -> CliStyle {
    let color = cli_interactive_output()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb");
    CliStyle { color }
}

fn startup_mode(arguments: &[String], front_door: &FrontDoor) -> String {
    let output = output_argument(arguments);
    let artifact = output
        .as_deref()
        .and_then(|path| std::path::Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .map_or("TeX", |extension| match extension {
            "pdf" => "PDF",
            "dvi" => "DVI",
            _ => "TeX",
        });
    match front_door {
        FrontDoor::Watch => format!("watch · {artifact}"),
        FrontDoor::ForkWatch => format!("fork watch · {artifact}"),
        FrontDoor::CacheWarm => format!("cache warm · {artifact}"),
        FrontDoor::Explain => format!("explain · {artifact}"),
        FrontDoor::Fix => format!("fix · {artifact}"),
        FrontDoor::Build => format!("build · {artifact}"),
        FrontDoor::Lint => format!("lint · {artifact}"),
        FrontDoor::Pass => format!("pass · {artifact}"),
        FrontDoor::Format => "format · LaTeX".to_string(),
        _ => artifact.to_string(),
    }
}

fn startup_text(arguments: &[String], front_door: &FrontDoor, style: CliStyle) -> String {
    let ai_top = style.violet("▄▀█ █");
    let tex_top = style.cyan("▀█▀ █▀▀ ▀▄▀");
    let ai_bottom = style.violet("█▀█ █");
    let tex_bottom = style.cyan(" █  ██▄ █ █");
    let name = style.bold("aiTeX");
    let version = env!("CARGO_PKG_VERSION");
    let mode = startup_mode(arguments, front_door);
    let subtitle = style.dim(&format!("v{version} · {mode} · modern TeX, built in Rust"));
    format!(
        "\n  {ai_top} {tex_top}\n  {ai_bottom} {tex_bottom}\n\n  {name} {subtitle}\n"
    )
}

fn show_startup(arguments: &[String], front_door: &FrontDoor) {
    if arguments.iter().any(|argument| argument == "--no-banner")
        || std::env::var_os("AITEX_NO_BANNER").is_some()
        || !cli_interactive_output()
    {
        return;
    }
    eprintln!("{}", startup_text(arguments, front_door, cli_style()));
}

fn human_duration(duration: std::time::Duration) -> String {
    let milliseconds = duration.as_secs_f64() * 1e3;
    if milliseconds < 1.0 {
        format!("{:.0} µs", duration.as_secs_f64() * 1e6)
    } else if milliseconds < 1_000.0 {
        format!("{milliseconds:.1} ms")
    } else {
        format!("{:.2} s", duration.as_secs_f64())
    }
}

fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn cli_phase(label: &str, detail: impl std::fmt::Display, duration: std::time::Duration) {
    // Repair builds run in a private subprocess whose stderr is captured for
    // diagnosis. Still emit the ordinary compact success phases there so the
    // parent `fix` command can replay them in its own polished UI.
    if !cli_present_output() && std::env::var_os("AITEX_REPAIR_CHILD").is_none() {
        return;
    }
    let style = cli_style();
    let mark = style.green("✓");
    let label = style.bold(&format!("{label:<9}"));
    let duration = style.dim(&human_duration(duration));
    eprintln!("  {mark} {label} {detail} {duration}");
}

fn cli_finish(output: Option<&str>, duration: std::time::Duration, rerun_needed: bool) {
    if !cli_present_output() {
        return;
    }
    let style = cli_style();
    let mark = if rerun_needed {
        style.yellow("◆")
    } else {
        style.green("◆")
    };
    let label = style.bold(&format!(
        "{:<9}",
        if rerun_needed { "Rerun" } else { "Complete" }
    ));
    let path = output.unwrap_or("transcript");
    let size = std::fs::metadata(path)
        .ok()
        .map(|metadata| format!(" · {}", human_bytes(metadata.len())))
        .unwrap_or_default();
    let note = if rerun_needed {
        " · auxiliary files changed"
    } else {
        ""
    };
    let duration = style.dim(&human_duration(duration));
    eprintln!("  {mark} {label} {path}{size}{note} {duration}\n");
}

/// A complete build owns the final summary. Its individual TeX passes still
/// report useful phases, but must not each claim that the document is done.
fn cli_pass_finish_visible(
    front_door: &FrontDoor,
    rerun_needed: bool,
    filesystem_watch: bool,
) -> bool {
    !matches!(
        front_door,
        FrontDoor::Build | FrontDoor::Lint | FrontDoor::Explain | FrontDoor::Fix
    ) && !(filesystem_watch && matches!(front_door, FrontDoor::Watch) && !rerun_needed)
}

fn cli_finish_pass(
    front_door: &FrontDoor,
    output: Option<&str>,
    duration: std::time::Duration,
    rerun_needed: bool,
) {
    if cli_pass_finish_visible(front_door, rerun_needed, filesystem_watch_mode()) {
        cli_finish(output, duration, rerun_needed);
    }
}

fn cli_fail(duration: std::time::Duration) {
    if !cli_present_output() {
        return;
    }
    let style = cli_style();
    eprintln!(
        "  {} {} {}\n",
        style.red("×"),
        style.bold("Failed"),
        style.dim(&human_duration(duration)),
    );
}

fn cli_fail_pass(front_door: &FrontDoor, duration: std::time::Duration) {
    if !matches!(front_door, FrontDoor::Build | FrontDoor::Explain | FrontDoor::Fix) {
        cli_fail(duration);
    }
}

fn cli_fail_complete(front_door: &FrontDoor, duration: std::time::Duration) {
    if !matches!(front_door, FrontDoor::Explain | FrontDoor::Fix) {
        cli_fail(duration);
    }
}

fn cli_show_diagnostics(
    diagnostics: &[aitex_engine::fast::Diagnostic],
    total: u64,
    log_path: &str,
    _input: &str,
) {
    if !cli_compact_output() {
        return;
    }
    let style = cli_style();
    const SHOWN: usize = 5;
    for diagnostic in diagnostics.iter().take(SHOWN) {
        #[cfg(feature = "ai")]
        let (presentation, location) = {
            let mut structured = structured_engine_diagnostic(diagnostic, _input);
            enrich_project_diagnostic(_input, &mut structured);
            let location = structured
                .location()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown location".into());
            (structured.presentation(), location)
        };
        #[cfg(not(feature = "ai"))]
        let location = if diagnostic.file.is_empty() {
            format!("line {}", diagnostic.line)
        } else {
            format!("{}:{}", diagnostic.file, diagnostic.line)
        };
        #[cfg(feature = "ai")]
        let message = &presentation.error;
        #[cfg(not(feature = "ai"))]
        let message = &diagnostic.message;
        eprintln!(
            "  {} {} {}",
            style.red("×"),
            style.bold(message),
            style.dim(&location),
        );
        let full_before = diagnostic.before.trim_start();
        let full_after = diagnostic.after.trim_end();
        if !full_before.is_empty() || !full_after.is_empty() {
            let before_chars: Vec<_> = full_before.chars().collect();
            let before_start = before_chars.len().saturating_sub(48);
            let clipped_before = before_chars[before_start..].iter().collect::<String>();
            let before = if before_start > 0 {
                format!("…{clipped_before}")
            } else {
                clipped_before
            };
            let after_chars: Vec<_> = full_after.chars().collect();
            let clipped_after = after_chars.iter().take(48).collect::<String>();
            let after = if after_chars.len() > 48 {
                format!("{clipped_after}…")
            } else {
                clipped_after
            };
            eprintln!("    {before}{after}");
            eprintln!("    {}{}", " ".repeat(before.chars().count()), style.red("^"));
        }
        #[cfg(feature = "ai")]
        {
            if let Some(reported) = &presentation.reported {
                eprintln!("    {}", style.dim(&format!("↳ reported · {reported}")));
            }
            let mut frames = presentation.provenance.iter().take(5).collect::<Vec<_>>();
            if presentation.provenance.len() > 5
                && let Some(causal) = presentation.provenance.last()
            {
                frames.push(causal);
            }
            for frame in frames {
                eprintln!("    {}", style.dim(&format!("↳ {frame}")));
            }
        }
        #[cfg(not(feature = "ai"))]
        for frame in diagnostic.provenance.iter().skip(1).take(4) {
            use aitex_engine::fast::DiagnosticProvenanceKind as Kind;
            let kind = match frame.kind {
                Kind::FailingToken => "token",
                Kind::MacroExpansion => "expands",
                Kind::MacroArgument => "argument",
                Kind::MacroDefinition => "defined",
                Kind::Include => "included",
            };
            let location = if frame.line == 0 {
                String::new()
            } else if frame.file.is_empty() {
                format!(" · line {}", frame.line)
            } else {
                format!(" · {}:{}", frame.file, frame.line)
            };
            let detail = if frame.detail.trim().is_empty() {
                String::new()
            } else {
                format!(" · {}", frame.detail)
            };
            eprintln!(
                "    {}",
                style.dim(&format!("↳ {kind} {}{location}{detail}", frame.label))
            );
        }
    }
    if total > SHOWN as u64 {
        eprintln!(
            "    {}",
            style.dim(&format!(
                "… {} more diagnostic{} · full transcript in {log_path}",
                total - SHOWN as u64,
                if total - SHOWN as u64 == 1 { "" } else { "s" },
            ))
        );
    }
}

/// Cache, checkpoint, and engine narration belongs in verbose terminal output,
/// while captured output remains unchanged for existing tooling.
macro_rules! cli_detail {
    ($($arg:tt)*) => {{
        if !cli_compact_output() {
            eprintln!($($arg)*);
        }
    }};
}

#[cfg(test)]
mod startup_tests {
    use super::{
        CliStyle, FrontDoor, cli_pass_finish_visible, human_bytes, human_duration, startup_text,
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn startup_is_a_compact_wordmark_with_version_and_mode() {
        let text = startup_text(
            &args(&["paper.tex"]),
            &FrontDoor::Build,
            CliStyle { color: false },
        );
        assert!(text.contains("▄▀█ █ ▀█▀ █▀▀ ▀▄▀"));
        assert!(text.contains("█▀█ █  █  ██▄ █ █"));
        assert!(text.contains(&format!(
            "aiTeX v{} · build · PDF",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!text.contains("\x1b["));
    }

    #[test]
    fn startup_distinguishes_public_command_modes() {
        let plain = CliStyle { color: false };
        let build = startup_text(&args(&["paper.tex"]), &FrontDoor::Build, plain);
        let pass = startup_text(
            &args(&["pass", "paper.tex", "-o", "paper.dvi"]),
            &FrontDoor::Pass,
            plain,
        );
        let watch = startup_text(
            &args(&["watch", "paper.tex", "-o", "paper.dvi"]),
            &FrontDoor::Watch,
            plain,
        );
        let warm = startup_text(
            &args(&["cache", "warm", "paper.tex", "-o", "paper.pdf"]),
            &FrontDoor::CacheWarm,
            plain,
        );
        let explain = startup_text(
            &args(&["explain", "paper.tex"]),
            &FrontDoor::Explain,
            plain,
        );
        let fix = startup_text(
            &args(&["fix", "paper.tex", "--dry-run"]),
            &FrontDoor::Fix,
            plain,
        );
        let format = startup_text(&args(&["format"]), &FrontDoor::Format, plain);
        assert!(build.contains("build · PDF"));
        assert!(pass.contains("pass · DVI"));
        assert!(watch.contains("watch · DVI"));
        assert!(warm.contains("cache warm · PDF"));
        assert!(explain.contains("explain · PDF"));
        assert!(fix.contains("fix · PDF"));
        assert!(format.contains("format · LaTeX"));
    }

    #[test]
    fn colored_startup_uses_ansi_but_keeps_the_wordmark_text() {
        let text = startup_text(
            &args(&["paper.tex"]),
            &FrontDoor::Build,
            CliStyle { color: true },
        );
        assert!(text.contains("\x1b[1;35m▄▀█ █\x1b[0m"));
        assert!(text.contains("\x1b[1;36m▀█▀ █▀▀ ▀▄▀\x1b[0m"));
    }

    #[test]
    fn human_units_stay_short() {
        assert_eq!(human_bytes(42), "42 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(
            human_duration(std::time::Duration::from_millis(1250)),
            "1.25 s"
        );
    }

    #[test]
    fn filesystem_watch_keeps_reruns_but_leaves_completion_to_the_build() {
        assert!(cli_pass_finish_visible(&FrontDoor::Watch, true, true));
        assert!(!cli_pass_finish_visible(&FrontDoor::Watch, false, true));
        assert!(cli_pass_finish_visible(&FrontDoor::Watch, false, false));
        assert!(cli_pass_finish_visible(&FrontDoor::Pass, false, false));
        assert!(!cli_pass_finish_visible(&FrontDoor::Build, true, false));
    }
}
