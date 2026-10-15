/// One long-lived `kpsewhich` instead of one per lookup.
///
/// A `latex.ltx` load asks for ~50 distinct names, and a fresh `kpsewhich`
/// process costs **5 ms** — a quarter of a second of `fork`/`exec` and kpathsea
/// start-up, which showed up in the profile as `poll`/`read_output` and has
/// nothing to do with the engine. pdfTeX links kpathsea in-process and pays
/// microseconds; this is how the comparison stays about the engines.
///
/// `-interactive` reads further names from stdin. Linux uses `stdbuf -oL`, and
/// macOS gives MacTeX a pseudo-terminal, so kpsewhich does not block-buffer its
/// replies. A *miss* prints nothing at all, so every query is followed by a
/// sentinel whose answer is known — the reply ends when that path comes back,
/// and anything before it is the hit.
struct Kpse {
    child: std::process::Child,
    out: Box<dyn std::io::BufRead>,
    sentinel_request: String,
    sentinel: String,
}

/// Keep the long-lived resolver out of the terminal's foreground process
/// group. Terminal emulators on macOS and Linux commonly derive a tab's label
/// from every process in that group; without this separation an `aitex watch`
/// tab is therefore advertised as `kpsewhich` for the whole session even
/// though aiTeX owns the service and the helper only answers private pipes.
///
/// The helper has no terminal input or output, so job-control isolation does
/// not change its protocol. It remains a child and is still reaped by `Drop`.
#[cfg(unix)]
fn isolate_resolver_process_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_resolver_process_group(_command: &mut std::process::Command) {}

#[derive(Clone, Copy, Debug, Default)]
struct ResolverMetrics {
    enabled: bool,
    local_queries: u64,
    local_hits: u64,
    helper_starts: u64,
    helper_start_failures: u64,
    helper_start: std::time::Duration,
    helper_queries: u64,
    helper_hits: u64,
    helper_query: std::time::Duration,
    fallback_processes: u64,
    fallback_process: std::time::Duration,
    pk_processes: u64,
    generated_pk: u64,
    pk_process: std::time::Duration,
    extractbb_processes: u64,
    extractbb_process: std::time::Duration,
    pdfinfo_processes: u64,
    pdfinfo_process: std::time::Duration,
}

thread_local! {
    /// `aitex watch` keeps one kpathsea helper alive across rebuilds. A normal
    /// CLI invocation still owns it only for the lifetime of that process.
    static HOT_KPSE: std::cell::RefCell<Option<Kpse>> =
        const { std::cell::RefCell::new(None) };
    static HOT_TEX_KPSE: std::cell::RefCell<Option<Kpse>> =
        const { std::cell::RefCell::new(None) };
    static HOT_PK_KPSE: std::cell::RefCell<std::collections::BTreeMap<String, Kpse>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static RESOLVER_METRICS: std::cell::RefCell<ResolverMetrics> =
        const { std::cell::RefCell::new(ResolverMetrics {
            enabled: false,
            local_queries: 0,
            local_hits: 0,
            helper_starts: 0,
            helper_start_failures: 0,
            helper_start: std::time::Duration::ZERO,
            helper_queries: 0,
            helper_hits: 0,
            helper_query: std::time::Duration::ZERO,
            fallback_processes: 0,
            fallback_process: std::time::Duration::ZERO,
            pk_processes: 0,
            generated_pk: 0,
            pk_process: std::time::Duration::ZERO,
            extractbb_processes: 0,
            extractbb_process: std::time::Duration::ZERO,
            pdfinfo_processes: 0,
            pdfinfo_process: std::time::Duration::ZERO,
        }) };
}

fn reset_resolver_metrics(enabled: bool) {
    RESOLVER_METRICS.with(|metrics| {
        *metrics.borrow_mut() = ResolverMetrics {
            enabled,
            ..ResolverMetrics::default()
        };
    });
}

fn resolver_metrics() -> ResolverMetrics {
    RESOLVER_METRICS.with(|metrics| *metrics.borrow())
}

fn record_resolver_metric(update: impl FnOnce(&mut ResolverMetrics)) {
    RESOLVER_METRICS.with(|metrics| {
        let mut metrics = metrics.borrow_mut();
        if metrics.enabled {
            update(&mut metrics);
        }
    });
}

impl Drop for Kpse {
    fn drop(&mut self) {
        // `Child` closes its pipes but deliberately does not wait on drop.
        // Close interactive stdin first so kpsewhich observes EOF, then reap
        // it. Besides avoiding a short-lived orphan, this charges the helper's
        // CPU and RSS to benchmark/resource accounting for the compiler that
        // actually requested the path lookups.
        drop(self.child.stdin.take());
        if self.child.wait().is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

const SENTINEL: &str = "texmf.cnf";
const TEX_SENTINEL: &str = "latex.ltx";

impl Kpse {
    fn start() -> Option<Self> {
        Self::start_with(&[], SENTINEL)
    }

    fn start_tex() -> Option<Self> {
        // The engine asks this resolver for TeX inputs, irrespective of their
        // suffix.  Without an explicit format, standalone `kpsewhich` guesses
        // from the extension and does not classify project inputs such as
        // `.pgf` or `.tikz` as TeX.  The engines themselves always perform a
        // `kpse_tex_format` lookup, including its configured case-fold search.
        Self::start_with(&["--format", "tex"], TEX_SENTINEL)
    }

    fn start_with(arguments: &[&str], sentinel_request: &str) -> Option<Self> {
        use std::process::{Command, Stdio};

        let started = std::time::Instant::now();

        // GNU `stdbuf` is the cheapest way to make kpathsea's interactive
        // protocol line-buffered on Linux. On macOS it relies on a preload
        // mechanism that does not affect MacTeX's `kpsewhich`: the first line
        // remains in stdio's block buffer, our one-second protocol guard
        // expires, and every lookup falls back to a fresh process. A small
        // document then spends roughly one minute waiting on path and font
        // resolution. Giving the child a pseudo-terminal for stdout makes
        // libc select line buffering naturally and keeps the one helper alive.
        #[cfg(target_os = "macos")]
        let result = (|| {
            let (child_stdout, parent_stdout) = macos_line_buffered_stdout()?;
            let mut command = Command::new("kpsewhich");
            isolate_resolver_process_group(&mut command);
            let child = command
                .args(arguments)
                .args(["-interactive", sentinel_request])
                .stdin(Stdio::piped())
                .stdout(child_stdout)
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            Self::finish_start(child, parent_stdout, sentinel_request)
        })();
        #[cfg(not(target_os = "macos"))]
        let result = (|| {
            let mut command = Command::new("stdbuf");
            isolate_resolver_process_group(&mut command);
            command.args(["-oL", "kpsewhich"]);
            let mut child = command
                .args(arguments)
                .args(["-interactive", sentinel_request])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            Self::finish_start(child, stdout, sentinel_request)
        })();
        record_resolver_metric(|metrics| {
            metrics.helper_starts += 1;
            metrics.helper_start += started.elapsed();
            metrics.helper_start_failures += u64::from(result.is_none());
        });
        result
    }

    #[cfg(unix)]
    fn finish_start<R>(
        mut child: std::process::Child,
        stdout: R,
        _sentinel_request: &str,
    ) -> Option<Self>
    where
        R: std::io::Read + std::os::fd::AsRawFd + 'static,
    {
        use std::io::BufRead as _;

        #[cfg(unix)]
        if !initial_kpse_output_ready(&stdout) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let mut out: Box<dyn std::io::BufRead> = Box::new(std::io::BufReader::new(stdout));
        let mut sentinel = String::new();
        out.read_line(&mut sentinel).ok()?;
        let sentinel = sentinel.trim().to_string();
        if sentinel.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        // The first lookup resolves the portable request to an existing
        // absolute path. Use that path for subsequent framing: kpathsea can
        // validate it directly instead of searching every configured tree for
        // `texmf.cnf` after each real query. The returned line is unchanged,
        // so this only removes repeated resolver work from the protocol.
        let sentinel_request = sentinel.clone();
        Some(Self {
            child,
            out,
            sentinel_request,
            sentinel,
        })
    }

    #[cfg(not(unix))]
    fn finish_start<R>(
        mut child: std::process::Child,
        stdout: R,
        _sentinel_request: &str,
    ) -> Option<Self>
    where
        R: std::io::Read + 'static,
    {
        use std::io::BufRead as _;

        let mut out: Box<dyn std::io::BufRead> = Box::new(std::io::BufReader::new(stdout));
        let mut sentinel = String::new();
        out.read_line(&mut sentinel).ok()?;
        let sentinel = sentinel.trim().to_string();
        if sentinel.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let sentinel_request = sentinel.clone();
        Some(Self {
            child,
            out,
            sentinel_request,
            sentinel,
        })
    }

    fn path(&mut self, name: &str) -> Option<String> {
        self.paths(&[name]).pop().flatten()
    }

    fn paths(&mut self, names: &[&str]) -> Vec<Option<String>> {
        use std::io::{BufRead, Write};
        let started = std::time::Instant::now();
        let Some(stdin) = self.child.stdin.as_mut() else {
            record_resolver_metric(|metrics| {
                metrics.helper_queries += names.len() as u64;
                metrics.helper_query += started.elapsed();
            });
            return vec![None; names.len()];
        };
        for name in names {
            if *name != self.sentinel_request
                && writeln!(stdin, "{name}\n{}", self.sentinel_request).is_err()
            {
                record_resolver_metric(|metrics| {
                    metrics.helper_queries += names.len() as u64;
                    metrics.helper_query += started.elapsed();
                });
                return vec![None; names.len()];
            }
        }
        if stdin.flush().is_err() {
            record_resolver_metric(|metrics| {
                metrics.helper_queries += names.len() as u64;
                metrics.helper_query += started.elapsed();
            });
            return vec![None; names.len()];
        }
        let answers = names
            .iter()
            .map(|name| {
                if *name == self.sentinel_request {
                    return Some(self.sentinel.clone());
                }
                let mut hit: Option<String> = None;
                loop {
                    let mut line = String::new();
                    if self.out.read_line(&mut line).ok()? == 0 {
                        return None;
                    }
                    let line = line.trim();
                    if line == self.sentinel {
                        return hit;
                    }
                    if hit.is_none() {
                        hit = Some(line.to_string());
                    }
                }
            })
            .collect::<Vec<_>>();
        let hits = answers.iter().filter(|answer| answer.is_some()).count() as u64;
        record_resolver_metric(|metrics| {
            metrics.helper_queries += names.len() as u64;
            metrics.helper_hits += hits;
            metrics.helper_query += started.elapsed();
        });
        answers
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn macos_line_buffered_stdout() -> Option<(std::process::Stdio, std::fs::File)> {
    use std::os::fd::{FromRawFd as _, OwnedFd};

    let mut master = -1;
    let mut slave = -1;
    // SAFETY: `openpty` initializes both descriptors on success. No terminal
    // attributes or window size are requested, and each descriptor is moved
    // immediately into one owning Rust value.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    if opened != 0 {
        return None;
    }
    // SAFETY: successful `openpty` returned two new, uniquely owned fds.
    let parent = unsafe { std::fs::File::from_raw_fd(master) };
    // SAFETY: as above; `Stdio` takes ownership of the slave descriptor.
    let child = unsafe { OwnedFd::from_raw_fd(slave) };
    Some((std::process::Stdio::from(child), parent))
}

#[cfg(unix)]
fn initial_kpse_output_ready(stdout: &impl std::os::fd::AsRawFd) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stdout.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Indexed kpathsea answers in milliseconds. A missing interactive initial
    // request otherwise waits for more stdin forever, so bound that protocol
    // ambiguity before attempting a blocking line read.
    #[allow(unsafe_code)]
    let result = unsafe { libc::poll(&mut descriptor, 1, 1_000) };
    result > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0
}

fn resolve_paths_hot(names: &[&str], ext: &str) -> Vec<Option<String>> {
    let mut resolved = names
        .iter()
        .map(|name| resolve_local_path(name, ext))
        .collect::<Vec<_>>();
    let local_hits = resolved.iter().filter(|path| path.is_some()).count() as u64;
    record_resolver_metric(|metrics| {
        metrics.local_queries += names.len() as u64;
        metrics.local_hits += local_hits;
    });
    let pending = resolved
        .iter()
        .enumerate()
        .filter_map(|(index, path)| path.is_none().then_some(index))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return resolved;
    }
    HOT_KPSE.with(|helper| {
        let mut helper = helper.borrow_mut();
        if helper.is_none() {
            *helper = Kpse::start();
        }
        let Some(helper) = helper.as_mut() else {
            return;
        };
        let requests = pending.iter().map(|index| names[*index]).collect::<Vec<_>>();
        let answers = helper.paths(&requests);
        let mut extension_pending = Vec::new();
        for (index, answer) in pending.iter().copied().zip(answers) {
            if answer.is_some() {
                resolved[index] = answer;
            } else if !ext.is_empty() {
                extension_pending.push(index);
            }
        }
        if !extension_pending.is_empty() {
            let extended = extension_pending
                .iter()
                .map(|index| format!("{}{}", names[*index], ext))
                .collect::<Vec<_>>();
            let requests = extended.iter().map(String::as_str).collect::<Vec<_>>();
            for (index, answer) in extension_pending.into_iter().zip(helper.paths(&requests)) {
                resolved[index] = answer;
            }
        }
    });
    resolved
}

fn resolve_pk_path_hot(dpi: &str, name: &str) -> Option<String> {
    #[cfg(not(unix))]
    return resolve_pk_path_once(dpi, name);

    #[cfg(unix)]
    HOT_PK_KPSE.with(|helpers| {
        let mut helpers = helpers.borrow_mut();
        if !helpers.contains_key(dpi) {
            // Interactive framing needs a file guaranteed to resolve at this
            // exact DPI. A fixed `cmr10.pk` sentinel deadlocks at uncommon
            // sizes such as 592dpi. Resolve the real first request once, then
            // use that proven name as this conversion's per-DPI sentinel. The
            // one-shot lookup also permits mktexpk to finish without confusing
            // legitimate generation time with an interactive protocol hang.
            let first_path = resolve_pk_path_once(dpi, name)?;
            if dpi == "600" {
                // The long-lived helper must retain the same generation policy
                // as the bootstrap lookup. Otherwise only the first bitmap at
                // 600dpi is generated and every later font is reported missing
                // from an otherwise complete TeX installation.
                let arguments = ["--mktex=pk", "--dpi", dpi, "--format", "pk"];
                if let Some(helper) = Kpse::start_with(&arguments, name) {
                    helpers.insert(dpi.to_string(), helper);
                }
            }
            return Some(first_path);
        }
        // `None` here is an authoritative answer from the live helper, not a
        // transport failure. Retrying every miss with a fresh process turns a
        // bitmap-heavy document into thousands of redundant forks.
        helpers.get_mut(dpi).and_then(|helper| helper.path(name))
    })
}

fn reset_hot_pk_resolvers() {
    // A generated bitmap used as a framing sentinel may be removed between
    // builds. Keep helpers conversion-local so the next build proves a fresh
    // sentinel instead of trusting a path retained by a long-lived watch.
    HOT_PK_KPSE.with(|helpers| helpers.borrow_mut().clear());
}

fn resolve_pk_path_once(dpi: &str, name: &str) -> Option<String> {
    let started = std::time::Instant::now();
    let output = std::process::Command::new("kpsewhich")
        // Unlike the engines, standalone kpsewhich does not enable its
        // mktexpk fallback by default.  A clean TEXMFVAR therefore returned
        // an authoritative-looking miss for ordinary EC/TC bitmap fonts even
        // though the installed TeX tree contained everything needed to make
        // them.  Request generation explicitly; the resulting bitmap stays
        // inside the caller's normal writable TeX cache.
        .args(["--mktex=pk", "--dpi", dpi, "--format", "pk", name])
        .output()
        .ok();
    let mut generated = output.as_ref().is_some_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stderr).contains("mktexpk")
    });
    let mut path = output.as_ref().and_then(|output| {
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })?
    });

    // At non-base sizes kpsewhich passes the requested DPI to mktexpk as both
    // the target and base device resolution. Values such as 657dpi therefore
    // cannot select a Metafont mode. Generate with aiTeX's 600dpi PDF device
    // and the exact rational magnification, placing the result in TEXMFVAR so
    // read-only supplemental TEXMFHOME trees remain untouched.
    if path.is_none() {
        let target = dpi.parse::<u32>().ok()?;
        let texmf_var = std::env::var_os("TEXMFVAR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::process::Command::new("kpsewhich")
                    .arg("-var-value=TEXMFVAR")
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| {
                        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        (!value.is_empty()).then(|| std::path::PathBuf::from(value))
                    })
            })?;
        let destination = texmf_var.join("fonts/pk/aitex");
        std::fs::create_dir_all(&destination).ok()?;
        let magnitude = match target.cmp(&600) {
            std::cmp::Ordering::Equal => "1+0/600".to_string(),
            std::cmp::Ordering::Greater => format!("1+{}/600", target - 600),
            std::cmp::Ordering::Less => format!("{target}/600"),
        };
        let font = name.strip_suffix(".pk").unwrap_or(name);
        if font.is_empty()
            || !font
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-+_.".contains(&byte))
        {
            return None;
        }
        let direct = std::process::Command::new("mktexpk")
            .args(["--destdir"])
            .arg(&destination)
            .args([
                "--dpi",
                dpi,
                "--bdpi",
                "600",
                "--mag",
                &magnitude,
                "--mfmode",
                "ljfour",
                font,
            ])
            .output()
            .ok();
        path = direct.as_ref().and_then(|output| {
            output.status.success().then(|| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_string)
            })?
        });
        generated = path.is_some();
    }
    record_resolver_metric(|metrics| {
        metrics.pk_processes += 1;
        metrics.generated_pk += u64::from(generated);
        metrics.pk_process += started.elapsed();
    });
    path
}

/// The four numbers `tex.web` §241 takes from the system clock, followed by the
/// local UTC offset in minutes and the seconds field: `\time` (minutes since
/// midnight), `\day`, `\month`, `\year`, offset, seconds. The engine is
/// deterministic and `no_std`, so reading the clock is the *host's* job. On
/// the supported Unix targets libc supplies local-time conversion without the
/// incompatible GNU and BSD `date` flag spellings.
///
/// `SOURCE_DATE_EPOCH` is honoured exactly as pdfTeX honours it, which is to say
/// only when `FORCE_SOURCE_DATE=1` is also set — pdfTeX otherwise uses the epoch
/// for the PDF's own timestamps and leaves `\year` on the wall clock. Verified
/// against `pdftex -ini` both ways.
/// Controlled clock used only by black-box cache tests. It is intentionally
/// absent from the cache key so one test can ask whether a persisted entry
/// survives a clock transition; real builds should use `SOURCE_DATE_EPOCH`.
type WallClock = (i64, i64, i64, i64, i64, i64);

fn parse_test_wall_clock(value: &str) -> Option<WallClock> {
    let values = value
        .split(',')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let (minutes, day, month, year, offset, second) = match values.as_slice() {
        [minutes, day, month, year] => (*minutes, *day, *month, *year, 0, 0),
        [minutes, day, month, year, offset] => (*minutes, *day, *month, *year, *offset, 0),
        [minutes, day, month, year, offset, second] => {
            (*minutes, *day, *month, *year, *offset, *second)
        }
        _ => return None,
    };
    ((0..24 * 60).contains(&minutes)
        && (1..=31).contains(&day)
        && (1..=12).contains(&month)
        && year >= 0
        && (-24 * 60..=24 * 60).contains(&offset)
        && (0..60).contains(&second))
        .then_some((minutes, day, month, year, offset, second))
}

fn test_wall_clock() -> Option<WallClock> {
    std::env::var("AITEX_TEST_CLOCK")
        .ok()
        .as_deref()
        .and_then(parse_test_wall_clock)
}

#[cfg(unix)]
fn wall_clock() -> WallClock {
    use std::time::{SystemTime, UNIX_EPOCH};

    if let Some(clock) = test_wall_clock() {
        return clock;
    }
    let forced = std::env::var("FORCE_SOURCE_DATE").as_deref() == Ok("1");
    let (seconds, utc) = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(epoch) if forced => match epoch.parse::<i64>() {
            Ok(seconds) => (seconds, true),
            Err(_) => return (0, 0, 0, 0, 0, 0),
        },
        _ => match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => (duration.as_secs() as i64, false),
            Err(_) => return (0, 0, 0, 0, 0, 0),
        },
    };
    let seconds: libc::time_t = match seconds.try_into() {
        Ok(seconds) => seconds,
        Err(_) => return (0, 0, 0, 0, 0, 0),
    };
    // SAFETY: `localtime_r`/`gmtime_r` initialize the caller-owned `tm` and
    // do not retain either pointer. Both are available on the supported macOS
    // and Linux targets.
    #[allow(unsafe_code)]
    let local = unsafe {
        let mut local: libc::tm = core::mem::zeroed();
        let result = if utc {
            libc::gmtime_r(&seconds, &mut local)
        } else {
            libc::localtime_r(&seconds, &mut local)
        };
        (!result.is_null()).then_some(local)
    };
    let Some(local) = local else {
        return (0, 0, 0, 0, 0, 0);
    };
    (
        i64::from(local.tm_hour * 60 + local.tm_min),
        i64::from(local.tm_mday),
        i64::from(local.tm_mon + 1),
        i64::from(local.tm_year + 1900),
        if utc { 0 } else { local.tm_gmtoff as i64 / 60 },
        i64::from(local.tm_sec),
    )
}

#[cfg(not(unix))]
fn wall_clock() -> WallClock {
    if let Some(clock) = test_wall_clock() {
        return clock;
    }
    let mut cmd = std::process::Command::new("date");
    let forced = std::env::var("FORCE_SOURCE_DATE").as_deref() == Ok("1");
    match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(e) if forced && e.parse::<i64>().is_ok() => {
            cmd.arg("-u").arg(format!("-d@{e}"));
        }
        _ => {}
    }
    let out = match cmd.arg("+%H %M %S %d %m %Y %z").output() {
        Ok(o) if o.status.success() => o.stdout,
        // No `date`: leave the parameters at zero, as TeX does on a clockless system.
        _ => return (0, 0, 0, 0, 0, 0),
    };
    let text = String::from_utf8_lossy(&out);
    let n: Vec<i64> = text
        .split_whitespace()
        .filter_map(|w| w.trim_start_matches('0').parse().ok().or(Some(0)))
        .collect();
    match n[..] {
        [h, mi, second, d, mo, y, zone] => {
            let sign = if zone < 0 { -1 } else { 1 };
            let zone = zone.abs();
            (
                h * 60 + mi,
                d,
                mo,
                y,
                sign * ((zone / 100) * 60 + zone % 100),
                second,
            )
        }
        _ => (0, 0, 0, 0, 0, 0),
    }
}

fn pdf_creation_date(clock: WallClock) -> String {
    let hour = clock.0.div_euclid(60);
    let minute = clock.0.rem_euclid(60);
    let zone = if clock.4 == 0 {
        "Z".to_string()
    } else {
        let sign = if clock.4 < 0 { '-' } else { '+' };
        let offset = clock.4.abs();
        format!("{sign}{:02}'{:02}'", offset / 60, offset % 60)
    };
    format!(
        "D:{:04}{:02}{:02}{hour:02}{minute:02}{:02}{zone}",
        clock.3, clock.2, clock.1, clock.5
    )
}

/// Resolve a TeX file name to a path: verbatim (and with `.tex`) as a path
/// first, then through kpathsea.
/// The order `resolve_path` tries candidates in: the extended name first, per §537,
/// with the bare name behind it as a fallback.
fn alloc_vec(name: &str, ext: &str, has_ext: bool) -> Vec<String> {
    if ext.is_empty() || has_ext {
        vec![name.to_string()]
    } else {
        vec![format!("{name}{ext}"), name.to_string()]
    }
}

fn tex_filesystem_path(name: &str, unicode_mode: bool) -> std::path::PathBuf {
    #[cfg(unix)]
    if !unicode_mode && name.chars().all(|c| (c as u32) < 256) {
        use std::os::unix::ffi::OsStringExt as _;
        let bytes = name.chars().map(|c| c as u8).collect();
        return std::ffi::OsString::from_vec(bytes).into();
    }
    std::path::PathBuf::from(name)
}

fn resolve_local_path(name: &str, ext: &str) -> Option<String> {
    let has_ext = name
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base.contains('.'));
    alloc_vec(name, ext, has_ext)
        .into_iter()
        .filter(|candidate| !candidate.is_empty())
        .find_map(|candidate| {
            let path = tex_filesystem_path(&candidate, unicode_flag());
            path.is_file().then(|| path.to_string_lossy().into_owned())
        })
}

fn resolve_path_with(
    kpse: &mut Option<Kpse>,
    name: &str,
    ext: &str,
    start: fn() -> Option<Kpse>,
    format: Option<&str>,
) -> Option<String> {
    // §537: `\input` appends `.tex` whenever the scanned name carries no extension of
    // its own, and then looks for *that* — it never tries the bare name.
    //
    // ```text
    // begin scan_file_name; {set |cur_name| to desired file name}
    // if cur_ext="" then cur_ext:=".tex";
    // pack_cur_name;
    // ```
    //
    // Trying the bare name first is not equivalent even when both files exist and
    // agree: §537 echoes the name it opened, so `\input tripos` in a directory holding
    // both `tripos` and `tripos.tex` logged `(./tripos` against TeX's `(./tripos.tex`.
    // §515 sets `cur_ext` from a `.` in the name, so `\input foo.bar` keeps `.bar` and
    // gets no `.tex`.
    //
    // The bare name stays on as a fallback rather than an alternative: it is reached
    // only where TeX would have found nothing at all and prompted.
    let local = resolve_local_path(name, ext);
    record_resolver_metric(|metrics| {
        metrics.local_queries += 1;
        metrics.local_hits += u64::from(local.is_some());
    });
    if let Some(path) = local {
        return Some(path);
    }
    // Starting the interactive helper costs several milliseconds. A project
    // snapshot hit often reads only local body files, so defer it until a
    // lookup actually falls through the filesystem fast path.
    if kpse.is_none() {
        *kpse = start();
    }
    if let Some(k) = kpse.as_mut() {
        if let Some(p) = k.path(name) {
            return Some(p);
        }
        // `\input foo` may mean `foo.tex`; kpathsea only infers that for some
        // formats, so ask again with the extension.
        if !ext.is_empty() {
            return k.path(&format!("{name}{ext}"));
        }
        return None;
    }
    // No persistent kpsewhich: one process per lookup, as before.
    let started = std::time::Instant::now();
    let mut command = std::process::Command::new("kpsewhich");
    if let Some(format) = format {
        command.args(["--format", format]);
    }
    let out = command.arg(name).output();
    record_resolver_metric(|metrics| {
        metrics.fallback_processes += 1;
        metrics.fallback_process += started.elapsed();
    });
    let out = out.ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

fn resolve_path(kpse: &mut Option<Kpse>, name: &str, ext: &str) -> Option<String> {
    resolve_path_with(kpse, name, ext, Kpse::start, None)
}

fn resolve_tex_path(kpse: &mut Option<Kpse>, name: &str, ext: &str) -> Option<String> {
    resolve_path_with(kpse, name, ext, Kpse::start_tex, Some("tex"))
}

fn resolve_path_hot(name: &str, ext: &str) -> Option<String> {
    HOT_KPSE.with(|kpse| resolve_path(&mut kpse.borrow_mut(), name, ext))
}

fn resolve_tex_path_hot(name: &str, ext: &str) -> Option<String> {
    HOT_TEX_KPSE.with(|kpse| resolve_tex_path(&mut kpse.borrow_mut(), name, ext))
}

/// Resolve a TeX metric and, on a genuine miss, permit the same bounded
/// `mktextfm` fallback that the TeX Live engines use.
///
/// Some classic families ship Metafont sources plus only their common design
/// sizes. NFSS can request an uncommon generated size such as `larm1728`; a
/// plain `kpsewhich` lookup says it is absent even though pdfTeX asks kpathsea
/// to create it in `TEXMFVAR`. The command is fixed, receives one validated
/// basename as an argument (never through a shell), and returns only an
/// existing path emitted by kpathsea.
fn resolve_tfm_path_hot(name: &str) -> Option<String> {
    let request = format!("{name}.tfm");
    if let Some(path) = resolve_path_hot(&request, "") {
        return Some(path);
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-+_.".contains(&byte))
    {
        return None;
    }
    let output = std::process::Command::new("kpsewhich")
        .args(["--mktex=tfm", "--format=tfm"])
        .arg(&request)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && std::path::Path::new(line).is_file())
        .map(str::to_string)
}

/// Produce the bounding-box sidecar expected by `dvipdfmx.def` without shell
/// escape and without writing into the source tree. The command is fixed and
/// the asset path has already been resolved by kpathsea, so this does not turn
/// TeX input into an arbitrary process invocation.
const GRAPHIC_EXTENSIONS: &[&str] = &[
    ".pdf", ".ai", ".png", ".jpg", ".jpeg", ".PDF", ".AI", ".PNG", ".JPG", ".JPEG",
];

fn has_graphic_extension(name: &str) -> bool {
    GRAPHIC_EXTENSIONS
        .iter()
        .any(|extension| name.ends_with(extension))
}

#[cfg(test)]
fn synthetic_xbb_candidates(name: &str, recent_asset: Option<&str>) -> Option<Vec<String>> {
    synthetic_xbb_request(name, recent_asset).map(|request| request.0)
}

fn synthetic_xbb_request(
    name: &str,
    recent_asset: Option<&str>,
) -> Option<(Vec<String>, Option<u32>, Option<String>, bool)> {
    let stem = name.strip_suffix(".xbb")?;
    if let Some(asset) = stem.strip_suffix(".aitex-eps-normalized") {
        return Some((vec![asset.to_string()], None, None, true));
    }
    if let Some((asset, selection)) = stem.rsplit_once(".aitex.") {
        let (page, pagebox) = selection.split_once('.')?;
        let page = if page.is_empty() {
            None
        } else {
            Some(page.parse::<u32>().ok()?.max(1))
        };
        let pagebox = pagebox.to_ascii_lowercase();
        if !matches!(
            pagebox.as_str(),
            "cropbox" | "mediabox" | "artbox" | "trimbox" | "bleedbox"
        ) {
            return None;
        }
        // An author-level rule such as `{.ai}{pdf}{*}{}` preserves the
        // selected `.ai` file for inclusion but asks the PDF reader for a
        // `.pdf` sidecar.  Prefer the immediately preceding successful
        // graphics probe when it names the same stem; if a real `.pdf` was
        // selected, `recent_asset` is that PDF and this simply deduplicates.
        let mut candidates = Vec::with_capacity(2);
        if let Some(recent_asset) = recent_asset {
            let requested_stem = std::path::Path::new(asset).file_stem();
            let recent_stem = std::path::Path::new(recent_asset).file_stem();
            if requested_stem.is_some() && requested_stem == recent_stem {
                candidates.push(recent_asset.to_string());
            }
        }
        if !candidates.iter().any(|present| present == asset) {
            candidates.push(asset.to_string());
        }
        return Some((candidates, page, Some(pagebox), false));
    }
    if has_graphic_extension(stem) {
        return Some((vec![stem.to_string()], None, None, false));
    }
    let mut candidates = Vec::with_capacity(GRAPHIC_EXTENSIONS.len());
    if let Some(asset) = recent_asset {
        let requested_stem = std::path::Path::new(stem).file_name();
        let asset_stem = std::path::Path::new(asset).file_stem();
        if requested_stem.is_some() && requested_stem == asset_stem {
            candidates.push(asset.to_string());
        }
    }
    for extension in GRAPHIC_EXTENSIONS {
        let candidate = format!("{stem}{extension}");
        if !candidates.iter().any(|present| present == &candidate) {
            candidates.push(candidate);
        }
    }
    Some((candidates, None, None, false))
}

fn synthetic_xbb(
    kpse: &mut Option<Kpse>,
    name: &str,
    recent_asset: Option<&str>,
) -> Option<(String, String)> {
    let (candidates, page, pagebox, normalized_eps) =
        synthetic_xbb_request(name, recent_asset)?;
    for candidate in candidates {
        let Some(path) = resolve_path(kpse, &candidate, "") else {
            continue;
        };
        let bytes = std::fs::read(&path).ok();
        let normalized_eps_geometry = normalized_eps
            .then(|| bytes.as_deref())
            .flatten()
            .and_then(|bytes| {
                if aitex_driver::pdf::directly_embeddable_eps(bytes) {
                    return eps_graphics_bbox(bytes).map(|bbox| (bbox, 1));
                }
                normalize_eps_pdf_cached(bytes)
                    .and_then(|pdf| aitex_driver::pdf::first_page_pdftex_bbox(&pdf))
                    .map(pdftex_pdf_xbb_bbox)
                    .or_else(|| eps_graphics_bbox(bytes))
                    .map(|bbox| (bbox, 1))
            });
        let direct_geometry = (page.unwrap_or(1) == 1
            && pagebox.as_deref().is_none_or(|value| value == "cropbox"))
        .then(|| bytes.as_deref())
        .flatten()
        .and_then(|bytes| {
            if candidate.to_ascii_lowercase().ends_with(".png") {
                pdftex_png_xbb_size(bytes).and_then(|(width, height)| {
                    Some((
                        [0.0, 0.0, width.parse().ok()?, height.parse().ok()?],
                        1,
                    ))
                })
            } else if bytes.starts_with(b"%PDF-") {
                let info = aitex_driver::pdf::direct_pdf_page_info(bytes, 1)?;
                Some((pdftex_pdf_xbb_bbox(info.bbox), info.page_count))
            } else {
                jpeg_xbb_size(bytes).map(|(width, height)| ([0.0, 0.0, width, height], 1))
            }
        });
        // Some publication PDFs have a damaged cross-reference table with an
        // obsolete page object that `pdfinfo` selects while MuPDF's page-tree
        // repair finds the live object (pdfTeX also reads the live box). Repair
        // and isolate the requested page before measuring it. Keep pdfinfo only
        // for the original document's page count; its reconstructed box is not
        // authoritative on this path. Uncommon non-crop page-box selections
        // retain the explicit pdfinfo query below.
        let geometry = normalized_eps_geometry
            .or(direct_geometry)
            .or_else(|| {
                bytes
                    .as_deref()
                    .filter(|bytes| bytes.starts_with(b"%PDF-"))
                    .filter(|_| {
                        pagebox
                            .as_deref()
                            .is_none_or(|pagebox| pagebox == "cropbox")
                    })
                    .and_then(|bytes| {
                        let selected_page = page.unwrap_or(1);
                        // extractbb is useful to detect a damaged incremental
                        // update while normalizing the page below, but its
                        // reported box is not the final pdfTeX geometry oracle.
                        // It can expose a raw CropBox beyond the MediaBox, or an
                        // obsolete box from an earlier incremental revision.
                        // The normalized page parser applies pdfTeX's MediaBox
                        // intersection and shares the exact coordinate system
                        // later retained by the imported Form.
                        let source_bbox = pdf_has_incremental_update(bytes)
                            .then(|| extractbb_pdf_bbox(&path, selected_page))
                            .flatten();
                        let normalized = normalize_pdf_page(&path, selected_page, bytes)?;
                        let normalized_bbox =
                            aitex_driver::pdf::first_page_pdftex_bbox(&normalized)?;
                        let bbox = repaired_pdf_xbb_bbox(source_bbox, normalized_bbox);
                        let pages = pdfinfo_geometry(&path, selected_page, pagebox.as_deref())
                            .map_or(1, |(_, pages)| pages);
                        Some((pdftex_pdf_xbb_bbox(bbox), pages))
                    })
            })
            .or_else(|| {
                bytes
                    .as_deref()
                    .filter(|bytes| bytes.starts_with(b"%PDF-"))
                    .and_then(|_| {
                        pdfinfo_geometry(&path, page.unwrap_or(1), pagebox.as_deref())
                    })
                    .map(|(bbox, pages)| (pdftex_pdf_xbb_bbox(bbox), pages))
            });
        if let Some(([llx, lly, urx, ury], pages)) = geometry {
            // pdfTeX learns a PNG's natural TeX dimensions from libpng, then
            // graphics.sty's `\Gin@defaultbp` converts those dimensions with
            // its historical 0.99626 fixed-point factor. Reproducing that
            // number here makes generic graphics compute the same requested
            // box and scale in our private-DVI route.
            let xbb = format!(
                "%%Title: {path}\n%%Creator: aiTeX\n%%Pages: {pages}\n%%BoundingBox: {llx:.6} {lly:.6} {urx:.6} {ury:.6}\n\
                 %%HiResBoundingBox: {llx:.6} {lly:.6} {urx:.6} {ury:.6}\n",
            );
            return Some((path, xbb));
        }
        let started = std::time::Instant::now();
        let output = extractbb_command(&path, page, pagebox.as_deref())
            .output();
        record_resolver_metric(|metrics| {
            metrics.extractbb_processes += 1;
            metrics.extractbb_process += started.elapsed();
        });
        let output = output.ok()?;
        if output.status.success() && output.stdout.starts_with(b"%%Title:") {
            let body = String::from_utf8_lossy(&output.stdout);
            let hires = body
                .lines()
                .find_map(|line| line.strip_prefix("%%HiResBoundingBox: "));
            let body = if let Some(hires) = hires {
                body.lines()
                    .map(|line| {
                        if line.starts_with("%%BoundingBox: ") {
                            format!("%%BoundingBox: {hires}")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n"
            } else {
                body.into_owned()
            };
            return Some((path, body));
        }
    }
    None
}

fn repaired_pdf_xbb_bbox(
    _source_bbox: Option<[f64; 4]>,
    normalized_bbox: [f64; 4],
) -> [f64; 4] {
    normalized_bbox
}

/// Match the natural box that pdfTeX exposes to `pdftex.def` for a PDF page.
///
/// pdfTeX first rounds a PDF extent (stored as a C `float`) to scaled points.
/// `pdftex.def` then passes that dimension through graphics.sty's historical
/// `.99626` conversion before treating its numeric value as big points. TeX
/// represents `.99626` as 65291/65536 and truncates the multiplication. Our
/// private-DVI route reads an XBB directly, so reproduce that round trip here.
fn pdftex_pdf_xbb_bbox([llx, lly, urx, ury]: [f64; 4]) -> [f64; 4] {
    let converted_extent = |extent: f64| {
        let raw_sp = ((extent as f32 as f64) * (6_578_176.0 / 100.0)).round() as i128;
        let xbb_number_sp = raw_sp * 65_291 / 65_536;
        xbb_number_sp as f64 / 65_536.0
    };
    [
        llx,
        lly,
        llx + converted_extent(urx - llx),
        lly + converted_extent(ury - lly),
    ]
}

fn pdfinfo_geometry(
    path: &str,
    page: u32,
    pagebox: Option<&str>,
) -> Option<([f64; 4], usize)> {
    let started = std::time::Instant::now();
    let page = page.to_string();
    let output = std::process::Command::new("pdfinfo")
        .args(["-box", "-f", &page, "-l", &page])
        .arg(path)
        .output();
    record_resolver_metric(|metrics| {
        metrics.pdfinfo_processes += 1;
        metrics.pdfinfo_process += started.elapsed();
    });
    let output = output.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout))
        .and_then(|text| parse_pdfinfo_geometry(&text, pagebox))
}

fn parse_pdfinfo_geometry(text: &str, pagebox: Option<&str>) -> Option<([f64; 4], usize)> {
    let pages = text.lines().find_map(|line| {
        line.strip_prefix("Pages:")
            .and_then(|value| value.trim().parse::<usize>().ok())
    })?;
    let label = match pagebox.unwrap_or("cropbox").to_ascii_lowercase().as_str() {
        "mediabox" => "MediaBox:",
        "artbox" => "ArtBox:",
        "trimbox" => "TrimBox:",
        "bleedbox" => "BleedBox:",
        _ => "CropBox:",
    };
    let values = text.lines().find_map(|line| {
        let (_, values) = line.split_once(label)?;
        let values = values
            .split_whitespace()
            .filter_map(|value| value.parse::<f64>().ok())
            .collect::<Vec<_>>();
        (values.len() == 4).then_some(values)
    })?;
    let mut bbox = [values[0], values[1], values[2], values[3]];
    let rotation = text
        .lines()
        .find_map(|line| {
            let (_, value) = line.split_once("rot:")?;
            value.trim().parse::<i32>().ok()
        })
        .unwrap_or(0)
        .rem_euclid(360);
    if matches!(rotation, 90 | 270) {
        let [llx, lly, urx, ury] = bbox;
        bbox = [llx, lly, llx + ury - lly, lly + urx - llx];
    }
    Some((bbox, pages))
}

fn extractbb_command(
    path: &str,
    page: Option<u32>,
    pagebox: Option<&str>,
) -> std::process::Command {
    let mut command = std::process::Command::new("extractbb");
    command.arg("-O");
    if let Some(page) = page {
        command.args(["-p", &page.to_string()]);
    }
    if let Some(pagebox) = pagebox {
        command.args(["-B", pagebox]);
    }
    command
        .arg(path)
            // Publication builds normally set openin_any=p. That policy is
            // correct for TeX input, but extractbb is itself kpathsea-aware
            // and would reject the absolute path already resolved and vetted
            // by this host callback. The fixed command has no author-supplied
            // options and only inspects that selected graphics asset.
            .env("openin_any", "a")
            .stderr(std::process::Stdio::null());
    command
}

fn synthetic_xbb_hot(
    requested: &str,
    selected_asset: Option<&str>,
) -> Option<(String, String)> {
    HOT_KPSE.with(|kpse| {
        synthetic_xbb(
            &mut kpse.borrow_mut(),
            requested,
            selected_asset,
        )
    })
}

#[cfg(test)]
mod kpse_metrics_tests {
    use super::*;

    #[test]
    fn pdfinfo_geometry_keeps_page_count_and_selected_box() {
        let text = concat!(
            "Pages:           7\n",
            "Page    3 MediaBox:      0.00     0.00   612.00   792.00\n",
            "Page    3 CropBox:      12.50    20.25   600.00   770.75\n",
        );
        assert_eq!(
            parse_pdfinfo_geometry(text, Some("cropbox")),
            Some(([12.5, 20.25, 600.0, 770.75], 7))
        );
        assert_eq!(
            parse_pdfinfo_geometry(text, Some("mediabox")),
            Some(([0.0, 0.0, 612.0, 792.0], 7))
        );
    }

    #[test]
    fn pdfinfo_geometry_applies_page_rotation_to_the_selected_box() {
        let text = concat!(
            "Pages:           1\n",
            "Page    1 rot:   90\n",
            "Page    1 CropBox:     213.05    32.03   392.83   759.88\n",
        );
        let (bbox, pages) = parse_pdfinfo_geometry(text, Some("cropbox")).expect("geometry");
        assert_eq!(pages, 1);
        for (actual, expected) in bbox.into_iter().zip([213.05, 32.03, 940.9, 211.81]) {
            assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
        }
    }

    #[test]
    fn repaired_pdf_xbb_uses_the_imported_forms_coordinate_box() {
        let raw_crop_beyond_media = [0.0, 0.0, 612.0, 792.0];
        let clipped_to_media = [0.0, 0.0, 550.579, 427.046];
        assert_eq!(
            repaired_pdf_xbb_bbox(Some(raw_crop_beyond_media), clipped_to_media),
            clipped_to_media,
        );

        let obsolete_incremental_crop = [72.0, 72.0, 648.0, 504.0];
        let current_page = [0.0, 0.0, 720.0, 576.0];
        assert_eq!(
            repaired_pdf_xbb_bbox(Some(obsolete_incremental_crop), current_page),
            current_page,
        );
    }

    #[test]
    fn test_clock_requires_a_valid_tex_clock_tuple() {
        assert_eq!(
            parse_test_wall_clock("106,9,9,2001"),
            Some((106, 9, 9, 2001, 0, 0))
        );
        assert_eq!(
            parse_test_wall_clock("106,9,9,2001,-420"),
            Some((106, 9, 9, 2001, -420, 0))
        );
        assert_eq!(
            parse_test_wall_clock("106,9,9,2001,-420,37"),
            Some((106, 9, 9, 2001, -420, 37))
        );
        assert_eq!(parse_test_wall_clock("1440,9,9,2001"), None);
        assert_eq!(parse_test_wall_clock("106,32,9,2001"), None);
        assert_eq!(parse_test_wall_clock("106,9,13,2001"), None);
        assert_eq!(parse_test_wall_clock("106,9,9"), None);
    }

    #[test]
    fn creation_date_has_pdf_timezone_syntax_and_second_precision() {
        assert_eq!(
            pdf_creation_date((9 * 60 + 7, 4, 8, 2026, -7 * 60, 31)),
            "D:20260804090731-07'00'"
        );
        assert_eq!(
            pdf_creation_date((12 * 60 + 34, 31, 12, 2001, 0, 0)),
            "D:20011231123400Z"
        );
    }

    #[test]
    fn extractbb_can_read_a_host_resolved_asset_under_restricted_tex_input() {
        let command = extractbb_command(
            "/resolved/example-image.pdf",
            Some(3),
            Some("artbox"),
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == "openin_any" && value.is_some_and(|value| value == "a")
        }));
        assert_eq!(
            command
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["-O", "-p", "3", "-B", "artbox", "/resolved/example-image.pdf"]
        );
    }

    #[test]
    fn synthetic_xbb_decodes_pdf_page_and_box_selection() {
        assert_eq!(
            synthetic_xbb_request("fig.pdf.aitex.3.artbox.xbb", None),
            Some((
                vec!["fig.pdf".to_string()],
                Some(3),
                Some("artbox".to_string()),
                false,
            ))
        );
        assert_eq!(
            synthetic_xbb_request("fig.pdf.aitex..cropbox.xbb", None),
            Some((
                vec!["fig.pdf".to_string()],
                None,
                Some("cropbox".to_string()),
                false,
            ))
        );
        assert_eq!(
            synthetic_xbb_request(
                "fig.pdf.aitex..artbox.xbb",
                Some("./fig.ai"),
            ),
            Some((
                vec!["./fig.ai".to_string(), "fig.pdf".to_string()],
                None,
                Some("artbox".to_string()),
                false,
            ))
        );
    }

    #[test]
    fn pdf_xbb_matches_pdftex_def_dimension_round_trip() {
        let [llx, lly, urx, ury] = pdftex_pdf_xbb_bbox([0.0, 0.0, 1476.0, 479.0]);
        assert_eq!(
            format!("{llx:.6} {lly:.6} {urx:.6} {ury:.6}"),
            "0.000000 0.000000 1475.996414 478.998825"
        );

        let [llx, lly, urx, ury] = pdftex_pdf_xbb_bbox([10.0, 20.0, 1486.0, 499.0]);
        assert_eq!(
            format!("{llx:.6} {lly:.6} {urx:.6} {ury:.6}"),
            "10.000000 20.000000 1485.996414 498.998825"
        );
    }

    #[cfg(unix)]
    #[test]
    fn byte_faithful_tex_names_resolve_utf8_filesystem_paths() {
        use std::os::unix::ffi::OsStrExt as _;

        let path = std::env::temp_dir().join(format!(
            "aitex-résolution-{}-{:?}.tex",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"test").unwrap();
        let tex_name: String = path.as_os_str().as_bytes().iter().copied().map(char::from).collect();
        let resolved = tex_filesystem_path(&tex_name, false);
        assert_eq!(resolved, path);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn local_resolution_is_counted_only_when_metrics_are_enabled() {
        let path = std::env::temp_dir().join(format!(
            "aitex-resolver-metrics-{}-{:?}.tex",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"test").unwrap();
        let request = path.to_str().unwrap();

        reset_resolver_metrics(false);
        assert_eq!(resolve_path(&mut None, request, ".tex"), Some(request.into()));
        assert_eq!(resolver_metrics().local_queries, 0);

        reset_resolver_metrics(true);
        assert_eq!(resolve_path(&mut None, request, ".tex"), Some(request.into()));
        let metrics = resolver_metrics();
        assert_eq!(metrics.local_queries, 1);
        assert_eq!(metrics.local_hits, 1);
        assert_eq!(metrics.helper_starts, 0);
        assert_eq!(metrics.fallback_processes, 0);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tex_resolver_uses_kpathsea_format_and_casefold_rules_for_pgf_inputs() {
        let directory = std::env::temp_dir().join(format!(
            "aitex-kpse-pgf-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let physical = directory.join("figure_lower.pgf");
        std::fs::write(&physical, b"test").unwrap();
        let request = directory.join("FIGURE_LOWER.pgf");
        let request = request.to_str().unwrap();

        let resolved = resolve_tex_path(&mut None, request, "").expect("case-folded PGF input");
        assert_eq!(std::fs::canonicalize(resolved).unwrap(), physical);

        std::fs::remove_file(physical).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
