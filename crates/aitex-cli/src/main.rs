// Standalone runner for the fast engine: phase timing + completeness frontier.
// Runs on a large-stack thread because some expansion paths still recurse on the
// host stack (a robustness item to make iterative later — TeX uses its input stack).
use aitex_engine::dvi::{DviWriter, FontDef};
use aitex_engine::fast::{Fast, ShipoutMeta};
use aitex_engine::scaled::Scaled;
use aitex_engine::shipout::ship_out_box;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::rc::Rc;
use std::time::Instant;

/// AITEX_PROFILE=1 — sample `aitex_engine::profile::PHASE` on a timer and print
/// where the run spent its time. There is no `perf` here, and a `gdb`-driven
/// sampler is too coarse to apportion a four-hundred-millisecond run; the
/// engine names its phase and this counts them.
fn start_sampler() -> Option<std::sync::Arc<Sampler>> {
    if std::env::var("AITEX_PROFILE").is_err() {
        return None;
    }
    use aitex_engine::profile;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let s = std::sync::Arc::new(Sampler {
        counts: core::array::from_fn(|_| AtomicU64::new(0)),
        stop: AtomicBool::new(false),
    });
    let t = std::sync::Arc::clone(&s);
    std::thread::spawn(move || {
        while !t.stop.load(Ordering::Relaxed) {
            let p = profile::PHASE.load(Ordering::Relaxed) as usize;
            if let Some(c) = t.counts.get(p) {
                c.fetch_add(1, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    });
    Some(s)
}

struct Sampler {
    counts: [std::sync::atomic::AtomicU64; aitex_engine::profile::PHASES],
    stop: std::sync::atomic::AtomicBool,
}

impl Sampler {
    fn report(&self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Relaxed);
        let total: u64 = self.counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        if total == 0 {
            return;
        }
        use std::sync::atomic::Ordering as O;
        eprintln!(
            "--- profile ({total} samples, {} allocations, {} MB allocated) ---",
            ALLOCS.load(O::Relaxed),
            BYTES.load(O::Relaxed) / 1_048_576
        );
        let mut rows: Vec<(u64, usize)> = self
            .counts
            .iter()
            .enumerate()
            .map(|(i, c)| (c.load(Ordering::Relaxed), i))
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        for (n, i) in rows {
            if n == 0 {
                continue;
            }
            eprintln!(
                "  {:>5.1}%  {:<10} {n:>6} samples   {:>6} MB allocated",
                100.0 * n as f64 / total as f64,
                aitex_engine::profile::name(i as u8),
                PHASE_BYTES[i].load(O::Relaxed) / 1_048_576
            );
        }
    }
}

/// Counts every allocation the run makes. `AITEX_PROFILE` prints the totals:
/// node lists are `Vec`s and a paragraph is thousands of them, so knowing
/// whether the time is in the allocator is worth twenty lines.
struct Counting;

static ALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TRACK_ALLOCATIONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Bytes currently held, and the high-water mark. `BYTES` is *traffic* — it
/// counts every allocation ever made — which says nothing about footprint. The
/// difference between `PEAK` and the process's RSS is what the allocator is
/// holding but not using, and the difference between `PEAK` and
/// `Fast::memory_report` is what lives outside the engine's own buffers.
static LIVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PEAK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[inline]
fn note_live(delta: i64) {
    use std::sync::atomic::Ordering::Relaxed;
    let now = if delta >= 0 {
        LIVE.fetch_add(delta as u64, Relaxed) + delta as u64
    } else {
        let amount = (-delta) as u64;
        LIVE.fetch_update(Relaxed, Relaxed, |live| Some(live.saturating_sub(amount)))
            .unwrap_or(0)
            .saturating_sub(amount)
    };
    // Relaxed max: a racing under-report is fine, this is a diagnostic and the
    // engine is single-threaded apart from the profile sampler.
    if now > PEAK.load(Relaxed) {
        PEAK.store(now, Relaxed);
    }
}

/// Bytes allocated while each phase was running — the same marker the sampler
/// reads, so "where the time goes" and "where the memory traffic goes" are
/// answered on the same axis.
static PHASE_BYTES: [std::sync::atomic::AtomicU64; aitex_engine::profile::PHASES] =
    [const { std::sync::atomic::AtomicU64::new(0) }; aitex_engine::profile::PHASES];

// SAFETY: every allocation operation is forwarded to `System` with the same
// pointer and layout it received. The added bookkeeping touches only atomics
// and neither dereferences nor changes an allocation.
#[allow(unsafe_code)]
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        if !TRACK_ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) {
            return unsafe { std::alloc::System.alloc(l) };
        }
        ALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BYTES.fetch_add(l.size() as u64, std::sync::atomic::Ordering::Relaxed);
        let p = aitex_engine::profile::PHASE.load(std::sync::atomic::Ordering::Relaxed) as usize;
        if let Some(c) = PHASE_BYTES.get(p) {
            c.fetch_add(l.size() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        note_live(l.size() as i64);
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        if !TRACK_ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) {
            return unsafe { std::alloc::System.dealloc(p, l) };
        }
        note_live(-(l.size() as i64));
        unsafe { std::alloc::System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
        if !TRACK_ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) {
            return unsafe { std::alloc::System.realloc(p, l, n) };
        }
        ALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BYTES.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        let ph = aitex_engine::profile::PHASE.load(std::sync::atomic::Ordering::Relaxed) as usize;
        if let Some(c) = PHASE_BYTES.get(ph) {
            c.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
        note_live(n as i64 - l.size() as i64);
        unsafe { std::alloc::System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// What to exit with. Set from inside the worker thread, read after it joins.
static EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

include!("cli/args.rs");
include!("cli/startup.rs");
include!("cli/build.rs");
include!("cli/lint.rs");
include!("cli/watch.rs");
#[cfg(feature = "ai")]
include!("cli/explain.rs");
#[cfg(feature = "ai")]
include!("cli/fix.rs");

fn aitex_version() -> String {
    match option_env!("AITEX_GIT_DESCRIBE") {
        Some(description) => format!("{} ({description})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let front_door = classify_front_door(&arguments);
    match front_door {
        FrontDoor::Run
        | FrontDoor::Build
        | FrontDoor::Lint
        | FrontDoor::Pass
        | FrontDoor::Explain
        | FrontDoor::Fix
        | FrontDoor::CacheWarm
        | FrontDoor::Watch
        | FrontDoor::ForkWatch => {}
        FrontDoor::Format => {
            show_startup(&arguments, &front_door);
            run_format_command(&arguments);
            let code = EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
            if code != 0 {
                std::process::exit(code);
            }
            return;
        }
        FrontDoor::CacheInspect => {
            let code = inspect_cache(std::path::Path::new(&arguments[2]));
            if code != 0 {
                std::process::exit(code);
            }
            return;
        }
        FrontDoor::CachePrune => {
            let limit = arguments
                .iter()
                .position(|argument| argument == "--max-bytes")
                .and_then(|at| arguments.get(at + 1))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_else(cache_limit_bytes);
            let code = prune_cache(std::path::Path::new(&arguments[2]), limit);
            if code != 0 {
                std::process::exit(code);
            }
            return;
        }
        FrontDoor::Help => {
            print!("aiTeX {}{HELP_BODY}", env!("CARGO_PKG_VERSION"));
            return;
        }
        FrontDoor::Version => {
            println!("aitex {}", aitex_version());
            return;
        }
        FrontDoor::UsageError => {
            eprintln!("{USAGE}");
            eprintln!("Try 'aitex --help' for more information.");
            std::process::exit(2);
        }
    }
    #[cfg(not(feature = "ai"))]
    if matches!(front_door, FrontDoor::Explain | FrontDoor::Fix) {
        eprintln!(
            "aitex: explain and fix are unavailable in this no-AI build; rebuild with `make` or `cargo build -p aitex-cli`"
        );
        std::process::exit(2);
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if matches!(
        arguments.first().map(String::as_str),
        Some("watch" | "fork-watch")
    ) && std::env::var_os("AITEX_WATCH_ALLOCATOR_CONFIGURED").is_none()
    {
        // One glibc arena keeps the DVI service inside its 24 MiB RSS gate.
        // PDF and body checkpoints also get a bounded trim threshold so
        // transient assembly or compression buffers do not become permanent
        // service RSS. Re-exec once so malloc sees its process-lifetime
        // settings before the first allocation; the PID and stdin/stdout
        // protocol remain unchanged. This uses CommandExt's safe exec wrapper
        // and leaves other allocators/platforms untouched.
        use std::os::unix::process::CommandExt as _;
        let executable = std::env::current_exe().unwrap_or_else(|error| {
            eprintln!("aitex watch: cannot locate executable: {error}");
            std::process::exit(1);
        });
        let pdf = arguments
            .windows(2)
            .any(|pair| pair[0] == "-o" && pair[1].ends_with(".pdf"))
            || std::env::var("AITEX_DVI").is_ok_and(|path| path.ends_with(".pdf"));
        let mut command = std::process::Command::new(executable);
        command
            .args(&arguments)
            .env("AITEX_WATCH_ALLOCATOR_CONFIGURED", "1")
            .env("MALLOC_ARENA_MAX", "1");
        if pdf || std::env::var_os("AITEX_INCREMENTAL").is_some() {
            command.env("MALLOC_TRIM_THRESHOLD_", "131072");
        }
        let error = command.exec();
        eprintln!("aitex watch: cannot configure service allocator: {error}");
        std::process::exit(1);
    }
    let profile = std::env::var_os("AITEX_PROFILE").is_some();
    let memory = std::env::var_os("AITEX_MEM").is_some();
    TRACK_ALLOCATIONS.store(profile || memory, std::sync::atomic::Ordering::Relaxed);
    aitex_engine::profile::set_enabled(profile);
    show_startup(&arguments, &front_door);
    let watch = matches!(front_door, FrontDoor::Watch);
    let build = matches!(front_door, FrontDoor::Build);
    let lint = matches!(front_door, FrontDoor::Lint);
    let explain = matches!(front_door, FrontDoor::Explain);
    let fix = matches!(front_door, FrontDoor::Fix);
    let cache_warm = matches!(front_door, FrontDoor::CacheWarm);
    let fork_watch = matches!(front_door, FrontDoor::ForkWatch);
    if fork_watch {
        run_fork_watch();
        let code = EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
        if code != 0 {
            std::process::exit(code);
        }
        return;
    }
    std::thread::Builder::new()
        // The output routine and math scripts re-enter the main loop
        // recursively, so nesting runs on the host stack. 8 GiB was chosen
        // defensively for the *layered* engine, whose gullet recursed per
        // expansion; the fast engine's frame stack is flat, and measurement
        // says a full `latex.ltx` load and a real paper both fit in **1 MB**.
        // The reservation is virtual and costs nothing here, but a wasm32 build
        // has to name a real number, so `AITEX_STACK_MB` makes it measurable.
        .stack_size(
            std::env::var("AITEX_STACK_MB")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .map_or(1 << 33, |mb| mb << 20),
        )
        .spawn(move || {
            if watch {
                run_watch();
            } else if explain {
                #[cfg(feature = "ai")]
                run_explain();
            } else if fix {
                #[cfg(feature = "ai")]
                run_fix();
            } else if build {
                run_native_build();
            } else if lint {
                run_lint();
            } else if cache_warm {
                run_cache_warm();
            } else {
                let _ = run();
            }
        })
        .unwrap()
        .join()
        .unwrap();
    let code = EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    if code != 0 {
        std::process::exit(code);
    }
}

/// Prime an exact completed-output entry. The first LaTeX pass commonly
/// creates `.aux`/`.toc`, so a one-pass "warm" would produce an entry that is
/// stale before the command returns. Repeat only while those next-pass inputs
/// change, with a hard bound for documents whose generated files never
/// converge.
fn run_cache_warm() {
    const MAX_WARM_PASSES: usize = 5;
    for pass in 1..=MAX_WARM_PASSES {
        EXIT_CODE.store(0, std::sync::atomic::Ordering::Relaxed);
        if pass > 1 {
            cli_detail!("aitex cache warm: convergence pass {pass}/{MAX_WARM_PASSES}");
        }
        let outcome = run();
        if EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return;
        }
        if !outcome.project_snapshot_expected
            || (outcome.project_snapshot_ready && !outcome.rerun_needed)
        {
            cli_detail!("aitex cache warm: ready after {pass} pass(es)");
            return;
        }
    }
    eprintln!(
        "aitex cache warm: auxiliary inputs did not stabilize after {MAX_WARM_PASSES} passes"
    );
    EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
}

/// Linux copy-on-write experiment. The parent is deliberately kept on
/// the original main thread: forking a multithreaded process and then running
/// allocator-heavy Rust in the child can inherit a library lock held by a
/// vanished thread. The parent validates the on-disk snapshot before every
/// request and retains one callback-free deserialized engine. The child takes
/// its private COW copy, installs fresh I/O callbacks in `run`, and exits.
#[cfg(target_os = "linux")]
fn run_fork_watch() {
    use std::io::{BufRead as _, Write as _};
    use std::sync::atomic::Ordering;

    if std::env::var_os("AITEX_PROFILE").is_some() {
        eprintln!("aitex fork-watch: profiling uses a sampler thread; using portable watch");
        run_stdin_watch();
        return;
    }

    let mut sequence = 1u64;
    let initial_started = std::time::Instant::now();
    run_cache_warm();
    let initial_code = EXIT_CODE.load(Ordering::Relaxed);
    if initial_code == 0 {
        let _ = refresh_fork_preloaded_engine();
    }
    cli_detail!(
        "AITEX_FORK_WATCH ready sequence={sequence} code={initial_code} duration_ms={:.3}",
        initial_started.elapsed().as_secs_f64() * 1e3,
    );
    if initial_code != 0 {
        return;
    }

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        match line {
            Ok(line) if line.trim() == "quit" => break,
            Ok(_) => {
                sequence += 1;
                let started = std::time::Instant::now();
                let _ = refresh_fork_preloaded_engine();
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                // SAFETY: fork-watch stays single-threaded, flushes Rust's
                // stdio buffers first, and the parent does no work until
                // waitpid returns. The child mutates only its COW address
                // space, then terminates with `_exit` so it cannot run copies
                // of parent-owned destructors or atexit handlers.
                #[allow(unsafe_code)]
                let pid = unsafe { libc::fork() };
                let (
                    code,
                    child_user_ms,
                    child_system_ms,
                    child_max_rss_kib,
                    child_read_bytes,
                    child_write_bytes,
                ) = if pid == 0 {
                    FORK_CHILD_ACTIVE.store(true, Ordering::Relaxed);
                    EXIT_CODE.store(0, Ordering::Relaxed);
                    let _ = run();
                    let code = EXIT_CODE.load(Ordering::Relaxed).clamp(0, 255);
                    let _ = std::io::stdout().flush();
                    let _ = std::io::stderr().flush();
                    // SAFETY: this is the child-only terminal operation
                    // described above; all observable output is already
                    // flushed and the OS reclaims its private memory.
                    #[allow(unsafe_code)]
                    unsafe {
                        libc::_exit(code);
                    }
                } else if pid < 0 {
                    eprintln!(
                        "aitex fork-watch: fork failed: {}; using one ordinary build",
                        std::io::Error::last_os_error()
                    );
                    EXIT_CODE.store(0, Ordering::Relaxed);
                    let _ = run();
                    (EXIT_CODE.load(Ordering::Relaxed), 0.0, 0.0, 0, 0, 0)
                } else {
                    let mut status = 0;
                    // SAFETY: `rusage` is a plain C output struct and wait4
                    // initializes every field before a successful return.
                    #[allow(unsafe_code)]
                    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
                    loop {
                        // SAFETY: `pid` is the positive child id returned by
                        // fork; `status` and `usage` are live writable values.
                        #[allow(unsafe_code)]
                        let waited = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
                        if waited == pid {
                            break;
                        }
                        if waited < 0
                            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                        {
                            continue;
                        }
                        status = 1 << 8;
                        break;
                    }
                    let code = if libc::WIFEXITED(status) {
                        libc::WEXITSTATUS(status)
                    } else if libc::WIFSIGNALED(status) {
                        128 + libc::WTERMSIG(status)
                    } else {
                        1
                    };
                    let milliseconds =
                        |time: libc::timeval| time.tv_sec as f64 * 1e3 + time.tv_usec as f64 / 1e3;
                    (
                        code,
                        milliseconds(usage.ru_utime),
                        milliseconds(usage.ru_stime),
                        usage.ru_maxrss.max(0) as u64,
                        usage.ru_inblock.max(0) as u64 * 512,
                        usage.ru_oublock.max(0) as u64 * 512,
                    )
                };
                EXIT_CODE.store(code, Ordering::Relaxed);
                cli_detail!(
                    concat!(
                        "AITEX_FORK_WATCH ready sequence={} code={} ",
                        "duration_ms={:.3} child_user_ms={:.3} ",
                        "child_system_ms={:.3} child_max_rss_kib={} ",
                        "child_read_bytes={} child_write_bytes={}"
                    ),
                    sequence,
                    code,
                    started.elapsed().as_secs_f64() * 1e3,
                    child_user_ms,
                    child_system_ms,
                    child_max_rss_kib,
                    child_read_bytes,
                    child_write_bytes,
                );
            }
            Err(error) => {
                eprintln!("aitex fork-watch: stdin failed: {error}");
                EXIT_CODE.store(1, Ordering::Relaxed);
                break;
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run_fork_watch() {
    eprintln!("aitex fork-watch: copy-on-write is unavailable; using portable watch");
    run_stdin_watch();
}

#[cfg(test)]
mod tests {
    use super::{
        FrontDoor, HotDviDocumentCache, HotFileCache, cache_root_for_arguments,
        classify_front_door, complete_file_source, dvi_document_key, eps_natural_bbox,
        input_argument, jpeg_xbb_size, load_unicode_options, normalize_rgba_png, output_argument,
        pdf_compression_workers, pdf_output_key, pdf_page_content_is_array, platform_cache_root,
        png_resolution, pngtopam_command, probe_argument, raster_natural_bbox, restore_base_format,
        rgba_png_idat, source_for_run, synthetic_xbb_candidates, unicode_compat_file,
        unicode_for_bitmap_slot, unicode_for_glyph,
    };

    #[test]
    fn displayed_version_keeps_package_and_source_identity() {
        let version = super::aitex_version();
        assert!(version.starts_with(env!("CARGO_PKG_VERSION")));
        if let Some(description) = option_env!("AITEX_GIT_DESCRIBE") {
            assert!(version.ends_with(&format!("({description})")));
        }
    }

    #[test]
    fn public_front_door_classifies_help_version_input_and_usage_errors() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).into())
                .collect::<Vec<_>>()
        };
        assert_eq!(classify_front_door(&args(&["paper.tex"])), FrontDoor::Build);
        assert_eq!(classify_front_door(&args(&["format"])), FrontDoor::Format);
        assert_eq!(
            classify_front_door(&args(&["format", "--output", "local.afmt"])),
            FrontDoor::Format
        );
        assert_eq!(
            classify_front_door(&args(&["format", "--output"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            output_argument(&args(&["paper.tex"])).as_deref(),
            Some("paper.pdf")
        );
        assert_eq!(
            output_argument(&args(&["sources/paper.tex", "-o", "result.dvi"])).as_deref(),
            Some("result.dvi")
        );
        assert_eq!(output_argument(&args(&["--ini", "plain.tex"])), None);
        assert_eq!(
            classify_front_door(&args(&["build", "paper.tex"])),
            FrontDoor::Build
        );
        assert_eq!(
            classify_front_door(&args(&[
                "lint",
                "paper.tex",
                "--deny",
                "warnings",
                "--json"
            ])),
            FrontDoor::Lint
        );
        assert_eq!(
            input_argument(&args(&["lint", "--deny", "warnings", "paper.tex"])),
            Some("paper.tex")
        );
        assert_eq!(
            classify_front_door(&args(&["lint", "paper.tex", "--deny", "errors"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["pass", "paper.tex"])),
            FrontDoor::Pass
        );
        assert_eq!(
            input_argument(&args(&["pass", "paper.tex"])),
            Some("paper.tex")
        );
        assert_eq!(
            input_argument(&args(&["build", "paper.tex"])),
            Some("paper.tex")
        );
        assert_eq!(
            classify_front_door(&args(&[
                "explain",
                "paper.tex",
                "--context",
                "compact",
                "--prompt-only",
            ])),
            FrontDoor::Explain
        );
        assert_eq!(
            classify_front_door(&args(&["explain", "paper.tex", "--openrouter-seed", "42",])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&[
                "fix",
                "paper.tex",
                "--provider",
                "openrouter",
                "--openrouter-provider",
                "openai/us-east",
                "--openrouter-seed",
                "42",
            ])),
            FrontDoor::Fix
        );
        assert_eq!(
            input_argument(&args(&[
                "explain",
                "--model",
                "claude-example",
                "paper.tex",
                "--allow-source-upload",
            ])),
            Some("paper.tex")
        );
        assert_eq!(
            classify_front_door(&args(&["explain", "paper.tex", "--provider", "unknown"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["explain", "paper.tex", "--provider", "codex"])),
            FrontDoor::Explain
        );
        assert_eq!(
            classify_front_door(&args(&[
                "explain",
                "paper.tex",
                "--provider",
                "openrouter",
                "--openrouter-provider",
                "openai/us-east",
                "--openrouter-seed",
                "42",
            ])),
            FrontDoor::Explain
        );
        assert_eq!(
            classify_front_door(&args(&["fix", "paper.tex", "--dry-run"])),
            FrontDoor::Fix
        );
        assert_eq!(
            input_argument(&args(&[
                "fix",
                "--model",
                "claude-example",
                "paper.tex",
                "--apply",
                "--allow-source-upload",
            ])),
            Some("paper.tex")
        );
        assert_eq!(
            classify_front_door(&args(&["fix", "paper.tex"])),
            FrontDoor::Fix
        );
        assert_eq!(
            classify_front_door(&args(&["explain", "paper.tex", "--context", "diagnostic"])),
            FrontDoor::Explain
        );
        assert_eq!(
            classify_front_door(&args(&["fix", "paper.tex", "--context", "diagnostic"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&[
                "fix",
                "paper.tex",
                "--json",
                "--route",
                "local",
                "--max-attempts",
                "3",
            ])),
            FrontDoor::Fix
        );
        assert_eq!(
            classify_front_door(&args(&["fix", "paper.tex", "--dry-run", "--apply",])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["fix", "paper.tex", "--apply", "--prompt-only"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&[
                "explain",
                "paper.tex",
                "--prompt-only",
                "--prompt-only"
            ])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["build", "paper.tex", "--ini"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["fork-watch", "paper.tex"])),
            FrontDoor::ForkWatch
        );
        assert_eq!(
            classify_front_door(&args(&["watch", "paper.tex"])),
            FrontDoor::Watch
        );
        assert_eq!(
            classify_front_door(&args(&["watch", "--stdin", "paper.tex"])),
            FrontDoor::Watch
        );
        assert_eq!(
            classify_front_door(&args(&["fork-watch", "--stdin", "paper.tex"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["watch", "--stdin", "--stdin", "paper.tex"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            input_argument(&args(&["fork-watch", "paper.tex"])),
            Some("paper.tex")
        );
        assert_eq!(
            classify_front_door(&args(&["./-paper.tex"])),
            FrontDoor::Build
        );
        assert_eq!(classify_front_door(&args(&["--help"])), FrontDoor::Help);
        assert_eq!(classify_front_door(&args(&["-h"])), FrontDoor::Help);
        assert_eq!(
            classify_front_door(&args(&["--ini", "paper.tex"])),
            FrontDoor::Run
        );
        assert_eq!(
            classify_front_door(&args(&["-ini", "paper.tex"])),
            FrontDoor::Run
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "--verbose"])),
            FrontDoor::Build
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "-v", "--verbose"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["--ini", "--fmt", "latex.afmt", "paper.tex"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["--version"])),
            FrontDoor::Version
        );
        assert_eq!(classify_front_door(&args(&["-V"])), FrontDoor::Version);
        assert_eq!(
            classify_front_door(&args(&["--unknown"])),
            FrontDoor::UsageError
        );
        assert_eq!(classify_front_door(&[]), FrontDoor::UsageError);
        let unicode = args(&[
            "--unicode",
            "--font",
            "Junicode VF",
            "--font-path",
            "fonts",
            "--font-path",
            "vendor/fonts",
            "--features",
            "-liga",
            "paper.tex",
        ]);
        assert_eq!(classify_front_door(&unicode), FrontDoor::Build);
        assert_eq!(input_argument(&unicode), Some("paper.tex"));
        assert_eq!(
            classify_front_door(&args(&["cache", "inspect", ".cache"])),
            FrontDoor::CacheInspect
        );
        assert_eq!(
            classify_front_door(&args(&["cache", "warm", "paper.tex"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["cache", "warm", "paper.tex", "-o", "paper.pdf"])),
            FrontDoor::CacheWarm
        );
        assert_eq!(
            input_argument(&args(&["cache", "warm", "paper.tex"])),
            Some("paper.tex")
        );
        assert_eq!(
            classify_front_door(&args(&["cache", "prune", ".cache"])),
            FrontDoor::CachePrune
        );

        let options_first = args(&[
            "-file-line-error",
            "-interaction=nonstopmode",
            "--fmt",
            "latex.afmt",
            "paper.tex",
            "-o",
            "paper.pdf",
        ]);
        assert_eq!(classify_front_door(&options_first), FrontDoor::Build);
        assert_eq!(input_argument(&options_first), Some("paper.tex"));
        assert_eq!(probe_argument(&options_first), None);
        assert_eq!(
            probe_argument(&args(&[
                "--fmt",
                "latex.afmt",
                "paper.tex",
                "--probe",
                "foo,bar",
                "-o",
                "paper.pdf",
            ])),
            Some("foo,bar")
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "stray"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "--fmt"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "--fmt", "a", "--fmt", "b"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "-interaction=eventually"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "-o", "paper.unknown"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "-o", "/dev/null"])),
            FrontDoor::Build
        );
        assert_eq!(
            classify_front_door(&args(&["paper.tex", "--cache", ".cache", "--no-cache",])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["cache", "inspect", ".cache", "extra"])),
            FrontDoor::UsageError
        );
        assert_eq!(
            classify_front_door(&args(&["cache", "prune", ".cache", "--max-bytes", "many",])),
            FrontDoor::UsageError
        );
    }

    #[test]
    fn unicode_project_config_is_relative_overridable_and_exact() {
        let root = std::env::temp_dir().join(format!(
            "aitex-unicode-config-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let fonts = root.join("fonts");
        let extra = root.join("extra");
        std::fs::create_dir_all(&fonts).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        let input = root.join("paper.tex");
        std::fs::write(&input, "text").unwrap();
        std::fs::write(
            root.join(".aitex.toml"),
            "[unicode]\nfont = \"Project Serif\"\nfont-paths = [\"fonts\"]\nlanguage = \"ar\"\nfeatures = \"Ligatures=Common\"\ndirection = \"rtl\"\n",
        )
        .unwrap();
        let arguments = vec![
            String::from("--font"),
            String::from("CLI Serif"),
            String::from("--font-path"),
            extra.to_string_lossy().into_owned(),
            input.to_string_lossy().into_owned(),
        ];
        let options = load_unicode_options(input.to_str().unwrap(), &arguments).unwrap();
        assert_eq!(options.family.as_deref(), Some("CLI Serif"));
        assert_eq!(options.language.as_deref(), Some("ar"));
        assert_eq!(options.direction.as_deref(), Some("rtl"));
        assert_eq!(options.font_directories[0], fonts);
        assert_eq!(options.font_directories[1], extra);
        let before = options.identity();
        let mut changed = options;
        changed.direction = Some(String::from("ltr"));
        assert_ne!(before, changed.identity());
        assert!(
            unicode_compat_file("fontspec.sty")
                .unwrap()
                .contains("setmainfont")
        );
        assert!(unicode_compat_file("polyglossia.sty").is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn implicit_cache_uses_xdg_or_the_native_platform_location() {
        use std::ffi::OsStr;
        use std::path::PathBuf;

        assert_eq!(
            platform_cache_root(
                Some(OsStr::new("/var/cache/me")),
                Some(OsStr::new("/home/me")),
                false,
                Some("work/main.tex"),
            ),
            PathBuf::from("/var/cache/me/aitex")
        );
        assert_eq!(
            platform_cache_root(
                None,
                Some(OsStr::new("/home/me")),
                false,
                Some("work/main.tex"),
            ),
            PathBuf::from("/home/me/.cache/aitex")
        );
        assert_eq!(
            platform_cache_root(
                Some(OsStr::new("relative-cache")),
                Some(OsStr::new("/home/me")),
                false,
                Some("work/main.tex"),
            ),
            PathBuf::from("/home/me/.cache/aitex")
        );
        assert_eq!(
            platform_cache_root(
                None,
                Some(OsStr::new("/Users/me")),
                true,
                Some("work/main.tex"),
            ),
            PathBuf::from("/Users/me/Library/Caches/aiTeX")
        );
        assert_eq!(
            platform_cache_root(None, None, false, Some("work/main.tex")),
            PathBuf::from("work/.aitex-cache")
        );
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).into())
                .collect::<Vec<_>>()
        };
        assert_eq!(cache_root_for_arguments(&args(&["pass", "main.tex"])), None);
        assert_eq!(
            cache_root_for_arguments(&args(&["pass", "main.tex", "--cache", ".project-cache"]))
                .as_deref(),
            Some(".project-cache")
        );
    }

    #[test]
    fn local_format_uses_an_adjacent_manifest() {
        assert_eq!(
            super::format_manifest_path(std::path::Path::new(
                "/home/me/.cache/aitex/formats/latex.afmt"
            )),
            std::path::PathBuf::from("/home/me/.cache/aitex/formats/latex.afmt.manifest")
        );
        assert!(
            super::developer_format_candidates(std::path::Path::new(
                "/src/aiTeX/target/release/aitex"
            ))
            .contains(&std::path::PathBuf::from(
                "/src/aiTeX/.aitex/format/fmt.afmt"
            ))
        );
    }

    fn png_with_phys(width: u32, height: u32, x_ppm: u32, y_ppm: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            png.extend_from_slice(&0u32.to_be_bytes()); // parsers here do not consume CRCs
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        let mut phys = Vec::new();
        phys.extend_from_slice(&x_ppm.to_be_bytes());
        phys.extend_from_slice(&y_ppm.to_be_bytes());
        phys.push(1);
        chunk(b"pHYs", &phys);
        chunk(b"IEND", &[]);
        png
    }

    #[test]
    fn font_specific_glyph_mapping_precedes_the_global_name() {
        let explicit = [
            ("a20".to_string(), "0013".to_string()),
            ("tfm:pzdr/a20".to_string(), "2714".to_string()),
            ("quoteleft".to_string(), "2018".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            unicode_for_glyph("pzdr", "a20", &explicit).as_deref(),
            Some("2714")
        );
        assert_eq!(
            unicode_for_glyph("other", "a20", &explicit).as_deref(),
            Some("0013")
        );
        assert_eq!(
            unicode_for_glyph("bbding10", "enc-33", &Default::default()).as_deref(),
            Some("2713")
        );
        assert_eq!(
            unicode_for_glyph("bbding10", "enc-37", &Default::default()).as_deref(),
            Some("2717")
        );
        assert_eq!(
            unicode_for_bitmap_slot("ectt0800", "ectt0800", 61, "equal", &Default::default())
                .as_deref(),
            Some("003D")
        );
        assert_eq!(
            unicode_for_bitmap_slot(
                "tctt0800",
                "tctt0800",
                42,
                "asteriskmath",
                &Default::default()
            )
            .as_deref(),
            Some("002A")
        );
        assert_eq!(
            unicode_for_bitmap_slot("larm1000", "larm1000", 96, "quoteleft", &explicit).as_deref(),
            Some("2018")
        );
    }

    #[test]
    fn png_density_drives_the_dvipdfmx_physical_bbox() {
        let png = png_with_phys(1184, 890, 5669, 5669);
        assert_eq!(png_resolution(&png), Some((144, 144)));
        assert_eq!(
            raster_natural_bbox("unused.png", &png),
            Some([0.0, 0.0, 591.99857, 444.99892])
        );
    }

    #[test]
    fn adam7_pngs_avoid_netpbms_corrupt_row_decoder() {
        let args = |command: &std::process::Command| {
            command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            args(&pngtopam_command("figure.png", true, 0)),
            ["-quiet", "-byrow", "-alpha", "figure.png"]
        );
        assert_eq!(
            args(&pngtopam_command("figure.png", true, 1)),
            ["-quiet", "-alpha", "figure.png"]
        );
    }

    #[test]
    fn jpeg_jfif_density_drives_the_dvipdfmx_physical_bbox() {
        let mut jpeg = b"\xff\xd8\xff\xe0\x00\x10JFIF\0\x01\x02\x01\x00\x96\x00\x96\0\0".to_vec();
        jpeg.extend_from_slice(
            b"\xff\xc0\x00\x11\x08\x00\x96\x01\x2c\x03\x01\x11\x00\x02\x11\x00\x03\x11\x00\xff\xd9",
        );
        assert_eq!(jpeg_xbb_size(&jpeg), Some((144.0, 72.0)));
        assert_eq!(
            raster_natural_bbox("unused.jpg", &jpeg),
            Some([0.0, 0.0, 144.0, 72.0])
        );
    }

    #[test]
    fn jpeg_jfif_centimetre_density_matches_pdftex_integer_dpi() {
        let mut jpeg = b"\xff\xd8\xff\xe0\x00\x10JFIF\0\x01\x02\x02\x00\x25\x00\x25\0\0".to_vec();
        jpeg.extend_from_slice(
            b"\xff\xc0\x00\x11\x08\x04\x38\x07\x80\x03\x01\x11\x00\x02\x11\x00\x03\x11\x00\xff\xd9",
        );
        let (width, height) = jpeg_xbb_size(&jpeg).expect("valid JPEG dimensions");
        assert!((width - 1920.0 * (72.0 / 93.0)).abs() < 1e-9);
        assert!((height - 1080.0 * (72.0 / 93.0)).abs() < 1e-9);
    }

    #[test]
    fn jpeg_exif_density_drives_the_dvipdfmx_physical_bbox() {
        let mut jpeg = b"\xff\xd8\xff\xe1\x00\x4aExif\0\0MM\0\x2a\0\0\0\x08\0\x03".to_vec();
        jpeg.extend_from_slice(
            b"\x01\x1a\0\x05\0\0\0\x01\0\0\0\x32\x01\x1b\0\x05\0\0\0\x01\0\0\0\x3a\x01\x28\0\x03\0\0\0\x01\0\x02\0\0\0\0\0\0\0\0\x01\x2c\0\0\0\x01\0\0\x01\x2c\0\0\0\x01",
        );
        jpeg.extend_from_slice(
            b"\xff\xc0\x00\x11\x08\x02\x9a\x03\xe8\x03\x01\x11\x00\x02\x11\x00\x03\x11\x00\xff\xd9",
        );
        assert_eq!(jpeg_xbb_size(&jpeg), Some((240.0, 159.84)));
        assert_eq!(
            raster_natural_bbox("unused.jpg", &jpeg),
            Some([0.0, 0.0, 240.0, 159.84])
        );
    }

    #[test]
    fn eps_dsc_bbox_survives_the_procedure_heavy_pdf_fallback() {
        let eps = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: (atend)\nshowpage\n\
                    %%BoundingBox: 17 18 437 333\n%%HiResBoundingBox: 17.0 18.0 437.25 333.5\n";
        assert_eq!(eps_natural_bbox(eps), Some([0.0, 0.0, 420.25, 315.5]));
        assert_eq!(
            raster_natural_bbox("unused.eps", eps),
            Some([0.0, 0.0, 420.25, 315.5])
        );
    }

    #[test]
    fn very_large_streamed_rgba_pngs_reach_the_native_normalizer() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            png.extend_from_slice(&0u32.to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&24_000u32.to_be_bytes());
        ihdr.extend_from_slice(&20_400u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        chunk(b"IEND", &[]);

        // The deliberately tiny IDAT cannot decode the advertised image, but
        // failure must now happen while decoding rather than at the streamed
        // dimension gate.
        assert!(rgba_png_idat(&png).is_some());
        assert!(normalize_rgba_png(&png).is_none());
    }

    #[test]
    fn rgba_png_normalization_produces_compressed_rgb_and_alpha_planes() {
        // Transparency first appears on the second row, exercising the lazy
        // alpha encoder's reconstruction of the preceding opaque row.
        let raw = [
            0, 10, 20, 30, 255, 40, 50, 60, 255, 0, 70, 80, 90, 255, 100, 110, 120, 128,
        ];
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let idat = encoder.finish().unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            png.extend_from_slice(&0u32.to_be_bytes());
        };
        chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0, 0, 0]);
        chunk(b"IDAT", &idat);
        chunk(b"IEND", &[]);

        let bundle = normalize_rgba_png(&png).expect("native RGBA normalization");
        let payload = bundle
            .strip_prefix(b"AITEX-PNG-ZLIB\0")
            .expect("native bundle marker");
        assert_eq!(&payload[..8], &[0, 0, 0, 2, 0, 0, 0, 2]);
        let rgb_len = u64::from_be_bytes(payload[8..16].try_into().unwrap()) as usize;
        let alpha_len = u64::from_be_bytes(payload[16..24].try_into().unwrap()) as usize;
        let decode = |bytes: &[u8]| {
            let mut decoded = Vec::new();
            let mut decoder = flate2::read::ZlibDecoder::new(bytes);
            std::io::Read::read_to_end(&mut decoder, &mut decoded).expect("zlib stream");
            decoded
        };
        let rgb = decode(&payload[24..24 + rgb_len]);
        let alpha = decode(&payload[24 + rgb_len..24 + rgb_len + alpha_len]);
        assert_eq!(rgb, [1, 10, 20, 30, 30, 30, 30, 4, 60, 60, 60, 30, 30, 30]);
        assert_eq!(alpha, [1, 255, 0, 4, 0, 129]);
    }

    #[test]
    fn top_level_file_gets_texs_conceptual_final_endline() {
        for (input, expected) in [
            ("", ""),
            ("abc", "abc\n"),
            ("abc\n", "abc\n"),
            ("abc\r", "abc\r"),
            ("abc\r\n", "abc\r\n"),
        ] {
            let mut source = input.to_string();
            complete_file_source(&mut source);
            assert_eq!(source, expected);
        }
    }

    #[test]
    fn pdf_source_pins_latex_and_late_graphics_packages_to_dvipdfmx() {
        let root = std::env::temp_dir().join(format!(
            "aitex-pdf-driver-source-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("main.tex");
        std::fs::write(
            &input,
            b"\\documentclass{article}\\begin{document}x\\end{document}",
        )
        .unwrap();
        let source = source_for_run(input.to_str().unwrap(), true, None).unwrap();
        assert!(source.contains("\\sys_load_backend:n{dvipdfmx}"));
        assert!(source.contains("\\ExplSyntaxOff\\pdfoutput=1"));
        assert!(!source.contains("\\AddToHook{class/before}"));
        assert!(!source.contains("\\PassOptionsToPackage{dvipdfmx}{color}"));
        assert!(!source.contains("\\PassOptionsToPackage{dvipdfmx}{graphicx}"));
        assert!(source.contains("\\AddToHook{package/color/after}"));
        assert!(source.contains("\\AddToHook{package/xcolor/after}"));
        assert!(source.contains(concat!(
            "\\AddToHook{package/graphics/after}{",
            "\\aitex@observegraphicsdriver\\aitex@ensuregraphicsdriver}",
        )));
        assert!(source.contains(concat!(
            "\\AddToHook{package/epstopdf-base/before}{",
            "\\ifaitex@explicitdvips",
            "\\ifaitex@pdftexoverride\\aitex@prepare@epstopdf\\fi",
            "\\else\\ifaitex@explicitdvipdfmx",
        )));
        assert!(source.contains("\\AddToHook{package/pinlabel/after}"));
        assert!(source.contains("\\chardef\\pdflastximagepages=128"));
        assert!(source.contains("\\def\\pdfximage cropbox ##1{\\Gread@pdf{##1.xbb}}"));
        assert!(source.contains(".pdf.aitex.\\Gin@page.\\Gin@pagebox.xbb"));
        assert!(source.contains(".ai.aitex.\\Gin@page.\\Gin@pagebox.xbb"));
        assert!(source.contains("\\def\\aitex@draftsafe@Gread##1##2"));
        assert!(source.contains(concat!(
            "\\def\\aitex@pdf@begin{",
            "\\ifaitex@pdfbegun\\else",
            "\\global\\aitex@pdfbeguntrue",
            "\\@ifundefined{paperwidth}{}",
            "{\\AtBeginDvi{\\special{aitex:latex-page-size ",
        )));
        assert!(source.contains("\\newif\\ifaitex@pdfbegun"));
        assert!(source.contains(concat!(
            "\\global\\aitex@explicitdvipstrue",
            "\\global\\aitex@pdftexoverridefalse",
        )));
        assert!(source.contains("\\AtBeginDocument{\\aitex@pdf@begin}"));
        assert!(source.contains(concat!(
            "\\ifx\\aitex@requiredpackage\\aitex@latexrelease",
            "\\AtBeginDocument{\\aitex@pdf@begin}",
        )));
        assert!(source.contains(concat!(
            "\\def\\aitex@pdf@begin@dvips{",
            "\\AtBeginDvi{\\special{aitex:pdftex-ignore-postscript}}",
        )));
        assert!(source.contains("\\special{aitex:pdftex-explicit-dvipdfmx}"));
        assert!(source.contains("\\special{aitex:pdftex-ignore-next-driver-graphic}"));
        assert!(source.contains("\\def\\aitex@pdftexdriver{pdftex.def}"));
        assert!(source.contains("\\global\\aitex@pdftexoverridetrue"));
        assert!(source.contains(concat!(
            "\\ifaitex@explicitdvipdfmx",
            "\\AtBeginDvi{\\special{aitex:pdftex-explicit-dvipdfmx}}",
            "\\ifaitex@pdftexoverride\\else",
        )));
        assert!(source.contains(concat!(
            "\\special{aitex:eps-size \\the\\Gin@req@width,",
            "\\the\\Gin@req@height}",
        )));
        assert!(source.contains(concat!(
            "\\let\\aitex@pgfsys@defineimage\\pgfsys@defineimage",
            "\\def\\pgfsys@defineimage{",
            "\\ifx\\pgf@imagewidth\\@empty",
            "\\ifx\\pgf@imageheight\\@empty",
            "\\ifx\\pgf@imagemask\\@empty",
        )));
        assert!(source.contains(concat!(
            "\\edef\\pgf@image{\\noexpand\\includegraphics",
            "[page=\\pgf@imagepage]{\\pgf@filename}}",
        )));
        assert!(source.contains(concat!(
            "\\@ifundefined{Gread@eps}{}{",
            "\\let\\aitex@saved@Gread@eps\\Gread@eps",
            "\\@ifundefined{ifaitex@epspdf}{\\newif\\ifaitex@epspdf}{}",
        )));
        assert!(source.contains(concat!(
            "\\@ifpackageloaded{epstopdf-base}{",
            "\\@ifundefined{aitex@epstopdf@epsrule}{",
            "\\aitex@epstopdfdrafttrue",
            "\\let\\Ginclude@eps\\relax",
        )));
        assert!(source.contains(concat!(
            "\\AddToHook{begindocument/before}{",
            "\\ifaitex@epstopdfneeded",
            "\\def\\aitex@load@epstopdf{",
            "\\@ifpackageloaded{graphics}{",
            "\\@ifundefined{DoNotLoadEpstopdf}{",
        )));
        assert!(source.contains(concat!(
            "\\ifaitex@explicitdvips",
            "\\ifaitex@pdftexoverride\\aitex@load@epstopdf\\fi",
            "\\else\\ifaitex@explicitdvipdfmx",
            "\\ifaitex@pdftexoverride\\aitex@load@epstopdf\\fi",
            "\\else\\aitex@load@epstopdf\\fi\\fi",
        )));
        assert!(source.contains(
            "\\@ifpackageloaded{epstopdf-base}{}{\\RequirePackage{epstopdf-base}[2009/07/12]}"
        ));
        assert!(source.contains("\\edef\\aitex@epspdf{\\Gin@base-eps-converted-to.pdf}"));
        assert!(source.contains(concat!(
            "\\Gread@generic{##1.aitex-eps-normalized.xbb}",
            "\\Gread@eps@aux",
        )));
        assert!(source.contains(concat!(
            "\\IfFileExists{\\aitex@epspdf}{",
            "\\aitex@epstopdf@setfile{##1}{##2}{##3}",
            "}{\\IfFileExists{\\Gin@base\\Gin@ext}{",
            "\\aitex@epstopdf@original@setfile{eps}{.eps}{\\Gin@base\\Gin@ext}",
        )));
        assert!(source.contains(concat!(
            "\\@ifundefined{pgfsys@invoke}{}{",
            "\\@ifundefined{pgfutil@everybye}{}{",
            "\\expandafter\\pgfutil@everybye\\expandafter{\\the\\pgfutil@everybye\\relax}",
            "}}",
        )));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn synthetic_xbb_tracks_the_selected_asset_across_extension_collisions() {
        let png =
            synthetic_xbb_candidates("K8a14.xbb", Some("./K8a14.png")).expect("sidecar candidates");
        assert_eq!(png.first().map(String::as_str), Some("./K8a14.png"));
        let pdf =
            synthetic_xbb_candidates("K8a14.xbb", Some("./K8a14.pdf")).expect("sidecar candidates");
        assert_eq!(pdf.first().map(String::as_str), Some("./K8a14.pdf"));
        assert_eq!(
            synthetic_xbb_candidates("K8a14.png.xbb", Some("./K8a14.pdf")),
            Some(vec!["K8a14.png".to_string()])
        );
        assert_eq!(
            synthetic_xbb_candidates("figure.ai.aitex..artbox.xbb", None),
            Some(vec!["figure.ai".to_string()])
        );
        assert_eq!(
            synthetic_xbb_candidates("figure.eps.aitex-eps-normalized.xbb", None),
            Some(vec!["figure.eps".to_string()])
        );
    }

    #[test]
    fn pdf_content_array_detection_ignores_an_indirect_stream() {
        assert!(pdf_page_content_is_array(
            b"<< /Type /Page /Contents [12 0 R 13 0 R] >>"
        ));
        assert!(pdf_page_content_is_array(
            b"<< /Type/Page /Contents\n\t[12 0 R] >>"
        ));
        assert!(!pdf_page_content_is_array(
            b"<< /Type /Page /Contents 12 0 R >>"
        ));
        assert!(!pdf_page_content_is_array(
            b"<< /Type /Page /OtherContents [12 0 R] >>"
        ));
    }

    #[test]
    fn hot_pdf_files_deduplicate_content_and_observe_rewrites() {
        let root = std::env::temp_dir().join(format!(
            "aitex-pdf-cache-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("first.enc");
        let second = root.join("second.enc");
        std::fs::write(&first, b"same bytes").unwrap();
        std::fs::write(&second, b"same bytes").unwrap();

        let mut cache = HotFileCache::default();
        let (first_digest, first_bytes) = cache.read(first.to_str().unwrap()).unwrap();
        let (second_digest, second_bytes) = cache.read(second.to_str().unwrap()).unwrap();
        assert_eq!(first_digest, second_digest);
        assert!(std::sync::Arc::ptr_eq(&first_bytes, &second_bytes));

        std::fs::write(&first, b"different bytes and size").unwrap();
        let (changed_digest, changed_bytes) = cache.read(first.to_str().unwrap()).unwrap();
        assert_ne!(changed_digest, first_digest);
        assert_eq!(changed_bytes.as_ref(), b"different bytes and size");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parsed_dvi_identity_includes_bytes_rotations_and_implicit_widths() {
        let names = vec!["cmr10".to_string()];
        let mut font = aitex_engine::tfm::TfmFont::default();
        font.at_size = aitex_engine::scaled::Scaled(655_360);
        let baseline = dvi_document_key(b"dvi", &[font.clone()], &names, &[0]);
        assert_ne!(
            baseline,
            dvi_document_key(b"changed", &[font.clone()], &names, &[0])
        );
        assert_ne!(
            baseline,
            dvi_document_key(b"dvi", &[font.clone()], &names, &[90])
        );
        font.at_size = aitex_engine::scaled::Scaled(786_432);
        assert_ne!(baseline, dvi_document_key(b"dvi", &[font], &names, &[0]));

        let mut cache = HotDviDocumentCache::default();
        let inserted = cache.insert(baseline.clone(), aitex_driver::dvi::Document::default());
        let hit = cache.get(&baseline).expect("cached page events");
        assert!(std::rc::Rc::ptr_eq(&inserted, &hit));
    }

    #[test]
    fn pdf_output_identity_distinguishes_stream_and_type1_modes() {
        let glyphs = std::collections::BTreeMap::new();
        let compact = aitex_driver::pdf::PdfOptions::default();
        assert_ne!(
            pdf_output_key("document", &glyphs, compact),
            pdf_output_key(
                "document",
                &glyphs,
                aitex_driver::pdf::PdfOptions::uncompressed(),
            ),
        );
        assert_ne!(
            pdf_output_key("document", &glyphs, compact),
            pdf_output_key("document", &glyphs, compact.with_type1_subsetting(false),),
        );
    }

    #[test]
    fn pdf_compression_worker_budget_is_bounded_and_overridable() {
        use std::ffi::OsStr;

        assert_eq!(pdf_compression_workers(None, 12), 4);
        assert_eq!(pdf_compression_workers(Some(OsStr::new("2")), 12), 2);
        assert_eq!(pdf_compression_workers(Some(OsStr::new("20")), 12), 4);
        assert_eq!(pdf_compression_workers(Some(OsStr::new("4")), 2), 2);
        assert_eq!(pdf_compression_workers(Some(OsStr::new("0")), 12), 4);
        assert_eq!(pdf_compression_workers(Some(OsStr::new("invalid")), 12), 4);
    }

    #[test]
    fn decoded_base_format_is_reused_and_content_invalidated() {
        let root = std::env::temp_dir().join(format!(
            "aitex-hot-format-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("format.afmt");

        let first = aitex_engine::fast::Fast::new();
        std::fs::write(&path, first.dump().unwrap()).unwrap();
        let (_, decoded, source) = restore_base_format(path.to_str().unwrap(), true, None).unwrap();
        assert!(decoded);
        assert_eq!(source, "decoded+retained");

        let (_, decoded, source) = restore_base_format(path.to_str().unwrap(), true, None).unwrap();
        assert!(!decoded);
        assert_eq!(source, "hot-clone");

        let mut changed = aitex_engine::fast::Fast::new();
        changed.run(r"\def\formatidentitychanged{yes}");
        std::fs::write(&path, changed.dump().unwrap()).unwrap();
        let (_, decoded, source) = restore_base_format(path.to_str().unwrap(), true, None).unwrap();
        assert!(decoded);
        assert_eq!(source, "decoded+retained");

        std::fs::remove_dir_all(root).unwrap();
    }
}

include!("cli/kpse.rs");
include!("cli/dvi.rs");
include!("cli/cache.rs");
include!("cli/format.rs");
include!("cli/incremental.rs");

/// The value after a flag, e.g. `-o out.dvi` or `--fmt latex.afmt`.
fn arg_after(flag: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == flag)
        .and_then(|i| a.get(i + 1))
        .cloned()
}

/// Resolve the preloaded LaTeX format. Explicit overrides remain authoritative;
/// normal document builds validate or create an engine- and TeX-matched format
/// in the user's platform cache.
fn format_argument() -> Option<String> {
    let explicit = arg_after("--fmt").or_else(|| std::env::var("AITEX_FORMAT").ok());
    if explicit.is_some()
        || std::env::args().any(|argument| matches!(argument.as_str(), "--dump" | "-ini" | "--ini"))
    {
        return explicit;
    }
    LOCAL_FORMAT.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(
                prepare_latex_format(None).map(|(path, _)| path.to_string_lossy().into_owned()),
            );
        }
        match slot.borrow().as_ref().expect("format result initialized") {
            Ok(path) => Some(path.clone()),
            Err(error) => {
                eprintln!("aitex: cannot prepare the local LaTeX format: {error}");
                std::process::exit(1);
            }
        }
    })
}

/// `-file-line-error`, as every other TeX engine spells it: report errors as
/// `file:line: message` instead of TeX's default `! message.` + `l.N`.
/// `aitex-harness` passes it to pdfTeX already (docs/history/PLAN.md §5); it can pass it here
/// too and get the same shape back.
fn file_line_error_flag() -> bool {
    std::env::args().any(|a| a == "-file-line-error" || a == "--file-line-error")
}

/// `-halt-on-error`, matching the option accepted by pdfTeX. aiTeX otherwise
/// uses nonstop recovery so an ordinary interactive build can report several
/// independent problems in one pass.
fn halt_on_error_flag() -> bool {
    std::env::args().any(|a| a == "-halt-on-error" || a == "--halt-on-error")
}

/// Canonical TeX82 compatibility for conformance suites such as TRIP. aiTeX's
/// default remains e-TeX because current LaTeX requires its extended register
/// bank and primitives.
fn tex82_flag() -> bool {
    std::env::args().any(|a| a == "--tex82")
}

fn unicode_flag() -> bool {
    std::env::args().any(|argument| argument == "--unicode")
}

include!("cli/pdf.rs");

/// `-o <file>` names the DVI/PDF output. It is a synonym for `AITEX_DVI`; the
/// flag wins if both are given.
fn dvi_flag() -> Option<String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    output_argument(&arguments)
}

include!("cli/transcript.rs");
include!("cli/run.rs");
