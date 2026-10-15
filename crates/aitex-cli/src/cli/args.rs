const USAGE: &str = concat!(
    "Usage: aitex <input.tex> [OPTIONS]\n",
    "       aitex build <input.tex> [OPTIONS]\n",
    "       aitex lint <input.tex> [--deny warnings] [--json] [OPTIONS]\n",
    "       aitex explain <input.tex> [OPTIONS]\n",
    "       aitex fix <input.tex> [--dry-run|--apply] [OPTIONS]\n",
    "       aitex pass <input.tex> [OPTIONS]\n",
    "       aitex watch <input.tex> [OPTIONS]\n",
    "       aitex fork-watch <input.tex> [OPTIONS]\n",
    "       aitex format [--output <path>]\n",
    "       aitex cache <inspect|warm|prune> ...",
);
const HELP_BODY: &str = r#" — TeX and LaTeX compiler

Usage: aitex <input.tex> [OPTIONS]
       aitex build <input.tex> [OPTIONS]
       aitex lint <input.tex> [--deny warnings] [--json] [OPTIONS]
       aitex explain <input.tex> [OPTIONS]
       aitex fix <input.tex> [--dry-run|--apply] [OPTIONS]
       aitex pass <input.tex> [OPTIONS]
       aitex watch <input.tex> [OPTIONS]
       aitex fork-watch <input.tex> [OPTIONS]
       aitex format [--output <path>]
       aitex cache inspect <directory>
       aitex cache warm <input.tex> -o <path> [OPTIONS]
       aitex cache prune <directory> [--max-bytes <bytes>]

Commands:
  <input.tex>           Run the complete cached build (the normal default)
  build <input.tex>     Explicit spelling of the complete build
  lint <input.tex>      Build, then run conservative artifact-backed checks
  pass <input.tex>      Run exactly one TeX pass
  explain <input.tex>   Explain the first build failure (omitted by no-AI build)
  fix <input.tex>       Interactive, isolated, validation-gated repair session
  watch <input.tex>     Rebuild a complete document when its inputs change
  format                Prebuild the local LaTeX format cache

Options:
  -o <path>             Write DVI or PDF (default: <jobname>.pdf)
  --fmt <path>          Override the locally generated LaTeX format
  -ini, --ini           Start from INITEX state instead of loading a format
  --cache <directory>   Override the platform cache directory
  --no-cache            Disable every persistent cache layer
  --no-project-cache    Use only the shared document-class layer
  --no-class-cache      Disable shared class snapshots
  --no-banner           Suppress the interactive startup display
  --stdin               Make watch rebuild once per input line (editor protocol)
  -v, --verbose         Show the TeX transcript and cache diagnostics
  --dump <path>         Dump the final engine state
  --probe <names>       Print comma-separated control-sequence meanings
  -halt-on-error        Stop after the first TeX error
  -file-line-error      Print diagnostics as file:line: message
  --tex82               Use canonical TeX82 register limits
  --unicode             Enable experimental native Unicode/OpenType PDF text
  --font <families>     Shape ordinary text with preferred OpenType families
  --font-path <paths>   Add a platform-separated project-font search path
  --language <tag>      Set the default BCP 47 shaping language
  --features <list>     Set OpenType features (+liga,-kern,ss01=1)
  --variations <list>   Set variable-font axes (wght=450,wdth=90)
  --direction <value>   Set auto, ltr, rtl, ttb, or btt text direction
  --dry-run             Validate a fix without changing the source
  --apply               Apply a complete validated repair chain, with backups
  --route <route>       local, ai-agent, ai-compact, ai-expanded, ai-conventional-log, or explain
  --json                Emit versioned JSONL repair events on stdout
  --deny warnings       Make lint findings produce a nonzero exit status
  --offline             Refuse every model route
  --session <id>        Add this attempt to an existing local repair record
  --max-attempts <n>    Bound repair attempts per session (default: 3)
  --max-agent-turns <n> Bound model/tool turns in one agent run (default: 12)
  --max-agent-builds <n> Bound complete builds in one agent run (default: 4)
  --max-input-tokens <n>  Bound estimated repair input tokens (default: 100000)
  --max-output-tokens <n> Bound provider output tokens (default: 16384)
  --max-elapsed <sec>   Bound the complete repair session (default: 600)
  --max-source-bytes <n> Bound source uploaded per request/agent run (default: 16384)
  --max-changed-lines <n> Bound the proposed source transaction (default: 200)
  --max-deleted-lines <n> Bound transaction deletions (default: 100)
  --max-cost-usd <usd>  Stop after reported spend reaches this amount
  --model <model>       Model for explain/fix; no model is selected implicitly
  --provider <name>     AI provider: anthropic, codex, or openrouter
  --openrouter-provider <slug>  Pin one OpenRouter provider; disable fallbacks
  --openrouter-seed <n> Reproducible seed when the selected provider supports it
  --context <profile>   compact (default), expanded, or conventional-log ablation
  --prompt-only         Print the model packet without sending it
  --allow-source-upload Permit selected source excerpts to leave this machine
  -h, --help            Print help
  -V, --version         Print version

Default cache:
  Unix/Linux            $XDG_CACHE_HOME/aitex or ~/.cache/aitex
  macOS                 $XDG_CACHE_HOME/aitex or ~/Library/Caches/aiTeX

The first LaTeX build creates and validates formats/latex.afmt there.
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrontDoor {
    Run,
    Build,
    Lint,
    Pass,
    Explain,
    Fix,
    Watch,
    ForkWatch,
    Format,
    CacheInspect,
    CacheWarm,
    CachePrune,
    Help,
    Version,
    UsageError,
}

fn input_argument(args: &[String]) -> Option<&str> {
    let cache_start = usize::from(
        args.first().map(String::as_str) == Some("cache")
            && args.get(1).map(String::as_str) == Some("warm"),
    ) * 2;
    let start = cache_start.max(usize::from(matches!(
        args.first().map(String::as_str),
        Some("build" | "lint" | "pass" | "explain" | "fix" | "watch" | "fork-watch")
    )));
    let mut skip_value = false;
    for argument in &args[start..] {
        if skip_value {
            skip_value = false;
            continue;
        }
        if matches!(
            argument.as_str(),
            "-o"
                | "--fmt"
                | "--cache"
                | "--dump"
                | "--probe"
                | "--max-bytes"
                | "--model"
                | "--provider"
                | "--context"
                | "--route"
                | "--session"
                | "--max-attempts"
                | "--max-agent-turns"
                | "--max-agent-builds"
                | "--max-input-tokens"
                | "--max-output-tokens"
                | "--max-elapsed"
                | "--max-source-bytes"
                | "--max-changed-lines"
                | "--max-deleted-lines"
                | "--max-cost-usd"
                | "--deny"
                | "--openrouter-provider"
                | "--openrouter-seed"
                | "--font"
                | "--font-path"
                | "--language"
                | "--features"
                | "--variations"
                | "--direction"
        ) {
            skip_value = true;
        } else if !argument.starts_with('-') {
            return Some(argument);
        }
    }
    None
}

/// The normal CLI is PDF-first. INITEX, dump, and probe jobs deliberately keep
/// their old transcript-only behavior because they are engine-development
/// operations rather than document builds.
fn output_argument(args: &[String]) -> Option<String> {
    if let Some(output) = args
        .iter()
        .position(|argument| argument == "-o")
        .and_then(|at| args.get(at + 1))
    {
        return Some(output.clone());
    }
    if let Ok(output) = std::env::var("AITEX_DVI") {
        return Some(output);
    }
    if args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "-ini" | "--ini" | "--dump" | "--probe"
        )
    }) {
        return None;
    }
    let input = std::path::Path::new(input_argument(args)?);
    let stem = input.file_stem()?.to_str()?;
    Some(format!("{stem}.pdf"))
}

fn probe_argument(args: &[String]) -> Option<&str> {
    args.iter()
        .position(|argument| argument == "--probe")
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

/// Validate the compile/watch/warm argument grammar.  Keeping this at the
/// front door is important: the rest of the runner intentionally uses cheap
/// flag lookups and can therefore assume there is exactly one value for each
/// option and exactly one input file.
fn valid_compile_arguments(args: &[String], start: usize) -> bool {
    let mut positionals = 0usize;
    let mut seen: Vec<&str> = Vec::new();
    let mut at = start;
    while at < args.len() {
        let argument = args[at].as_str();
        let key = if matches!(
            argument,
            "-o"
                | "--fmt"
                | "--cache"
                | "--dump"
                | "--probe"
                | "--font"
                | "--font-path"
                | "--language"
                | "--features"
                | "--variations"
                | "--direction"
        ) {
            if at + 1 >= args.len()
                || (args[at + 1].starts_with('-')
                    && !(argument == "--features" && !args[at + 1].starts_with("--")))
            {
                return false;
            }
            if argument == "-o"
                && args[at + 1] != "/dev/null"
                && !matches!(
                    std::path::Path::new(&args[at + 1])
                        .extension()
                        .and_then(|extension| extension.to_str()),
                    Some("dvi" | "pdf")
                )
            {
                return false;
            }
            at += 2;
            argument
        } else if matches!(argument, "-halt-on-error" | "--halt-on-error") {
            at += 1;
            "halt-on-error"
        } else if matches!(argument, "-file-line-error" | "--file-line-error") {
            at += 1;
            "file-line-error"
        } else if matches!(
            argument,
            "--no-cache"
                | "--no-project-cache"
                | "--no-class-cache"
                | "--no-banner"
                | "-v"
                | "--verbose"
                | "-ini"
                | "--ini"
                | "--tex82"
                | "--unicode"
                | "-no-shell-escape"
        ) {
            at += 1;
            if matches!(argument, "-ini" | "--ini") {
                "ini"
            } else if matches!(argument, "-v" | "--verbose") {
                "verbose"
            } else {
                argument
            }
        } else if let Some(mode) = argument.strip_prefix("-interaction=") {
            if !matches!(mode, "batchmode" | "nonstopmode" | "scrollmode" | "errorstopmode") {
                return false;
            }
            at += 1;
            "-interaction"
        } else if argument.starts_with('-') {
            return false;
        } else {
            positionals += 1;
            at += 1;
            continue;
        };
        if key != "--font-path" && seen.contains(&key) {
            return false;
        }
        seen.push(key);
    }
    positionals == 1
        && !(seen.contains(&"--cache") && seen.contains(&"--no-cache"))
        && !(seen.contains(&"--fmt") && seen.contains(&"ini"))
}

fn valid_watch_arguments(args: &[String]) -> bool {
    let stdin_count = args
        .iter()
        .filter(|argument| argument.as_str() == "--stdin")
        .count();
    if stdin_count > 1 {
        return false;
    }
    let compile = args
        .iter()
        .enumerate()
        .filter(|(index, argument)| *index == 0 || argument.as_str() != "--stdin")
        .map(|(_, argument)| argument.clone())
        .collect::<Vec<_>>();
    valid_compile_arguments(&compile, 1)
}

fn valid_lint_arguments(args: &[String]) -> bool {
    let mut compile = Vec::new();
    let mut seen_json = false;
    let mut seen_deny = false;
    let mut at = 1;
    while at < args.len() {
        match args[at].as_str() {
            "--json" if !seen_json => {
                seen_json = true;
                at += 1;
            }
            "--deny" if !seen_deny && args.get(at + 1).map(String::as_str) == Some("warnings") => {
                seen_deny = true;
                at += 2;
            }
            _ => {
                compile.push(args[at].clone());
                at += 1;
            }
        }
    }
    valid_compile_arguments(&compile, 0)
        && !compile.iter().any(|argument| {
            matches!(argument.as_str(), "-ini" | "--ini" | "--dump" | "--probe")
        })
}

fn valid_explain_arguments(args: &[String]) -> bool {
    let mut compile = Vec::new();
    let mut seen = Vec::new();
    let mut at = 1;
    while at < args.len() {
        match args[at].as_str() {
            option @ ("--model"
            | "--provider"
            | "--context"
            | "--openrouter-provider"
            | "--openrouter-seed") => {
                let Some(value) = args.get(at + 1) else {
                    return false;
                };
                if value.starts_with('-') || seen.contains(&option) {
                    return false;
                }
                if option == "--provider"
                    && !matches!(value.as_str(), "anthropic" | "codex" | "openrouter")
                {
                    return false;
                }
                if option == "--context"
                    && !matches!(value.as_str(), "diagnostic" | "compact" | "expanded")
                {
                    return false;
                }
                if option == "--openrouter-seed" && value.parse::<u64>().is_err() {
                    return false;
                }
                seen.push(option);
                at += 2;
            }
            option @ ("--prompt-only" | "--allow-source-upload") => {
                if seen.contains(&option) {
                    return false;
                }
                seen.push(option);
                at += 1;
            }
            _ => {
                compile.push(args[at].clone());
                at += 1;
            }
        }
    }
    let provider = args
        .windows(2)
        .find(|pair| pair[0] == "--provider")
        .map(|pair| pair[1].as_str())
        .unwrap_or("anthropic");
    valid_compile_arguments(&compile, 0)
        && (!(seen.contains(&"--openrouter-provider")
            || seen.contains(&"--openrouter-seed"))
            || provider == "openrouter")
}

fn valid_fix_arguments(args: &[String]) -> bool {
    let mut compile = Vec::new();
    let mut seen = Vec::new();
    let mut mode = None;
    let mut at = 1;
    while at < args.len() {
        match args[at].as_str() {
            option @ ("--model"
            | "--provider"
            | "--context"
            | "--route"
            | "--session"
            | "--max-attempts"
            | "--max-agent-turns"
            | "--max-agent-builds"
            | "--max-input-tokens"
            | "--max-output-tokens"
            | "--max-elapsed"
            | "--max-source-bytes"
            | "--max-changed-lines"
            | "--max-deleted-lines"
            | "--max-cost-usd"
            | "--openrouter-provider"
            | "--openrouter-seed") => {
                let Some(value) = args.get(at + 1) else {
                    return false;
                };
                if value.starts_with('-') || seen.contains(&option) {
                    return false;
                }
                if option == "--provider"
                    && !matches!(value.as_str(), "anthropic" | "codex" | "openrouter")
                {
                    return false;
                }
                if option == "--context"
                    && !matches!(
                        value.as_str(),
                        "compact" | "expanded" | "conventional-log"
                    )
                {
                    return false;
                }
                if option == "--route"
                    && !matches!(
                        value.as_str(),
                        "local"
                            | "ai-agent"
                            | "ai-compact"
                            | "ai-expanded"
                            | "ai-conventional-log"
                            | "explain"
                    )
                {
                    return false;
                }
                if matches!(
                    option,
                    "--max-attempts"
                        | "--max-agent-turns"
                        | "--max-agent-builds"
                        | "--max-input-tokens"
                        | "--max-output-tokens"
                        | "--max-elapsed"
                        | "--max-source-bytes"
                        | "--max-changed-lines"
                        | "--max-deleted-lines"
                ) && value.parse::<u64>().ok().filter(|number| *number > 0).is_none()
                {
                    return false;
                }
                if option == "--max-cost-usd"
                    && value
                        .parse::<f64>()
                        .ok()
                        .filter(|number| number.is_finite() && *number > 0.0)
                        .is_none()
                {
                    return false;
                }
                if option == "--openrouter-seed" && value.parse::<u64>().is_err() {
                    return false;
                }
                seen.push(option);
                at += 2;
            }
            option @ ("--dry-run" | "--apply") => {
                if mode.replace(option).is_some() {
                    return false;
                }
                at += 1;
            }
            option @ ("--allow-source-upload" | "--json" | "--offline") => {
                if seen.contains(&option) {
                    return false;
                }
                seen.push(option);
                at += 1;
            }
            _ => {
                compile.push(args[at].clone());
                at += 1;
            }
        }
    }
    let provider = args
        .windows(2)
        .find(|pair| pair[0] == "--provider")
        .map(|pair| pair[1].as_str())
        .unwrap_or("anthropic");
    valid_compile_arguments(&compile, 0)
        && !compile.iter().any(|argument| {
            matches!(argument.as_str(), "-ini" | "--ini" | "--dump" | "--probe")
        })
        && (!(seen.contains(&"--openrouter-provider")
            || seen.contains(&"--openrouter-seed"))
            || provider == "openrouter")
}

fn classify_front_door(args: &[String]) -> FrontDoor {
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help") {
        FrontDoor::Help
    } else if args.len() == 1 && matches!(args[0].as_str(), "-V" | "--version") {
        FrontDoor::Version
    } else if args.first().map(String::as_str) == Some("format") {
        if args.len() == 1
            || (args.len() == 3 && args[1] == "--output" && !args[2].starts_with('-'))
        {
            FrontDoor::Format
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("build") {
        if valid_compile_arguments(args, 1)
            && !args.iter().any(|argument| {
                matches!(
                    argument.as_str(),
                    "-ini" | "--ini" | "--dump" | "--probe"
                )
            })
        {
            FrontDoor::Build
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("lint") {
        if valid_lint_arguments(args) {
            FrontDoor::Lint
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("explain") {
        if valid_explain_arguments(args) {
            FrontDoor::Explain
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("fix") {
        if valid_fix_arguments(args) {
            FrontDoor::Fix
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("pass") {
        if valid_compile_arguments(args, 1) {
            FrontDoor::Pass
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("watch") {
        if valid_watch_arguments(args) {
            FrontDoor::Watch
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("fork-watch") {
        if valid_compile_arguments(args, 1) {
            FrontDoor::ForkWatch
        } else {
            FrontDoor::UsageError
        }
    } else if args.first().map(String::as_str) == Some("cache") {
        match args.get(1).map(String::as_str) {
            Some("inspect") if args.len() == 3 && !args[2].starts_with('-') => {
                FrontDoor::CacheInspect
            }
            Some("warm")
                if valid_compile_arguments(args, 2)
                    && args[2..]
                        .windows(2)
                        .any(|pair| pair.first().map(String::as_str) == Some("-o")) =>
            {
                FrontDoor::CacheWarm
            }
            Some("prune")
                if (args.len() == 3 && !args[2].starts_with('-'))
                    || (args.len() == 5
                        && !args[2].starts_with('-')
                        && args[3] == "--max-bytes"
                        && args[4].parse::<u64>().is_ok()) =>
            {
                FrontDoor::CachePrune
            }
            _ => FrontDoor::UsageError,
        }
    } else if valid_compile_arguments(args, 0) {
        if args.iter().any(|argument| {
            matches!(
                argument.as_str(),
                "-ini" | "--ini" | "--dump" | "--probe"
            )
        }) {
            FrontDoor::Run
        } else {
            FrontDoor::Build
        }
    } else {
        FrontDoor::UsageError
    }
}

fn platform_cache_root(
    xdg_cache_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    macos: bool,
    input: Option<&str>,
) -> std::path::PathBuf {
    if let Some(root) = xdg_cache_home.map(std::path::PathBuf::from)
        && root.is_absolute()
    {
        return root.join("aitex");
    }
    if let Some(home) = home.map(std::path::PathBuf::from) {
        return if macos {
            home.join("Library/Caches/aiTeX")
        } else {
            home.join(".cache/aitex")
        };
    }
    std::path::Path::new(input.unwrap_or("."))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".aitex-cache")
}

fn cache_root_for_arguments(args: &[String]) -> Option<String> {
    if let Some(root) = args
        .iter()
        .position(|argument| argument == "--cache")
        .and_then(|at| args.get(at + 1))
    {
        return Some(root.clone());
    }
    matches!(
        classify_front_door(args),
        FrontDoor::Build
            | FrontDoor::Lint
            | FrontDoor::Explain
            | FrontDoor::Fix
            | FrontDoor::Watch
            | FrontDoor::ForkWatch
            | FrontDoor::CacheWarm
    )
    .then(|| {
        platform_cache_root(
            std::env::var_os("XDG_CACHE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            cfg!(target_os = "macos"),
            input_argument(args),
        )
        .to_string_lossy()
        .into_owned()
    })
}

fn cache_root_argument() -> Option<String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    cache_root_for_arguments(&arguments)
}
