const FORMAT_DEPENDENCY_KEY: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

thread_local! {
    static LOCAL_FORMAT: std::cell::RefCell<Option<Result<String, String>>> =
        const { std::cell::RefCell::new(None) };
}

fn format_environment_key() -> String {
    let mut identity = Vec::new();
    key_field(&mut identity, "format-cache-schema", b"1");
    key_field(
        &mut identity,
        "compiler-version",
        env!("CARGO_PKG_VERSION").as_bytes(),
    );
    if let Some(path) = std::env::var_os("PATH") {
        key_field(&mut identity, "PATH", &os_str_cache_bytes(&path));
    }
    for (name, value) in relevant_environment() {
        key_field(&mut identity, &name, &value);
    }
    sha256_hex(&identity)
}

fn format_manifest_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.manifest",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("latex.afmt")
    ))
}

fn format_lock_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.lock",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("latex.afmt")
    ))
}

fn format_cache_path(arguments: &[String]) -> PathBuf {
    let root = arguments
        .iter()
        .position(|argument| argument == "--cache")
        .and_then(|at| arguments.get(at + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            platform_cache_root(
                std::env::var_os("XDG_CACHE_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
                cfg!(target_os = "macos"),
                input_argument(arguments),
            )
        });
    root.join("formats/latex.afmt")
}

fn developer_format_candidates(executable: &Path) -> Vec<PathBuf> {
    executable
        .parent()
        .into_iter()
        .flat_map(|directory| directory.ancestors().take(4))
        .map(|ancestor| ancestor.join(".aitex/format/fmt.afmt"))
        .collect()
}

fn remember_format_identity(path: &Path, digest: String) {
    if let Ok(metadata) = std::fs::metadata(path) {
        HOT_FORMAT_IDENTITIES.with(|cache| {
            cache.borrow_mut().insert(
                path.to_path_buf(),
                (file_signature(&metadata), format!("sha256:{digest}")),
            );
        });
    }
}

fn validate_local_format(path: &Path, environment_key: &str) -> Result<(), String> {
    let manifest_path = format_manifest_path(path);
    let metadata = std::fs::metadata(&manifest_path)
        .map_err(|error| format!("format manifest is unavailable: {error}"))?;
    if metadata.len() > MAX_CACHE_MANIFEST_BYTES {
        return Err("format manifest exceeds size limit".to_string());
    }
    let manifest = parse_manifest(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("cannot read format manifest: {error}"))?,
    )?;
    if manifest.kind != CacheKind::Format || manifest.key != environment_key {
        return Err("format environment changed".to_string());
    }
    let format_metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot inspect format: {error}"))?;
    if format_metadata.len() != manifest.payload_bytes {
        return Err("format size does not match its manifest".to_string());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read format: {error}"))?;
    aitex_engine::fast::Fast::check_format_header(&bytes)
        .map_err(|error| format!("incompatible format: {error}"))?;
    let digest = sha256_hex(&bytes);
    if digest != manifest.payload_sha256 {
        return Err("format checksum does not match its manifest".to_string());
    }
    dependencies_hold(&manifest.dependencies)?;
    remember_format_identity(path, digest);
    Ok(())
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        return std::fs::canonicalize(candidate).ok();
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| std::fs::canonicalize(candidate).ok())
    })
}

fn kpsewhich_file(name: &str, working_directory: &Path) -> Result<PathBuf, String> {
    let result = std::process::Command::new("kpsewhich")
        .arg(name)
        .current_dir(working_directory)
        .output()
        .map_err(|error| {
            format!("cannot run kpsewhich: {error}; install TeX Live or MacTeX and put kpsewhich on PATH")
        })?;
    let output = String::from_utf8_lossy(&result.stdout);
    let path = PathBuf::from(output.trim());
    if !result.status.success() || !path.is_file() {
        return Err(format!(
            "TeX Live file {name:?} was not found; install a LaTeX-capable TeX Live or MacTeX"
        ));
    }
    std::fs::canonicalize(&path)
        .map_err(|error| format!("cannot resolve {}: {error}", path.display()))
}

fn stage_language_config(
    working_directory: &Path,
) -> Result<(bool, Vec<CacheDependency>), String> {
    let Ok(language_dat) = kpsewhich_file("language.dat", working_directory) else {
        return Ok((false, Vec::new()));
    };
    let Ok(ibyhyph) = kpsewhich_file("ibyhyph.tex", working_directory) else {
        return Ok((false, Vec::new()));
    };
    let ibyhyph_bytes = std::fs::read(&ibyhyph)
        .map_err(|error| format!("cannot read {}: {error}", ibyhyph.display()))?;
    if !ibyhyph_bytes
        .windows(b"\\savinghyphcodes".len())
        .any(|window| window == b"\\savinghyphcodes")
    {
        return Ok((false, Vec::new()));
    }

    let source = std::fs::read_to_string(&language_dat)
        .map_err(|error| format!("cannot read {}: {error}", language_dat.display()))?;
    let mut output = String::with_capacity(source.len());
    let mut changed = false;
    for line in source.split_inclusive('\n') {
        if line.split_whitespace().next() == Some("ibycus") {
            output.push_str(
                "% ibycus disabled for aiTeX: ibyhyph.tex requires unsupported \\savinghyphcodes\n",
            );
            changed = true;
        } else {
            output.push_str(line);
        }
    }
    if changed {
        std::fs::write(working_directory.join("language.dat"), output)
            .map_err(|error| format!("cannot stage language.dat: {error}"))?;
    }
    let dependencies = if changed {
        vec![
            CacheDependency::from_resolution(
                "language.dat",
                Some(language_dat.to_string_lossy().as_ref()),
            ),
            CacheDependency::from_resolution(
                "ibyhyph.tex",
                Some(ibyhyph.to_string_lossy().as_ref()),
            ),
        ]
    } else {
        Vec::new()
    };
    Ok((changed, dependencies))
}

fn write_format_dependency_log(path: &Path, dependencies: &[CacheDependency]) -> Result<(), String> {
    let manifest = CacheManifest {
        kind: CacheKind::Format,
        key: FORMAT_DEPENDENCY_KEY.to_string(),
        payload_bytes: 0,
        payload_sha256: FORMAT_DEPENDENCY_KEY.to_string(),
        created_unix: now_unix(),
        clock: None,
        dependencies: normalized_dependencies(dependencies),
    };
    atomic_write(path, manifest_text(&manifest).as_bytes())
        .map_err(|error| format!("cannot write format dependency log: {error}"))
}

fn read_format_dependency_log(path: &Path) -> Result<Vec<CacheDependency>, String> {
    let manifest = parse_manifest(
        &std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read format dependency log: {error}"))?,
    )?;
    if manifest.kind != CacheKind::Format || manifest.key != FORMAT_DEPENDENCY_KEY {
        return Err("invalid format dependency log".to_string());
    }
    Ok(manifest.dependencies)
}

fn format_failure_tail(path: &Path) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    const MAX_TAIL_BYTES: u64 = 128 * 1024;
    let text = (|| -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let bytes = file.metadata()?.len();
        file.seek(SeekFrom::Start(bytes.saturating_sub(MAX_TAIL_BYTES)))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        Ok(String::from_utf8_lossy(&tail).into_owned())
    })()
    .unwrap_or_default();
    text.lines()
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_local_format(path: &Path, environment_key: &str) -> Result<bool, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "format destination has no parent directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create format cache: {error}"))?;

    let lock_path = format_lock_path(path);
    let wait_started = Instant::now();
    let lock = loop {
        if validate_local_format(path, environment_key).is_ok() {
            return Ok(false);
        }
        match acquire_cache_lock(&lock_path)
            .map_err(|error| format!("cannot lock format cache: {error}"))?
        {
            Some(lock) => break lock,
            None if wait_started.elapsed() < std::time::Duration::from_secs(300) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            None => return Err("timed out waiting for another format build".to_string()),
        }
    };

    if validate_local_format(path, environment_key).is_ok() {
        drop(lock);
        return Ok(false);
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let working_directory = parent.join(format!(
        ".latex-format-build-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&working_directory)
        .map_err(|error| format!("cannot create format workspace: {error}"))?;

    let result = (|| {
        let latex = kpsewhich_file("latex.ltx", &working_directory)?;
        let kpsewhich = executable_on_path("kpsewhich")
            .ok_or_else(|| "kpsewhich was not found on PATH".to_string())?;
        let (excluded_ibycus, mut extra_dependencies) =
            stage_language_config(&working_directory)?;
        extra_dependencies.push(CacheDependency::from_resolution(
            kpsewhich.to_string_lossy().as_ref(),
            Some(kpsewhich.to_string_lossy().as_ref()),
        ));

        let dependency_log = working_directory.join("dependencies.manifest");
        let transcript = working_directory.join("build.log");
        let transcript_file = std::fs::File::create(&transcript)
            .map_err(|error| format!("cannot create format transcript: {error}"))?;
        let stderr_file = transcript_file
            .try_clone()
            .map_err(|error| format!("cannot clone format transcript: {error}"))?;
        let executable = std::env::current_exe()
            .map_err(|error| format!("cannot locate the aiTeX executable: {error}"))?;
        let mut command = std::process::Command::new(executable);
        command
            .arg(&latex)
            .arg("--ini")
            .arg("--no-banner")
            .current_dir(&working_directory)
            .stdout(transcript_file)
            .stderr(stderr_file)
            .env("AITEX_BUILD_LATEX_FORMAT", "1")
            .env("AITEX_FORMAT_DEPENDENCIES", &dependency_log)
            .env("AITEX_MAX_COMMANDS", "2000000000")
            .env("AITEX_MAX_STEPS", "2000000000")
            .env(
                "TEXINPUTS",
                format!(
                    "{}:{}",
                    working_directory.to_string_lossy(),
                    std::env::var("TEXINPUTS").unwrap_or_default()
                ),
            )
            .env_remove("AITEX_FORMAT");
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start the format builder: {error}"))?;
        let timeout = std::env::var("AITEX_MKFMT_TIMEOUT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(300);
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("cannot wait for the format builder: {error}"))?
            {
                break status;
            }
            if started.elapsed() >= std::time::Duration::from_secs(timeout) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "could not build the LaTeX format within {timeout} seconds\n{}",
                    format_failure_tail(&transcript)
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        let generated = working_directory.join(format!(
            "{}.fmt",
            latex
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("latex")
        ));
        if !status.success() || !generated.is_file() {
            return Err(format!(
                "could not build a LaTeX format from {}\n{}",
                latex.display(),
                format_failure_tail(&transcript)
            ));
        }

        let generated_bytes = std::fs::metadata(&generated)
            .map_err(|error| format!("cannot inspect generated format: {error}"))?
            .len();
        if generated_bytes > MAX_CACHE_OBJECT_BYTES {
            return Err("generated format exceeds the 512 MiB cache limit".to_string());
        }
        let bytes = std::fs::read(&generated)
            .map_err(|error| format!("cannot read generated format: {error}"))?;
        aitex_engine::fast::Fast::check_format_header(&bytes)
            .map_err(|error| format!("generated an unusable format: {error}"))?;
        let mut dependencies = read_format_dependency_log(&dependency_log)?;
        dependencies.retain(|dependency| {
            dependency.resolved.as_deref().is_none_or(|resolved| {
                let resolved = Path::new(resolved);
                resolved.is_absolute() && !resolved.starts_with(&working_directory)
            })
        });
        dependencies.extend(extra_dependencies);
        let dependencies = normalized_dependencies(&dependencies);
        let digest = sha256_hex(&bytes);
        let manifest = CacheManifest {
            kind: CacheKind::Format,
            key: environment_key.to_string(),
            payload_bytes: bytes.len() as u64,
            payload_sha256: digest.clone(),
            created_unix: now_unix(),
            clock: None,
            dependencies,
        };
        atomic_write(path, &bytes)
            .map_err(|error| format!("cannot publish local format: {error}"))?;
        atomic_write(
            &format_manifest_path(path),
            manifest_text(&manifest).as_bytes(),
        )
        .map_err(|error| format!("cannot publish format manifest: {error}"))?;
        remember_format_identity(path, digest);
        Ok(excluded_ibycus)
    })();

    let _ = std::fs::remove_dir_all(&working_directory);
    drop(lock);
    result.map(|_| true)
}

fn prepare_latex_format(destination: Option<PathBuf>) -> Result<(PathBuf, bool), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let environment_key = format_environment_key();
    if destination.is_none()
        && let Ok(executable) = std::env::current_exe()
        && let Some(path) = developer_format_candidates(&executable)
            .into_iter()
            .find(|path| validate_local_format(path, &environment_key).is_ok())
    {
        return Ok((path, false));
    }
    let path = destination.unwrap_or_else(|| format_cache_path(&arguments));
    if validate_local_format(&path, &environment_key).is_ok() {
        return Ok((path, false));
    }
    let started = Instant::now();
    let built = build_local_format(&path, &environment_key)?;
    if built {
        cli_phase("Format", "local TeX Live", started.elapsed());
    }
    Ok((path, built))
}

fn run_format_command(arguments: &[String]) {
    let output = arguments
        .windows(2)
        .find(|pair| pair[0] == "--output")
        .map(|pair| PathBuf::from(&pair[1]));
    match prepare_latex_format(output) {
        Ok((path, built)) => println!(
            "{} {}",
            if built { "built" } else { "ready" },
            path.display()
        ),
        Err(error) => {
            eprintln!("aitex format: {error}");
            EXIT_CODE.store(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
